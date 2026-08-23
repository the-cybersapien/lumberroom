//! `lumberroom login`: dynamic registration, PKCE S256, a loopback listener, code exchange.
//!
//! RFC 8252. A CLI cannot hold a redirect URI a browser would accept as a real origin, so it binds
//! loopback and registers that exact URI. The server compares `redirect_uri` byte for byte at both
//! `/authorize` and `/token`, never by prefix, which is the whole reason the port is pinned and
//! persisted rather than ephemeral: an ephemeral port works for exactly one login, and every later
//! one reuses the stored `client_id` while binding a port that client was never registered with.

use base64::Engine;
use chrono::{SecondsFormat, Utc};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::client::{err, Client, Payload, Result};
use crate::out;

/// The first-ever login's port. Fixed rather than ephemeral, and persisted with the client id.
pub const DEFAULT_LOOPBACK_PORT: u16 = 8976;

const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

pub fn base64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn random_bytes(n: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).map_err(|e| err(format!("no entropy available: {e}")))?;
    Ok(buf)
}

/// RFC 7636 §4.2: `BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))`, unpadded.
pub fn pkce_challenge(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

/// Node writes `new Date(...).toISOString()` into the shared config file. Millisecond precision
/// with a `Z`, so the other client parses back what this one wrote.
pub fn expires_at(ttl_seconds: i64) -> String {
    (Utc::now() + chrono::Duration::seconds(ttl_seconds))
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn open_browser(url: &str) {
    let (program, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(target_os = "windows") {
        ("cmd", vec!["/c", "start", "", url])
    } else {
        ("xdg-open", vec![url])
    };
    // Best effort. The URL is always printed as well.
    let _ = std::process::Command::new(program).args(args).spawn();
}

/// What came back on the loopback callback.
enum Callback {
    Code(String),
    Failed(String),
}

/// Serve until one real `/callback` arrives.
///
/// Accepting once is not enough. Browsers open speculative connections they never write a request
/// on and ask for `/favicon.ico` unprompted, so a single accept can consume a connection that
/// carries no callback and then wait forever on one that never comes.
async fn wait_for_callback(listener: TcpListener, path: &str, expected_state: &str) -> Result<String> {
    let deadline = tokio::time::sleep(SIGN_IN_TIMEOUT);
    tokio::pin!(deadline);

    loop {
        let (mut socket, _) = tokio::select! {
            accepted = listener.accept() => accepted.map_err(|e| err(format!("loopback accept failed: {e}")))?,
            _ = &mut deadline => {
                return Err(err("timed out waiting for the browser sign-in (5 minutes)"));
            }
        };

        let mut buf = [0u8; 8192];
        let read = match socket.read(&mut buf).await {
            Ok(0) | Err(_) => continue,
            Ok(n) => n,
        };
        let request = String::from_utf8_lossy(&buf[..read]).to_string();
        let Some(target) = request.split_whitespace().nth(1) else { continue };

        let url = match reqwest::Url::parse(&format!("http://127.0.0.1{target}")) {
            Ok(u) => u,
            Err(_) => {
                respond(&mut socket, 400, "").await;
                continue;
            }
        };
        if url.path() != path {
            respond(&mut socket, 404, "not found").await;
            continue;
        }

        let mut code = None;
        let mut state = None;
        let mut error = None;
        for (k, v) in url.query_pairs() {
            match k.as_ref() {
                "code" => code = Some(v.into_owned()),
                "state" => state = Some(v.into_owned()),
                "error" => error = Some(v.into_owned()),
                _ => {}
            }
        }

        let (page, outcome) = if let Some(e) = error {
            (
                "lumberroom: sign-in was cancelled or failed. You can close this window.",
                Callback::Failed(format!("authorization server returned error={e}")),
            )
        } else if state.as_deref() != Some(expected_state) {
            (
                "lumberroom: state mismatch, aborting. You can close this window.",
                Callback::Failed("state parameter mismatch on the OAuth callback".to_string()),
            )
        } else if let Some(c) = code {
            (
                "lumberroom: signed in. You can close this window and return to the terminal.",
                Callback::Code(c),
            )
        } else {
            (
                "lumberroom: no authorization code received. You can close this window.",
                Callback::Failed("callback carried no authorization code".to_string()),
            )
        };

        respond(&mut socket, 200, &format!("<html><body>{page}</body></html>")).await;
        match outcome {
            Callback::Code(c) => return Ok(c),
            Callback::Failed(message) => return Err(err(message)),
        }
    }
}

async fn respond(socket: &mut tokio::net::TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.flush().await;
}

pub async fn login(client: &Client, args: &crate::args::Args) -> Result<()> {
    let state = hex(&random_bytes(16)?);
    let verifier = base64url(&random_bytes(32)?);
    let challenge = pkce_challenge(&verifier);

    let saved_client_id = client.file.borrow().oauth("client_id").map(str::to_string);
    let reregistering = args.present("reregister") || saved_client_id.is_none();
    let port = if reregistering {
        args.int("port", DEFAULT_LOOPBACK_PORT as i64) as u16
    } else {
        let saved_port = client
            .file
            .borrow()
            .oauth("redirect_uri")
            .and_then(|u| reqwest::Url::parse(u).ok())
            .and_then(|u| u.port())
            .unwrap_or(DEFAULT_LOOPBACK_PORT);
        if let Some(asked) = args.value("port").and_then(crate::args::parse_int_prefix) {
            if asked != saved_port as i64 {
                out(&format!(
                    "note: ignoring --port; reusing port {saved_port} already registered for client {}.",
                    saved_client_id.clone().unwrap_or_default()
                ));
                out("Pass --reregister to register a fresh client on a different port.");
            }
        }
        saved_port
    };
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let listener = TcpListener::bind(("127.0.0.1", port)).await.map_err(|e| {
        err(format!(
            "cannot bind the OAuth loopback listener on 127.0.0.1:{port} ({e}). Free that port, or \
run with --reregister --port <n> to register a fresh client on a different one."
        ))
    })?;

    let mut client_id = saved_client_id.clone().unwrap_or_default();
    let mut client_secret = client.file.borrow().oauth("client_secret").map(str::to_string);

    if reregistering {
        let (status, body) = client
            .http_request(
                reqwest::Method::POST,
                "/oauth/register",
                Some(json!({
                    "client_name": "lumberroom",
                    "redirect_uris": [redirect_uri],
                    "grant_types": ["authorization_code", "refresh_token"],
                    "token_endpoint_auth_method": "none",
                    "software_id": "lumberroom",
                    "software_version": env!("CARGO_PKG_VERSION"),
                })),
            )
            .await?;
        if status == 404 {
            return Err(err("server has no /oauth/register: it is not running in oauth or oidc mode"));
        }
        if status >= 300 {
            return Err(err(format!("client registration failed ({status}): {body}")));
        }
        let reg: crate::wire::RegistrationResponse = serde_json::from_value(body.clone())
            .map_err(|e| err(format!("registration response is not the expected shape ({e}): {body}")))?;
        client_id = reg.client_id;
        client_secret = reg.client_secret;
        out(&format!("registered client {client_id} on redirect {redirect_uri}"));
    } else {
        out(&format!(
            "reusing client {client_id} on redirect {redirect_uri} (--reregister to start over)"
        ));
    }

    let mut authorize = reqwest::Url::parse(&format!("{}/oauth/authorize", client.cfg.http_base))
        .map_err(|e| err(format!("cannot build the authorize URL: {e}")))?;
    {
        let mut q = authorize.query_pairs_mut();
        q.append_pair("response_type", "code");
        q.append_pair("client_id", &client_id);
        q.append_pair("redirect_uri", &redirect_uri);
        q.append_pair("code_challenge", &challenge);
        // RFC 7636's default when this is omitted is `plain`, so it is always sent.
        q.append_pair("code_challenge_method", "S256");
        q.append_pair("scope", args.value("scope").unwrap_or("memory.read memory.write"));
        // RFC 8707. The resource identifier is the MCP endpoint, matching the server's
        // AuthConfig.resource_url.
        q.append_pair("resource", &client.cfg.mcp_url);
        q.append_pair("state", &state);
    }
    let authorize = authorize.to_string();

    out("opening your browser to sign in. If nothing opens, visit:");
    out(&format!("  {authorize}"));
    open_browser(&authorize);

    let code = wait_for_callback(listener, "/callback", &state).await?;

    let mut form = vec![
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code),
        ("redirect_uri".to_string(), redirect_uri.clone()),
        ("client_id".to_string(), client_id.clone()),
        ("code_verifier".to_string(), verifier),
        ("resource".to_string(), client.cfg.mcp_url.clone()),
    ];
    if let Some(secret) = client_secret.clone() {
        form.push(("client_secret".to_string(), secret));
    }

    let token_url = format!("{}/oauth/token", client.cfg.http_base);
    let res = client.send(reqwest::Method::POST, &token_url, Payload::Form(form)).await?;
    let status = res.status().as_u16();
    let body: Value = res.json().await.unwrap_or_else(|_| json!({}));
    let token: crate::wire::TokenResponse = match serde_json::from_value(body.clone()) {
        Ok(t) if status < 300 => t,
        _ => return Err(err(format!("token exchange failed ({status}): {body}"))),
    };

    let expires_at = expires_at(token.expires_in.unwrap_or(3600));
    let mut oauth = Map::new();
    oauth.insert("client_id".into(), json!(client_id));
    oauth.insert("client_secret".into(), json!(client_secret));
    // Persisted so the next login rebinds this exact port. The redirect URI has to match byte for
    // byte on every future /authorize call.
    oauth.insert("redirect_uri".into(), json!(redirect_uri));
    oauth.insert("access_token".into(), json!(token.access_token));
    oauth.insert("refresh_token".into(), json!(token.refresh_token));
    oauth.insert("token_type".into(), json!(token.token_type.unwrap_or_else(|| "Bearer".into())));
    oauth.insert("expires_at".into(), json!(expires_at));

    let mut patch = Map::new();
    patch.insert("url".into(), json!(client.cfg.base_url));
    patch.insert("oauth".into(), Value::Object(oauth));
    let had_static_token = client.file.borrow().str_field("token").is_some();
    client
        .file
        .borrow_mut()
        .save(patch)
        .map_err(|e| err(format!("cannot write the config file: {e}")))?;

    if had_static_token {
        out("note: config.json still has a static \"token\" too; that one wins until you remove it.");
    }
    out(&format!("signed in. Access token expires {expires_at}."));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_the_rfc_7636_appendix_b_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_generated_verifier_is_url_safe_and_within_the_rfc_length_bounds() {
        let v = base64url(&random_bytes(32).unwrap());
        assert_eq!(v.len(), 43);
        assert!(v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        let c = pkce_challenge(&v);
        assert_eq!(c.len(), 43);
        assert!(!c.contains('='));
    }

    #[test]
    fn state_is_thirty_two_hex_characters() {
        let s = hex(&random_bytes(16).unwrap());
        assert_eq!(s.len(), 32);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn expires_at_is_an_iso_instant_node_can_parse() {
        let s = expires_at(3600);
        assert!(s.ends_with('Z'), "{s}");
        assert_eq!(s.len(), 24, "{s}");
        let parsed = chrono::DateTime::parse_from_rfc3339(&s).unwrap();
        let delta = parsed.timestamp() - Utc::now().timestamp();
        assert!((3595..=3605).contains(&delta), "{delta}");
    }

    /// A real listener, a real browser-shaped preconnect, a real callback.
    #[tokio::test]
    async fn the_listener_survives_a_connection_that_sends_nothing() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move { wait_for_callback(listener, "/callback", "st8").await });

        // A speculative connection that closes without writing, then a favicon, then the callback.
        let dead = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        drop(dead);
        let _ = reqwest::get(format!("http://127.0.0.1:{port}/favicon.ico")).await;
        let page = reqwest::get(format!("http://127.0.0.1:{port}/callback?code=abc123&state=st8"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        assert!(page.contains("signed in"));
        assert_eq!(handle.await.unwrap().unwrap(), "abc123");
    }

    #[tokio::test]
    async fn a_state_mismatch_is_refused() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move { wait_for_callback(listener, "/callback", "expected").await });
        let _ = reqwest::get(format!("http://127.0.0.1:{port}/callback?code=abc&state=forged")).await;
        let e = handle.await.unwrap().unwrap_err();
        assert!(e.message.contains("state parameter mismatch"), "{}", e.message);
    }

    #[tokio::test]
    async fn an_error_redirect_is_reported_rather_than_waited_out() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move { wait_for_callback(listener, "/callback", "st8").await });
        let _ = reqwest::get(format!("http://127.0.0.1:{port}/callback?error=access_denied&state=st8")).await;
        let e = handle.await.unwrap().unwrap_err();
        assert!(e.message.contains("error=access_denied"), "{}", e.message);
    }
}
