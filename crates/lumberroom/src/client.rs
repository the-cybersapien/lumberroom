//! HTTP and MCP transport.
//!
//! Two surfaces, one credential. The admin routes are plain JSON over `http_base`, and the tools
//! are JSON-RPC over Streamable HTTP at `/mcp`. Both go through `send`, so the automatic refresh
//! on 401 covers every call rather than the ones somebody remembered to wrap.

use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};

use crate::config::{FileConfig, Resolved};

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
        Ok(Self { http, cfg, file: RefCell::new(file), token, request_id: Cell::new(0) })
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
                format!("timed out after {}ms talking to {}", self.cfg.timeout_ms, self.cfg.mcp_url),
                3,
            )
        } else {
            err(e.to_string())
        }
    }

    fn build(&self, method: reqwest::Method, url: &str, payload: &Payload) -> reqwest::RequestBuilder {
        let mut req = self
            .http
            .request(method, url)
            .header("accept", "application/json, text/event-stream")
            // How the instrumentation tells "the hook asked" apart from "the model chose to".
            .header("x-memory-invocation", self.cfg.invocation.as_str());
        let token = self.token.borrow().clone();
        if !token.is_empty() {
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
    pub async fn send(&self, method: reqwest::Method, url: &str, payload: Payload) -> Result<reqwest::Response> {
        let res = self
            .build(method.clone(), url, &payload)
            .send()
            .await
            .map_err(|e| self.net_err(e))?;
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
    pub async fn refresh(&self) -> bool {
        let (refresh_token, client_id, client_secret, existing) = {
            let file = self.file.borrow();
            let Some(rt) = file.oauth("refresh_token") else { return false };
            let Some(cid) = file.oauth("client_id") else { return false };
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

        let url = format!("{}/oauth/token", self.cfg.http_base);
        let Ok(res) = self.http.post(&url).form(&form).send().await else { return false };
        if !res.status().is_success() {
            return false;
        }
        let Ok(body) = res.json::<Value>().await else { return false };
        let Some(access) = body.get("access_token").and_then(Value::as_str) else { return false };

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
        if self.file.borrow_mut().save(patch).is_err() {
            return false;
        }
        *self.token.borrow_mut() = access.to_string();
        true
    }

    /// An admin or health route. Returns the status beside the body, because every caller here
    /// branches on the status and node's `httpRequest` does the same.
    pub async fn http_request(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<(u16, Value)> {
        let url = format!("{}{}", self.cfg.http_base, path);
        let payload = match body {
            Some(v) => Payload::Json(v),
            None => Payload::None,
        };
        let res = self.send(method, &url, payload).await?;
        let status = res.status().as_u16();
        let text = res.text().await.map_err(|e| self.net_err(e))?;
        let json = serde_json::from_str::<Value>(&text)
            .unwrap_or_else(|_| json!({ "raw": truncate(&text, 300) }));
        Ok((status, json))
    }

    pub async fn http_get(&self, path: &str) -> Result<(u16, Value)> {
        self.http_request(reqwest::Method::GET, path, None).await
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
        let res = self
            .send(reqwest::Method::POST, &self.cfg.mcp_url, Payload::Json(body))
            .await?;
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
}
