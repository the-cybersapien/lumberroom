//! The two AES-256-GCM layers that make up one private row.
//!
//! Content is sealed under a DEK that exists for that row alone, and the DEK is wrapped under the
//! KEK. The row keeps the ciphertext, both nonces and the wrapped DEK; the KEK stays outside the
//! database (see `super` for what that does and does not defend).

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use uuid::Uuid;
use zeroize::Zeroizing;

use super::kek::Kek;
use crate::domain::errors::{DomainError, Result};

/// The algorithm label written to the enc_alg column.
///
/// Frozen. It lands in a column, [`open`] refuses a row that carries anything else, and a new
/// scheme takes a new label rather than editing this one.
pub const ALG: &str = "aes-256-gcm/envelope-v1";

/// 96 bits. The width AES-GCM is specified and reviewed for; a different width changes the counter
/// construction and is not worth being creative about.
const NONCE_LEN: usize = 12;
const DEK_LEN: usize = 32;

/// What the client is told when a row will not open, whatever the reason.
const OPAQUE: &str = "could not decrypt content";

/// The ciphertext side of one row. Field names match the migration 008 columns.
#[derive(Debug, Clone)]
pub struct SealedContent {
    pub content_ct: Vec<u8>,
    pub content_nonce: Vec<u8>,
    pub dek_wrapped: Vec<u8>,
    pub dek_nonce: Vec<u8>,
    pub enc_alg: &'static str,
}

/// AES-256-GCM twice: content under a fresh DEK, DEK under the KEK. The row id is bound in as
/// associated data so a ciphertext moved to another row fails to open rather than decrypting.
pub fn seal(kek: &Kek, row_id: Uuid, plaintext: &str) -> Result<SealedContent> {
    let mut dek: Zeroizing<[u8; DEK_LEN]> = Zeroizing::new([0u8; DEK_LEN]);
    super::fill_random(&mut dek[..])?;

    let mut content_nonce = [0u8; NONCE_LEN];
    let mut dek_nonce = [0u8; NONCE_LEN];
    super::fill_random(&mut content_nonce)?;
    super::fill_random(&mut dek_nonce)?;

    let aad = associated_data(&row_id);

    // Separate nonces for the two layers. They are under different keys, so sharing one would be
    // safe, and generating two costs nothing and removes the question.
    let content_ct = cipher(&dek[..])?
        .encrypt(&Nonce::from(content_nonce), Payload { msg: plaintext.as_bytes(), aad })
        .map_err(|e| DomainError::internal(format!("content seal failed: {e}")))?;

    let dek_wrapped = cipher(&kek[..])?
        .encrypt(&Nonce::from(dek_nonce), Payload { msg: &dek[..], aad })
        .map_err(|e| DomainError::internal(format!("dek wrap failed: {e}")))?;

    // `dek` drops here and zeroizes. What the cipher expanded it into does not: the aes-gcm
    // `zeroize` feature is off in this build, so the round-key schedule is left to the allocator.
    // Turning it on is a Cargo.toml change and belongs to whoever owns that file.
    Ok(SealedContent {
        content_ct,
        content_nonce: content_nonce.to_vec(),
        dek_wrapped,
        dek_nonce: dek_nonce.to_vec(),
        enc_alg: ALG,
    })
}

