//! Mode C's lifecycle, driven end to end against a loopback stub.
//!
//! `batch.rs`'s own tests are pure functions over a body and a results array. This drives
//! `batch::run` itself: a real run directory under `LUMBERROOM_STATE_DIR`, a real config file under
//! `LUMBERROOM_CONFIG`, a real `reqwest` call over a real socket. What it settles is the part that lives
//! between those functions: that a submission writes an id nobody has to keep, that polling a
//! submitted run never creates a second batch, that a finished batch splits once and only once,
//! and that a resubmission carries the failed chunks and nothing else.
//!
//! What it does not settle is the wire contract. The stub answers what `batch.rs` believes
//! OpenRouter answers, so a disagreement about where `id` sits in the 202, whether the results
//! array is called `results`, or whether a poll carries `usage` at all survives this test intact.
//! Only a real submission closes that.
//!
//! One test function, in order, because `LUMBERROOM_CONFIG` and `LUMBERROOM_STATE_DIR` are process-wide and
//! the steps depend on each other's state.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::ingest::batch::{self, BatchAction, BatchArgs};
use crate::ingest::{write_json, ChunkRef, PlanCounters, RunPaths, RunState, Worklist};

// ---------------------------------------------------------------------------
// The stub
// ---------------------------------------------------------------------------

#[derive(Default)]
struct StubState {
    /// Every body a POST arrived with, in order. Step 1 reads the first, step 6 the second.
    created: Vec<Value>,
    ids: usize,
    /// What the next GET answers. The test rewrites this between steps.
    poll: Value,
}

impl StubState {
    fn answer(&mut self, method: &str, body: Value) -> (u16, Value) {
        if method == "POST" {
            self.created.push(body);
            self.ids += 1;
            let id = format!("batch_test_{}", self.ids);
            self.poll = json!({ "id": id, "status": "validating", "request_counts": {} });
            (202, json!({ "id": id, "status": "validating" }))
        } else {
            (200, self.poll.clone())
        }
    }
}

async fn stub() -> (SocketAddr, Arc<Mutex<StubState>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("the stub binds a port");
    let addr = listener.local_addr().expect("the stub has an address");
    let state = Arc::new(Mutex::new(StubState::default()));
    let served = state.clone();
    tokio::spawn(async move { serve(listener, served).await });
    (addr, state)
}

async fn serve(listener: TcpListener, state: Arc<Mutex<StubState>>) {
    loop {
        let Ok((mut sock, _)) = listener.accept().await else { return };
        let state = state.clone();
        tokio::spawn(async move {
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 4096];
            // Headers first, then exactly `content-length` more bytes. A read loop that stops at
            // the first packet reads a truncated body on any request bigger than one segment,
            // which is every batch of more than a chunk or two.
            let head_end = loop {
                match sock.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
                if let Some(at) = find(&buf, b"\r\n\r\n") {
                    break at + 4;
                }
            };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
            let want = content_length(&head);
            while buf.len() < head_end + want {
                match sock.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let body: Value = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
            let method = head.split_whitespace().next().unwrap_or("").to_string();

            let (code, reply) = state.lock().expect("the stub lock is not poisoned").answer(&method, body);
            let text = reply.to_string();
            let response = format!(
                "HTTP/1.1 {code} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                if code == 202 { "Accepted" } else { "OK" },
                text.len(),
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.flush().await;
        });
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn content_length(head: &str) -> usize {
    head.lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// The run directory and the config file
// ---------------------------------------------------------------------------

static SCRATCH: AtomicUsize = AtomicUsize::new(0);

fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let n = SCRATCH.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lumberroom-batch-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scratch directory is writable");
    dir
}

/// 0600, because `resolve` refuses a config file readable by group or other before it reads a
/// single field out of it.
fn write_config(dir: &std::path::Path, name: &str, value: &Value) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, serde_json::to_vec_pretty(value).expect("the config serialises"))
        .expect("the config file is writable");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("the config file takes 0600");
    }
    path
}

fn plant_run(run_id: uuid::Uuid, chunks: usize) -> RunPaths {
    let paths = RunPaths::new(run_id).expect("the state directory resolves");
    paths.create().expect("the run directory is writable");
    let worklist = Worklist {
        run_id,
        created_at: chrono::Utc::now(),
        scope: json!({ "source": "fixture" }),
        include_tool_output: false,
        files: vec![],
        spans: vec![],
        chunks: (0..chunks)
            .map(|index| ChunkRef { index, span_ids: vec![format!("s{index}")] })
            .collect(),
        counters: PlanCounters::default(),
    };
    write_json(&paths.worklist(), &worklist).expect("the worklist writes");
    for index in 0..chunks {
        write_json(&paths.chunk_in(index), &json!([{ "id": format!("s{index}"), "text": "a span" }]))
            .expect("the chunk input writes");
    }
    paths
}

fn read_state(paths: &RunPaths) -> RunState {
    paths.read_state()
}

