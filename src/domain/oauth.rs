//! The pure half of the built-in OAuth 2.1 authorization server: wire types, PKCE, credential
//! generation, and redirect-URI validation.
//!
//! Decision 0002 chose a built-in authorization server over an external issuer, so every rule an
//! external issuer would have enforced now lives here. Phase 2 spec §2 lists the ways a real MCP
//! client fails against a nearly-correct server, and most of those failures are silent: the client
//! shows a generic error, or no error, and the flow simply never completes. The rules that belong
//! in pure code rather than in a handler are here so they can be tested as rules.
//!
//! No I/O. Nothing here reads a database, a socket or a clock.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use url::Url;

use crate::domain::errors::{DomainError, Result};
use crate::domain::policy::NamespaceGrant;
use crate::domain::types::Sensitivity;

/// Scopes carry no policy weight here. Authorization is the `GrantProfile` the owner picks at the
/// consent screen, which is a server-side record a client cannot influence. A scope string exists
/// only because clients display one and RFC 6749 requires it in a token response when it differs
/// from the request.
const DEFAULT_SCOPE: &str = "lumberroom:memory";

// ---- RFC 7591 dynamic client registration ----

/// What a client posts to `/register`, as JSON.
///
/// Phase 2 spec §2 prefers manually issued credentials over dynamic registration, because both
/// Claude and ChatGPT mint a fresh client on every connection and the registrations accumulate.
/// The endpoint exists anyway: refusing it means those surfaces cannot connect at all.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistrationRequest {
    pub redirect_uris: Vec<String>,
    pub client_name: Option<String>,
    pub grant_types: Option<Vec<String>>,
    pub response_types: Option<Vec<String>>,
    pub token_endpoint_auth_method: Option<String>,
    pub scope: Option<String>,
    pub software_id: Option<String>,
    pub software_version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegistrationResponse {
    pub client_id: String,
    pub client_id_issued_at: i64,
    pub client_name: String,
    pub redirect_uris: Vec<String>,
    pub grant_types: Vec<String>,
    pub response_types: Vec<String>,
    pub token_endpoint_auth_method: String,
}

// ---- /authorize ----

#[derive(Debug, Clone, Deserialize)]
pub struct AuthorizeRequest {
    pub response_type: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub code_challenge_method: Option<String>,
    pub state: Option<String>,
    pub scope: Option<String>,
    /// RFC 8707. The audience the resulting token is bound to.
    pub resource: Option<String>,
}

/// Validated form of the above. Constructing one is the only way to proceed.
#[derive(Debug, Clone)]
pub struct AuthorizeIntent {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub state: Option<String>,
    pub scope: String,
    pub resource: Option<String>,
}

