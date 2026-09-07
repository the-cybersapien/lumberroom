//! HTTP and MCP transport.
//!
//! Two surfaces, one credential. The admin routes are plain JSON over `http_base`, and the tools
//! are JSON-RPC over Streamable HTTP at `/mcp`. Both go through `send`, so the automatic refresh
//! on 401 covers every call rather than the ones somebody remembered to wrap.

use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::config::{FileConfig, Resolved};
use crate::oauth::{Endpoints, METADATA_PATH};

/// A failure with the exit code the owner and the acceptance scripts read.
///
/// 1 general, 2 auth or no credential, 3 timeout. These are `bin/lumberroom.mjs`'s codes and they are
/// part of the contract: `scripts/policy-test.sh` distinguishes a refused call from a broken one.
#[derive(Debug)]
pub struct CliError {
    pub message: String,
    pub code: i32,
}

pub type Result<T> = std::result::Result<T, CliError>;

pub fn err<S: Into<String>>(message: S) -> CliError {
    CliError { message: message.into(), code: 1 }
}

pub fn err_code<S: Into<String>>(message: S, code: i32) -> CliError {
    CliError { message: message.into(), code }
}

/// What a request carries, if anything. The token endpoint takes form encoding and nothing else:
/// a stack wired only for JSON returns 415 there while `/oauth/register` keeps working, which
/// reads as almost-working.
pub enum Payload {
    None,
    Json(Value),
    Form(Vec<(String, String)>),
}

pub struct ToolOutput {
    pub structured: Value,
    pub text: String,
}

pub struct Client {
    http: reqwest::Client,
    pub cfg: Resolved,
    pub file: RefCell<FileConfig>,
    token: RefCell<String>,
    request_id: Cell<i64>,
    endpoints: RefCell<Option<Rc<Endpoints>>>,
}

