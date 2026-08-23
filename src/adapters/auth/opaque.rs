//! Access tokens issued by the built-in authorization server (decision 0002).
//!
//! The token is opaque and stored only as a SHA-256 hash, so validation is one indexed lookup
//! rather than a signature check. That buys revocation that takes effect on the next request, which
//! a self-signed JWT cannot offer without a revocation list that is this table anyway.
//!
//! Every path through here fails CLOSED. An earlier build of this service granted everything when
//! no grant matched, which meant any token the issuer signed held full access. A token whose client
//! row is missing, revoked, unconsented, or carries an empty grant authorizes nothing.

use async_trait::async_trait;
use chrono::Utc;
use std::sync::Arc;

use super::{bearer, fingerprint, Authenticator};
use crate::domain::errors::{DomainError, Result};
use crate::domain::oauth::hash_token;
use crate::domain::types::Principal;
use crate::ports::OauthStore;

pub struct OpaqueTokenAuthenticator {
    store: Arc<dyn OauthStore>,
}

impl OpaqueTokenAuthenticator {
    pub fn new(store: Arc<dyn OauthStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Authenticator for OpaqueTokenAuthenticator {
    fn mode(&self) -> &'static str {
        "oauth"
    }

    async fn authenticate(&self, authorization: Option<&str>) -> Result<Principal> {
        let token = bearer(authorization)?;

        // The hash is the primary key, so an unknown token costs one index probe. No constant-time
        // comparison is needed or possible here: the database compares hashes, and an attacker who
        // could time the difference would still need a preimage.
        let record = self
            .store
            .find_token(&hash_token(token))
            .await?
            .ok_or_else(|| DomainError::forbidden("invalid bearer token"))?;

        // Expiry before revocation, so a stale token that was also revoked reads as expired: the
        // client's correct response is to refresh, and "revoked" would send it to a login screen.
        if record.expires_at <= Utc::now() {
            return Err(DomainError::forbidden("access token has expired"));
        }
        if record.revoked_at.is_some() {
            return Err(DomainError::forbidden("access token has been revoked"));
        }

        // A token outliving its client is the case the fail-closed rule exists for. The client row
        // holds the grant, so no client row means no grant, which means no access.
        let client = self.store.find_client(&record.client_id).await?.ok_or_else(|| {
            DomainError::forbidden("the client this token was issued to no longer exists")
        })?;
        if !client.is_live() {
            return Err(DomainError::forbidden("this client has been revoked"));
        }
        // Registration is not authorization. A self-registered client exists and holds nothing
        // until the owner logs in and consents, and consent is what writes the grant.
        if !client.has_consent() {
            return Err(DomainError::forbidden(
                "this client is registered but not yet approved by the owner",
            ));
        }

        // Reads and writes are separate axes, so a write-only or read-only client is legitimate.
        // Both empty is not: it is a consented row that somehow lost its grant, and admitting it
        // would produce a principal that authenticates and then fails every operation with a
        // confusing 403 instead of a clear one here.
        if client.read.is_empty() && client.write.is_empty() {
            return Err(DomainError::forbidden(
                "this client holds no namespace grant",
            ));
        }

        // Fire and forget: last_used_at is an observability field, and a failed UPDATE must not
        // fail the request that was otherwise authorized. Called after every check so a rejected
        // token cannot be used to keep a revoked client looking active.
        self.store.touch_client(&record.client_id);

        Ok(Principal {
            // client_id, never client_name. A client picks its own name at registration and at
            // least one surface reports itself as another product's name; the name is for the
            // consent screen and the audit log, the id is what authorizes.
            client: client.client_id.clone(),
            token_id: fingerprint(token),
            mode: "oauth",
            scopes: record
                .scope
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            read: client.read.clone(),
            write: client.write.clone(),
            registry_write: client.registry_write,
            sealed_capable: client.sealed_capable,
            may_delete: client.may_delete,
            may_ingest: client.may_ingest,
            may_read_history: client.may_read_history,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::policy::NamespaceGrant;
    use crate::domain::types::Sensitivity;
    use crate::ports::oauth::*;
    use chrono::{Duration, Utc};
    use std::sync::Mutex;

    /// Holds one token and one client. Every method the authenticator does not call panics rather
    /// than returning a default, so a future change that starts calling one is visible.
    struct Store {
        token: Option<AccessTokenRecord>,
        client: Option<OauthClientRecord>,
        touched: Mutex<Vec<String>>,
    }

    fn token(client_id: &str) -> AccessTokenRecord {
        AccessTokenRecord {
            token_hash: hash_token("opaque-token"),
            client_id: client_id.into(),
            scope: "memory.read memory.write".into(),
            resource: Some("https://lumberroom.example.com/mcp".into()),
            family_id: uuid::Uuid::nil(),
            expires_at: Utc::now() + Duration::hours(1),
            revoked_at: None,
        }
    }

    fn client(client_id: &str) -> OauthClientRecord {
        OauthClientRecord {
            client_id: client_id.into(),
            secret_hash: None,
            client_name: "Claude Code".into(),
            redirect_uris: vec![],
            grant_types: vec!["authorization_code".into()],
            registered_via: "dcr".into(),
            software_id: None,
            read: vec![NamespaceGrant::new("user:me", Sensitivity::Private)],
            write: vec![NamespaceGrant::open("global")],
            registry_write: false,
            sealed_capable: false,
            may_delete: false,
            may_ingest: false,
            may_read_history: false,
            consented_at: Some(Utc::now()),
            profile: Some("standard".into()),
            created_at: Utc::now(),
            last_used_at: None,
            revoked_at: None,
        }
    }

    fn store(token: AccessTokenRecord, client: Option<OauthClientRecord>) -> Arc<Store> {
        Arc::new(Store { token: Some(token), client, touched: Mutex::new(vec![]) })
    }

    fn auth(store: Arc<Store>) -> OpaqueTokenAuthenticator {
        OpaqueTokenAuthenticator::new(store)
    }

    #[async_trait]
    impl OauthStore for Store {
        async fn find_token(&self, token_hash: &str) -> Result<Option<AccessTokenRecord>> {
            Ok(self.token.clone().filter(|t| t.token_hash == token_hash))
        }
        async fn find_client(&self, client_id: &str) -> Result<Option<OauthClientRecord>> {
            Ok(self.client.clone().filter(|c| c.client_id == client_id))
        }
        fn touch_client(&self, client_id: &str) {
            self.touched.lock().unwrap().push(client_id.to_string());
        }

        async fn register_client(&self, _c: NewOauthClient) -> Result<()> {
            unimplemented!("not on the authentication path")
        }
        async fn list_clients(&self, _include_revoked: bool) -> Result<Vec<OauthClientRecord>> {
            unimplemented!("not on the authentication path")
        }
        async fn set_client_grant(&self, _client_id: &str, _g: ClientGrantUpdate) -> Result<()> {
            unimplemented!("not on the authentication path")
        }
        async fn revoke_client(&self, _client_id: &str) -> Result<bool> {
            unimplemented!("not on the authentication path")
        }
        async fn insert_code(&self, _c: NewAuthCode) -> Result<()> {
            unimplemented!("not on the authentication path")
        }
        async fn consume_code(&self, _code_hash: &str) -> Result<CodeOutcome> {
            unimplemented!("not on the authentication path")
        }
        async fn insert_token(&self, _t: NewAccessToken) -> Result<()> {
            unimplemented!("not on the authentication path")
        }
        async fn revoke_token(&self, _token_hash: &str) -> Result<bool> {
            unimplemented!("not on the authentication path")
        }
        async fn insert_refresh(&self, _r: NewRefreshToken) -> Result<()> {
            unimplemented!("not on the authentication path")
        }
        async fn rotate_refresh(&self, _token_hash: &str) -> Result<RefreshOutcome> {
            unimplemented!("not on the authentication path")
        }
        async fn revoke_family(&self, _family_id: uuid::Uuid) -> Result<()> {
            unimplemented!("not on the authentication path")
        }
        async fn purge_expired(&self) -> Result<u64> {
            unimplemented!("not on the authentication path")
        }
    }

    const HEADER: Option<&str> = Some("Bearer opaque-token");

    #[tokio::test]
    async fn builds_the_principal_from_the_clients_grant() {
        let s = store(token("c1"), Some(client("c1")));
        let p = auth(Arc::clone(&s)).authenticate(HEADER).await.unwrap();
        assert_eq!(p.client, "c1", "the id authorizes, never the self-chosen name");
        assert_eq!(p.mode, "oauth");
        assert_eq!(p.read_patterns(), vec!["user:me"]);
        assert_eq!(p.read[0].max, Sensitivity::Private);
        assert_eq!(p.write_patterns(), vec!["global"]);
        assert_eq!(p.scopes, vec!["memory.read", "memory.write"]);
        assert_eq!(p.token_id.len(), 12);
        assert!(!p.token_id.contains("opaque"));
        assert_eq!(s.touched.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn carries_the_sealed_and_delete_capabilities_the_owner_granted() {
        let mut c = client("c1");
        c.sealed_capable = true;
        c.may_delete = true;
        c.may_ingest = true;
        c.registry_write = true;
        let p = auth(store(token("c1"), Some(c))).authenticate(HEADER).await.unwrap();
        assert!(p.sealed_capable);
        assert!(p.may_delete);
        assert!(p.may_ingest);
        assert!(p.registry_write);
    }

    #[tokio::test]
    async fn rejects_a_token_the_store_does_not_know() {
        let s = store(token("c1"), Some(client("c1")));
        let err = auth(s).authenticate(Some("Bearer other-token")).await.unwrap_err();
        assert_eq!(err.kind.http_status(), 403);
    }

    #[tokio::test]
    async fn rejects_an_expired_token() {
        let mut t = token("c1");
        t.expires_at = Utc::now() - Duration::seconds(1);
        let s = store(t, Some(client("c1")));
        let err = auth(Arc::clone(&s)).authenticate(HEADER).await.unwrap_err();
        assert!(err.client_message().contains("expired"));
        assert!(s.touched.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn rejects_a_revoked_token() {
        let mut t = token("c1");
        t.revoked_at = Some(Utc::now());
        let err = auth(store(t, Some(client("c1")))).authenticate(HEADER).await.unwrap_err();
        assert!(err.client_message().contains("revoked"));
    }

    #[tokio::test]
    async fn rejects_a_token_whose_client_row_is_gone() {
        let err = auth(store(token("c1"), None)).authenticate(HEADER).await.unwrap_err();
        assert_eq!(err.kind.http_status(), 403);
    }

    #[tokio::test]
    async fn rejects_a_live_token_held_by_a_revoked_client() {
        let mut c = client("c1");
        c.revoked_at = Some(Utc::now());
        let s = store(token("c1"), Some(c));
        let err = auth(Arc::clone(&s)).authenticate(HEADER).await.unwrap_err();
        assert_eq!(err.kind.http_status(), 403);
        assert!(
            s.touched.lock().unwrap().is_empty(),
            "a revoked client must not keep looking active"
        );
    }

    #[tokio::test]
    async fn rejects_a_client_the_owner_has_not_consented_to() {
        let mut c = client("c1");
        c.consented_at = None;
        let err = auth(store(token("c1"), Some(c))).authenticate(HEADER).await.unwrap_err();
        assert_eq!(err.kind.http_status(), 403);
    }

    #[tokio::test]
    async fn an_empty_grant_authorizes_nothing_rather_than_everything() {
        let mut c = client("c1");
        c.read = vec![];
        c.write = vec![];
        let err = auth(store(token("c1"), Some(c))).authenticate(HEADER).await.unwrap_err();
        assert_eq!(err.kind.http_status(), 403);
    }

    #[tokio::test]
    async fn admits_a_write_only_client() {
        let mut c = client("c1");
        c.read = vec![];
        let p = auth(store(token("c1"), Some(c))).authenticate(HEADER).await.unwrap();
        assert!(p.read.is_empty());
        assert_eq!(p.write_patterns(), vec!["global"]);
    }

    #[tokio::test]
    async fn rejects_a_request_with_no_authorization_header() {
        let err = auth(store(token("c1"), Some(client("c1")))).authenticate(None).await.unwrap_err();
        assert!(err.client_message().contains("missing Authorization header"));
    }
}