fn batch_record(paths: &RunPaths) -> Value {
    read_state(paths).batch.unwrap_or(Value::Null)
}

fn args(run_id: uuid::Uuid, addr: SocketAddr, action: BatchAction, retry_failed: bool) -> BatchArgs {
    BatchArgs {
        run_id,
        provider: "custom".to_string(),
        model: Some("stub-model".to_string()),
        base_url: Some(format!("http://{addr}/v1")),
        action,
        dry_run: false,
        retry_failed,
        yes: true,
        timeout_secs: 10,
    }
}

fn ok_result(index: usize, facts: usize) -> Value {
    let facts: Vec<Value> = (0..facts)
        .map(|n| {
            json!({
                "content": format!("fact {n} from chunk {index}"),
                "namespace": "project:lumberroom",
                "source_span_id": format!("s{index}"),
            })
        })
        .collect();
    let content = json!({ "facts": facts }).to_string();
    json!({
        "custom_id": format!("chunk-{index:02}"),
        "response": {
            "status_code": 200,
            "request_id": format!("req-{index}"),
            "body": { "choices": [ { "message": { "content": content } } ] }
        },
        "error": null
    })
}

// ---------------------------------------------------------------------------
// The lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_run_submits_polls_splits_once_and_resubmits_only_what_failed() {
    let (addr, stub_state) = stub().await;
    let home = scratch_dir("home");
    let state_dir = scratch_dir("state");
    std::env::set_var("LUMBERROOM_STATE_DIR", &state_dir);

    // 0. A provider with no batch endpoint stops at the config file rather than falling back to
    // the synchronous path, which would spend a day's turnaround the owner did not ask for.
    let bare = write_config(&home, "no-batch.json", &json!({}));
    std::env::set_var("LUMBERROOM_CONFIG", &bare);
    let stray = uuid::Uuid::new_v4();
    plant_run(stray, 1);
    let e = batch::run(&args(stray, addr, BatchAction::Advance, false))
        .await
        .expect_err("there is nowhere to post");
    assert!(e.message.contains("declares no batch endpoint"), "{}", e.message);

    let cfg = write_config(
        &home,
        "config.json",
        &json!({ "ingest": { "providers": { "custom": {
            "batch": { "endpoint": format!("http://{addr}/batches") }
        } } } }),
    );
    std::env::set_var("LUMBERROOM_CONFIG", &cfg);

    let run_id = uuid::Uuid::new_v4();
    let paths = plant_run(run_id, 3);

    // 1. Submit. The id has to reach state.json: it is the only handle on results that sit on a
    // provider for thirty days, and nothing local can ask for them without it.
    batch::run(&args(run_id, addr, BatchAction::Advance, false))
        .await
        .expect("the submission goes out");
    let record = batch_record(&paths);
    assert_eq!(record["id"], "batch_test_1", "the batch id did not reach state.json: {record}");
    assert_eq!(record["provider"], "custom");
    assert_eq!(record["chunks"], json!([0, 1, 2]));
    assert!(record["collected_at"].is_null(), "nothing has been split yet: {record}");

    // The body the stub received, which is the request contract this driver owns.
    let sent = stub_state.lock().unwrap().created[0].clone();
    assert_eq!(sent["endpoint"], "/v1/chat/completions");
    assert_eq!(sent["model"], "stub-model");
    let requests = sent["requests"].as_array().expect("requests is an array").clone();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2]["custom_id"], "chunk-02");
    assert!(requests[2]["body"].get("model").is_none(), "the batch names the model once");
    // `custom` defaults json_mode off, so this is the divergence that would have 400'd every
    // request of a real OpenRouter batch a day after it went out.
    assert!(
        requests[0]["body"].get("response_format").is_none(),
        "response_format went out for a provider with json_mode off: {}",
        requests[0]["body"]
    );
    assert_eq!(requests[0]["body"]["reasoning"]["enabled"], json!(false));

    // 2. Advance again while it runs. One batch per run, whatever a scheduler does on a timer.
    batch::run(&args(run_id, addr, BatchAction::Advance, false)).await.expect("the poll succeeds");
    {
        let s = stub_state.lock().unwrap();
        assert_eq!(s.created.len(), 1, "polling created a second batch");
    }
    assert_eq!(batch_record(&paths)["status"], "validating");

    // 3. Fetching a running batch is an error rather than a run of empty chunk files.
    let e = batch::run(&args(run_id, addr, BatchAction::Fetch, false))
        .await
        .expect_err("a running batch has nothing to split");
    assert!(e.message.contains("nothing to split"), "{}", e.message);

    // 4. Complete it. Chunk 0 comes back, chunk 1 errored, chunk 2 never appears at all, and one
    // result belongs to nobody. All four land somewhere the owner can read.
    stub_state.lock().unwrap().poll = json!({
        "id": "batch_test_1",
        "status": "completed",
        "request_counts": { "total": 3, "completed": 1, "failed": 1 },
        "usage": { "prompt_tokens": 120, "completion_tokens": 30 },
        "results": [
            ok_result(0, 2),
            { "custom_id": "chunk-01", "response": null, "error": { "message": "no such model" } },
            { "custom_id": "req-0001", "response": null, "error": null },
        ]
    });
    // `--batch-status` asks and creates nothing, which is what makes it safe on a timer next to a
    // human who is watching. A status that split the results would spend the fetch nobody asked for.
    batch::run(&args(run_id, addr, BatchAction::Status, false)).await.expect("status prints");
    assert!(!paths.chunk_out(0).exists(), "--batch-status wrote a chunk file");
    assert_eq!(stub_state.lock().unwrap().created.len(), 1, "--batch-status resent the job");

    batch::run(&args(run_id, addr, BatchAction::Advance, false)).await.expect("the split succeeds");

    assert!(paths.chunk_out(0).exists(), "chunk 0 came back and has no out/ file");
    assert!(!paths.chunk_out(1).exists(), "a failed chunk must not leave an out/ file");
    let state = read_state(&paths);
    assert_eq!(state.done, vec![0usize]);
    let failed: Vec<(usize, String)> =
        state.failed.iter().map(|f| (f.index, f.reason.clone())).collect();
    assert_eq!(
        failed,
        vec![(1, "batch_error".to_string()), (2, "missing_from_batch".to_string())],
        "a chunk the batch never mentioned has to read as failed, not as a short run"
    );
    assert_eq!(state.usage.requests, 1);
    assert_eq!(state.usage.prompt_tokens, 120);
    assert_eq!(state.usage.completion_tokens, 30);
    assert!(!batch_record(&paths)["collected_at"].is_null(), "the split was not recorded");

    // 5. Poll the finished batch again. Free, and the owner will do it, so it must not count the
    // same tokens twice.
    batch::run(&args(run_id, addr, BatchAction::Advance, false)).await.expect("a second poll is fine");
    let state = read_state(&paths);
    assert_eq!(state.usage.prompt_tokens, 120, "a second poll counted the batch's tokens again");
    assert_eq!(state.usage.requests, 1);

    // 6. Resubmit the failures. A finished batch is spent, and the new record replaces it.
    batch::run(&args(run_id, addr, BatchAction::Advance, true)).await.expect("the resubmission goes out");
    let sent = {
        let s = stub_state.lock().unwrap();
        assert_eq!(s.created.len(), 2, "--retry-failed did not submit a second batch");
        s.created[1].clone()
    };
    let requests = sent["requests"].as_array().expect("requests is an array");
    let ids: Vec<&str> = requests.iter().filter_map(|r| r["custom_id"].as_str()).collect();
    assert_eq!(ids, vec!["chunk-01", "chunk-02"], "the second batch resent a chunk that succeeded");
    let record = batch_record(&paths);
    assert_eq!(record["id"], "batch_test_2");
    assert_eq!(record["chunks"], json!([1, 2]));
    assert!(record["collected_at"].is_null(), "the new batch starts uncollected");

    std::env::remove_var("LUMBERROOM_CONFIG");
    std::env::remove_var("LUMBERROOM_STATE_DIR");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&state_dir);
}