pub fn open(kek: &Kek, row_id: Uuid, sealed: &SealedContent) -> Result<String> {
    if sealed.enc_alg != ALG {
        // Worth its own log line rather than folding into the generic failure: this one says "run
        // the migration", the others say "check the key".
        tracing::error!(
            row_id = %row_id,
            enc_alg = sealed.enc_alg,
            expected = ALG,
            "row was sealed by an algorithm this build does not implement"
        );
        return Err(DomainError::internal(OPAQUE));
    }

    // A nonce arrives from a `bytea` column that anything with database write access can set, and
    // the convenient `Nonce::from_slice` panics on any length but 12. Convert fallibly and treat a
    // wrong length as one more way this row does not open.
    let content_nonce = nonce(&sealed.content_nonce).ok_or_else(|| unopenable(row_id, "nonce"))?;
    let dek_nonce = nonce(&sealed.dek_nonce).ok_or_else(|| unopenable(row_id, "nonce"))?;

    let aad = associated_data(&row_id);

    let dek = Zeroizing::new(
        cipher(&kek[..])?
            .decrypt(&Nonce::from(dek_nonce), Payload { msg: &sealed.dek_wrapped, aad })
            .map_err(|_| unopenable(row_id, "dek-unwrap"))?,
    );
    if dek.len() != DEK_LEN {
        return Err(unopenable(row_id, "dek-length"));
    }

    let content = cipher(&dek[..])?
        .decrypt(&Nonce::from(content_nonce), Payload { msg: &sealed.content_ct, aad })
        .map_err(|_| unopenable(row_id, "content"))?;

    // The plaintext leaves as a String because the read path renders it. The DEK does not leave at
    // all, which is the boundary that matters.
    String::from_utf8(content).map_err(|_| unopenable(row_id, "utf8"))
}

/// The row id, bound into both layers as AES-GCM associated data.
///
/// Without it, anyone with database write access swaps one row's ciphertext, nonce and wrapped DEK
/// for another's and the server decrypts it under the wrong identity: the wrong namespace, the
/// wrong sensitivity, the wrong grant check. GCM authenticates the bytes, not where they were
/// found, so the row identity has to be authenticated on purpose.
///
/// Raw uuid bytes rather than a formatted string, so there is no hyphenation or casing choice for a
/// later change to get wrong and strand every existing row.
fn associated_data(row_id: &Uuid) -> &[u8] {
    row_id.as_bytes()
}

/// `None` for any length but 12, which the caller turns into the same failure a bad tag produces.
fn nonce(raw: &[u8]) -> Option<[u8; NONCE_LEN]> {
    <[u8; NONCE_LEN]>::try_from(raw).ok()
}

fn cipher(key: &[u8]) -> Result<Aes256Gcm> {
    Aes256Gcm::new_from_slice(key)
        .map_err(|_| DomainError::internal("envelope key is not 256 bits"))
}

