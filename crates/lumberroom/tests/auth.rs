//! The 401-then-refresh path, run against a real socket.
//!
//! A stub rather than a mock: the failure this guards against is replaying the stale
//! `Authorization` header on the retry, and only a server that reads the second request's headers
//! can tell the difference.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use lumberroom::client::Client;
use lumberroom::config::{self, FileConfig};

#[derive(Default)]
struct Log {
    /// The bearer token on each `/admin/whoami` request, in order.
    whoami_tokens: Vec<String>,
    token_grants: Vec<String>,
}

/// A three-route stub: whoami, the token endpoint, and nothing else.
async fn stub(listener: TcpListener, log: Arc<Mutex<Log>>, fresh: &'static str) {
    loop {
        let Ok((mut socket, _)) = listener.accept().await else { return };
        let mut buf = vec![0u8; 8192];
        let Ok(n) = socket.read(&mut buf).await else { continue };
        if n == 0 {
            continue;
        }
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        let first = request.lines().next().unwrap_or_default().to_string();
        let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
        let bearer = request
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
            .and_then(|l| l.split_once(' ').map(|(_, v)| v.trim().to_string()))
            .unwrap_or_default()
            .trim_start_matches("Bearer ")
            .to_string();
        let body = request.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();

        let (status, payload) = if path.starts_with("/admin/whoami") {
            log.lock().unwrap().whoami_tokens.push(bearer.clone());
            if bearer == fresh {
                (200, json!({ "client": "cli", "mode": "oauth" }))
            } else {
                (401, json!({ "error": "unauthorized" }))
            }
        } else if path.starts_with("/oauth/token") {
            let grant = body
                .split('&')
                .find_map(|kv| kv.strip_prefix("grant_type="))
                .unwrap_or_default()
                .to_string();
            log.lock().unwrap().token_grants.push(grant);
            (200, json!({ "access_token": fresh, "token_type": "Bearer", "expires_in": 60 }))
        } else {
            (404, json!({ "error": "not_found" }))
        };

        let body = payload.to_string();
        let response = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.flush().await;
    }
}

fn temp_config(name: &str, value: serde_json::Value) -> FileConfig {
    let dir = std::env::temp_dir().join(format!("lumberroom-auth-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.json");
    std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    FileConfig::load(path)
}

fn client_for(file: FileConfig, base: &str) -> Client {
    let env: HashMap<String, String> = HashMap::new();
    let resolved = config::resolve(&env, &file, Some(base), None, None, false, Some("4000"));
    Client::new(resolved, file).unwrap()
}

#[tokio::test]
async fn a_401_triggers_one_refresh_and_the_retry_carries_the_new_token() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let log = Arc::new(Mutex::new(Log::default()));
    tokio::spawn(stub(listener, log.clone(), "fresh-token"));

    let file = temp_config(
        "refresh",
        json!({
            "url": base,
            "keptByTheOtherClient": { "a": 1 },
            "oauth": {
                "client_id": "c1",
                "access_token": "stale-token",
                "refresh_token": "rt",
                "redirect_uri": "http://127.0.0.1:8976/callback"
            }
        }),
    );
    let path = file.path.clone();
    let client = client_for(file, &base);

    let (status, body) = client.http_get("/admin/whoami").await.unwrap();
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["mode"], json!("oauth"));

    let log = log.lock().unwrap();
    assert_eq!(log.whoami_tokens, vec!["stale-token".to_string(), "fresh-token".to_string()]);
    assert_eq!(log.token_grants, vec!["refresh_token".to_string()]);

    // The new credential is on disk, the unrelated key the node client owns is still there, and the
    // file is owner-only.
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["oauth"]["access_token"], json!("fresh-token"));
    assert_eq!(saved["oauth"]["refresh_token"], json!("rt"), "an absent rotation keeps the old token");
    assert_eq!(saved["oauth"]["client_id"], json!("c1"));
    assert_eq!(saved["oauth"]["redirect_uri"], json!("http://127.0.0.1:8976/callback"));
    assert_eq!(saved["keptByTheOtherClient"]["a"], json!(1));
    assert!(saved["oauth"]["expires_at"].as_str().unwrap().ends_with('Z'));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[tokio::test]
async fn without_a_refresh_token_the_401_is_returned_as_it_stands() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let log = Arc::new(Mutex::new(Log::default()));
    tokio::spawn(stub(listener, log.clone(), "fresh-token"));

    let file = temp_config("norefresh", json!({ "url": base, "token": "static-and-wrong" }));
    let path = file.path.clone();
    let client = client_for(file, &base);

    let (status, _) = client.http_get("/admin/whoami").await.unwrap();
    assert_eq!(status, 401);

    let log = log.lock().unwrap();
    assert_eq!(log.whoami_tokens, vec!["static-and-wrong".to_string()], "no retry");
    assert!(log.token_grants.is_empty(), "no token call without a refresh token");
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[tokio::test]
async fn a_static_token_is_sent_ahead_of_an_oauth_access_token() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let log = Arc::new(Mutex::new(Log::default()));
    tokio::spawn(stub(listener, log.clone(), "static-wins"));

    let file = temp_config(
        "static",
        json!({
            "url": base,
            "token": "static-wins",
            "oauth": { "client_id": "c1", "access_token": "oauth-loses", "refresh_token": "rt" }
        }),
    );
    let path = file.path.clone();
    let client = client_for(file, &base);

    let (status, _) = client.http_get("/admin/whoami").await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(log.lock().unwrap().whoami_tokens, vec!["static-wins".to_string()]);
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}
