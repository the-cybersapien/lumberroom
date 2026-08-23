//! The built-in OAuth 2.1 authorization server (decision 0002).
//!
//! It exists so that `docker compose up` produces an MCP server a hosted client can connect to,
//! with no external identity provider to register, configure or pay for. The Claude.ai family,
//! Cowork and ChatGPT will not take a static bearer header; this is what they talk to instead.
//!
//! **Registration is not authorization.** RFC 7591 dynamic registration is open by default, and it
//! has to be: both Claude and ChatGPT mint a fresh client on every connection, and refusing them
//! means those surfaces cannot connect at all. What makes that safe is that a registered client holds
//! nothing. It is created with an empty grant, no consent, and no path to a token. The only thing
//! that ever attaches a grant is `POST /oauth/consent`, which needs the owner's password and a CSRF
//! token bound to the sign-in that produced it. So an open registration endpoint costs a row in a
//! table, and the boundary sits where the owner is, not where the client is.
//!
//! The layout:
//!
//! - `routes` the endpoints, and the whole authorization flow.
//! - `pages` the two HTML pages the owner sees. Server-rendered, no JavaScript, no external assets.
//! - `session` the signed owner cookie and the CSRF token bound to it.
//! - `limiter` failed-password throttling, in memory.
//!
//! Storage is `ports::OauthStore`, so nothing here knows Postgres exists.

pub mod limiter;
pub mod pages;
pub mod routes;
pub mod session;

use std::sync::Arc;

use axum::Router;

use crate::adapters::auth::Authenticator;
use crate::config::Config;
use crate::ports::OauthStore;

pub use routes::{metadata_document, AuthServer};

/// Everything this server serves, ready to merge into the main router.
///
/// Mount it only in `AUTH_MODE=oauth`. In the other modes these paths must 404 rather than answer,
/// because a metadata document advertising an authorization server that will not issue a token is
/// worse for a client than no document at all.
pub fn router(
    cfg: Arc<Config>,
    store: Arc<dyn OauthStore>,
    auth: Arc<dyn Authenticator>,
) -> Router {
    routes::routes().with_state(AuthServer::new(cfg, store, auth))
}
