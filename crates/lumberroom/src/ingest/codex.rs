//! The Codex parser: exclusions E5 to E7 and the `call_id` join.
//!
//! Codex records `call_id` on both halves of a tool call, and `function_call` carries a `namespace`
//! field holding `mcp__<server>__`, which identifies an MCP memory server with one string
//! comparison and no join at all. The join is still built, because `custom_tool_call` carries a
//! bare `name`.
//!
//! Measured against real files on this machine: `namespace` does not always carry the trailing
//! double underscore the spec names (`mcp__agentmemory` as well as `mcp__agentmemory__`, both seen
//! in the same corpus), so the memory check normalises before calling `is_memory_tool`. A prefix
//! comparison against the literal string would miss the short form and let a memory-tool call
//! through as ordinary tool output.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::client::Result;
use crate::ingest::claude::FileParse;
use crate::ingest::spans::ClassifiedEntry;
use crate::ingest::{backstop_token, is_memory_tool, FenceState, Limits, PlanCounters, Speaker};

/// What a `function_call` or `custom_tool_call` told the join about itself, kept until its output
/// arrives. Codex always writes the call before the matching output in one file, so one forward
/// pass is enough.
struct CallInfo {
    name: String,
    is_memory: bool,
}

/// The per-entry facts a `ClassifiedEntry` needs whatever the classifier decides.
struct Ctx {
    uuid: Option<String>,
    timestamp: Option<DateTime<Utc>>,
    cwd: Option<String>,
    byte_start: i64,
    byte_end: i64,
}

/// Same contract as the Claude parser, and the same counters.
///
/// The one trap: `response_item` / `role: "user"` is not the owner. One session held 3
/// `user_message` events against 4 `role: "user"` entries, one of which was `<environment_context>`.
/// Only `event_msg` with `payload.type == "user_message"` is `owner_typed`.
pub fn parse_file(
    path: &Path,
    start: i64,
    ceiling: i64,
    limits: &Limits,
    counters: &mut PlanCounters,
) -> Result<FileParse> {
    let mut out = FileParse::default();
    let mut calls: HashMap<String, CallInfo> = HashMap::new();
    let mut fence = FenceState::default();

    let stats = crate::ingest::reader::for_each_line(
        path,
        start.max(0) as u64,
        ceiling.max(0) as u64,
        limits.max_line_bytes,
        |line, byte_start, byte_end| {
            counters.entries_seen += 1;
            out.entries_seen += 1;

            // The fence runs on the raw line before anything reads its envelope type. The marker
            // can land inside a `function_call_output` string, and a scan sitting after the type
            // switch would never meet it there. Bound to the run's own uuid, same as the Claude
            // Code parser: an unrelated line quoting the marker text falls through instead of
            // opening a fence with nothing to close it.
            if fence.observe(line, byte_start, counters) {
                return Ok(());
            }

            let Ok(v) = serde_json::from_str::<Value>(line) else {
                counters.unknown("entry_type", "unparseable_line");
                return Ok(());
            };

            let entry_type = v.get("type").and_then(Value::as_str).unwrap_or("");
            let payload = v.get("payload").cloned().unwrap_or(Value::Null);
            let ts = str_field(&v, "timestamp")
                .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|d| d.with_timezone(&Utc));

            classify_entry(
                entry_type,
                &payload,
                Ctx { uuid: None, timestamp: ts, cwd: out.cwd.clone(), byte_start, byte_end },
                limits,
                counters,
                &mut calls,
                &mut out,
            );
            Ok(())
        },
    )?;

    out.session_id = out.session_id.clone().or_else(|| session_id_from_filename(path));
    out.is_sidechain = false;
    out.consumed = stats.consumed;
    out.fence_open = fence.is_open();
    out.fence_open_byte = fence.open_since();
    if out.fence_open {
        counters.fences_unclosed += 1;
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn classify_entry(
    entry_type: &str,
    payload: &Value,
    ctx: Ctx,
    limits: &Limits,
    counters: &mut PlanCounters,
    calls: &mut HashMap<String, CallInfo>,
    out: &mut FileParse,
) {
    match entry_type {
        "session_meta" => {
            if out.session_id.is_none() {
                out.session_id = str_field(payload, "id");
            }
            if out.cwd.is_none() {
                out.cwd = str_field(payload, "cwd");
            }
            counters.exclude("system");
        }
        "turn_context" => {
            // The freshest `cwd` signal in the file: a session that `cd`s mid-run should tag its
            // later spans with where the owner actually was, not where it started.
            if let Some(c) = str_field(payload, "cwd") {
                out.cwd = Some(c);
            }
            counters.exclude("system");
        }
        "token_count" | "compacted" => {
            counters.exclude("system");
        }
        "event_msg" => classify_event_msg(payload, ctx, limits, counters, out),
        "response_item" => classify_response_item(payload, ctx, limits, counters, calls, out),
        "" => {
            counters.unknown("entry_type", "missing_type");
            counters.exclude("unknown");
        }
        other => {
            counters.exclude("unknown");
            counters.unknown("entry_type", other);
        }
    }
}

fn classify_event_msg(
    payload: &Value,
    ctx: Ctx,
    limits: &Limits,
    counters: &mut PlanCounters,
    out: &mut FileParse,
) {
    let kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "user_message" => {
            let text = payload.get("message").and_then(Value::as_str).unwrap_or("").to_string();
            push(out, counters, ctx, Speaker::OwnerTyped, None, text, limits);
        }
        "agent_message" => {
            // Renders the same text `response_item` / `message` / `role: assistant` already
            // carries. Measured at 30 against 30 in one file: keeping both would double every
            // fact this parser could ever propose.
            counters.exclude("duplicate_agent_message");
        }
        _ => {
            counters.exclude("system");
        }
    }
}