impl AuthorizeRequest {
    /// Checks response_type, that the challenge method is S256, and that redirect_uri is one of
    /// the client's registered URIs by EXACT string match.
    ///
    /// redirect_uri goes first on purpose. Every other error could in principle be reported by
    /// redirecting to the client with `error=`, and doing that before the URI is known to be
    /// registered turns this endpoint into an open redirector. Because a caller cannot tell the
    /// failures apart from a `DomainError`, the contract is that any error out of here is rendered
    /// as a page and never redirected.
    pub fn validate(self, registered: &[String]) -> Result<AuthorizeIntent> {
        // Exact string equality, never a prefix or an origin comparison. A registered
        // `https://host/cb` must not admit `https://host/cb/evil` or `https://host/cb?x=1`, and
        // origin matching would admit both.
        if !registered.iter().any(|uri| uri == &self.redirect_uri) {
            return Err(DomainError::validation(
                "redirect_uri does not exactly match a redirect URI registered for this client",
            ));
        }

        if self.response_type != "code" {
            return Err(DomainError::validation(format!(
                "unsupported response_type {:?}. This server issues authorization codes only.",
                self.response_type
            )));
        }

        // RFC 7636 §4.3: when code_challenge_method is omitted the default is `plain`, not S256.
        // Assuming S256 for a client that meant `plain` would verify a challenge that equals the
        // verifier, which is PKCE switched off while looking switched on. So absent means refused.
        // The value is case-sensitive, so `s256` is refused too rather than repaired.
        match self.code_challenge_method.as_deref() {
            Some("S256") => {}
            Some(other) => {
                return Err(DomainError::validation(format!(
                    "unsupported code_challenge_method {other:?}. Use S256."
                )))
            }
            None => {
                return Err(DomainError::validation(
                    "code_challenge_method is required and must be S256. \
                     Omitting it means 'plain' under RFC 7636, which this server refuses.",
                ))
            }
        }

        // An S256 challenge is 32 bytes of base64url without padding, so its length is fixed. The
        // check catches a client that sends a raw verifier while claiming S256.
        if self.code_challenge.len() != 43 || !is_base64url(&self.code_challenge) {
            return Err(DomainError::validation(
                "code_challenge is not a base64url-encoded SHA-256 digest",
            ));
        }

        // RFC 8707 §2: the resource indicator is an absolute URI and carries no fragment. A token
        // bound to a resource the client did not spell out exactly is a token that can be replayed
        // against the wrong audience.
        if let Some(resource) = &self.resource {
            if resource.contains('#') || Url::parse(resource).is_err() {
                return Err(DomainError::validation(
                    "resource must be an absolute URI without a fragment",
                ));
            }
        }

        let scope = match self.scope.as_deref().map(str::trim) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => DEFAULT_SCOPE.to_string(),
        };

        Ok(AuthorizeIntent {
            client_id: self.client_id,
            redirect_uri: self.redirect_uri,
            code_challenge: self.code_challenge,
            state: self.state,
            scope,
            resource: self.resource,
        })
    }
}

// ---- /token ----

/// What arrives at `/token`. Phase 2 spec §2: this endpoint must accept form encoding while
/// `/register` takes JSON. A stack wired for JSON only returns 415 here while registration
/// succeeds, which reads as almost-working.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    pub code: Option<String>,
    pub redirect_uri: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub code_verifier: Option<String>,
    pub refresh_token: Option<String>,
    pub resource: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    pub scope: String,
}

/// The OAuth error shape. Clients read 'error' and nothing else, so it has to be one of the
/// registered codes rather than free text.
///
/// The registered codes for these endpoints are `invalid_request`, `invalid_client`,
/// `invalid_grant`, `unauthorized_client`, `unsupported_grant_type`, `invalid_scope` (RFC 6749
/// §5.2), `access_denied`, `unsupported_response_type`, `server_error`,
/// `temporarily_unavailable` (§4.1.2.1), and `invalid_target` (RFC 8707 §2.2). Anything else is
/// invisible to a client.
#[derive(Debug, Clone, Serialize)]
pub struct OauthError {
    pub error: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_description: Option<String>,
}

impl OauthError {
    pub fn new(error: &'static str, description: impl Into<String>) -> Self {
        Self { error, error_description: Some(description.into()) }
    }

    /// 400 for most, 401 for invalid_client.
    ///
    /// This type covers protocol failures the caller could fix, so it never produces a 5xx. A
    /// fault of ours is a `DomainError::internal` and does not travel as an OAuth error body.
    pub fn http_status(&self) -> u16 {
        match self.error {
            "invalid_client" => 401,
            _ => 400,
        }
    }
}

// ---- the grant the owner assigns at the consent screen ----

/// What the owner picks once, per client, at the consent screen.
///
/// The profile is the boundary, not the scope string the client asked for. Phase 2 spec §3 is
/// blunt about why: a grant that cannot tell two clients apart is decoration, and the only signal
/// that is actually a boundary is the credential the server issued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantProfile {
    Full,
    Standard,
    Narrow,
}