/// One error for every way a row can fail to open.
///
/// The client must not learn whether the key was wrong or the ciphertext was edited. An attacker
/// with database write access who can tell those apart has an oracle: flip a byte, read the
/// response, and learn which rows a given key opens. `Internal` already hides the message from a
/// client, and the message stays generic anyway so that changing the kind later cannot reopen the
/// oracle by accident. The operator gets the stage from the log.
fn unopenable(row_id: Uuid, stage: &'static str) -> DomainError {
    tracing::warn!(row_id = %row_id, stage, "private row failed to open");
    DomainError::internal(OPAQUE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> Kek {
        Zeroizing::new([seed; DEK_LEN])
    }

    #[test]
    fn a_sealed_row_opens_back_to_its_plaintext() {
        let (kek, id) = (key(7), Uuid::new_v4());
        // A long one too, because length is the only property of the input GCM treats differently.
        let long = "a".repeat(8192);
        for plaintext in ["", "hunter2", "ledger: ₹4,20,000 on 2026-08-19", long.as_str()] {
            let sealed = seal(&kek, id, plaintext).unwrap();
            assert_eq!(open(&kek, id, &sealed).unwrap(), plaintext);
        }
    }

    #[test]
    fn a_sealed_row_records_the_shapes_the_migration_expects() {
        let sealed = seal(&key(7), Uuid::new_v4(), "x").unwrap();
        assert_eq!(sealed.enc_alg, ALG);
        assert_eq!(sealed.content_nonce.len(), NONCE_LEN);
        assert_eq!(sealed.dek_nonce.len(), NONCE_LEN);
        // 32 bytes of DEK plus a 16-byte GCM tag.
        assert_eq!(sealed.dek_wrapped.len(), DEK_LEN + 16);
    }

    #[test]
    fn the_plaintext_never_appears_in_the_ciphertext() {
        let sealed = seal(&key(7), Uuid::new_v4(), "hunter2").unwrap();
        assert!(!sealed.content_ct.windows(7).any(|w| w == b"hunter2"));
    }

    #[test]
    fn a_different_kek_cannot_open_the_row() {
        let id = Uuid::new_v4();
        let sealed = seal(&key(7), id, "personal:finance").unwrap();
        assert!(open(&key(8), id, &sealed).is_err());
    }

    #[test]
    fn a_row_moved_under_another_id_fails_to_open() {
        let kek = key(7);
        let sealed = seal(&kek, Uuid::new_v4(), "the salary number").unwrap();
        assert!(
            open(&kek, Uuid::new_v4(), &sealed).is_err(),
            "row id is associated data, so a swapped row must not decrypt"
        );
    }

    #[test]
    fn flipping_one_byte_of_the_content_fails_to_open() {
        let (kek, id) = (key(7), Uuid::new_v4());
        let mut sealed = seal(&kek, id, "the salary number").unwrap();
        sealed.content_ct[0] ^= 0x01;
        assert!(open(&kek, id, &sealed).is_err());
    }

    #[test]
    fn flipping_one_byte_of_the_wrapped_dek_fails_to_open() {
        let (kek, id) = (key(7), Uuid::new_v4());
        let mut sealed = seal(&kek, id, "the salary number").unwrap();
        sealed.dek_wrapped[0] ^= 0x01;
        assert!(open(&kek, id, &sealed).is_err());
    }

    #[test]
    fn flipping_one_byte_of_either_nonce_fails_to_open() {
        let (kek, id) = (key(7), Uuid::new_v4());

        let mut content = seal(&kek, id, "x").unwrap();
        content.content_nonce[0] ^= 0x01;
        assert!(open(&kek, id, &content).is_err());

        let mut dek = seal(&kek, id, "x").unwrap();
        dek.dek_nonce[0] ^= 0x01;
        assert!(open(&kek, id, &dek).is_err());
    }

    #[test]
    fn two_seals_of_one_plaintext_share_no_ciphertext_and_no_nonce() {
        let (kek, id) = (key(7), Uuid::new_v4());
        let a = seal(&kek, id, "same input").unwrap();
        let b = seal(&kek, id, "same input").unwrap();

        // A fresh DEK and a fresh nonce per seal, so two identical facts do not look identical.
        assert_ne!(a.content_ct, b.content_ct);
        assert_ne!(a.content_nonce, b.content_nonce);
        assert_ne!(a.dek_nonce, b.dek_nonce);
        assert_ne!(a.dek_wrapped, b.dek_wrapped);
    }

    #[test]
    fn a_nonce_of_the_wrong_length_is_an_error_and_not_a_panic() {
        let (kek, id) = (key(7), Uuid::new_v4());

        let mut short = seal(&kek, id, "x").unwrap();
        short.content_nonce.truncate(8);
        assert!(open(&kek, id, &short).is_err());

        let mut long = seal(&kek, id, "x").unwrap();
        long.dek_nonce.push(0);
        assert!(open(&kek, id, &long).is_err());

        let mut empty = seal(&kek, id, "x").unwrap();
        empty.content_nonce.clear();
        assert!(open(&kek, id, &empty).is_err());
    }

    #[test]
    fn a_row_sealed_by_an_unknown_algorithm_is_refused() {
        let (kek, id) = (key(7), Uuid::new_v4());
        let mut sealed = seal(&kek, id, "x").unwrap();
        sealed.enc_alg = "aes-256-gcm/envelope-v2";
        assert!(open(&kek, id, &sealed).is_err());
    }

    #[test]
    fn a_failure_to_open_tells_a_client_nothing_about_why() {
        let (kek, id) = (key(7), Uuid::new_v4());
        let sealed = seal(&kek, id, "x").unwrap();

        let wrong_key = open(&key(8), id, &sealed).unwrap_err();
        let mut tampered = sealed.clone();
        tampered.content_ct[0] ^= 0x01;
        let edited = open(&kek, id, &tampered).unwrap_err();

        assert_eq!(
            wrong_key.client_message(),
            edited.client_message(),
            "a client that can tell these apart has a decryption oracle"
        );
    }
}
