//! The archive routes: one file out, one file in.
//!
//! Two endpoints and no business logic. They authenticate, turn a request body into a `Sealing`,
//! hand the bytes to `services::archive`, and translate what comes back. Every grant check is the
//! service's, including the one that decides whether this caller may read the whole store at all.
//!
//! # The passphrase
//!
//! It arrives in the request body on both routes, including the GET. A query string reaches the
//! access log of every proxy in front of this server and a header reaches most of them, and this
//! one value opens every private fact in the store. A body on GET is unusual and an intermediary
//! is free to drop it; `lumberroom archive export` is the only client of this route and its
//! transport sends one.
//!
//! Nothing below writes the passphrase to a log, an error message or a response. `serde_json`
//! error text quotes the input around the failure, which is why this module parses bodies by hand
//! and reports the position instead of forwarding the message.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use secrecy::SecretString;
use serde::de::DeserializeOwned;
use serde::Deserialize;

use lumberroom_archive::container::Sealing;
use lumberroom_archive::reader::Archive;
use lumberroom_archive::ArchiveError;

use super::{authed, domain_error, Http};
use crate::domain::errors::DomainError;
use crate::services::archive;

/// The largest import body this surface accepts, before base64 and before decompression.
///
/// Distinct from `ARCHIVE_MAX_DECOMPRESSED_BYTES`, which bounds what a file inflates to. This one
/// bounds what reaches the process at all, and axum's own default of 2MB would refuse any real
/// store. Base64 costs a third on top of the file, so the archive this admits is around 190MB.
const MAX_IMPORT_BYTES: usize = 256 * 1024 * 1024;

pub fn routes() -> Router<Http> {
    Router::new()
        .route("/admin/archive/export", get(export).post(export))
        .route("/admin/archive/import", post(import))
        .layer(DefaultBodyLimit::max(MAX_IMPORT_BYTES))
}

/// No `Debug`, here or on `ImportBody`. Both hold the passphrase, and a derived formatter is how
/// one reaches a log line somebody adds six months from now.
#[derive(Default, Deserialize)]
struct ExportBody {
    /// Reaches `Sealing` and nothing else.
    #[serde(default)]
    passphrase: Option<String>,
    /// The caller saying out loud that it wants a file anyone holding it can read.
    #[serde(default)]
    allow_plaintext: bool,
}

/// The whole store as one `.lumber` file.
///
/// `Content-Disposition` names the file so a browser and `curl -OJ` both land on something with the
/// right extension. The bytes are already sealed when they get here, so nothing on this path can
/// downgrade what the caller asked for.
async fn export(State(http): State<Http>, headers: HeaderMap, body: Bytes) -> Response {
    let ctx = match authed(&http, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    // An absent body is the same request as an empty one. It then fails on the missing passphrase,
    // which is the message worth sending.
    let asked: ExportBody = if body.is_empty() {
        ExportBody::default()
    } else {
        match decode(&body) {
            Ok(b) => b,
            Err(e) => return domain_error(&e, "archive_export_failed"),
        }
    };
    let sealing = match sealing_for(asked.passphrase, asked.allow_plaintext) {
        Ok(s) => s,
        Err(e) => return domain_error(&e, "archive_export_failed"),
    };

    match archive::build(&ctx, &sealing).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (header::CONTENT_DISPOSITION, "attachment; filename=\"store.lumber\""),
                // An archive is one store at one instant. A cache holding it is a copy of every
                // private fact in it sitting somewhere nobody chose.
                (header::CACHE_CONTROL, "no-store"),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => domain_error(&e, "archive_export_failed"),
    }
}

#[derive(Deserialize)]
struct ImportBody {
    /// The `.lumber` file, base64. NDJSON carries no bytes and the CLI speaks JSON to every other
    /// route on this surface.
    archive_base64: String,
    #[serde(default)]
    passphrase: Option<String>,
    #[serde(default)]
    allow_plaintext: bool,
    /// `false` merges into whatever this install already holds. `true` restores ids and timestamps
    /// into an empty one, and the service refuses a target that is not empty.
    #[serde(default)]
    restore: bool,
    /// Decrypt, parse, validate and report. Nothing is written.
    #[serde(default)]
    dry_run: bool,
    /// The id map from a run that stopped halfway. Idempotence lives here: an archive id already in
    /// this map is skipped rather than written a second time.
    #[serde(default)]
    prior_id_map: HashMap<String, String>,
}

