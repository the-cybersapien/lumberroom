//! The owner's browser session for the consent screen, and the CSRF token bound to it.
//!
//! There is one account on this server, so a session is not a row in a table: it is a signed
//! statement that a password was checked before a moment in time. HMAC-SHA256 over an expiry,
//! keyed by `OAUTH_COOKIE_SECRET`, which means a restart does not sign the owner out mid-flow and
//! there is no session store to expire, replicate or leak.
//!
//! The cookie flags are not decoration. Each one is a failure that has been seen in the wild:
//!
//! - `HttpOnly`, so a script injected into any page served here cannot read it.
//! - `Secure`, except when the public URL is loopback. A local test cannot log in over a cookie the
//!   browser refuses to send back on plain http, and requiring https there means the flow can only
//!   be exercised on a deployed box.
//! - `SameSite=Lax`, never `Strict`. The consent POST happens on a page the owner arrived at by
//!   following a redirect from the client, and `Strict` withholds the cookie on exactly that
//!   navigation. The symptom is a consent screen that logs the owner out when they press Allow.
//! - `Path=/oauth`, so the cookie is not attached to `/mcp` or to a tool call.

use axum::http::HeaderMap;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::config::Config;
use crate::domain::oauth::hashes_match;

pub const COOKIE_NAME: &str = "lumberroom_owner";

/// A verified session. Holding one means the password was checked and the expiry has not passed.
#[derive(Debug, Clone)]
pub struct OwnerSession {
    pub expires_at: i64,
    /// The cookie's own signature, used to bind a CSRF token to this session and no other.
    signature: String,
}

#[derive(Debug, Clone)]
pub struct Sessions {
    secret: String,
    ttl_secs: i64,
    secure: bool,
}

impl Sessions {
    pub fn from_config(cfg: &Config) -> Self {
        Self::new(
            cfg.oauth.cookie_secret.clone(),
            cfg.oauth.session_ttl_secs,
            // Loopback is the only exception, and it is decided from the configured public URL
            // rather than from the request, because the request arrives from a proxy that terminated
            // TLS and looks like plain http from here.
            !(cfg.public_url.starts_with("http://127.0.0.1")
                || cfg.public_url.starts_with("http://localhost")
                || cfg.public_url.starts_with("http://[::1]")),
        )
    }

    pub fn new(secret: String, ttl_secs: i64, secure: bool) -> Self {
        Self { secret, ttl_secs, secure }
    }

    /// The cookie value: a version, an absolute expiry, and a signature over both.
    pub fn issue(&self, now: i64) -> String {
        let expires_at = now + self.ttl_secs;
        format!("v1.{expires_at}.{}", self.sign("session", &[&expires_at.to_string()]))
    }

    pub fn set_cookie(&self, value: &str) -> String {
        let mut cookie = format!(
            "{COOKIE_NAME}={value}; Path=/oauth; HttpOnly; SameSite=Lax; Max-Age={}",
            self.ttl_secs
        );
        if self.secure {
            cookie.push_str("; Secure");
        }
        cookie
    }

    /// Verify whichever cookie in the request carries a live signature.
    ///
    /// Every cookie of this name is tried rather than the first: a stale cookie left at `Path=/` by
    /// an earlier deployment is sent alongside the current one, in an order the browser chooses, and
    /// stopping at the first would log the owner out at random.
    pub fn verify(&self, headers: &HeaderMap, now: i64) -> Option<OwnerSession> {
        headers
            .get_all(axum::http::header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|header| header.split(';'))
            .filter_map(|pair| pair.trim().split_once('='))
            .filter(|(name, _)| *name == COOKIE_NAME)
            .find_map(|(_, value)| self.open(value.trim(), now))
    }

    /// Verify one cookie value directly. The login handler needs this: it has just issued a value
    /// and has to hold the session it represents to mint the consent form's CSRF token, before the
    /// browser has sent the cookie back.
    pub fn open(&self, value: &str, now: i64) -> Option<OwnerSession> {
        let mut parts = value.split('.');
        if parts.next()? != "v1" {
            return None;
        }
        let expires_at_raw = parts.next()?;
        let signature = parts.next()?;
        if parts.next().is_some() {
            return None;
        }

        // Signature first, then expiry. Parsing the expiry from an unverified string is fine, but
        // acting on it is not, and checking the signature first means a tampered expiry never
        // reaches a comparison that could be read as a decision.
        if !hashes_match(&self.sign("session", &[expires_at_raw]), signature) {
            return None;
        }

        let expires_at: i64 = expires_at_raw.parse().ok()?;
        if expires_at <= now {
            return None;
        }

        Some(OwnerSession { expires_at, signature: signature.to_string() })
    }

