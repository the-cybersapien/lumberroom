//! OIDC/JWT validation against an external authorization server, Logto by default.
//!
//! The server is an OAuth resource server only: it never issues or refreshes tokens, which keeps
//! "do not hand-roll OAuth" intact while leaving the token path available.

use async_trait::async_trait;
use jsonwebtoken::{decode, decode_header, jwk::JwkSet, DecodingKey, Validation};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

use super::{bearer, fingerprint, Authenticator};
use crate::config::{ClientGrant, Config};
use crate::domain::errors::{DomainError, Result};
use crate::domain::types::Principal;

const JWKS_TTL: Duration = Duration::from_secs(600);

pub struct OidcAuthenticator {
    issuer: String,
    audience: String,
    jwks_uri: String,
    required_scopes: Vec<String>,
    client_claims: Vec<String>,
    grants: HashMap<String, ClientGrant>,
    cache: Arc<RwLock<Option<(JwkSet, Instant)>>>,
    http: reqwest::Client,
}

impl OidcAuthenticator {
    pub fn new(cfg: &Config) -> Result<Self> {
        Ok(Self {
            issuer: cfg.auth.issuer.clone(),
            audience: cfg.auth.audience.clone(),
            jwks_uri: cfg.auth.jwks_uri.clone(),
            required_scopes: cfg.auth.required_scopes.clone(),
            client_claims: cfg.auth.client_claims.clone(),
            grants: cfg.auth.grants.iter().map(|g| (g.client.clone(), g.clone())).collect(),
            cache: Arc::new(RwLock::new(None)),
            http: reqwest::Client::builder()
                // The discovery and token budget on hosted clients is around ten seconds.
                .timeout(Duration::from_secs(5))
                .build()
                .map_err(|e| DomainError::internal("cannot build http client").with_source(e))?,
        })
    }

    async fn jwks(&self) -> Result<JwkSet> {
        if let Some((set, at)) = self.cache.read().await.as_ref() {
            if at.elapsed() < JWKS_TTL {
                return Ok(set.clone());
            }
        }
        let set: JwkSet = self
            .http
            .get(&self.jwks_uri)
            .send()
            .await
            .map_err(|e| DomainError::unavailable("cannot reach the JWKS endpoint").with_source(e))?
            .json()
            .await
            .map_err(|e| DomainError::unavailable("JWKS response was not valid JSON").with_source(e))?;
        *self.cache.write().await = Some((set.clone(), Instant::now()));
        Ok(set)
    }

    /// Fail closed. An earlier build granted everything when no grant matched, which meant any
    /// token the issuer signed held full access. An unknown client is refused, never defaulted.
    fn grant_for(&self, client: &str) -> Result<&ClientGrant> {
        self.grants.get(client).ok_or_else(|| {
            DomainError::forbidden(format!("client {client} has no namespace grant configured"))
        })
    }
}

#[async_trait]
impl Authenticator for OidcAuthenticator {
    fn mode(&self) -> &'static str {
        "oidc"
    }

    async fn authenticate(&self, authorization: Option<&str>) -> Result<Principal> {
        let token = bearer(authorization)?;
        let header = decode_header(token)
            .map_err(|e| DomainError::forbidden(format!("token rejected: {e}")))?;
        let kid = header
            .kid
            .ok_or_else(|| DomainError::forbidden("token rejected: no key id"))?;

        let jwks = self.jwks().await?;
        let jwk = jwks
            .find(&kid)
            .ok_or_else(|| DomainError::forbidden("token rejected: unknown key id"))?;
        let key = DecodingKey::from_jwk(jwk)
            .map_err(|e| DomainError::forbidden(format!("token rejected: {e}")))?;

        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.audience]);
        validation.leeway = 30;

        let data = decode::<serde_json::Value>(token, &key, &validation)
            .map_err(|e| DomainError::forbidden(format!("token rejected: {e}")))?;
        let claims = data.claims;

        let scopes: Vec<String> = claims
            .get("scope")
            .or_else(|| claims.get("scp"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .split(|c: char| c == ' ' || c == ',')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();

        let missing: Vec<&String> = self
            .required_scopes
            .iter()
            .filter(|s| !scopes.contains(s))
            .collect();
        if !missing.is_empty() {
            return Err(DomainError::forbidden(format!(
                "token missing required scope(s): {}",
                missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            )));
        }

        let client = client_from_claims(&claims, &self.client_claims);
        let grant = self.grant_for(&client)?;

        Ok(Principal {
            client,
            token_id: fingerprint(token),
            mode: "oidc",
            scopes,
            // Each glob carries its own ceiling, and an explicitly empty list stays empty.
            read: grant.read_grants(),
            write: grant.write_grants(),
            registry_write: grant.registry_write,
            // A property of the client rather than of the grant: the flag asserts the client can
            // decrypt locally, which no claim in the token can establish.
            sealed_capable: grant.effective_sealed_capable(),
            may_delete: grant.effective_may_delete(),
            may_ingest: grant.effective_may_ingest(),
            may_read_history: false,
        })
    }
}