fn classify_response_item(
    payload: &Value,
    ctx: Ctx,
    limits: &Limits,
    counters: &mut PlanCounters,
    calls: &mut HashMap<String, CallInfo>,
    out: &mut FileParse,
) {
    let item_type = payload.get("type").and_then(Value::as_str).unwrap_or("");
    match item_type {
        "message" => {
            let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
            match role {
                "developer" => {
                    counters.exclude("developer");
                }
                "assistant" => {
                    let text = message_text(payload);
                    let ctx = Ctx { uuid: str_field(payload, "id"), ..ctx };
                    push(out, counters, ctx, Speaker::MainModel, None, text, limits);
                }
                "user" if message_text(payload).contains("<environment_context>") => {
                    counters.exclude("environment_context");
                }
                _ => {
                    // Every other role, `user` included: §5 is explicit that Codex `role: "user"`
                    // is not the owner outside `event_msg`.
                    counters.exclude("system");
                }
            }
        }
        "function_call" | "custom_tool_call" => {
            let name = payload.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let namespace = payload.get("namespace").and_then(Value::as_str);
            let is_memory = namespace.map(namespace_is_memory).unwrap_or(false) || is_memory_tool(&name);
            if let Some(cid) = payload.get("call_id").and_then(Value::as_str) {
                calls.insert(cid.to_string(), CallInfo { name, is_memory });
            }
            counters.exclude(if is_memory { "memory_tool" } else { "system" });
        }
        "function_call_output" | "custom_tool_call_output" => {
            let call_id = payload.get("call_id").and_then(Value::as_str);
            match call_id.and_then(|cid| calls.get(cid)) {
                None => {
                    // Never kept with an unknown name. A result whose call sits below the
                    // watermark cannot be shown to be from a non-memory tool, so it is not shown.
                    counters.exclude("tool_result_unjoined");
                }
                Some(info) if info.is_memory => {
                    counters.exclude("memory_tool");
                }
                Some(info) => {
                    let name = info.name.clone();
                    let text = truncate(&output_text(payload), limits.span_chars);
                    push(out, counters, ctx, Speaker::ToolReturned, Some(name), text, limits);
                }
            }
        }
        // `reasoning` and anything a later Codex release adds land here: §4.3 wants an
        // unrecognised *entry* shape counted, not guessed at, but a known `response_item` subtype
        // with no dedicated rule is ordinary harness noise, not unknown.
        _ => {
            counters.exclude("system");
        }
    }
}

/// E3 runs here so no path reaches `out.entries` without it. E5 should have caught every one of
/// these already: `<agentmemory-context>` lands inside a Codex `developer` entry, so a zero count
/// on `agentmemory_context` is the evidence E5 is doing its job, not proof the check is unreachable.
fn push(
    out: &mut FileParse,
    counters: &mut PlanCounters,
    ctx: Ctx,
    speaker: Speaker,
    tool_name: Option<String>,
    text: String,
    _limits: &Limits,
) {
    if let Some(token) = backstop_token(&text) {
        *counters.backstop.entry(token.to_string()).or_insert(0) += 1;
        counters.exclude("backstop");
        return;
    }
    if text.trim().is_empty() {
        // Counted rather than dropped, the same as the Claude side. A reasoning item or a call with
        // no rendered text lands here, and an exclusion with no counter is an exclusion nobody
        // finds.
        counters.exclude("empty_text");
        return;
    }
    counters.speaker(speaker);
    out.entries.push(ClassifiedEntry {
        uuid: ctx.uuid,
        speaker,
        tool_name,
        timestamp: ctx.timestamp,
        cwd: ctx.cwd,
        byte_start: ctx.byte_start,
        byte_end: ctx.byte_end,
        text,
    });
}

