//! Tries each configured credential source in order and takes the first that succeeds.
//!
//! Why a chain rather than one mode: static bearer tokens have to keep working on the day OAuth is
//! switched on, because the `lumberroom` CLI and the surfaces that only ever send a bearer header share
//! one deployment with the browser surfaces that require OAuth.
//!
//! The interesting part is the failure. All links failing produces exactly one error, and that
//! error decides what the caller's 401 challenge says, so it has to be informative about the
//! credential the caller actually presented without confirming which other credential types this
//! deployment accepts. Three rules, in order:
//!
//! 1. No parseable bearer header at all fails once, up front. A caller that sent nothing is a
//!    client that has not authenticated yet, and a caller that sent something wrong is a client
//!    that failed; the first should be offered a login and the second should not be told to refresh
//!    a token it does not hold.
//! 2. Otherwise, if the presented credential has the format one link accepts, that link's error is
//!    the answer. A JWT that fails validation deserves the reason, and a link that could not reach
//!    its issuer must not have an outage reported as bad credentials.
//! 3. Otherwise the answer is a flat "invalid credentials". Naming the links that rejected it would
//!    enumerate the credential types the deployment holds.

use async_trait::async_trait;
use std::sync::Arc;

use super::{bearer, Authenticator};
use crate::domain::errors::{DomainError, Kind, Result};
use crate::domain::types::Principal;

/// What a caller with no Authorization header is told. `src/http` matches on this to decide whether
/// the 401 challenge carries `error="invalid_token"` or no error code at all, so the wording is a
/// contract and the test at the bottom of this file pins it. It repeats what `super::bearer`
/// answers for a missing header, and the two must stay identical.
const MISSING_CREDENTIAL: &str = "missing Authorization header";

pub struct ChainAuthenticator {
    links: Vec<Arc<dyn Authenticator>>,
    mode: &'static str,
}

impl ChainAuthenticator {
    pub fn new(links: Vec<Arc<dyn Authenticator>>, mode: &'static str) -> Self {
        Self { links, mode }
    }
}

#[async_trait]
impl Authenticator for ChainAuthenticator {
    /// The configured mode, not the mode of whichever link answered, so `/readyz` keeps reporting
    /// one value for the deployment.
    fn mode(&self) -> &'static str {
        self.mode
    }

    async fn authenticate(&self, authorization: Option<&str>) -> Result<Principal> {
        // An absent or blank header is a client that has not authenticated yet. Some clients send
        // the header with an empty value on their first attempt, which is the same situation.
        if authorization.map(str::trim).unwrap_or("").is_empty() {
            return Err(DomainError::forbidden(MISSING_CREDENTIAL));
        }
        // Every link takes a bearer token, so a header in another scheme fails here rather than
        // once per link. This is what keeps "sent nothing" distinguishable from "sent something
        // wrong" after the links have all rejected the credential.
        let token = bearer(authorization)?;

        let mut shaped: Option<DomainError> = None;
        let mut fault: Option<DomainError> = None;

        for link in &self.links {
            match link.authenticate(authorization).await {
                Ok(principal) => return Ok(principal),
                Err(e) => {
                    if claims_shape(link.mode(), token) {
                        shaped = Some(e);
                    } else if e.kind != Kind::Forbidden && fault.is_none() {
                        // A saturated database or an unreachable JWKS endpoint is our failure, not
                        // the caller's. Reporting it as invalid credentials sends the owner to
                        // regenerate a token that was always fine.
                        fault = Some(e);
                    }
                }
            }
        }

        Err(shaped.or(fault).unwrap_or_else(|| DomainError::forbidden("invalid credentials")))
    }
}

/// Whether a credential is in the format this link accepts. Static tokens claim nothing: they are
/// whatever the operator generated, so no format identifies them and treating them as a match would
/// make every failure report the static link's error.
fn claims_shape(mode: &str, token: &str) -> bool {
    match mode {
        "oidc" => looks_like_jwt(token),
        "oauth" => looks_like_opaque_token(token),
        _ => false,
    }
}