pub fn client_from_claims(claims: &serde_json::Value, order: &[String]) -> String {
    for claim in order {
        if let Some(v) = claims.get(claim).and_then(|v| v.as_str()) {
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    "unknown-client".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::policy::NamespaceGrant;
    use crate::domain::types::Sensitivity;

    #[test]
    fn prefers_the_first_claim_present_in_order() {
        let claims = serde_json::json!({"azp": "a", "client_id": "c", "sub": "s"});
        let order: Vec<String> = ["client_id", "azp", "sub"].iter().map(|s| s.to_string()).collect();
        assert_eq!(client_from_claims(&claims, &order), "c");

        let claims = serde_json::json!({"azp": "a", "sub": "s"});
        assert_eq!(client_from_claims(&claims, &order), "a");
    }

    #[test]
    fn falls_back_rather_than_failing_so_the_row_is_still_attributed() {
        let order: Vec<String> = vec!["client_id".to_string()];
        assert_eq!(client_from_claims(&serde_json::json!({}), &order), "unknown-client");
    }

    // Built by hand rather than through `new`, which needs a whole Config. The grant lookup is the
    // part worth testing without a live issuer; the signature path is an integration test.
    fn authenticator(grants: Vec<ClientGrant>) -> OidcAuthenticator {
        OidcAuthenticator {
            issuer: "https://id.example.com".into(),
            audience: "https://lumberroom.example.com/mcp".into(),
            jwks_uri: "https://id.example.com/oidc/jwks".into(),
            required_scopes: vec![],
            client_claims: vec!["client_id".into()],
            grants: grants.into_iter().map(|g| (g.client.clone(), g)).collect(),
            cache: Arc::new(RwLock::new(None)),
            http: reqwest::Client::new(),
        }
    }

    fn grant(client: &str, read: Option<Vec<NamespaceGrant>>) -> ClientGrant {
        ClientGrant {
            client: client.into(),
            token: None,
            read,
            write: Some(vec![NamespaceGrant::open("global")]),
            registry_write: false,
            sealed_capable: false,
            may_delete: false,
            may_ingest: false,
            may_read_history: false,
        }
    }

    #[test]
    fn a_client_with_no_configured_grant_is_refused_rather_than_defaulted() {
        let a = authenticator(vec![grant("browser", Some(vec![NamespaceGrant::open("global")]))]);
        let err = a.grant_for("some-other-client").unwrap_err();
        assert_eq!(
            err.kind.http_status(),
            403,
            "an unmatched client once meant every token the issuer signed held full access"
        );
    }

    #[test]
    fn a_configured_client_gets_exactly_the_grant_it_was_given() {
        let a = authenticator(vec![grant(
            "browser",
            Some(vec![NamespaceGrant::new("user:me", Sensitivity::Private)]),
        )]);
        let g = a.grant_for("browser").unwrap();
        assert_eq!(g.read_grants()[0].max, Sensitivity::Private);
        assert_eq!(g.write_grants(), vec![NamespaceGrant::open("global")]);
        assert!(!g.effective_sealed_capable());
        assert!(!g.effective_may_delete());
    }

    #[test]
    fn an_unrestricted_grant_is_sealed_capable_and_a_restricted_one_is_not() {
        let a = authenticator(vec![grant("mac", None), grant("browser", Some(vec![]))]);
        assert!(a.grant_for("mac").unwrap().effective_sealed_capable());
        let restricted = a.grant_for("browser").unwrap();
        assert!(!restricted.effective_sealed_capable());
        assert!(restricted.read_grants().is_empty(), "empty must not widen to full access");
    }
}