    /// A CSRF token bound to this session AND to the authorization request on the page.
    ///
    /// Both bindings are load-bearing. Without the session binding, a token minted for one login is
    /// good for the next. Without the parameter binding, a page the owner visits can POST a consent
    /// for a client that is already registered, using parameters the owner never saw, and the owner's
    /// live session makes it succeed. The consent form is the one place in this server where a single
    /// POST hands a third party the owner's memory.
    pub fn csrf(
        &self,
        session: &OwnerSession,
        client_id: &str,
        redirect_uri: &str,
        code_challenge: &str,
        state: &str,
    ) -> String {
        self.sign(
            "csrf",
            &[&session.signature, client_id, redirect_uri, code_challenge, state],
        )
    }

    pub fn csrf_ok(
        &self,
        session: &OwnerSession,
        client_id: &str,
        redirect_uri: &str,
        code_challenge: &str,
        state: &str,
        presented: &str,
    ) -> bool {
        hashes_match(
            &self.csrf(session, client_id, redirect_uri, code_challenge, state),
            presented,
        )
    }

    /// A CSRF token for one console action on one row, bound to this session.
    ///
    /// The label is `csrf-console` and not `csrf`, so a token minted for the consent form cannot be
    /// spent on the queue and a queue token cannot hand a client the store. The id is signed with
    /// the action for the same reason at a smaller scale: the queue prints every waiting proposal
    /// on one page, and a token good for any row would let an Approve button that was rendered for
    /// one proposal write another.
    pub fn console_csrf(&self, session: &OwnerSession, action: &str, id: &str) -> String {
        self.sign("csrf-console", &[&session.signature, action, id])
    }

    pub fn console_csrf_ok(
        &self,
        session: &OwnerSession,
        action: &str,
        id: &str,
        presented: &str,
    ) -> bool {
        hashes_match(&self.console_csrf(session, action, id), presented)
    }