impl Client {
    pub fn new(cfg: Resolved, file: FileConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(cfg.timeout_ms))
            // Redirects are off: a 302 on an authenticated admin route would replay the bearer
            // token at whatever host the response named.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| err(format!("cannot build the HTTP client: {e}")))?;
        let token = RefCell::new(cfg.token.clone());
        Ok(Self {
            http,
            cfg,
            file: RefCell::new(file),
            token,
            request_id: Cell::new(0),
            endpoints: RefCell::new(None),
        })
    }

    /// Where the OAuth flow goes, resolved once per run and reused.
    ///
    /// Against the hosted deployment this reads the RFC 8414 document, because its issuer and its
    /// API base are different hosts. Against anything else it builds the endpoints off the base
    /// exactly as this client always has, and makes no request at all: the deployments that were
    /// never broken get no new latency and no new way to fail.
    ///
    /// The discovery request carries no credential and skips `send`. Discovery is public, and a 401
    /// there must not start the refresh this call may itself be serving.
    pub async fn oauth_endpoints(&self) -> Result<Rc<Endpoints>> {
        let cached = self.endpoints.borrow().clone();
        if let Some(endpoints) = cached {
            return Ok(endpoints);
        }

        let endpoints = if crate::oauth::is_hosted(&self.cfg.http_base) {
            let url = format!("{}{}", self.cfg.http_base, METADATA_PATH);
            let res =
                self.http.get(&url).header("accept", "application/json").send().await.map_err(
                    |e| {
                        err(format!("cannot reach the authorization server metadata at {url}: {e}"))
                    },
                )?;
            let status = res.status().as_u16();
            if status >= 300 {
                return Err(err(format!(
                    "the authorization server metadata at {url} answered {status}"
                )));
            }
            let document = res.json::<Value>().await.map_err(|e| {
                err(format!("the authorization server metadata at {url} is not JSON: {e}"))
            })?;
            Endpoints::from_metadata(&document, &url)?
        } else {
            Endpoints::base_relative(&self.cfg.http_base)
        };

        let endpoints = Rc::new(endpoints);
        *self.endpoints.borrow_mut() = Some(endpoints.clone());
        Ok(endpoints)
    }

    pub fn token(&self) -> String {
        self.token.borrow().clone()
    }

    pub fn has_token(&self) -> bool {
        !self.token.borrow().is_empty()
    }

    fn net_err(&self, e: reqwest::Error) -> CliError {
        if e.is_timeout() {
            err_code(
                format!(
                    "timed out after {}ms talking to {}",
                    self.cfg.timeout_ms, self.cfg.mcp_url
                ),
                3,
            )
        } else {
            err(e.to_string())
        }
    }

    fn build(
        &self,
        method: reqwest::Method,
        url: &str,
        payload: &Payload,
    ) -> reqwest::RequestBuilder {
        let mut req = self
            .http
            .request(method, url)
            .header("accept", "application/json, text/event-stream")
            // How the instrumentation tells "the hook asked" apart from "the model chose to".
            .header("x-memory-invocation", self.cfg.invocation.as_str());
        // The credential goes only where it cannot be read off the wire. Every URL this client
        // reaches now comes from `oauth_endpoints`, and one of its two paths builds from an
        // operator-supplied base whose scheme nothing checked, so a base of `http://a-remote-host`
        // used to put a bearer token in the clear on every request. Dropping the header rather than
        // failing keeps the refusal at the server, which answers 401 and says so.
        let token = self.token.borrow().clone();
        if !token.is_empty() && crate::oauth::may_carry_credential(url) {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        match payload {
            Payload::None => req.header("content-type", "application/json"),
            Payload::Json(v) => req.header("content-type", "application/json").body(v.to_string()),
            Payload::Form(pairs) => req.form(pairs),
        }
    }

    /// One request, one automatic refresh on 401, one retry. Never a loop.
    ///
    /// The request is rebuilt for the retry rather than replayed, so the second attempt carries the
    /// refreshed token instead of the stale header that caused the 401.
    pub async fn send(
        &self,
        method: reqwest::Method,
        url: &str,
        payload: Payload,
    ) -> Result<reqwest::Response> {
        let res =
            self.build(method.clone(), url, &payload).send().await.map_err(|e| self.net_err(e))?;
        if res.status().as_u16() != 401 {
            return Ok(res);
        }
        if self.file.borrow().oauth("refresh_token").is_none() {
            return Ok(res);
        }
        if !self.refresh().await {
            return Ok(res);
        }
        self.build(method, url, &payload).send().await.map_err(|e| self.net_err(e))
    }

    /// Exchange the refresh token, persist the result, adopt the new access token.
    ///
    /// A refresh that fails returns false and the caller's own 401 handling reports it. Retrying a
    /// bad refresh token is how a revoked credential turns into a hang instead of an error.
    ///
    /// The config lock is held across the whole exchange, from the read of the refresh token to
    /// the save of its replacement. A lock around the save alone cannot prevent the lockout: two
    /// processes both read the same token, both send it, and the server, which rotates on first
    /// presentation and revokes the family on the second, kills the credential for good. Under
    /// the lock the second process re-reads the file before it sends anything, so it presents the
    /// rotated token instead of the spent one. The wait for the lock is this client's request
    /// timeout plus margin, so a holder stuck on a slow token endpoint cannot block every other
    /// lumberroom process on the machine indefinitely; a waiter that outlasts it gets an error
    /// naming the lock file.
    pub async fn refresh(&self) -> bool {
        let path = self.file.borrow().path.clone();
        // The refresh token is read out of this file and sent. A file every local account can
        // read is reported here, and the 401 the caller is handling stands.
        if let Err(e) = crate::config::refuse_loose_permissions(&path) {
            eprintln!("{e}");
            return false;
        }
        let lock_wait = std::time::Duration::from_millis(self.cfg.timeout_ms)
            + std::time::Duration::from_secs(5);
        let lock = match crate::config::FileConfig::lock_config(&path, lock_wait) {
            Ok(lock) => lock,
            Err(e) => {
                eprintln!("cannot lock the config file for the refresh: {e}");
                return false;
            }
        };
        // The in-memory copy of the file predates every other process's refresh since this one
        // started. The token to send is whatever is on disk now, read under the lock so nothing
        // rotates it in between.
        *self.file.borrow_mut() = FileConfig::load(path);

        let (refresh_token, client_id, client_secret, existing) = {
            let file = self.file.borrow();
            let Some(rt) = file.oauth("refresh_token") else {
                eprintln!(
                    "cannot refresh: the config file has no refresh_token. Run `lumberroom \
                     login` to sign in again."
                );
                return false;
            };
            let Some(cid) = file.oauth("client_id") else {
                eprintln!(
                    "cannot refresh: the config file has no client_id, which `lumberroom login` \
                     writes when it registers the client. Sign in again."
                );
                return false;
            };
            let secret = file.oauth("client_secret").map(str::to_string);
            let existing = file.value.get("oauth").cloned().unwrap_or_else(|| json!({}));
            (rt.to_string(), cid.to_string(), secret, existing)
        };

        let mut form = vec![
            ("grant_type".to_string(), "refresh_token".to_string()),
            ("refresh_token".to_string(), refresh_token.clone()),
            ("client_id".to_string(), client_id),
        ];
        if let Some(secret) = client_secret {
            form.push(("client_secret".to_string(), secret));
        }

        let url = match self.oauth_endpoints().await {
            Ok(endpoints) => endpoints.token.clone(),
            Err(e) => {
                eprintln!("{}", e.message);
                return false;
            }
        };
        // The refresh token is the longest-lived credential this client holds, so the same rule
        // applies and this one refuses outright rather than continuing without it.
        if !crate::oauth::may_carry_credential(&url) {
            // The endpoint is deliberately not printed. It carries no credential, but it is derived
            // from the same value the token travels to, and a refusal message is not worth teaching
            // the next reader that anything off that path is safe to log. The operator configured
            // the URL and can read it back from their own config.
            eprintln!(
                "refusing to send a refresh token over plain http to a host that is not loopback: \
it would go on the wire in the clear. Point the CLI at https, or at 127.0.0.1."
            );
            return false;
        }
        let res = match self.http.post(&url).form(&form).send().await {
            Ok(res) => res,
            Err(e) => {
                eprintln!("cannot reach the token endpoint: {e}");
                return false;
            }
        };
        if !res.status().is_success() {
            let status = res.status().as_u16();
            let body = res.text().await.unwrap_or_default();
            eprintln!(
                "the token endpoint refused the refresh ({status}): {}",
                truncate(&body, 300)
            );
            if body.contains("invalid_grant") {
                eprintln!(
                    "the server refused the refresh token itself (spent, expired, or revoked \
                     when an earlier run failed mid-rotation). Run `lumberroom login` to sign \
                     in again."
                );
            }
            return false;
        }
        let body = match res.json::<Value>().await {
            Ok(body) => body,
            Err(e) => {
                eprintln!("the token endpoint's answer is not JSON: {e}");
                return false;
            }
        };
        let Some(access) = body.get("access_token").and_then(Value::as_str) else {
            eprintln!("the token endpoint's answer has no access_token in it");
            return false;
        };

        let mut oauth = existing.as_object().cloned().unwrap_or_default();
        oauth.insert("access_token".into(), json!(access));
        oauth.insert(
            "refresh_token".into(),
            body.get("refresh_token").cloned().unwrap_or_else(|| json!(refresh_token)),
        );
        oauth.insert(
            "token_type".into(),
            body.get("token_type").cloned().unwrap_or_else(|| json!("Bearer")),
        );
        let ttl = body.get("expires_in").and_then(Value::as_i64).unwrap_or(3600);
        oauth.insert("expires_at".into(), json!(crate::oauth::expires_at(ttl)));

        let mut patch = Map::new();
        patch.insert("oauth".into(), Value::Object(oauth));
        if let Err(e) = self.file.borrow_mut().save_locked(&lock, patch) {
            // The rotation already happened server-side, so the token still on disk is spent.
            // Saying so here is the difference between a diagnosable failure and the next
            // run's invalid_grant arriving with no explanation.
            eprintln!(
                "cannot write the refreshed credential to the config file: {e}\nThe new tokens \
                 were not saved and the refresh token on disk has already been rotated \
                 server-side, so the next refresh will be refused. Run `lumberroom login`."
            );
            return false;
        }
        *self.token.borrow_mut() = access.to_string();
        drop(lock);
        true
    }

    /// An admin or health route. Returns the status beside the body, because every caller here
    /// branches on the status and node's `httpRequest` does the same.
    pub async fn http_request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(u16, Value)> {
        let url = format!("{}{}", self.cfg.http_base, path);
        self.http_request_url(method, &url, body).await
    }

    /// The same call against an absolute URL, for the endpoints discovery names. Those can sit on
    /// a different host from `http_base`, so a path is not enough to address them.
    pub async fn http_request_url(
        &self,
        method: reqwest::Method,
        url: &str,
        body: Option<Value>,
    ) -> Result<(u16, Value)> {
        let payload = match body {
            Some(v) => Payload::Json(v),
            None => Payload::None,
        };
        let res = self.send(method, url, payload).await?;
        let status = res.status().as_u16();
        let text = res.text().await.map_err(|e| self.net_err(e))?;
        let json = serde_json::from_str::<Value>(&text)
            .unwrap_or_else(|_| json!({ "raw": truncate(&text, 300) }));
        Ok((status, json))
    }

    pub async fn http_get(&self, path: &str) -> Result<(u16, Value)> {
        self.http_request(reqwest::Method::GET, path, None).await
    }

    /// A route whose response is a file rather than JSON. Same auth, same refresh, raw bytes.
    ///
    /// `http_request` reads the body with `res.text()`, which substitutes U+FFFD for every byte
    /// sequence that is not valid UTF-8. An archive is gzip inside age, so that path hands back a
    /// file that is corrupt and the corruption surfaces at restore time. The caller decodes the
    /// body itself when the status says the bytes are an error message.
    pub async fn http_send_bytes(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(u16, Vec<u8>)> {
        let url = format!("{}{}", self.cfg.http_base, path);
        let payload = match body {
            Some(v) => Payload::Json(v),
            None => Payload::None,
        };
        let res = self.send(method, &url, payload).await?;
        let status = res.status().as_u16();
        let bytes = res.bytes().await.map_err(|e| self.net_err(e))?;
        Ok((status, bytes.to_vec()))
    }

    /// One JSON-RPC call over the MCP transport.
    pub async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        self.request_id.set(self.request_id.get() + 1);
        let body = json!({
            "jsonrpc": "2.0",
            "id": self.request_id.get(),
            "method": method,
            "params": params,
        });
        let res = self.send(reqwest::Method::POST, &self.cfg.mcp_url, Payload::Json(body)).await?;
        let status = res.status().as_u16();
        if status == 401 || status == 403 {
            let detail = res.text().await.unwrap_or_default();
            return Err(err_code(
                format!("auth rejected ({status}). Check the token. {}", truncate(&detail, 200)),
                2,
            ));
        }
        let content_type = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let text = res.text().await.map_err(|e| self.net_err(e))?;
        let body = read_body(&content_type, &text, status)?;
        if let Some(e) = body.get("error").filter(|v| !v.is_null()) {
            let message = e
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| e.to_string());
            return Err(err(format!("{method}: {message}")));
        }
        Ok(body.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Streamable HTTP dropped sessions in revision 2026-07-28, which is what makes a bare
    /// initialize-then-call pair valid with no session id to carry. Sending it before every call
    /// keeps this client restart-proof.
    pub async fn initialize(&self) -> Result<()> {
        self.rpc(
            "initialize",
            json!({
                "protocolVersion": "2026-07-28",
                "capabilities": {},
                "clientInfo": { "name": format!("lumberroom-{}", self.cfg.invocation), "version": env!("CARGO_PKG_VERSION") },
            }),
        )
        .await
        .map(|_| ())
    }

    pub async fn call_tool(&self, name: &str, args: Value) -> Result<ToolOutput> {
        self.initialize().await?;
        let result = self.rpc("tools/call", json!({ "name": name, "arguments": args })).await?;
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if result.get("isError").and_then(Value::as_bool).unwrap_or(false) {
            return Err(err(if text.is_empty() { "tool error".to_string() } else { text }));
        }
        Ok(ToolOutput {
            structured: result.get("structuredContent").cloned().unwrap_or(Value::Null),
            text,
        })
    }
}

/// Streamable HTTP answers with either JSON or a single SSE frame. Accept both, and take the last
/// data line: a server that streams progress notifications puts the result at the end.
pub fn read_body(content_type: &str, text: &str, status: u16) -> Result<Value> {
    if content_type.contains("text/event-stream") {
        let last = text
            .split('\n')
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .next_back();
        let Some(last) = last else {
            return Err(err(format!("empty SSE response: {}", truncate(text, 200))));
        };
        return serde_json::from_str(last)
            .map_err(|e| err(format!("unparseable SSE frame: {e}: {}", truncate(last, 200))));
    }
    serde_json::from_str(text)
        .map_err(|_| err(format!("unexpected response ({status}): {}", truncate(text, 300))))
}

/// Character-safe truncation. Byte slicing a UTF-8 error body panics on a multi-byte boundary, and
/// an error path that panics hides the error it was reporting.
pub fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_frames_yield_the_last_payload() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"a\":1}}\n\n";
        let v = read_body("text/event-stream", body, 200).unwrap();
        assert_eq!(v["result"]["a"], json!(1));
    }

    #[test]
    fn plain_json_parses_too() {
        let v = read_body("application/json", "{\"result\":{\"b\":2}}", 200).unwrap();
        assert_eq!(v["result"]["b"], json!(2));
    }

    #[test]
    fn a_non_json_body_reports_the_status_and_the_text() {
        let e = read_body("text/html", "<html>502 upstream</html>", 502).unwrap_err();
        assert!(e.message.contains("unexpected response (502)"), "{}", e.message);
        assert!(e.message.contains("502 upstream"));
    }

    #[test]
    fn truncation_does_not_split_a_multibyte_character() {
        assert_eq!(truncate("héllo", 3), "hél");
    }

    /// The double-refresh race, against a token endpoint that rotates and refuses replays the
    /// way the server does: the first presentation of a refresh token gets a new pair, the
    /// second presentation of a spent one gets invalid_grant and the family is dead.
    mod refresh_race {
        use super::*;
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        struct TokenServer {
            current: String,
            issues: u64,
            /// Every refresh_token value ever presented, spent or not.
            presentations: Vec<String>,
        }

        /// One request in, one response out. Just enough HTTP for a form-encoded token request;
        /// the crate has no server framework by design, and the login tests in `oauth.rs` use the
        /// same hand-rolled shape.
        async fn serve_one(socket: &mut tokio::net::TcpStream, state: &Mutex<TokenServer>) {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let n = socket.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(at) = find_header_end(&buf) {
                    break at;
                }
            };
            let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
            let length = headers
                .lines()
                .filter_map(|l| l.split_once(':'))
                .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while buf.len() < header_end + 4 + length {
                match socket.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let body =
                String::from_utf8_lossy(&buf[header_end + 4..header_end + 4 + length]).to_string();
            let presented = body
                .split('&')
                .find_map(|pair| pair.strip_prefix("refresh_token="))
                .unwrap_or_default()
                .to_string();

            // The lock is scoped so no await below holds the guard: a MutexGuard across an
            // await makes the whole server future !Send and tokio::spawn refuses it.
            let (status, answer) = {
                let mut st = state.lock().unwrap();
                st.presentations.push(presented.clone());
                if presented == st.current {
                    st.issues += 1;
                    let issued = st.issues;
                    st.current = format!("r{issued}");
                    (
                        200,
                        json!({
                            "access_token": format!("a{issued}"),
                            "refresh_token": format!("r{issued}"),
                            "token_type": "Bearer",
                            "expires_in": 3600,
                        })
                        .to_string(),
                    )
                } else {
                    (
                        400,
                        json!({ "error": "invalid_grant", "error_description": "already used" })
                            .to_string(),
                    )
                }
            };
            let reason = if status == 200 { "OK" } else { "Bad Request" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: \
                 {}\r\nconnection: close\r\n\r\n{answer}",
                answer.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.flush().await;
        }

        fn find_header_end(buf: &[u8]) -> Option<usize> {
            buf.windows(4).position(|w| w == b"\r\n\r\n")
        }

        fn fixture_config() -> std::path::PathBuf {
            use crate::config::restrict;
            let dir = std::env::temp_dir().join(format!(
                "lumberroom-refresh-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("config.json");
            std::fs::write(&path, r#"{"oauth":{"refresh_token":"r0","client_id":"cid"}}"#).unwrap();
            restrict(&path).unwrap();
            path
        }

        /// One CLI process: its own runtime, its own client, its own in-memory view of the
        /// config file. A thread with a runtime of its own is the closest a test gets to a
        /// second `lumberroom` invocation on one machine. The barrier stands between loading
        /// the file and refreshing, so both processes hold the same stale view when they start,
        /// which is the state two invocations that began earlier are in.
        fn refresh_as_a_process(
            path: std::path::PathBuf,
            port: u16,
            barrier: &std::sync::Barrier,
        ) -> (bool, String) {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let adopted = rt.block_on(async move {
                let env: HashMap<String, String> = HashMap::from([(
                    "LUMBERROOM_URL".to_string(),
                    format!("http://127.0.0.1:{port}"),
                )]);
                let file = FileConfig::load(path);
                let resolved = crate::config::resolve(&env, &file, None, None, None, false, None);
                let client = Client::new(resolved, file).unwrap();
                barrier.wait();
                let refreshed = client.refresh().await;
                (refreshed, client.token())
            });
            drop(rt);
            adopted
        }

        #[tokio::test]
        async fn two_clients_never_present_the_same_refresh_token() {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let state = Arc::new(Mutex::new(TokenServer {
                current: "r0".into(),
                issues: 0,
                presentations: Vec::new(),
            }));

            let server_state = state.clone();
            let server = tokio::spawn(async move {
                // Two requests arrive; accept them one at a time.
                for _ in 0..2 {
                    let Ok((mut socket, _)) = listener.accept().await else { break };
                    serve_one(&mut socket, &server_state).await;
                }
            });

            let path = fixture_config();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let first_path = path.clone();
            let first_barrier = barrier.clone();
            let first =
                std::thread::spawn(move || refresh_as_a_process(first_path, port, &first_barrier));
            let second_path = path.clone();
            let second =
                std::thread::spawn(move || refresh_as_a_process(second_path, port, &barrier));

            // join on the blocking pool so the server on this runtime keeps answering while
            // both processes run.
            let ((a, token_a), (b, token_b)) = tokio::task::spawn_blocking(move || {
                (first.join().unwrap(), second.join().unwrap())
            })
            .await
            .unwrap();
            assert!(a, "one of the two refreshes failed");
            assert!(b, "one of the two refreshes failed");
            server.await.unwrap();

            let st = state.lock().unwrap();
            let mut seen = std::collections::BTreeSet::new();
            for token in &st.presentations {
                assert!(seen.insert(token.clone()), "refresh token {token:?} was presented twice");
            }
            assert_eq!(st.current, "r2", "both rotations happened server-side");

            let back: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(back["oauth"]["refresh_token"], json!("r2"));
            assert_eq!(back["oauth"]["access_token"], json!("a2"));
            // Which process won the lock first is not fixed, so the two adopted access
            // tokens are asserted as a set.
            let adopted = [token_a, token_b];
            assert!(
                adopted.iter().any(|t| t == "a1") && adopted.iter().any(|t| t == "a2"),
                "each process ended with its own rotation's access token: {adopted:?}"
            );
            std::fs::remove_dir_all(path.parent().unwrap()).ok();
        }
    }
}