/// The body `scripts/ingest-test.sh`'s provider stub answers a poll with, captured from that
/// script's own python and routed through this driver. The shell gate cannot fail a shape this
/// code refuses without the failure reading as a broken CLI, so the shape is pinned here where a
/// mismatch names itself.
#[test]
fn the_exit_tests_stub_answers_a_shape_this_driver_routes() {
    let body: Value = serde_json::from_str(
        r#"{
          "id": "batch_stub_1",
          "status": "completed",
          "request_counts": {"total": 1, "completed": 1, "failed": 0},
          "usage": {"prompt_tokens": 111, "completion_tokens": 22},
          "results": [{
            "custom_id": "chunk-00",
            "error": null,
            "response": {
              "status_code": 200,
              "request_id": "r-0",
              "body": {"choices": [{"message": {"content":
                "{\"facts\": [{\"content\": \"a batched fact\", \"namespace\": \"project:ingest-test\", \"source_span_id\": \"s0\", \"speaker\": \"owner_typed\", \"confidence\": \"high\"}]}"
              }}]}
            }
          }]
        }"#,
    )
    .expect("the captured body parses");

    assert!(batch::Status::parse(body["status"].as_str().unwrap()).has_results());
    let results = body["results"].as_array().expect("results is an array");
    let routed = batch::route_results(results, &[0]);
    assert!(routed.failed.is_empty(), "{:?}", routed.failed);
    assert!(routed.unroutable.is_empty());
    assert_eq!(routed.ok.len(), 1);
    assert_eq!(routed.ok[0].1.facts.len(), 1);
    assert_eq!(routed.ok[0].1.facts[0].source_span_id, "s0");
}