    /// HMAC-SHA256 over a label and a list of fields, hex encoded.
    ///
    /// Each field is length-prefixed rather than joined by a delimiter. A redirect URI can contain
    /// any character a delimiter could be, and without the prefix the pair ("a", "b|c") and the pair
    /// ("a|b", "c") sign identically, which is a CSRF token that transfers between two different
    /// authorization requests.
    fn sign(&self, label: &str, fields: &[&str]) -> String {
        // HMAC accepts a key of any length, so this cannot fail. Config already refuses a secret
        // shorter than 32 characters at boot.
        let mut mac = Hmac::<Sha256>::new_from_slice(self.secret.as_bytes())
            .expect("HMAC-SHA256 accepts a key of any length");
        mac.update(label.as_bytes());
        for field in fields {
            mac.update(&(field.len() as u64).to_le_bytes());
            mac.update(field.as_bytes());
        }
        hex::encode(mac.finalize().into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sessions() -> Sessions {
        Sessions::new("k".repeat(32), 900, true)
    }

    fn cookies(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            axum::http::HeaderValue::from_str(value).unwrap(),
        );
        headers
    }

    #[test]
    fn a_freshly_issued_cookie_verifies() {
        let s = sessions();
        let value = s.issue(1_000);
        let session = s.verify(&cookies(&format!("{COOKIE_NAME}={value}")), 1_001).unwrap();
        assert_eq!(session.expires_at, 1_900);
    }

    #[test]
    fn a_cookie_verifies_alongside_unrelated_cookies() {
        let s = sessions();
        let value = s.issue(1_000);
        let header = format!("other=1; {COOKIE_NAME}={value}; another=2");
        assert!(s.verify(&cookies(&header), 1_001).is_some());
    }

    #[test]
    fn a_stale_cookie_of_the_same_name_does_not_hide_the_live_one() {
        let s = sessions();
        let live = s.issue(1_000);
        let header = format!("{COOKIE_NAME}=v1.9999.deadbeef; {COOKIE_NAME}={live}");
        assert!(s.verify(&cookies(&header), 1_001).is_some());
    }

    #[test]
    fn an_expired_cookie_is_refused() {
        let s = sessions();
        let value = s.issue(1_000);
        assert!(s.verify(&cookies(&format!("{COOKIE_NAME}={value}")), 1_900).is_none());
        assert!(s.verify(&cookies(&format!("{COOKIE_NAME}={value}")), 100_000).is_none());
    }

    #[test]
    fn an_extended_expiry_invalidates_the_signature() {
        let s = sessions();
        let value = s.issue(1_000);
        let signature = value.rsplit('.').next().unwrap();
        let forged = format!("v1.99999999.{signature}");
        assert!(s.verify(&cookies(&format!("{COOKIE_NAME}={forged}")), 1_001).is_none());
    }

    #[test]
    fn a_cookie_signed_with_another_secret_is_refused() {
        let value = sessions().issue(1_000);
        let other = Sessions::new("j".repeat(32), 900, true);
        assert!(other.verify(&cookies(&format!("{COOKIE_NAME}={value}")), 1_001).is_none());
    }

    #[test]
    fn a_malformed_cookie_is_refused_rather_than_panicking() {
        let s = sessions();
        for value in ["", "v1", "v1.1000", "v2.1000.abc", "v1.1000.abc.def", "....", "v1.abc.abc"] {
            assert!(
                s.verify(&cookies(&format!("{COOKIE_NAME}={value}")), 1_001).is_none(),
                "{value:?} must not verify"
            );
        }
    }

    #[test]
    fn no_cookie_at_all_is_no_session() {
        assert!(sessions().verify(&HeaderMap::new(), 1_001).is_none());
        assert!(sessions().verify(&cookies("other=1"), 1_001).is_none());
    }

    #[test]
    fn the_cookie_carries_httponly_lax_and_a_path_under_oauth() {
        let s = sessions();
        let cookie = s.set_cookie(&s.issue(1_000));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"), "Strict withholds the cookie on the consent POST");
        assert!(!cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Path=/oauth"));
        assert!(cookie.contains("Secure"));
    }

    #[test]
    fn the_secure_flag_is_dropped_only_for_a_loopback_deployment() {
        let local = Sessions::new("k".repeat(32), 900, false);
        assert!(!local.set_cookie("x").contains("Secure"));
    }

    #[test]
    fn a_csrf_token_is_bound_to_the_session() {
        let s = sessions();
        let a = s.open(&s.issue(1_000), 1_001).unwrap();
        let b = s.open(&s.issue(2_000), 2_001).unwrap();
        let for_a = s.csrf(&a, "c", "https://x/cb", "chal", "st");
        assert!(s.csrf_ok(&a, "c", "https://x/cb", "chal", "st", &for_a));
        assert!(
            !s.csrf_ok(&b, "c", "https://x/cb", "chal", "st", &for_a),
            "a token minted in one session must not work in another"
        );
    }

    #[test]
    fn a_csrf_token_is_bound_to_every_authorize_parameter() {
        let s = sessions();
        let session = s.open(&s.issue(1_000), 1_001).unwrap();
        let token = s.csrf(&session, "c", "https://x/cb", "chal", "st");
        assert!(!s.csrf_ok(&session, "other", "https://x/cb", "chal", "st", &token));
        assert!(!s.csrf_ok(&session, "c", "https://evil/cb", "chal", "st", &token));
        assert!(!s.csrf_ok(&session, "c", "https://x/cb", "other", "st", &token));
        assert!(!s.csrf_ok(&session, "c", "https://x/cb", "chal", "other", &token));
        assert!(!s.csrf_ok(&session, "c", "https://x/cb", "chal", "st", ""));
    }

    #[test]
    fn a_console_token_for_one_row_does_not_approve_another() {
        let s = sessions();
        let session = s.open(&s.issue(1_000), 1_001).unwrap();
        let token = s.console_csrf(&session, "approve", "row-a");
        assert!(s.console_csrf_ok(&session, "approve", "row-a", &token));
        assert!(
            !s.console_csrf_ok(&session, "approve", "row-b", &token),
            "the queue prints every row on one page, so a token has to name its own"
        );
        assert!(!s.console_csrf_ok(&session, "reject", "row-a", &token));
        assert!(!s.console_csrf_ok(&session, "approve", "row-a", ""));

        let other = s.open(&s.issue(2_000), 2_001).unwrap();
        assert!(!s.console_csrf_ok(&other, "approve", "row-a", &token));
    }

    /// The consent form is the one POST that hands a stranger the whole store. Separate labels are
    /// what stop a token taken from either surface being spent on the other.
    #[test]
    fn a_consent_token_is_not_spendable_on_the_console() {
        let s = sessions();
        let session = s.open(&s.issue(1_000), 1_001).unwrap();
        let consent = s.csrf(&session, "approve", "row-a", "chal", "st");
        assert!(!s.console_csrf_ok(&session, "approve", "row-a", &consent));

        let console = s.console_csrf(&session, "approve", "row-a");
        assert!(!s.csrf_ok(&session, "approve", "row-a", "chal", "st", &console));
    }

    #[test]
    fn fields_cannot_be_shifted_across_a_delimiter_to_forge_a_token() {
        // Without length prefixes these two sign identically, and the token from one authorization
        // request would be accepted for the other.
        let s = sessions();
        let session = s.open(&s.issue(1_000), 1_001).unwrap();
        let a = s.csrf(&session, "client", "https://x/cb", "chal", "st");
        let b = s.csrf(&session, "clienthttps://x/cb", "", "chal", "st");
        assert_ne!(a, b);
    }
}