impl GrantProfile {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "full" => Some(Self::Full),
            "standard" => Some(Self::Standard),
            "narrow" => Some(Self::Narrow),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Standard => "standard",
            Self::Narrow => "narrow",
        }
    }

    /// One line the owner reads on the consent screen before choosing.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Full => {
                "Every namespace at every level, including sealed items. Can delete memories, \
                 change the registry and post ingested proposals."
            }
            Self::Standard => {
                "Your own notes, shared facts and project notes. Nothing private or sealed, no \
                 deletes, no registry changes."
            }
            Self::Narrow => {
                "Your own notes and shared facts. No project notes, nothing private or sealed, no \
                 deletes, no registry changes."
            }
        }
    }

    pub fn read(self) -> Vec<NamespaceGrant> {
        self.namespaces()
    }

    /// Read and write are the same set at every profile. Phase 2 spec §3 starts grants symmetric
    /// and the asymmetry that matters is the sensitivity ceiling, which both axes already carry.
    pub fn write(self) -> Vec<NamespaceGrant> {
        self.namespaces()
    }

    fn namespaces(self) -> Vec<NamespaceGrant> {
        match self {
            Self::Full => vec![NamespaceGrant::new("*", Sensitivity::Sealed)],
            Self::Standard => vec![
                NamespaceGrant::open("user:me"),
                NamespaceGrant::open("global"),
                NamespaceGrant::open("project:*"),
            ],
            Self::Narrow => {
                vec![NamespaceGrant::open("user:me"), NamespaceGrant::open("global")]
            }
        }
    }

    /// Holding a sealed ceiling and being able to decrypt a sealed item are separate. The flag is
    /// the second one, so a profile cannot reach sealed content by being handed a wider glob.
    pub fn sealed_capable(self) -> bool {
        matches!(self, Self::Full)
    }

    pub fn may_delete(self) -> bool {
        matches!(self, Self::Full)
    }

    /// Posting proposals is an operator action, so only the profile that already carries delete and
    /// registry writes carries it. A hosted client filling the queue is the failure this gate
    /// exists for.
    pub fn may_ingest(self) -> bool {
        matches!(self, Self::Full)
    }

    /// Registry writes are an operator action. A model that can rewrite `services.postgres.endpoint`
    /// can redirect the operator, so this stays off outside the owner's own client.
    pub fn registry_write(self) -> bool {
        matches!(self, Self::Full)
    }
}

// ---- credentials ----

/// CSPRNG bytes, base64url without padding. Used for client ids, codes, access and refresh
/// tokens. Panic-free: an error from the OS RNG is an error, not a weaker token.
pub fn random_token(bytes: usize) -> Result<String> {
    // Bounded because both ends are bugs with no visible symptom. `bytes == 0` returns an empty
    // string that hashes and compares like any other token, and a caller asking for kilobytes has
    // confused bytes with bits.
    if !(16..=64).contains(&bytes) {
        return Err(DomainError::internal(format!(
            "token length {bytes} is outside the supported 16..=64 bytes"
        )));
    }
    let mut buf = vec![0u8; bytes];
    // The error is formatted into the message rather than attached as a source: `getrandom::Error`
    // implements `std::error::Error` only under its `std` feature, and depending on another crate
    // in the tree to switch that on is a build that breaks for an unrelated reason.
    getrandom::fill(&mut buf).map_err(|e| {
        DomainError::internal(format!("could not read randomness from the OS: {e}"))
    })?;
    Ok(URL_SAFE_NO_PAD.encode(&buf))
}