/// `namespace` on a real `function_call` was seen both as `mcp__agentmemory__` and as the shorter
/// `mcp__agentmemory`, missing the separator the tool name itself would have carried. Comparing
/// the raw field against `is_memory_tool` alone misses the short form, since the prefix it checks
/// against is longer than the field would then be.
fn namespace_is_memory(namespace: &str) -> bool {
    if is_memory_tool(namespace) {
        return true;
    }
    let normalised = format!("{}__", namespace.trim_end_matches('_'));
    is_memory_tool(&normalised)
}

/// Text out of a `message` item's `content` array. `input_text` and `output_text` items both carry
/// a plain `text` field; anything else in the array (an image, say) contributes nothing here.
fn message_text(payload: &Value) -> String {
    payload
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Text out of a tool output. Both output shapes measured on this machine carry a plain string in
/// `output`; a structured `{content: "..."}` form is handled in case a newer Codex build sends one.
fn output_text(payload: &Value) -> String {
    if let Some(s) = payload.get("output").and_then(Value::as_str) {
        return s.to_string();
    }
    payload
        .get("output")
        .and_then(|o| o.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn truncate(text: &str, cap: usize) -> String {
    if text.chars().count() <= cap {
        return text.to_string();
    }
    text.chars().take(cap).collect()
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from)
}

/// `session_meta` carries the real id, but a resumed range that never reaches that first line
/// still needs one: `rollout-<date>-<time>-<uuid>.jsonl` puts a full uuid at the end, five groups
/// split on `-`, after a date-time prefix that also uses `-`.
fn session_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let parts: Vec<&str> = stem.split('-').collect();
    if parts.len() < 5 {
        return None;
    }
    let tail = parts[parts.len() - 5..].join("-");
    if tail.len() == 36 && tail.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        Some(tail)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{FENCE_BEGIN, FENCE_END, FENCE_RUN};
    use serde_json::json;

    fn limits() -> Limits {
        Limits {
            span_chars: 6000,
            chunk_spans: 40,
            chunk_chars: 24_000,
            max_line_bytes: 1024 * 1024,
            max_files: 100,
            max_entries: 100_000,
            retention_days: 7,
        }
    }

    fn parse(name: &str, lines: &[Value]) -> (FileParse, PlanCounters) {
        let dir = std::env::temp_dir().join(format!("lumberroom-codex-{}-{}", std::process::id(), name));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout-2026-07-04T18-55-31-019f2d4e-42f0-71d1-bc07-a5e595f48656.jsonl");
        let body: String =
            lines.iter().map(|v| format!("{}\n", serde_json::to_string(v).unwrap())).collect();
        std::fs::write(&path, &body).unwrap();
        let mut counters = PlanCounters::default();
        let parsed = parse_file(&path, 0, body.len() as i64, &limits(), &mut counters).unwrap();
        (parsed, counters)
    }

    fn excluded(c: &PlanCounters, rule: &str) -> i32 {
        c.entries_excluded.get(rule).copied().unwrap_or(0)
    }

    #[test]
    fn owner_typed_comes_only_from_user_message_event() {
        let (p, c) = parse(
            "owner",
            &[
                json!({"type":"event_msg","timestamp":"2026-07-04T13:00:00Z","payload":{"type":"user_message","message":"hello"}}),
                json!({"type":"response_item","timestamp":"2026-07-04T13:00:01Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n<cwd>/x</cwd>\n</environment_context>"}]}}),
            ],
        );
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].speaker, Speaker::OwnerTyped);
        assert_eq!(p.entries[0].text, "hello");
        assert_eq!(excluded(&c, "environment_context"), 1);
    }

    #[test]
    fn developer_role_is_excluded_with_no_speaker_counted() {
        let (p, c) = parse(
            "dev",
            &[json!({"type":"response_item","timestamp":"2026-07-04T13:00:00Z","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions>..."}]}})],
        );
        assert!(p.entries.is_empty());
        assert_eq!(c.speakers.get("hook_injected"), None);
        assert_eq!(excluded(&c, "developer"), 1);
    }

    #[test]
    fn duplicate_agent_message_is_dropped() {
        let (p, c) = parse(
            "dup",
            &[
                json!({"type":"response_item","timestamp":"2026-07-04T13:00:00Z","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"the real one"}]}}),
                json!({"type":"event_msg","timestamp":"2026-07-04T13:00:01Z","payload":{"type":"agent_message","message":"the real one"}}),
            ],
        );
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].speaker, Speaker::MainModel);
        assert_eq!(excluded(&c, "duplicate_agent_message"), 1);
    }

    #[test]
    fn memory_tool_excluded_by_namespace_with_no_join() {
        let (p, c) = parse(
            "memns",
            &[
                json!({"type":"response_item","timestamp":"2026-07-04T13:00:00Z","payload":{"type":"function_call","name":"memory_sessions","namespace":"mcp__agentmemory","call_id":"call_1","arguments":"{}"}}),
                json!({"type":"response_item","timestamp":"2026-07-04T13:00:01Z","payload":{"type":"function_call_output","call_id":"call_1","output":"[]"}}),
            ],
        );
        assert!(p.entries.is_empty());
        assert_eq!(excluded(&c, "memory_tool"), 2);
    }

    #[test]
    fn custom_tool_call_joins_by_call_id_for_its_name() {
        let (p, _) = parse(
            "custom",
            &[
                json!({"type":"response_item","timestamp":"2026-07-04T13:00:00Z","payload":{"type":"custom_tool_call","name":"apply_patch","call_id":"call_9","input":"*** Begin Patch"}}),
                json!({"type":"response_item","timestamp":"2026-07-04T13:00:01Z","payload":{"type":"custom_tool_call_output","call_id":"call_9","output":"Success"}}),
            ],
        );
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].speaker, Speaker::ToolReturned);
        assert_eq!(p.entries[0].tool_name.as_deref(), Some("apply_patch"));
        assert_eq!(p.entries[0].text, "Success");
    }

    #[test]
    fn unjoined_output_is_dropped_and_counted() {
        let (p, c) = parse(
            "unjoined",
            &[json!({"type":"response_item","timestamp":"2026-07-04T13:00:00Z","payload":{"type":"function_call_output","call_id":"call_missing","output":"orphan"}})],
        );
        assert!(p.entries.is_empty());
        assert_eq!(excluded(&c, "tool_result_unjoined"), 1);
    }

    #[test]
    fn system_types_are_excluded_with_no_speaker_counted() {
        let (p, c) = parse(
            "system",
            &[
                json!({"type":"session_meta","timestamp":"2026-07-04T13:00:00Z","payload":{"id":"019f2d4e-42f0-71d1-bc07-a5e595f48656","cwd":"/proj"}}),
                json!({"type":"turn_context","timestamp":"2026-07-04T13:00:01Z","payload":{"cwd":"/proj"}}),
                json!({"type":"compacted","timestamp":"2026-07-04T13:00:02Z","payload":{}}),
                json!({"type":"response_item","timestamp":"2026-07-04T13:00:03Z","payload":{"type":"reasoning","id":"rs_1","summary":[]}}),
            ],
        );
        assert!(p.entries.is_empty());
        assert!(c.speakers.is_empty());
        assert_eq!(excluded(&c, "system"), 4);
        assert_eq!(p.session_id.as_deref(), Some("019f2d4e-42f0-71d1-bc07-a5e595f48656"));
        assert_eq!(p.cwd.as_deref(), Some("/proj"));
        assert!(!p.is_sidechain);
    }

    #[test]
    fn unknown_entry_type_is_counted_not_dropped_silently() {
        let (p, c) = parse(
            "unknown",
            &[json!({"type":"world_state","timestamp":"2026-07-04T13:00:00Z","payload":{"full":true}})],
        );
        assert!(p.entries.is_empty());
        assert_eq!(*c.unknown_types.get("entry_type:world_state").unwrap(), 1);
        assert_eq!(excluded(&c, "unknown"), 1);
    }

    #[test]
    fn fence_drops_everything_between_a_matching_begin_and_end() {
        let (p, c) = parse(
            "fence",
            &[
                json!({"type":"event_msg","timestamp":"2026-07-04T13:00:00Z","payload":{"type":"user_message","message":"before"}}),
                json!({"type":"response_item","timestamp":"2026-07-04T13:00:01Z","payload":{"type":"function_call_output","call_id":"x","output":format!("{FENCE_BEGIN}0199e5ef-9698-7cb3-8c23-3297e08d3c03")}}),
                json!({"type":"event_msg","timestamp":"2026-07-04T13:00:02Z","payload":{"type":"user_message","message":"inside the fence, never seen"}}),
                json!({"type":"response_item","timestamp":"2026-07-04T13:00:03Z","payload":{"type":"function_call_output","call_id":"y","output":format!("{FENCE_END}0199e5ef-9698-7cb3-8c23-3297e08d3c03")}}),
                json!({"type":"event_msg","timestamp":"2026-07-04T13:00:04Z","payload":{"type":"user_message","message":"after"}}),
            ],
        );
        assert_eq!(p.entries.len(), 2);
        assert_eq!(p.entries[0].text, "before");
        assert_eq!(p.entries[1].text, "after");
        assert_eq!(excluded(&c, "ingest_fence"), 3);
        assert_eq!(c.fenced_entries, 3);
        assert!(!p.fence_open);
        assert_eq!(c.fences_unclosed, 0);
    }

    #[test]
    fn a_close_with_no_matching_open_uuid_never_ends_the_fence() {
        // The bare `FENCE_END` this used to accept (a `contains` match with no run id) closed
        // every future run's fence just as well as its own; that is retired. A close carrying the
        // wrong uuid, or none at all, must leave a genuinely open fence open.
        let (p, c) = parse(
            "fence-bare-close",
            &[
                json!({"type":"response_item","timestamp":"2026-07-04T13:00:01Z","payload":{"type":"function_call_output","call_id":"x","output":format!("{FENCE_BEGIN}0199e5ef-9698-7cb3-8c23-3297e08d3c03")}}),
                json!({"type":"response_item","timestamp":"2026-07-04T13:00:02Z","payload":{"type":"function_call_output","call_id":"y","output":FENCE_END}}),
                json!({"type":"event_msg","timestamp":"2026-07-04T13:00:03Z","payload":{"type":"user_message","message":"still fenced"}}),
            ],
        );
        assert!(p.entries.is_empty());
        assert!(p.fence_open);
        assert_eq!(c.unknown_types.get("fence_marker:close_mismatch"), Some(&1));
    }

    #[test]
    fn fence_run_marker_also_opens_a_fence() {
        let (p, _) = parse(
            "fence-run",
            &[json!({"type":"event_msg","timestamp":"2026-07-04T13:00:00Z","payload":{"type":"user_message","message":format!("{FENCE_RUN}0199e5ef-9698-7cb3-8c23-3297e08d3c03")}})],
        );
        assert!(p.entries.is_empty());
    }

    #[test]
    fn a_bare_begin_marker_with_no_uuid_does_not_open_a_fence() {
        // A line that merely quotes the marker text (a grep hit, a fetched page) must fall
        // through to ordinary parsing rather than swallowing the rest of the file.
        let (p, c) = parse(
            "fence-no-uuid",
            &[json!({"type":"event_msg","timestamp":"2026-07-04T13:00:00Z","payload":{"type":"user_message","message":FENCE_BEGIN}})],
        );
        assert_eq!(p.entries.len(), 1, "no fence, so the line is an ordinary entry");
        assert!(!p.fence_open);
        assert_eq!(c.unknown_types.get("fence_marker:begin_no_uuid"), Some(&1));
    }

    #[test]
    fn fence_left_open_at_ceiling_is_reported_with_its_opening_offset() {
        let (p, c) = parse(
            "fence-open",
            &[json!({"type":"event_msg","timestamp":"2026-07-04T13:00:00Z","payload":{"type":"user_message","message":format!("{FENCE_BEGIN}0199e5ef-9698-7cb3-8c23-3297e08d3c03")}})],
        );
        assert!(p.entries.is_empty());
        assert!(p.fence_open);
        assert_eq!(c.fences_unclosed, 1);
        assert_eq!(p.fence_open_byte, Some(0), "the offset of the line that opened it");
    }

    #[test]
    fn backstop_fires_against_the_historical_preamble_text() {
        let (p, c) = parse(
            "backstop",
            &[json!({"type":"event_msg","timestamp":"2026-07-04T13:00:00Z","payload":{"type":"user_message","message":"Durable memory for this user, retrieved automatically at session start, follows."}})],
        );
        assert!(p.entries.is_empty());
        assert_eq!(*c.backstop.get("digest_preamble").unwrap(), 1);
        assert_eq!(excluded(&c, "backstop"), 1);
    }

    #[test]
    fn namespace_without_trailing_separator_still_matches() {
        assert!(namespace_is_memory("mcp__agentmemory"));
        assert!(namespace_is_memory("mcp__agentmemory__"));
        assert!(!namespace_is_memory("mcp__github"));
    }

    #[test]
    fn session_id_falls_back_to_the_filename() {
        let path = Path::new("/x/rollout-2026-07-04T18-55-31-019f2d4e-42f0-71d1-bc07-a5e595f48656.jsonl");
        assert_eq!(
            session_id_from_filename(path).as_deref(),
            Some("019f2d4e-42f0-71d1-bc07-a5e595f48656")
        );
    }
}
