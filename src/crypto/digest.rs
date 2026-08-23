//! The content digest: one keyed hash of one normalised text, shared by `recall_emission` and
//! `ingest_proposal.fingerprint`.
//!
//! The first version of this join used an unkeyed SHA-256 of the normalised plaintext. That turned
//! every emission row into a verification oracle: anyone holding a database dump could hash a
//! guessed sentence and confirm it byte for byte against a row whose content was otherwise only
//! ciphertext, and anyone holding a narrow credential could do the same through the emission
//! lookup. A private row's confidentiality then rested on nobody guessing the sentence, which is
//! no confidentiality at all for the sentences worth guessing.
//!
//! HMAC-SHA256 under a key derived from the KEK closes the dump case. Nothing can be verified
//! without the KEK, and a holder of the KEK already decrypts the rows. The derivation is labelled so
//! the digest key is a different key from the one that wraps DEKs and from the fingerprint, and a
//! compromise of one tells nothing about the others.
//!
//! **One normaliser and one function, or the join never meets.** Every producer of a content
//! digest calls [`Digester::digest`]; a second implementation anywhere gives the echo check a hash
//! that can never match a proposal, which is the quiet way to build an unreachable safety layer.

use std::sync::Arc;

use hmac::{Hmac, Mac};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use super::kek::{Kek, KeyProvider};
use crate::domain::errors::Result;

/// The label the digest key is derived under. Frozen: every stored emission and every proposal
/// fingerprint is an HMAC under the key this label produces, so changing it orphans all of them
/// at once. A new label is a new migration that deletes the old rows, never an edit here.
const DIGEST_KEY_LABEL: &[u8] = b"lumberroom/content-digest/v1";

/// 32 bytes derived from the KEK, zeroized on drop. Held for one request and never stored.
pub struct DigestKey(Zeroizing<[u8; 32]>);

impl DigestKey {
    /// HMAC-SHA256 of a fixed label under the KEK. One way: the KEK cannot be recovered from this
    /// key, so a process that only needs digests could hold this and not the KEK.
    pub fn derive(kek: &Kek) -> Self {
        let mut mac = Hmac::<Sha256>::new_from_slice(&kek[..])
            .expect("HMAC-SHA256 takes a key of any length");
        mac.update(DIGEST_KEY_LABEL);
        let tag = mac.finalize().into_bytes();
        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(&tag);
        Self(key)
    }
}

/// Produces content digests. Keyed when the deployment holds a KEK, unkeyed otherwise.
///
/// The unkeyed case is a deployment with `KEK_PROVIDER=none`. Such a store refuses every private
/// write, so every row it holds is plaintext already and a dump holder learns nothing from a hash
/// of it. The trap is the transition: switching a KEK on changes every digest, so emissions and
/// proposal fingerprints recorded before the switch never meet the ones recorded after. A
/// rejected proposal stops blocking its content once, and that is the whole cost.
pub struct Digester {
    key: Option<DigestKey>,
}

impl Digester {
    /// Reads the KEK once. The file provider reads sixty-four bytes and the env provider reads a
    /// variable, which is nothing beside the search that called this.
    pub async fn from_provider(provider: Option<&Arc<dyn KeyProvider>>) -> Result<Self> {
        let key = match provider {
            Some(p) => Some(DigestKey::derive(&p.kek().await?)),
            None => None,
        };
        Ok(Self { key })
    }

    /// A digester with no key. For tests and for the one code path that has no provider at all.
    pub fn unkeyed() -> Self {
        Self { key: None }
    }

    pub fn is_keyed(&self) -> bool {
        self.key.is_some()
    }

    /// Hex, 64 characters either way, so the column shape is the same under both constructions.
    pub fn digest(&self, text: &str) -> String {
        let normalised = normalise(text);
        match &self.key {
            Some(DigestKey(key)) => {
                let mut mac = Hmac::<Sha256>::new_from_slice(&key[..])
                    .expect("HMAC-SHA256 takes a key of any length");
                mac.update(normalised.as_bytes());
                hex::encode(mac.finalize().into_bytes())
            }
            None => hex::encode(Sha256::digest(normalised.as_bytes())),
        }
    }
}

/// Lowercase, collapse whitespace, strip terminal punctuation.
///
/// The same text has to produce the same digest whether a model wrote it with a full stop or a
/// transcript quoted it without one. `services::ingest` uses this for its substring check too, so
/// the normalisation a quote passes is the one its fingerprint was computed under.
pub fn normalise(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    collapsed.trim_end_matches(['.', '!', '?', ',', ';', ':']).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kek(fill: u8) -> Kek {
        Zeroizing::new([fill; 32])
    }

    fn keyed(fill: u8) -> Digester {
        Digester { key: Some(DigestKey::derive(&kek(fill))) }
    }

    #[test]
    fn one_digest_for_content_that_differs_only_in_shape() {
        let d = keyed(1);
        assert_eq!(d.digest("Dana prefers tabs."), d.digest("dana   prefers tabs"));
        assert_ne!(d.digest("the port is 8787"), d.digest("the port is 8080"));
    }

    #[test]
    fn a_keyed_digest_is_not_the_plain_hash_of_the_text() {
        let plain = Digester::unkeyed().digest("the port is 8787");
        let keyed = keyed(1).digest("the port is 8787");
        assert_ne!(
            plain, keyed,
            "a dump holder must not be able to verify a guess without the KEK"
        );
        assert_eq!(plain, hex::encode(Sha256::digest(b"the port is 8787")));
    }

    #[test]
    fn two_keks_give_two_digests_for_one_text() {
        assert_ne!(keyed(1).digest("same text"), keyed(2).digest("same text"));
    }

    #[test]
    fn the_digest_key_is_not_the_kek_and_not_the_fingerprint() {
        let k = kek(7);
        let derived = DigestKey::derive(&k);
        assert_ne!(&derived.0[..], &k[..]);
        let derived_hex = hex::encode(&derived.0[..]);
        assert_ne!(&derived_hex[..32], super::super::kek::fingerprint(&k).as_str());
    }

    #[test]
    fn both_constructions_produce_sixty_four_hex_characters() {
        for d in [Digester::unkeyed(), keyed(3)] {
            let out = d.digest("anything");
            assert_eq!(out.len(), 64);
            assert!(out.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn normalisation_strips_terminal_punctuation_and_case() {
        assert_eq!(normalise("  Hello,   World!  "), "hello, world");
        assert_eq!(normalise("x:"), "x");
        assert_eq!(normalise(""), "");
    }
}