/// SHA-256, lowercase hex. Codes and tokens are stored only as this.
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Constant-time comparison of two hashes.
///
/// Unequal lengths compare false without a timing signal worth having: both inputs are
/// fixed-length hex from `hash_token`, so a length difference means a caller passed the wrong
/// kind of string, not an attacker probing.
pub fn hashes_match(a: &str, b: &str) -> bool {
    bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// RFC 7636 S256: BASE64URL(SHA256(ASCII(verifier))) == challenge, compared in constant time.
/// Rejects a verifier outside the 43..=128 character range and a non-S256 method.
pub fn verify_pkce_s256(challenge: &str, verifier: &str) -> bool {
    if !is_valid_verifier(verifier) {
        return false;
    }
    let computed = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    bool::from(computed.as_bytes().ct_eq(challenge.as_bytes()))
}

/// RFC 7636 §4.1: 43 to 128 characters drawn from the unreserved set. Checking the alphabet first
/// makes the length check byte-safe, since every accepted character is one ASCII byte.
fn is_valid_verifier(verifier: &str) -> bool {
    verifier.bytes().all(|b| {
        b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_' || b == b'~'
    }) && (43..=128).contains(&verifier.len())
}

fn is_base64url(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// How many redirect URIs one client may register. The list is walked on every authorize.
pub const MAX_REDIRECT_URIS: usize = 8;
/// Per URI. Browsers cap a URL near this, and every registered URI is stored and compared on each
/// authorize, so a longer one is a storage cost with no client that could use it.
pub const MAX_REDIRECT_URI: usize = 2048;

/// The whole list as one check: at least one, at most `MAX_REDIRECT_URIS`, none longer than
/// `MAX_REDIRECT_URI`, each structurally valid. `/oauth/register` and the console's client form
/// both store what passes here, so the two cannot disagree about what a redirect list may hold.
pub fn validate_redirect_uris(uris: &[String]) -> Result<()> {
    if uris.is_empty() {
        return Err(DomainError::validation("redirect_uris must hold at least one URI"));
    }
    if uris.len() > MAX_REDIRECT_URIS {
        return Err(DomainError::validation(format!("at most {MAX_REDIRECT_URIS} redirect URIs")));
    }
    for uri in uris {
        if uri.len() > MAX_REDIRECT_URI {
            return Err(DomainError::validation(format!(
                "a redirect URI is longer than {MAX_REDIRECT_URI} characters"
            )));
        }
        validate_redirect_uri(uri).map_err(|e| {
            DomainError::validation(format!("{uri}: {}", e.client_message()))
        })?;
    }
    Ok(())
}

/// Structural check at registration time. Must reject: a non-absolute URI, a fragment, plain
/// http to a non-loopback host, and anything that is not http/https or a private-use scheme.
///
/// This validates, it does not normalise. The stored string is whatever the client registered,
/// because `/authorize` compares byte for byte and a URI that was lowercased or given a
/// trailing slash at registration would stop matching what the client sends.
pub fn validate_redirect_uri(uri: &str) -> Result<()> {
    // Checked on the raw string rather than through the parser. RFC 6749 §3.1.2 forbids a
    // fragment, and an empty one (`...cb#`) parses to `Some("")`, which a parser check that asks
    // whether a fragment exists can read either way.
    if uri.contains('#') {
        return Err(DomainError::validation("redirect_uri must not contain a fragment"));
    }

    let parsed = Url::parse(uri).map_err(|e| {
        DomainError::validation(format!("redirect_uri must be an absolute URI: {e}"))
    })?;

    // Credentials in a redirect URI end up in browser history and in the authorization request
    // that is logged next to it.
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(DomainError::validation("redirect_uri must not carry userinfo"));
    }

    match parsed.scheme() {
        "https" => {
            if parsed.host().is_none() {
                return Err(DomainError::validation("https redirect_uri needs a host"));
            }
            Ok(())
        }
        // A local CLI receives its code on a loopback listener, which cannot hold a certificate,
        // so RFC 8252 §7.3 allows plain http there and only there. Any port, because the port is
        // chosen at run time.
        "http" => {
            if is_loopback(&parsed) {
                Ok(())
            } else {
                Err(DomainError::validation(
                    "plain http is allowed only for a loopback redirect_uri. Use https.",
                ))
            }
        }
        // RFC 8252 §7.1 private-use scheme: a reverse-DNS name the app owns, so it contains a dot.
        // Requiring the dot is also what keeps `javascript:` and `data:` out.
        scheme if scheme.contains('.') => Ok(()),
        scheme => Err(DomainError::validation(format!(
            "redirect_uri scheme {scheme:?} is not supported. Use https, a loopback http URI, or \
             a private-use scheme such as com.example.app:/callback."
        ))),
    }
}

/// Exact host match, never a suffix or a substring: `localhost.attacker.example` contains
/// "localhost" and is not loopback.
fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d == "localhost",
        // Covers the whole of 127.0.0.0/8, which is what a CLI may bind.
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 appendix B.
    const RFC_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const RFC_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    fn authorize(redirect_uri: &str) -> AuthorizeRequest {
        AuthorizeRequest {
            response_type: "code".into(),
            client_id: "abc".into(),
            redirect_uri: redirect_uri.into(),
            code_challenge: RFC_CHALLENGE.into(),
            code_challenge_method: Some("S256".into()),
            state: Some("xyz".into()),
            scope: None,
            resource: None,
        }
    }

    #[test]
    fn the_rfc_7636_appendix_b_vector_verifies() {
        assert!(verify_pkce_s256(RFC_CHALLENGE, RFC_VERIFIER));
    }

    #[test]
    fn a_verifier_that_is_not_the_one_behind_the_challenge_is_refused() {
        let mut wrong = RFC_VERIFIER.to_string();
        wrong.replace_range(0..1, "e");
        assert!(!verify_pkce_s256(RFC_CHALLENGE, &wrong));
    }

    #[test]
    fn a_challenge_that_is_the_verifier_itself_is_refused() {
        // What accepting a `plain` challenge under an S256 label would look like.
        assert!(!verify_pkce_s256(RFC_VERIFIER, RFC_VERIFIER));
    }

    #[test]
    fn a_verifier_at_either_length_bound_verifies() {
        assert!(verify_pkce_s256("ZtNPunH49FD35FWYhT5Tv8I7vRKQJ8uxMaL0_9eHjNA", &"a".repeat(43)));
        assert!(verify_pkce_s256("cK4cUwf1JQ1cueQHQrqWE_zfm42ett05MzBEOy1e_70", &"b".repeat(128)));
    }

    #[test]
    fn a_verifier_shorter_than_43_characters_is_refused_before_it_is_hashed() {
        let short = "a".repeat(42);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(short.as_bytes()));
        assert!(!verify_pkce_s256(&challenge, &short), "a correct hash of a short verifier still fails");
    }

    #[test]
    fn a_verifier_longer_than_128_characters_is_refused() {
        let long = "a".repeat(129);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(long.as_bytes()));
        assert!(!verify_pkce_s256(&challenge, &long));
    }

    #[test]
    fn a_verifier_outside_the_unreserved_alphabet_is_refused() {
        let bad = format!("{}+/", "a".repeat(41));
        assert_eq!(bad.len(), 43);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(bad.as_bytes()));
        assert!(!verify_pkce_s256(&challenge, &bad));
    }

    #[test]
    fn an_empty_verifier_is_refused() {
        assert!(!verify_pkce_s256("", ""));
    }

    #[test]
    fn hash_token_is_lowercase_hex_sha256() {
        assert_eq!(
            hash_token("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hashes_match_only_on_equal_strings() {
        assert!(hashes_match(&hash_token("code"), &hash_token("code")));
        assert!(!hashes_match(&hash_token("code"), &hash_token("code ")));
        assert!(!hashes_match(&hash_token("code"), "short"));
        assert!(!hashes_match("", &hash_token("code")));
    }

    #[test]
    fn a_random_token_is_base64url_without_padding_and_never_repeats() {
        let a = random_token(32).unwrap();
        let b = random_token(32).unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 43, "32 bytes is 43 unpadded base64url characters");
        assert!(is_base64url(&a), "{a} must be url-safe and unpadded");
        assert!(!a.contains('='));
    }

    #[test]
    fn a_token_length_outside_the_supported_range_is_an_error_not_a_weak_token() {
        assert!(random_token(0).is_err());
        assert!(random_token(8).is_err());
        assert!(random_token(4096).is_err());
    }

    #[test]
    fn a_valid_authorize_request_becomes_an_intent() {
        let registered = vec!["https://lumberroom.example/cb".to_string()];
        let intent = authorize("https://lumberroom.example/cb").validate(&registered).unwrap();
        assert_eq!(intent.client_id, "abc");
        assert_eq!(intent.state.as_deref(), Some("xyz"));
        assert_eq!(intent.scope, DEFAULT_SCOPE, "an absent scope gets the default");
    }

    #[test]
    fn a_requested_scope_and_resource_pass_through() {
        let registered = vec!["https://lumberroom.example/cb".to_string()];
        let mut req = authorize("https://lumberroom.example/cb");
        req.scope = Some("  lumberroom:memory  ".into());
        req.resource = Some("https://lumberroom.example/mcp".into());
        let intent = req.validate(&registered).unwrap();
        assert_eq!(intent.scope, "lumberroom:memory");
        assert_eq!(intent.resource.as_deref(), Some("https://lumberroom.example/mcp"));
    }

    #[test]
    fn a_resource_with_a_fragment_or_no_scheme_is_refused() {
        let registered = vec!["https://lumberroom.example/cb".to_string()];
        let mut req = authorize("https://lumberroom.example/cb");
        req.resource = Some("https://lumberroom.example/mcp#a".into());
        assert!(req.validate(&registered).is_err());

        let mut req = authorize("https://lumberroom.example/cb");
        req.resource = Some("/mcp".into());
        assert!(req.validate(&registered).is_err());
    }

    #[test]
    fn only_the_code_response_type_is_accepted() {
        let registered = vec!["https://lumberroom.example/cb".to_string()];
        let mut req = authorize("https://lumberroom.example/cb");
        req.response_type = "token".into();
        assert!(req.validate(&registered).is_err());
    }

    #[test]
    fn an_omitted_code_challenge_method_is_refused_rather_than_read_as_s256() {
        let registered = vec!["https://lumberroom.example/cb".to_string()];
        let mut req = authorize("https://lumberroom.example/cb");
        req.code_challenge_method = None;
        let err = req.validate(&registered).unwrap_err();
        assert!(
            err.client_message().contains("S256"),
            "RFC 7636 defaults an omitted method to plain, so silence is not consent"
        );
    }

    #[test]
    fn the_plain_challenge_method_is_refused() {
        let registered = vec!["https://lumberroom.example/cb".to_string()];
        let mut req = authorize("https://lumberroom.example/cb");
        req.code_challenge_method = Some("plain".into());
        assert!(req.validate(&registered).is_err());
    }

    #[test]
    fn the_challenge_method_is_compared_case_sensitively() {
        let registered = vec!["https://lumberroom.example/cb".to_string()];
        let mut req = authorize("https://lumberroom.example/cb");
        req.code_challenge_method = Some("s256".into());
        assert!(req.validate(&registered).is_err());
    }

    #[test]
    fn a_challenge_that_is_not_a_base64url_digest_is_refused() {
        let registered = vec!["https://lumberroom.example/cb".to_string()];
        let mut req = authorize("https://lumberroom.example/cb");
        req.code_challenge = "too-short".into();
        assert!(req.validate(&registered).is_err());

        let mut req = authorize("https://lumberroom.example/cb");
        req.code_challenge = format!("{}+/", "a".repeat(41));
        assert!(req.validate(&registered).is_err());
    }

    #[test]
    fn a_trailing_slash_is_a_different_redirect_uri() {
        let registered = vec!["https://lumberroom.example/cb".to_string()];
        assert!(authorize("https://lumberroom.example/cb/").validate(&registered).is_err());
    }

    #[test]
    fn a_query_string_makes_a_redirect_uri_stop_matching() {
        let registered = vec!["https://lumberroom.example/cb".to_string()];
        assert!(authorize("https://lumberroom.example/cb?next=x").validate(&registered).is_err());
    }

    #[test]
    fn a_registered_uri_is_never_treated_as_a_prefix() {
        let registered = vec!["https://lumberroom.example/cb".to_string()];
        assert!(authorize("https://lumberroom.example/cb/evil").validate(&registered).is_err());
        assert!(authorize("https://lumberroom.example.evil/cb").validate(&registered).is_err());
    }

    #[test]
    fn a_client_with_no_registered_uris_can_authorize_nothing() {
        assert!(authorize("https://lumberroom.example/cb").validate(&[]).is_err());
    }

    #[test]
    fn one_of_several_registered_uris_is_enough() {
        let registered = vec![
            "https://lumberroom.example/cb".to_string(),
            "http://127.0.0.1:7000/callback".to_string(),
        ];
        assert!(authorize("http://127.0.0.1:7000/callback").validate(&registered).is_ok());
    }

    #[test]
    fn the_redirect_uri_is_checked_before_anything_else() {
        // Otherwise the handler is tempted to report the other failure by redirecting to an
        // address nobody has vouched for.
        let mut req = authorize("https://attacker.example/cb");
        req.response_type = "token".into();
        req.code_challenge_method = None;
        let err = req.validate(&["https://lumberroom.example/cb".to_string()]).unwrap_err();
        assert!(err.client_message().contains("redirect_uri"));
    }

    #[test]
    fn an_https_redirect_uri_is_accepted() {
        assert!(validate_redirect_uri("https://lumberroom.example/oauth/callback").is_ok());
        assert!(validate_redirect_uri("https://lumberroom.example/cb?x=1").is_ok());
    }

    #[test]
    fn a_loopback_redirect_uri_may_use_plain_http_on_any_port() {
        assert!(validate_redirect_uri("http://127.0.0.1:53219/callback").is_ok());
        assert!(validate_redirect_uri("http://127.0.0.1/callback").is_ok());
        assert!(validate_redirect_uri("http://localhost:8080/cb").is_ok());
        assert!(validate_redirect_uri("http://[::1]:8080/cb").is_ok());
    }

    #[test]
    fn plain_http_to_anything_other_than_loopback_is_refused() {
        assert!(validate_redirect_uri("http://lumberroom.example/cb").is_err());
        assert!(
            validate_redirect_uri("http://localhost.attacker.example/cb").is_err(),
            "a host that merely contains 'localhost' is not loopback"
        );
        assert!(validate_redirect_uri("http://127.0.0.1.attacker.example/cb").is_err());
    }

    #[test]
    fn a_fragment_is_refused_even_when_it_is_empty() {
        assert!(validate_redirect_uri("https://lumberroom.example/cb#frag").is_err());
        assert!(validate_redirect_uri("https://lumberroom.example/cb#").is_err());
    }

    #[test]
    fn a_relative_uri_is_refused() {
        assert!(validate_redirect_uri("/callback").is_err());
        assert!(validate_redirect_uri("lumberroom.example/cb").is_err());
        assert!(validate_redirect_uri("").is_err());
    }

    #[test]
    fn a_private_use_scheme_is_accepted_and_a_bare_word_scheme_is_not() {
        assert!(validate_redirect_uri("com.example.app:/oauth2redirect").is_ok());
        assert!(validate_redirect_uri("com.example.app://callback").is_ok());
        assert!(validate_redirect_uri("javascript:alert(1)").is_err());
        assert!(validate_redirect_uri("data:text/html,x").is_err());
        assert!(validate_redirect_uri("ftp://lumberroom.example/cb").is_err());
    }

    #[test]
    fn userinfo_in_a_redirect_uri_is_refused() {
        assert!(validate_redirect_uri("https://user@lumberroom.example/cb").is_err());
        assert!(validate_redirect_uri("https://user:pw@lumberroom.example/cb").is_err());
    }

    #[test]
    fn the_redirect_list_is_capped_in_count_and_length_wherever_it_is_validated() {
        let ok: Vec<String> = vec!["https://lumberroom.example/cb".into()];
        assert!(validate_redirect_uris(&ok).is_ok());
        assert!(validate_redirect_uris(&[]).is_err(), "an empty list registers nothing");
        let many: Vec<String> =
            (0..=MAX_REDIRECT_URIS).map(|i| format!("https://lumberroom.example/cb{i}")).collect();
        assert!(validate_redirect_uris(&many).is_err(), "one over the count cap");
        let long = vec![format!("https://lumberroom.example/{}", "a".repeat(MAX_REDIRECT_URI))];
        assert!(validate_redirect_uris(&long).is_err(), "one over the length cap");
        let bad = vec!["https://lumberroom.example/cb#frag".to_string()];
        let e = validate_redirect_uris(&bad).unwrap_err();
        assert!(e.client_message().contains("#frag"), "the refusal names the URI: {}", e.client_message());
    }

    #[test]
    fn invalid_client_is_the_only_401() {
        assert_eq!(OauthError::new("invalid_client", "no").http_status(), 401);
        assert_eq!(OauthError::new("invalid_grant", "no").http_status(), 400);
        assert_eq!(OauthError::new("invalid_request", "no").http_status(), 400);
        assert_eq!(OauthError::new("server_error", "no").http_status(), 400);
    }

    #[test]
    fn an_error_without_a_description_serialises_to_the_code_alone() {
        let e = OauthError { error: "invalid_grant", error_description: None };
        assert_eq!(serde_json::to_string(&e).unwrap(), r#"{"error":"invalid_grant"}"#);
    }

    #[test]
    fn the_full_profile_reaches_everything_at_every_level() {
        let p = GrantProfile::Full;
        let expected = vec![NamespaceGrant::new("*", Sensitivity::Sealed)];
        assert_eq!(p.read(), expected);
        assert_eq!(p.write(), expected);
        assert!(p.sealed_capable());
        assert!(p.may_delete());
        assert!(p.may_ingest());
        assert!(p.registry_write());
    }

    #[test]
    fn the_standard_profile_adds_projects_and_stops_at_open() {
        let p = GrantProfile::Standard;
        let expected = vec![
            NamespaceGrant::new("user:me", Sensitivity::Open),
            NamespaceGrant::new("global", Sensitivity::Open),
            NamespaceGrant::new("project:*", Sensitivity::Open),
        ];
        assert_eq!(p.read(), expected);
        assert_eq!(p.write(), expected);
        assert!(!p.sealed_capable());
        assert!(!p.may_delete());
        assert!(!p.registry_write());
    }

    #[test]
    fn the_narrow_profile_sees_only_the_owner_and_shared_facts() {
        let p = GrantProfile::Narrow;
        let expected = vec![
            NamespaceGrant::new("user:me", Sensitivity::Open),
            NamespaceGrant::new("global", Sensitivity::Open),
        ];
        assert_eq!(p.read(), expected);
        assert_eq!(p.write(), expected);
        assert!(!p.sealed_capable());
        assert!(!p.may_delete());
        assert!(!p.registry_write());
    }

    #[test]
    fn only_the_full_profile_carries_the_capability_flags() {
        for p in [GrantProfile::Standard, GrantProfile::Narrow] {
            assert!(!p.sealed_capable(), "{}", p.as_str());
            assert!(!p.may_delete(), "{}", p.as_str());
            assert!(!p.may_ingest(), "{}", p.as_str());
            assert!(!p.registry_write(), "{}", p.as_str());
        }
    }

    #[test]
    fn a_profile_round_trips_through_its_string_form() {
        for p in [GrantProfile::Full, GrantProfile::Standard, GrantProfile::Narrow] {
            assert_eq!(GrantProfile::parse(p.as_str()), Some(p));
            assert_eq!(GrantProfile::parse(&format!(" {} ", p.as_str().to_uppercase())), Some(p));
            assert!(!p.describe().is_empty());
        }
        assert_eq!(GrantProfile::parse("admin"), None);
        assert_eq!(GrantProfile::parse(""), None);
    }

    #[test]
    fn a_profile_serialises_as_the_same_lowercase_word_it_parses() {
        assert_eq!(serde_json::to_string(&GrantProfile::Narrow).unwrap(), r#""narrow""#);
        assert_eq!(
            serde_json::from_str::<GrantProfile>(r#""full""#).unwrap(),
            GrantProfile::Full
        );
    }
}
