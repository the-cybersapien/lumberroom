//! Encryption for the `private` sensitivity level.
//!
//! Per-row envelope encryption. A fresh 256-bit DEK per row, content sealed with AES-256-GCM, the
//! DEK itself wrapped with AES-256-GCM under a KEK that never touches this disk. Per-row grain
//! earns two things: a delete becomes a crypto-shred, because dropping the row drops the only copy
//! of the key that opens it, and a KEK rotation rewraps a thousand small keys instead of
//! re-encrypting gigabytes of content.
//!
//! # Where the KEK lives, and what that defends
//!
//! The Phase 3 spec put the KEK in OCI Vault. That was written when there was one deployment
//! target. lumberroom now has to come up with `docker compose up` on any host, so the KEK sits behind
//! [`KeyProvider`], with a file-based and an environment-based implementation here and an external
//! KMS as a third implementation later.
//!
//! The local providers have a weaker threat model than the Vault design, and the difference is
//! worth stating rather than implying. **A KEK in a file on the same disk as the database defends a
//! stolen database dump and a leaked backup, and does not defend a stolen whole disk or a live
//! compromise of the box.** Whoever takes the disk image takes the key file sitting on it, and
//! whoever runs code on the box reads the KEK out of this process. [`EnvKeyProvider`] is weaker
//! again: the environment of a container is readable by anything that can inspect the container.
//!
//! Two further limits, both already written down in `docs/research/encryption-and-sensitivity.md`
//! and repeated here so nobody has to go looking. The read path decrypts in process to answer a
//! search, so a live compromise reads private content exactly as the service does. And the
//! embedding of a private row stays plaintext, because search has to work, which leaks the gist of
//! the row to anyone holding the database.
//!
//! The KEK also keys the content digest in [`digest`]. A hash of a private row's plaintext that
//! anyone could recompute is a way to confirm a guess against the ciphertext, so the hash is an
//! HMAC under a key derived from the KEK and is worth nothing to a dump holder.

pub mod digest;
pub mod envelope;
pub mod kek;

pub use digest::{DigestKey, Digester};
pub use envelope::{open, seal, SealedContent, ALG};
pub use kek::{fingerprint, generate_kek_hex, EnvKeyProvider, FileKeyProvider, Kek, KeyProvider};

use crate::domain::errors::{DomainError, Result};

/// CSPRNG bytes from the OS. Every key and every nonce in this module comes from here.
///
/// Nonces are generated and never derived or counted. A counter is the tempting option and it is
/// the one that ends badly: restore a backup, or run two writers, and the counter repeats. A
/// repeated nonce under one AES-GCM key destroys confidentiality and hands over the authentication
/// key, so both messages lose integrity too. A 96-bit random nonce has no such failure mode at the
/// volumes a personal store writes.
pub(crate) fn fill_random(dest: &mut [u8]) -> Result<()> {
    // getrandom::Error implements std::error::Error only under its `std` feature, which this build
    // does not enable, so the detail goes into the message rather than into a source.
    getrandom::fill(dest).map_err(|e| DomainError::internal(format!("os rng failure: {e}")))
}