/// Apply an archive, or say what applying it would do.
///
/// The report comes back whether or not anything was written, because the dry run and the commit
/// answer the same question and a caller comparing the two should not have to parse two shapes.
async fn import(State(http): State<Http>, headers: HeaderMap, body: Bytes) -> Response {
    let ctx = match authed(&http, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let asked: ImportBody = match decode(&body) {
        Ok(b) => b,
        Err(e) => return domain_error(&e, "archive_import_failed"),
    };
    let sealing = match sealing_for(asked.passphrase, asked.allow_plaintext) {
        Ok(s) => s,
        Err(e) => return domain_error(&e, "archive_import_failed"),
    };

    let raw = match base64::engine::general_purpose::STANDARD.decode(&asked.archive_base64) {
        Ok(r) => r,
        Err(_) => {
            return domain_error(
                &DomainError::validation("archive_base64 is not valid base64"),
                "archive_import_failed",
            )
        }
    };

    // The ceiling is the deployment's, read from config like every other setting. The crate's own
    // constant is the default behind it and is never read here.
    let ceiling = ctx.cfg.quality.archive_max_decompressed_bytes;
    let incoming = match Archive::open(&raw, &sealing, ceiling) {
        Ok(a) => a,
        Err(e) => return domain_error(&refused(e), "archive_import_failed"),
    };

    let mode = if asked.restore { archive::Mode::Restore } else { archive::Mode::Merge };
    match archive::apply(&ctx, &incoming, mode, asked.dry_run, &asked.prior_id_map).await {
        Ok(report) => Json(report).into_response(),
        Err(e) => domain_error(&e, "archive_import_failed"),
    }
}

/// Turn the caller's two fields into a `Sealing`, and refuse the silent third case.
///
/// A missing passphrase becomes a refusal rather than a plaintext file. The archive holds every
/// private fact in the store, so writing one unencrypted has to be something the caller asked for
/// in words, matching the refusal `crates/ops` already makes for a database dump.
fn sealing_for(passphrase: Option<String>, allow_plaintext: bool) -> Result<Sealing, DomainError> {
    match passphrase.filter(|p| !p.is_empty()) {
        Some(p) => Ok(Sealing::Passphrase(SecretString::from(p))),
        None if allow_plaintext => Ok(Sealing::Plaintext),
        None => Err(DomainError::validation(
            "an archive carries every private fact in the store, so it needs a passphrase. Send \
             one, or send allow_plaintext to accept a file that anyone holding it can read.",
        )),
    }
}

/// Parse a request body without letting serde's message reach the caller.
///
/// `serde_json` quotes the input around the failure, and this body carries a passphrase beside the
/// archive. The position goes to the log and the caller hears which shape was expected.
fn decode<T: DeserializeOwned>(raw: &Bytes) -> Result<T, DomainError> {
    serde_json::from_slice(raw).map_err(|e| {
        tracing::warn!(line = e.line(), column = e.column(), "archive request body did not parse");
        DomainError::validation(
            "the request body is not the JSON this route expects. The parser's message is not \
             repeated here, because it quotes the body and the body carries a passphrase.",
        )
    })
}

/// Map a format failure onto the taxonomy this surface already has.
///
/// Every case is the caller's file rather than this server, so each one is a validation failure and
/// the reason travels back. A wrong passphrase included: the caller can send another, and 401 would
/// tell it to reauthenticate against a server it is already authenticated to.
fn refused(e: ArchiveError) -> DomainError {
    match e {
        // The record itself stays out of the message. An archive holds private content and an
        // error travels into whatever transcript, log or bug report it lands in next.
        ArchiveError::Malformed { line, source } => DomainError::validation(format!(
            "record {line} of this archive is not valid JSON. The record is not repeated here \
             on purpose."
        ))
        .with_source(source),
        ArchiveError::Io(source) => {
            DomainError::validation("this archive did not decompress").with_source(source)
        }
        other => DomainError::validation(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_passphrase_seals_and_an_empty_one_does_not_pass_for_consent() {
        assert!(matches!(
            sealing_for(Some("correct horse battery staple".into()), false),
            Ok(Sealing::Passphrase(_))
        ));
        // An empty string is a client that read a blank prompt, not a client asking for plaintext.
        assert!(sealing_for(Some(String::new()), false).is_err());
        assert!(sealing_for(None, false).is_err());
        assert!(matches!(sealing_for(None, true), Ok(Sealing::Plaintext)));
    }

    #[test]
    fn the_import_body_needs_only_the_bytes() {
        let body = Bytes::from(r#"{"archive_base64":"YmFzZTY0"}"#);
        let asked: ImportBody = decode(&body).unwrap();
        assert_eq!(asked.archive_base64, "YmFzZTY0");
        assert!(!asked.restore, "merge is the default, because restore refuses a live store");
        assert!(!asked.dry_run);
        assert!(asked.prior_id_map.is_empty());
    }

    #[test]
    fn a_parse_failure_does_not_carry_the_body_back() {
        let body = Bytes::from(r#"{"archive_base64":"YmFzZTY0","passphrase":9}"#);
        let Err(e) = decode::<ImportBody>(&body) else {
            panic!("a numeric passphrase is not a passphrase")
        };
        assert!(!e.client_message().contains('9'));
    }
}
