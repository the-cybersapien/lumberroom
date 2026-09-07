//! The 401-then-refresh path, run against a real socket.
//!
//! A stub rather than a mock: the failure this guards against is replaying the stale
//! `Authorization` header on the retry, and only a server that reads the second request's headers
//! can tell the difference.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Barrier, Mutex};

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
    // The CLI refuses to read a token file other users can read, so the fixture has to look like
    // one the CLI wrote.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
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
    assert_eq!(
        saved["oauth"]["refresh_token"],
        json!("rt"),
        "an absent rotation keeps the old token"
    );
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

/// Single-use rotation with replay revocation, the contract the real authorization server
/// enforces in `rotate()` (`src/authserver/routes.rs`): the current refresh token exchanges for
/// the next pair, anything already spent is a replay and revokes the family, and no later grant
/// succeeds against it.
#[derive(Default)]
struct Rotation {
    current: Option<String>,
    presented: Vec<String>,
    /// What the stub answered for each presentation, in order, for a failing test to show.
    answered: Vec<(String, u16)>,
}

async fn rotating_stub(listener: TcpListener, rotation: Arc<Mutex<Rotation>>) {
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
        let body = request.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();

        if !path.starts_with("/oauth/token") {
            let response = "HTTP/1.1 404 X\r\ncontent-type: application/json\r\ncontent-length: 24\r\nconnection: close\r\n\r\n{\"error\":\"not_found\"}\r\n";
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.flush().await;
            continue;
        }

        let token = body
            .split('&')
            .find_map(|kv| kv.strip_prefix("refresh_token="))
            .unwrap_or_default()
            .to_string();
        // The guard lives in its own block so no await in this loop can hold it: a
        // MutexGuard across an await makes the whole stub future !Send and tokio::spawn
        // refuses it.
        let (status, payload) = {
            let mut state = rotation.lock().unwrap();
            state.presented.push(token.clone());
            if state.current.as_deref() == Some(token.as_str()) {
                // The first exchange issues rt-2: a rotation that handed back the token it was
                // just presented would make a replay of that token indistinguishable from a
                // legitimate refresh, and the whole test would prove nothing.
                let generation = state.presented.len() + 1;
                let next = format!("rt-{generation}");
                state.current = Some(next.clone());
                state.answered.push((token.clone(), 200));
                (
                    200,
                    json!({
                        "access_token": format!("at-{generation}"),
                        "refresh_token": next,
                        "token_type": "Bearer",
                        "expires_in": 60
                    }),
                )
            } else {
                state.current = None;
                state.answered.push((token.clone(), 400));
                (
                    400,
                    json!({
                        "error": "invalid_grant",
                        "error_description": "refresh token unknown or already spent; token family revoked"
                    }),
                )
            }
        };

        let text = payload.to_string();
        let response = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
            text.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.flush().await;
    }
}

/// One CLI process: its own runtime, its own client, its own in-memory view of the config file.
/// A thread with a runtime of its own is the closest a test gets to a second `lumberroom`
/// invocation on one machine.
fn refresh_as_a_process(path: PathBuf, base: String, barrier: Arc<Barrier>) -> bool {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async move {
        let file = FileConfig::load(path);
        let env: HashMap<String, String> = HashMap::new();
        let resolved = config::resolve(&env, &file, Some(&base), None, None, false, Some("4000"));
        let client = Client::new(resolved, file).unwrap();
        // Both processes hold the same pre-refresh view of the file before either exchanges,
        // which is the state two invocations that started earlier are in.
        barrier.wait();
        client.refresh().await
    })
}

/// The lockout bug, end to end. Two processes refresh off one config file at the same time; both
/// loaded `rt-1` before either moved. The server rotates on first presentation and revokes the
/// family on the second, so if both send `rt-1` the disk ends up holding a dead token and every
/// later refresh is an `invalid_grant`. When the exchange serialises under the config lock, the
/// second process reads the rotated token before it sends anything, and the third refresh, the
/// one that stands for every future run on that machine, still works.
#[tokio::test]
async fn two_processes_refreshing_at_once_never_replay_a_spent_token() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let rotation =
        Arc::new(Mutex::new(Rotation { current: Some("rt-1".to_string()), ..Default::default() }));
    tokio::spawn(rotating_stub(listener, rotation.clone()));

    let file = temp_config(
        "double-refresh",
        json!({
            "url": base,
            "oauth": { "client_id": "c1", "access_token": "stale", "refresh_token": "rt-1" }
        }),
    );
    let path = file.path.clone();
    let barrier = Arc::new(Barrier::new(2));

    let p = path.clone();
    let b = base.clone();
    let start = barrier.clone();
    let first = std::thread::spawn(move || refresh_as_a_process(p, b, start));
    let base_for_second = base.clone();
    let second =
        std::thread::spawn(move || refresh_as_a_process(path.clone(), base_for_second, barrier));
    // join on the blocking pool so the stub on this runtime keeps answering while both run.
    let (r1, r2) =
        tokio::task::spawn_blocking(move || (first.join().unwrap(), second.join().unwrap()))
            .await
            .unwrap();
    assert!(r1, "the process that won the race still reported failure");
    assert!(r2, "the process that lost the race replayed a spent token and got invalid_grant");

    // The family must still be alive: a third run reads whatever is on disk and refreshes.
    let third = {
        let file = FileConfig::load(file.path.clone());
        client_for(file, &base)
    };
    assert!(
        third.refresh().await,
        "the refresh token left on disk was already spent or revoked; every later run is locked out"
    );

    let log = rotation.lock().unwrap();
    assert_eq!(
        log.presented,
        vec!["rt-1".to_string(), "rt-2".to_string(), "rt-3".to_string()],
        "each refresh token went to the server exactly once; answered: {:?}",
        log.answered
    );

    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file.path).unwrap()).unwrap();
    assert_eq!(saved["oauth"]["refresh_token"], json!("rt-4"));
    std::fs::remove_dir_all(file.path.parent().unwrap()).ok();
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