/// Three non-empty base64url segments. The header is not decoded: this only decides which error to
/// report, and the link itself does the real parsing.
fn looks_like_jwt(token: &str) -> bool {
    let mut parts = token.split('.');
    let ok = matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), Some(c))
        if !a.is_empty() && !b.is_empty() && !c.is_empty()
            && is_base64url(a) && is_base64url(b) && is_base64url(c));
    ok && parts.next().is_none()
}

/// What `domain::oauth::random_token` produces: base64url, no padding, no dots. The length floor
/// only excludes obvious noise; a static token can also satisfy this, which costs nothing because
/// the opaque link's rejection of an unknown token says no more than the generic message does.
fn looks_like_opaque_token(token: &str) -> bool {
    token.len() >= 20 && is_base64url(token)
}

fn is_base64url(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub {
        mode: &'static str,
        succeeds: bool,
        kind: Kind,
        message: &'static str,
    }

    impl Stub {
        fn ok(mode: &'static str) -> Arc<dyn Authenticator> {
            Arc::new(Stub { mode, succeeds: true, kind: Kind::Forbidden, message: "" })
        }

        fn rejects(mode: &'static str, message: &'static str) -> Arc<dyn Authenticator> {
            Arc::new(Stub { mode, succeeds: false, kind: Kind::Forbidden, message })
        }

        fn fails(mode: &'static str, kind: Kind, message: &'static str) -> Arc<dyn Authenticator> {
            Arc::new(Stub { mode, succeeds: false, kind, message })
        }
    }

    #[async_trait]
    impl Authenticator for Stub {
        fn mode(&self) -> &'static str {
            self.mode
        }

        async fn authenticate(&self, _authorization: Option<&str>) -> Result<Principal> {
            if self.succeeds {
                let mut p = Principal::empty(self.mode);
                p.mode = self.mode;
                return Ok(p);
            }
            Err(DomainError::new(self.kind, self.message))
        }
    }

    const OPAQUE: &str = "Bearer 8Qb1kQZ0rT7xFhVn2sYpLdMwEjKuAcBg";
    const JWT: &str = "Bearer eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJhIn0.c2ln";

    fn chain(links: Vec<Arc<dyn Authenticator>>, mode: &'static str) -> ChainAuthenticator {
        ChainAuthenticator::new(links, mode)
    }

    #[tokio::test]
    async fn takes_the_first_link_that_succeeds() {
        let c = chain(vec![Stub::rejects("token", "no"), Stub::ok("oauth")], "oauth");
        assert_eq!(c.authenticate(Some(OPAQUE)).await.unwrap().mode, "oauth");
    }

    #[tokio::test]
    async fn stops_before_asking_a_later_link() {
        let c = chain(
            vec![Stub::ok("token"), Stub::fails("oidc", Kind::Internal, "must not run")],
            "oidc",
        );
        assert_eq!(c.authenticate(Some(JWT)).await.unwrap().mode, "token");
    }

    #[tokio::test]
    async fn reports_a_missing_header_differently_from_a_wrong_credential() {
        let c = chain(vec![Stub::rejects("token", "invalid bearer token")], "token");
        let missing = c.authenticate(None).await.unwrap_err();
        assert_eq!(
            missing.client_message(),
            MISSING_CREDENTIAL,
            "src/http matches this string to leave error= off the 401 challenge"
        );
        let wrong = c.authenticate(Some(OPAQUE)).await.unwrap_err();
        assert_ne!(wrong.client_message(), MISSING_CREDENTIAL);
        // Some clients send the header with an empty value on the first attempt. That is a client
        // with no credential, not a client with a bad one.
        assert_eq!(
            c.authenticate(Some("   ")).await.unwrap_err().client_message(),
            MISSING_CREDENTIAL
        );
    }

    #[tokio::test]
    async fn refuses_a_header_that_is_not_a_bearer_header_without_consulting_a_link() {
        let c = chain(vec![Stub::fails("oidc", Kind::Internal, "must not run")], "oidc");
        let err = c.authenticate(Some("Basic dXNlcjpwdw==")).await.unwrap_err();
        assert!(err.client_message().contains("Bearer"));
    }

    #[tokio::test]
    async fn a_credential_nobody_recognises_gets_one_flat_error() {
        let c = chain(
            vec![
                Stub::rejects("token", "invalid bearer token"),
                Stub::rejects("oidc", "token rejected: no key id"),
            ],
            "oidc",
        );
        // An opaque token is not a JWT, so the oidc link's reason would confirm that this
        // deployment accepts JWTs at all.
        let err = c.authenticate(Some(OPAQUE)).await.unwrap_err();
        assert_eq!(err.client_message(), "invalid credentials");
        assert_eq!(err.kind.http_status(), 403);
    }

    #[tokio::test]
    async fn a_jwt_that_fails_validation_gets_the_oidc_links_reason() {
        let c = chain(
            vec![
                Stub::rejects("token", "invalid bearer token"),
                Stub::rejects("oidc", "token rejected: unknown key id"),
            ],
            "oidc",
        );
        let err = c.authenticate(Some(JWT)).await.unwrap_err();
        assert_eq!(err.client_message(), "token rejected: unknown key id");
    }

    #[tokio::test]
    async fn an_opaque_token_that_fails_validation_gets_the_oauth_links_reason() {
        let c = chain(
            vec![
                Stub::rejects("token", "invalid bearer token"),
                Stub::rejects(
                    "oauth",
                    "this client is registered but not yet approved by the owner",
                ),
            ],
            "oauth",
        );
        let err = c.authenticate(Some(OPAQUE)).await.unwrap_err();
        assert!(err.client_message().contains("not yet approved"));
    }

    #[tokio::test]
    async fn a_dependency_outage_is_never_reported_as_bad_credentials() {
        let c = chain(
            vec![
                Stub::rejects("token", "invalid bearer token"),
                Stub::fails("oauth", Kind::Unavailable, "cannot reach the database"),
            ],
            "oauth",
        );
        // The credential does not look opaque, so rule 2 does not apply and rule 3 would hide a
        // 503 behind a 401 the owner would spend the outage debugging.
        let err = c.authenticate(Some("Bearer not.an.opaque.token")).await.unwrap_err();
        assert_eq!(err.kind.http_status(), 503);
    }

    #[tokio::test]
    async fn reports_the_configured_mode_rather_than_the_link_that_answered() {
        let c = chain(vec![Stub::ok("token")], "oauth");
        assert_eq!(c.mode(), "oauth");
    }

    #[test]
    fn tells_the_two_credential_formats_apart() {
        assert!(looks_like_jwt("aGVhZGVy.cGF5bG9hZA.c2ln"));
        assert!(!looks_like_jwt("aGVhZGVy.cGF5bG9hZA"));
        assert!(!looks_like_jwt("a.b.c.d"));
        assert!(!looks_like_jwt("aGVhZGVy..c2ln"));
        assert!(looks_like_opaque_token("8Qb1kQZ0rT7xFhVn2sYpLdMwEjKuAcBg"));
        assert!(!looks_like_opaque_token("aGVhZGVy.cGF5bG9hZA.c2ln"), "a JWT is not opaque");
        assert!(!looks_like_opaque_token("short"));
    }

    #[test]
    fn a_static_token_claims_no_format_of_its_own() {
        assert!(!claims_shape("token", "8Qb1kQZ0rT7xFhVn2sYpLdMwEjKuAcBg"));
        assert!(!claims_shape("token", "aGVhZGVy.cGF5bG9hZA.c2ln"));
    }
}
