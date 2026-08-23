//! The pin. Every fixture under `tests/fixtures/` is a response the server produces, transcribed
//! from the code that builds it, with the file and symbol named above each case.
//!
//! Two directions, because they fail differently. A response that changes shape breaks a client
//! that already shipped, so those are checked by deserializing the fixture and asserting the
//! fields this client prints survive. A request that changes shape fails at the server's
//! `Deserialize` at runtime and never at compile time, so those are checked by serializing and
//! comparing the exact key set against the server's struct.
//!
//! What this does not do is watch the server. It pins the client against a transcription, so a
//! server-side rename lands here only when somebody updates the fixture. Closing that loop needs a
//! test in the server crate that serializes the real types into these same files and diffs; that
//! file is in `tests/`, which this work does not own, and it is in the return's not_done.

use lumberroom::wire::*;
use std::collections::BTreeSet;

fn fixture(name: &str) -> serde_json::Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn keys(value: &serde_json::Value) -> BTreeSet<String> {
    value.as_object().expect("an object").keys().cloned().collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

// ---- responses ----

/// `readyz` in `src/http/mod.rs`.
#[test]
fn readyz_carries_ok_and_the_server_auth_mode() {
    let r: Ready = serde_json::from_value(fixture("readyz.json")).unwrap();
    assert!(r.ok);
    assert_eq!(r.auth_mode.as_deref(), Some("oauth"));
}

/// `whoami` in `src/http/mod.rs`. `mode` is the credential's mode, not the server's.
#[test]
fn whoami_carries_the_client_and_the_credential_mode() {
    let w: Whoami = serde_json::from_value(fixture("whoami.json")).unwrap();
    assert_eq!(w.client, "claude-code");
    assert_eq!(w.mode, "token");
}

/// `services::search::SearchResult` and `Hit`, as `memory_search` structured content.
#[test]
fn a_search_hit_carries_id_namespace_content_and_score() {
    let r: SearchResult = serde_json::from_value(fixture("memory_search.json")).unwrap();
    assert_eq!(r.hits.len(), 1);
    let hit = &r.hits[0];
    assert_eq!(hit.namespace, "user:me");
    assert!(hit.content.starts_with("The owner runs lumberroom"));
    assert!((hit.score - 0.81274).abs() < 1e-9);
    assert_eq!(hit.id, "9f1c2b4e-0000-4a1b-8c3d-1122334455aa");
}

/// `domain::types::WriteOutcome`. `superseded` and `possible_conflicts` are skipped when empty, so
/// the fixture omits them and this must still parse.
#[test]
fn a_write_outcome_parses_without_its_optional_fields() {
    let w: WriteOutcome = serde_json::from_value(fixture("memory_write.json")).unwrap();
    assert!(!w.deduplicated);
    assert_eq!(w.namespace, "user:me");
}

/// `domain::types::Memory`, the whole row, as `GET /admin/memory/{id}` returns it.
#[test]
fn a_memory_row_parses_and_ignores_the_fields_this_client_never_prints() {
    let m: Memory = serde_json::from_value(fixture("memory.json")).unwrap();
    assert_eq!(m.sensitivity, "open");
    assert_eq!(m.tags, vec!["infra".to_string()]);
    assert_eq!(m.source_client, "claude-code");
    assert_eq!(m.created_at, "2026-08-19T11:02:31.442Z");
}

#[test]
fn the_stale_review_is_a_list_of_memory_rows() {
    let r: StaleReview = serde_json::from_value(fixture("review_stale.json")).unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0].namespace, "global");
}

/// `conflict_side` in `src/http/mod.rs` is narrower than a `Memory`, which is why the client has a
/// separate type for it rather than reusing one.
#[test]
fn a_conflict_pair_has_two_sides_and_a_similarity() {
    let r: ConflictReview = serde_json::from_value(fixture("review_conflicts.json")).unwrap();
    let pair = &r.pairs[0];
    assert!((pair.similarity - 0.9412).abs() < 1e-9);
    assert_eq!(pair.older.content, "The owner prefers tabs.");
    assert_eq!(pair.newer.namespace, "user:me");
}

#[test]
fn the_registry_review_has_both_lists() {
    let r: RegistryReview = serde_json::from_value(fixture("review_registry.json")).unwrap();
    assert_eq!(r.due_for_review[0].key, "host:db.prod");
    assert_eq!(r.non_canonical[0].kind, "service");
}

#[test]
fn an_export_page_is_memory_rows() {
    let p: ExportPage = serde_json::from_value(fixture("export.json")).unwrap();
    assert_eq!(p.rows[0].tags.len(), 2);
}

/// `tool_stats_body` in `src/http/mod.rs`, whose rows are `ports::tool_calls::ToolCallStats`.
#[test]
fn per_tool_stats_keep_their_snake_case_latency_keys() {
    let s: ToolStats = serde_json::from_value(fixture("statsz_by_tool.json")).unwrap();
    assert_eq!(s.window_hours, 168);
    assert_eq!(s.totals.unprompted_rate, Some(0.25));
    assert_eq!(s.by_tool[0].p50_ms, Some(41));
    assert_eq!(s.by_tool[0].p95_ms, None);
    // The rename that once turned every latency into "-ms" with nothing failing.
    let raw = fixture("statsz_by_tool.json");
    assert!(raw["by_tool"][0].get("p50_ms").is_some());
    assert!(raw["by_tool"][0].get("p50-ms").is_none());
}

