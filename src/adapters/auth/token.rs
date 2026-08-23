//! Bearer tokens from configuration. The Phase 1 deploy path.
//!
//! The token to client to namespace-grant mapping is identical in both modes, which is the part
//! PRD §8 asks to get right now so per-client denials land without a rewrite.

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::{bearer, Authenticator};
use crate::config::ClientGrant;
use crate::domain::errors::{DomainError, Result};
use crate::domain::types::Principal;

pub fn fingerprint(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))[..12].to_string()
}

struct Entry {
    grant: ClientGrant,
    digest: [u8; 32],
}

pub struct TokenAuthenticator {
    entries: Vec<Entry>,
}

impl TokenAuthenticator {
    pub fn new(grants: &[ClientGrant]) -> Self {
        let entries = grants
            .iter()
            .filter(|g| g.token.is_some())
            .map(|g| Entry {
                grant: g.clone(),
                digest: Sha256::digest(g.token.as_ref().unwrap().as_bytes()).into(),
            })
            .collect();
        Self { entries }
    }
}

#[async_trait]
impl Authenticator for TokenAuthenticator {
    fn mode(&self) -> &'static str {
        "token"
    }

    async fn authenticate(&self, authorization: Option<&str>) -> Result<Principal> {
        let token = bearer(authorization)?;
        let presented: [u8; 32] = Sha256::digest(token.as_bytes()).into();

        // Compare against every entry so a wrong token costs the same time as a right one.
        let mut matched: Option<&Entry> = None;
        for entry in &self.entries {
            if bool::from(presented.ct_eq(&entry.digest)) {
                matched = Some(entry);
            }
        }
        let entry = matched.ok_or_else(|| DomainError::forbidden("invalid bearer token"))?;

        Ok(Principal {
            client: entry.grant.client.clone(),
            token_id: fingerprint(token),
            mode: "token",
            scopes: vec![],
            // Each glob carries its own ceiling. An explicitly empty list stays empty: a grant that
            // says "nothing" must never widen into full access on its way through here.
            read: entry.grant.read_grants(),
            write: entry.grant.write_grants(),
            registry_write: entry.grant.registry_write,
            // Sealed capability is a property of the client rather than of the grant, so it comes
            // from the flag and not from the ceiling the grant happens to hold.
            sealed_capable: entry.grant.effective_sealed_capable(),
            may_delete: entry.grant.effective_may_delete(),
            may_ingest: entry.grant.effective_may_ingest(),
            may_read_history: entry.grant.effective_may_read_history(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::domain::policy::NamespaceGrant;
    use crate::domain::types::Sensitivity;

    fn grants() -> Vec<ClientGrant> {
        vec![
            // Unrestricted: the owner's own client, everything at every level.
            ClientGrant {
                client: "mac".into(),
                token: Some("t".repeat(32)),
                read: None,
                write: None,
                registry_write: true,
                sealed_capable: false,
                may_delete: false,
                may_ingest: false,
                may_read_history: false,
            },
            ClientGrant {
                client: "browser".into(),
                token: Some("b".repeat(32)),
                read: Some(vec![
                    NamespaceGrant::new("user:me", Sensitivity::Private),
                    NamespaceGrant::open("global"),
                ]),
                write: Some(vec![NamespaceGrant::open("global")]),
                registry_write: false,
                sealed_capable: false,
                may_delete: true,
                may_ingest: true,
                may_read_history: true,
            },
        ]
    }

    #[tokio::test]
    async fn maps_a_token_to_its_client_and_grants() {
        let a = TokenAuthenticator::new(&grants());
        let p = a.authenticate(Some(&format!("Bearer {}", "b".repeat(32)))).await.unwrap();
        assert_eq!(p.client, "browser");
        assert_eq!(p.write_patterns(), vec!["global"]);
        assert_eq!(p.read_patterns(), vec!["user:me", "global"]);
        assert_eq!(p.read[0].max, Sensitivity::Private);
        assert_eq!(p.read[1].max, Sensitivity::Open);
        assert_eq!(p.token_id.len(), 12);
        assert!(!p.registry_write);
        assert!(p.may_delete);
        assert!(p.may_ingest);
    }

    #[tokio::test]
    async fn an_unrestricted_grant_carries_every_capability_it_implies() {
        let a = TokenAuthenticator::new(&grants());
        let p = a.authenticate(Some(&format!("Bearer {}", "t".repeat(32)))).await.unwrap();
        assert_eq!(p.read, NamespaceGrant::everything());
        assert_eq!(p.write, NamespaceGrant::everything());
        assert!(p.registry_write);
        assert!(p.sealed_capable, "an unrestricted grant is the operator's own client");
        assert!(!p.may_delete, "deleting stays opt-in even for the owner's client");
        assert!(!p.may_ingest, "filling the proposal queue is asked for by name, not implied");
    }

    #[tokio::test]
    async fn a_restricted_grant_is_not_sealed_capable_unless_it_says_so() {
        let a = TokenAuthenticator::new(&grants());
        let p = a.authenticate(Some(&format!("Bearer {}", "b".repeat(32)))).await.unwrap();
        assert!(
            !p.sealed_capable,
            "a client may hold a sealed ceiling and still only ever receive ciphertext"
        );
    }

    #[tokio::test]
    async fn an_explicitly_empty_grant_stays_empty() {
        let g = vec![ClientGrant {
            client: "readonly".into(),
            token: Some("e".repeat(32)),
            read: Some(vec![]),
            write: Some(vec![]),
            registry_write: false,
            sealed_capable: false,
            may_delete: false,
            may_ingest: false,
            may_read_history: false,
        }];
        let p = TokenAuthenticator::new(&g)
            .authenticate(Some(&format!("Bearer {}", "e".repeat(32))))
            .await
            .unwrap();
        assert!(p.read.is_empty(), "empty must not widen to full access");
        assert!(p.write.is_empty());
        assert!(!p.sealed_capable);
    }

    #[tokio::test]
    async fn rejects_an_unknown_token() {
        let a = TokenAuthenticator::new(&grants());
        let err = a.authenticate(Some(&format!("Bearer {}", "z".repeat(32)))).await.unwrap_err();
        assert_eq!(err.kind.http_status(), 403);
    }

    #[tokio::test]
    async fn rejects_a_token_that_is_a_prefix_of_a_real_one() {
        let a = TokenAuthenticator::new(&grants());
        assert!(a.authenticate(Some(&format!("Bearer {}", "t".repeat(31)))).await.is_err());
    }

    #[tokio::test]
    async fn never_leaks_the_token_in_the_fingerprint() {
        let a = TokenAuthenticator::new(&grants());
        let p = a.authenticate(Some(&format!("Bearer {}", "t".repeat(32)))).await.unwrap();
        assert!(!p.token_id.contains("tttt"));
    }
}