/// `client_stats_body` in `src/http/mod.rs`, rows from `ports::tool_calls::ClientStats`.
#[test]
fn per_client_stats_carry_the_two_rates_the_client_prints() {
    let s: ClientStats = serde_json::from_value(fixture("statsz_by_client.json")).unwrap();
    let row = &s.by_client[0];
    assert_eq!(row.write_to_read_ratio, Some(0.33));
    assert_eq!(row.unprompted_write_rate, Some(0.5));
    assert_eq!(row.reads, 9);
}

/// `clients` in `src/authserver/routes.rs`.
#[test]
fn the_client_list_carries_consent_and_revocation() {
    let l: ClientList = serde_json::from_value(fixture("oauth_clients.json")).unwrap();
    let c = &l.clients[0];
    assert_eq!(c.client_name, "lumberroom");
    assert_eq!(c.registered_via, "dcr");
    assert!(c.consented_at.is_some());
    assert!(c.revoked_at.is_none());
}

#[test]
fn a_token_response_carries_the_refresh_token_and_the_ttl() {
    let t: TokenResponse = serde_json::from_value(fixture("oauth_token.json")).unwrap();
    assert_eq!(t.access_token, "at_abc");
    assert_eq!(t.refresh_token.as_deref(), Some("rt_def"));
    assert_eq!(t.expires_in, Some(3600));
}

/// A server that grows a field must not break a client already installed on a laptop.
#[test]
fn an_unknown_field_on_a_response_is_ignored() {
    let mut raw = fixture("whoami.json");
    raw["some_future_field"] = serde_json::json!({ "a": 1 });
    let w: Whoami = serde_json::from_value(raw).unwrap();
    assert_eq!(w.client, "claude-code");
}

// ---- requests ----

/// `RegistryWrite` in `src/http/mod.rs`: namespace, kind, key, value, and an optional sensitivity
/// this client does not send.
#[test]
fn a_registry_write_sends_exactly_the_four_required_keys() {
    let req = RegistryWriteRequest {
        namespace: "global",
        kind: "host",
        key: "host:db.prod",
        value: serde_json::json!({ "addr": "10.0.0.4" }),
    };
    let v = serde_json::to_value(req).unwrap();
    assert_eq!(keys(&v), set(&["namespace", "kind", "key", "value"]));
}

/// `SupersedeBody` in `src/http/mod.rs`.
#[test]
fn a_supersede_sends_new_id_and_nothing_else() {
    let v = serde_json::to_value(SupersedeRequest { new_id: "2222" }).unwrap();
    assert_eq!(keys(&v), set(&["new_id"]));
}

/// `mcp::SearchArgs`. Every optional is skipped when absent, so the server's own defaults apply.
#[test]
fn search_arguments_omit_what_was_not_asked_for() {
    let v = serde_json::to_value(SearchArgsRequest {
        query: "what do I prefer".into(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(keys(&v), set(&["query"]));

    let v = serde_json::to_value(SearchArgsRequest {
        query: "q".into(),
        namespaces: Some(vec!["user:me".into()]),
        limit: Some(5),
        project: Some("lumberroom".into()),
        as_of: None,
    })
    .unwrap();
    assert_eq!(keys(&v), set(&["query", "namespaces", "limit", "project"]));
}

/// `mcp::WriteArgs`.
#[test]
fn write_arguments_always_carry_content_and_namespace() {
    let v = serde_json::to_value(WriteArgsRequest {
        content: "a fact".into(),
        namespace: "user:me".into(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(keys(&v), set(&["content", "namespace"]));

    let v = serde_json::to_value(WriteArgsRequest {
        content: "a fact".into(),
        namespace: "user:me".into(),
        tags: Some(vec!["infra".into()]),
        supersedes: Some("1111".into()),
        occurred_at: None,
    })
    .unwrap();
    assert_eq!(keys(&v), set(&["content", "namespace", "tags", "supersedes"]));
}

/// `mcp::RegistryArgs`.
#[test]
fn registry_get_arguments_send_kind_and_key_at_minimum() {
    let v = serde_json::to_value(RegistryArgsRequest {
        kind: "host".into(),
        key: "host:db.prod".into(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(keys(&v), set(&["kind", "key"]));
}

/// `mcp::BootstrapArgs`. Project is the only argument, and an absent one is absent rather than null.
#[test]
fn bootstrap_arguments_are_a_project_or_an_empty_object() {
    let v = serde_json::to_value(BootstrapArgsRequest::default()).unwrap();
    assert!(keys(&v).is_empty());
    let v = serde_json::to_value(BootstrapArgsRequest { project: Some("/repo".into()) }).unwrap();
    assert_eq!(keys(&v), set(&["project"]));
}
