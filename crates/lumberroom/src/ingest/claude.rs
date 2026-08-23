//! The Claude Code parser: exclusions E1 to E4, the `tool_use_id` join, and the speaker classifier.
//!
//! This is where the correctness lives. Every exclusion is counted by the rule that made it, and an
//! entry type nobody recognises is counted rather than dropped silently.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use crate::client::Result;
use crate::ingest::spans::ClassifiedEntry;
use crate::ingest::{backstop_token, is_memory_tool, FenceState, Limits, PlanCounters, Speaker};

#[derive(Debug, Clone, Default)]
pub struct FileParse {
    pub entries: Vec<ClassifiedEntry>,
    pub entries_seen: i64,
    /// From the first entry carrying one.
    pub session_id: Option<String>,
    pub is_sidechain: bool,
    pub cwd: Option<String>,
    /// One past the last complete line read. The plan ceiling is a bound, not a promise.
    pub consumed: i64,
    /// A fence opened inside this range and never closed by a matching end marker.
    pub fence_open: bool,
    /// Byte offset the fence opened at, when `fence_open` is true. `plan` holds the file's
    /// watermark here instead of at `consumed`, so an unclosed fence costs a re-read of the
    /// fenced region rather than losing everything after it for good.
    pub fence_open_byte: Option<i64>,
}

/// The subtypes measured in this corpus. Membership buys nothing on its own: every attachment is
/// dropped whatever its subtype. The list exists so a Claude Code release that adds one lands in
/// `unknown_types` instead of passing unremarked.
const KNOWN_ATTACHMENT_SUBTYPES: &[&str] = &[
    "hook_success",
    "hook_additional_context",
    "edited_text_file",
    "new_directory",
    "new_file",
    "selected_lines_in_ide",
    "opened_file_in_ide",
    "todo",
    "diagnostics",
    "command_permissions",
    "nested_memory",
    "ultramemory",
    "queued_command",
    "mcp_resource",
];

/// Entry types the harness writes. Excluded, counted, and never a surprise.
const SYSTEM_TYPES: &[&str] = &[
    "system",
    "summary",
    "mode",
    "permission-mode",
    "bridge-session",
    "ai-title",
    "pr-link",
    "last-prompt",
    "queue-operation",
    "started",
    "result",
];

/// Openers Claude Code writes into the `user` slot that the owner never typed.
///
/// `<task-notification>` earns its place from a real file: this project's own transcript holds 38
/// of them, none carrying `isMeta`, and one relays a subagent's report of a `memory_search`. That
/// is a memory tool's output reaching the owner's slot with no `tool_use` anywhere for E2 to join
/// against, so the prefix is the only handle on it.
const COMMAND_PREFIXES: &[&str] = &[
    "<command-name>",
    "<local-command-stdout>",
    "<command-message>",
    "<task-notification>",
    "<local-command-caveat>",
];

/// Stream `[start, ceiling)` and return the entries that survive.
///
/// The `tool_use_id` to name map is built as the walk goes, since a result always follows its use
/// in the same file. Failing to build that join is the specific bug that makes provenance-based
/// exclusion impossible: a `tool_result` carries no tool name of its own, so a result with no
/// matching `tool_use` in range is dropped and counted rather than kept with an unknown name.
pub fn parse_file(
    path: &Path,
    start: i64,
    ceiling: i64,
    limits: &Limits,
    counters: &mut PlanCounters,
) -> Result<FileParse> {
    let from_agent_file =
        path.file_name().and_then(|n| n.to_str()).map(|n| n.starts_with("agent-")).unwrap_or(false);

    let mut out =
        FileParse { is_sidechain: from_agent_file, consumed: start, ..FileParse::default() };
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut fence = FenceState::default();

    let stats = crate::ingest::reader::for_each_line(
        path,
        start.max(0) as u64,
        ceiling.max(0) as u64,
        limits.max_line_bytes,
        |line, byte_start, byte_end| {
            counters.entries_seen += 1;
            out.entries_seen += 1;

            // The fence runs on the raw line before anything reads its type. The marker the owner
            // needs to see lands inside a `tool_result`, so a scan sitting after E1 or E2 would
            // never meet it and the fix would do nothing. Bound to the run's own uuid rather than
            // a bare `contains`, so a line that merely quotes the marker (a grep hit, a fetched
            // page) falls through to ordinary parsing instead of swallowing the rest of the file.
            if fence.observe(line, byte_start, counters) {
                return Ok(());
            }

            let Ok(v) = serde_json::from_str::<Value>(line) else {
                counters.unknown("entry_type", "unparseable_line");
                return Ok(());
            };

            if out.session_id.is_none() {
                out.session_id = str_field(&v, "sessionId");
            }
            if out.cwd.is_none() {
                out.cwd = str_field(&v, "cwd");
            }
            if v.get("isSidechain").and_then(|x| x.as_bool()).unwrap_or(false) {
                out.is_sidechain = true;
            }

            classify_entry(
                &v,
                line_ctx(&v, byte_start, byte_end),
                from_agent_file,
                limits,
                counters,
                &mut tool_names,
                &mut out.entries,
            );
            Ok(())
        },
    )?;

    out.consumed = stats.consumed;
    out.fence_open = fence.is_open();
    out.fence_open_byte = fence.open_since();
    if out.fence_open {
        counters.fences_unclosed += 1;
    }
    Ok(out)
}

/// The per-entry facts a `ClassifiedEntry` needs whatever the classifier decides.
struct Ctx {
    uuid: Option<String>,
    timestamp: Option<chrono::DateTime<chrono::Utc>>,
    cwd: Option<String>,
    byte_start: i64,
    byte_end: i64,
}

fn line_ctx(v: &Value, byte_start: i64, byte_end: i64) -> Ctx {
    Ctx {
        uuid: str_field(v, "uuid"),
        timestamp: str_field(v, "timestamp")
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|t| t.with_timezone(&chrono::Utc)),
        cwd: str_field(v, "cwd"),
        byte_start,
        byte_end,
    }
}

#[allow(clippy::too_many_arguments)]
fn classify_entry(
    v: &Value,
    ctx: Ctx,
    from_agent_file: bool,
    limits: &Limits,
    counters: &mut PlanCounters,
    tool_names: &mut HashMap<String, String>,
    out: &mut Vec<ClassifiedEntry>,
) {
    let entry_type = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    let sidechain = v.get("isSidechain").and_then(|x| x.as_bool()).unwrap_or(false);

    // E1. Whole, whatever the subtype. `hook_success` and `hook_additional_context` carry the
    // injected text on different keys, so dropping the entry is the only version with no seam.
    if entry_type == "attachment" {
        counters.exclude("attachment");
        let subtype = v
            .get("attachment")
            .and_then(|a| a.get("type"))
            .or_else(|| v.get("subtype"))
            .and_then(|x| x.as_str())
            .unwrap_or("none");
        if !KNOWN_ATTACHMENT_SUBTYPES.contains(&subtype) {
            counters.unknown("attachment_subtype", subtype);
        }
        return;
    }

    let content = v.get("message").and_then(|m| m.get("content"));

    if entry_type == "assistant" {
        // E2, the join half that supplies the names. Memory tools go into the map too: without the
        // name their results cannot be recognised as memory results a few lines later.
        if let Some(items) = content.and_then(|c| c.as_array()) {
            for item in items {
                if item.get("type").and_then(|x| x.as_str()) != Some("tool_use") {
                    continue;
                }
                let (Some(id), Some(name)) = (str_field(item, "id"), str_field(item, "name"))
                else {
                    continue;
                };
                if is_memory_tool(&name) {
                    counters.exclude("memory_tool");
                }
                tool_names.insert(id, name);
            }
        }
        let speaker =
            if sidechain || from_agent_file { Speaker::Subagent } else { Speaker::MainModel };
        let text = text_items(content);
        push(out, counters, ctx, speaker, None, text);
        return;
    }

    if entry_type == "user" {
        let results = tool_results(content);
        if !results.is_empty() {
            // A `text` item sitting beside a `tool_result` in one entry is machine text in the
            // owner's slot, and it goes nowhere. Counted, because the safe direction is still a
            // direction and an uncounted drop is one nobody finds.
            let stray = content
                .and_then(|c| c.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter(|i| i.get("type").and_then(|x| x.as_str()) == Some("text"))
                        .count()
                })
                .unwrap_or(0);
            for _ in 0..stray {
                counters.exclude("text_beside_tool_result");
            }
            emit_tool_results(&results, ctx, limits, counters, tool_names, out);
            return;
        }
        classify_user(v, ctx, from_agent_file, sidechain, content, counters, out);
        return;
    }

    if SYSTEM_TYPES.contains(&entry_type) || entry_type.starts_with("file-history") {
        counters.exclude("system");
        return;
    }

    counters.exclude("unknown");
    counters.unknown("entry_type", entry_type);
}

/// A `user` entry with no `tool_result` in it. The only path to `owner_typed`, and the one
/// misclassification that costs the owner a fact he never said.
#[allow(clippy::too_many_arguments)]
fn classify_user(
    v: &Value,
    ctx: Ctx,
    from_agent_file: bool,
    sidechain: bool,
    content: Option<&Value>,
    counters: &mut PlanCounters,
    out: &mut Vec<ClassifiedEntry>,
) {
    let Some(content) = content else {
        counters.exclude("unknown");
        counters.unknown("entry_type", "user_no_content");
        return;
    };

    // A list holding anything other than `text` is machine content in the owner's slot. It never
    // reaches `owner_typed`, and dropping it beats guessing what it was.
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => {
            if !items.iter().all(|i| i.get("type").and_then(|x| x.as_str()) == Some("text")) {
                counters.exclude("mixed_content");
                return;
            }
            text_items(Some(content))
        }
        _ => {
            counters.exclude("unknown");
            counters.unknown("entry_type", "user_content_shape");
            return;
        }
    };

    // E4. The parent model wrote this, so it is never the owner's word and never auto-approvable.
    if from_agent_file {
        push(out, counters, ctx, Speaker::MainModel, None, text);
        return;
    }
    if sidechain {
        push(out, counters, ctx, Speaker::Subagent, None, text);
        return;
    }
    if v.get("isMeta").and_then(|x| x.as_bool()).unwrap_or(false) {
        counters.exclude("meta_user");
        return;
    }

    // A compaction summary is a plain-string `user` entry carrying `isCompactSummary`, and it
    // restates the whole session in the summariser's words with whatever was injected into it. It
    // reads as the owner typing and it is a paraphrase, which defeats every text-level check the
    // spec relies on: E3 only fires when a literal token survives the rewrite. The spec calls
    // compaction deferred because nobody had found one; entry 4338 of this project's own transcript
    // is one, and it embeds the digest.
    if v.get("isCompactSummary").and_then(|x| x.as_bool()).unwrap_or(false)
        || text.trim_start().starts_with("This session is being continued from a previous")
    {
        counters.exclude("compact_summary");
        return;
    }

    // Anything the harness put here rather than the owner. The field is absent on an entry the
    // owner typed, so an unknown value is treated as not the owner.
    match str_field(v, "promptSource") {
        Some(source) if source != "user" => {
            counters.exclude("prompt_source_not_user");
            return;
        }
        _ => {}
    }

    let head = text.trim_start();
    if COMMAND_PREFIXES.iter().any(|p| head.starts_with(p)) {
        counters.exclude("command_text");
        return;
    }
    if head.starts_with("<system-reminder>") {
        counters.exclude("system_reminder_in_user");
        return;
    }
    push(out, counters, ctx, Speaker::OwnerTyped, None, text);
}

/// E2, the result half. One entry per result: a `user` entry carries every parallel call's answer.
fn emit_tool_results(
    results: &[&Value],
    ctx: Ctx,
    limits: &Limits,
    counters: &mut PlanCounters,
    tool_names: &HashMap<String, String>,
    out: &mut Vec<ClassifiedEntry>,
) {
    for item in results {
        let name = str_field(item, "tool_use_id").and_then(|id| tool_names.get(&id).cloned());
        let Some(name) = name else {
            // Never kept with an unknown name. A result whose use sits below the watermark cannot
            // be shown to be from a non-memory tool, so it is not shown at all.
            counters.exclude("tool_result_unjoined");
            continue;
        };
        if is_memory_tool(&name) {
            counters.exclude("memory_tool");
            continue;
        }
        let text = truncate(&result_text(item.get("content")), limits.span_chars);
        let inner = Ctx {
            uuid: ctx.uuid.clone(),
            timestamp: ctx.timestamp,
            cwd: ctx.cwd.clone(),
            byte_start: ctx.byte_start,
            byte_end: ctx.byte_end,
        };
        push(out, counters, inner, Speaker::ToolReturned, Some(name), text);
    }
}

/// E3 runs here so no path reaches `out` without it. E1 should have caught every one of these; the
/// counter is what proves E1 still works after a Claude Code release.
fn push(
    out: &mut Vec<ClassifiedEntry>,
    counters: &mut PlanCounters,
    ctx: Ctx,
    speaker: Speaker,
    tool_name: Option<String>,
    text: String,
) {
    if let Some(token) = backstop_token(&text) {
        *counters.backstop.entry(token.to_string()).or_insert(0) += 1;
        counters.exclude("backstop");
        return;
    }
    if text.trim().is_empty() {
        // An assistant entry holding nothing but tool_use blocks lands here, and so does a user
        // entry whose text items are all blank. Counted rather than dropped: an exclusion with no
        // counter is an exclusion nobody finds, and this one is most of the gap between entries
        // seen and speakers counted.
        counters.exclude("empty_text");
        return;
    }
    counters.speaker(speaker);
    out.push(ClassifiedEntry {
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

fn tool_results(content: Option<&Value>) -> Vec<&Value> {
    content
        .and_then(|c| c.as_array())
        .map(|items| {
            items
                .iter()
                .filter(|i| i.get("type").and_then(|x| x.as_str()) == Some("tool_result"))
                .collect()
        })
        .unwrap_or_default()
}

/// The `text` fields of every `text` item, in order.
fn text_items(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter(|i| i.get("type").and_then(|x| x.as_str()) == Some("text"))
            .filter_map(|i| i.get("text").and_then(|x| x.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A `tool_result` carries a string on some calls and a content array on others.
fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(_)) => text_items(content),
        _ => String::new(),
    }
}

fn truncate(text: &str, cap: usize) -> String {
    if text.chars().count() <= cap {
        return text.to_string();
    }
    text.chars().take(cap).collect()
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn limits() -> Limits {
        Limits {
            span_chars: 6000,
            chunk_spans: 40,
            chunk_chars: 24_000,
            max_line_bytes: 1024 * 1024,
            max_files: 100,
            max_entries: 1000,
            retention_days: 7,
        }
    }

    /// Write the entries as one JSONL file and parse it, which is the only way to exercise the
    /// reader, the fence and the classifier on the path production takes.
    fn parse(name: &str, entries: &[Value]) -> (FileParse, PlanCounters) {
        parse_named(&format!("{name}.jsonl"), entries)
    }

    fn parse_named(file: &str, entries: &[Value]) -> (FileParse, PlanCounters) {
        let dir = std::env::temp_dir().join(format!(
            "lumberroom-claude-{}-{}",
            std::process::id(),
            file.replace('/', "-")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(file);
        let body: String =
            entries.iter().map(|e| format!("{}\n", serde_json::to_string(e).unwrap())).collect();
        std::fs::write(&path, body.as_bytes()).unwrap();
        let l = limits();
        let mut counters = PlanCounters::default();
        let parsed = parse_file(&path, 0, body.len() as i64, &l, &mut counters).unwrap();
        (parsed, counters)
    }

    fn user_text(text: &str) -> Value {
        json!({"type":"user","uuid":"u1","sessionId":"s","cwd":"/w","message":{"role":"user","content":text}})
    }

    fn excluded(c: &PlanCounters, rule: &str) -> i32 {
        c.entries_excluded.get(rule).copied().unwrap_or(0)
    }

    #[test]
    fn a_typed_string_is_the_owner() {
        let (p, c) = parse("owner", &[user_text("use pgvector, not qdrant")]);
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].speaker, Speaker::OwnerTyped);
        assert_eq!(p.entries[0].text, "use pgvector, not qdrant");
        assert_eq!(p.session_id.as_deref(), Some("s"));
        assert_eq!(c.speakers.get("owner_typed").copied(), Some(1));
        assert_eq!(c.entries_seen, 1);
    }

    #[test]
    fn a_text_only_list_is_the_owner_and_a_mixed_list_is_not() {
        let all_text = json!({"type":"user","uuid":"a","message":{"content":[
            {"type":"text","text":"first"},{"type":"text","text":"second"}]}});
        let mixed = json!({"type":"user","uuid":"b","message":{"content":[
            {"type":"text","text":"hi"},{"type":"image","source":{}}]}});
        let (p, c) = parse("lists", &[all_text, mixed]);
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].speaker, Speaker::OwnerTyped);
        assert_eq!(p.entries[0].text, "first\nsecond");
        assert_eq!(excluded(&c, "mixed_content"), 1);
    }

    #[test]
    fn machine_text_in_the_owner_slot_is_counted_by_its_own_rule() {
        let (p, c) = parse(
            "machine",
            &[
                user_text("<command-name>/graphify</command-name>"),
                user_text("<local-command-stdout>ok</local-command-stdout>"),
                user_text("<command-message>running</command-message>"),
                user_text("<system-reminder>be careful</system-reminder>"),
                json!({"type":"user","isMeta":true,"message":{"content":"caveat"}}),
            ],
        );
        assert!(p.entries.is_empty());
        assert_eq!(excluded(&c, "command_text"), 3);
        assert_eq!(excluded(&c, "system_reminder_in_user"), 1);
        assert_eq!(excluded(&c, "meta_user"), 1);
    }

    #[test]
    fn a_tool_result_joins_to_its_use_and_an_orphan_is_dropped() {
        let use_read = json!({"type":"assistant","uuid":"a1","message":{"content":[
            {"type":"text","text":"reading"},
            {"type":"tool_use","id":"t1","name":"Read","input":{}}]}});
        let result = json!({"type":"user","uuid":"a2","message":{"content":[
            {"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"file body"}]}]}});
        let orphan = json!({"type":"user","uuid":"a3","message":{"content":[
            {"type":"tool_result","tool_use_id":"nope","content":"stray"}]}});
        let (p, c) = parse("join", &[use_read, result, orphan]);

        assert_eq!(p.entries.len(), 2);
        assert_eq!(p.entries[0].speaker, Speaker::MainModel);
        assert_eq!(p.entries[0].text, "reading");
        assert_eq!(p.entries[1].speaker, Speaker::ToolReturned);
        assert_eq!(p.entries[1].tool_name.as_deref(), Some("Read"));
        assert_eq!(p.entries[1].text, "file body");
        assert_eq!(excluded(&c, "tool_result_unjoined"), 1);
    }

    #[test]
    fn a_memory_tool_loses_both_halves() {
        let call = json!({"type":"assistant","uuid":"a1","message":{"content":[
            {"type":"tool_use","id":"m1","name":"mcp__lumberroom__memory_write","input":{}}]}});
        let result = json!({"type":"user","uuid":"a2","message":{"content":[
            {"type":"tool_result","tool_use_id":"m1","content":"stored"}]}});
        let (p, c) = parse("memtool", &[call, result]);
        assert!(p.entries.is_empty());
        assert_eq!(excluded(&c, "memory_tool"), 2);
    }

    #[test]
    fn parallel_results_each_become_an_entry() {
        let uses = json!({"type":"assistant","uuid":"a1","message":{"content":[
            {"type":"tool_use","id":"t1","name":"Read","input":{}},
            {"type":"tool_use","id":"t2","name":"Grep","input":{}}]}});
        let results = json!({"type":"user","uuid":"a2","message":{"content":[
            {"type":"tool_result","tool_use_id":"t1","content":"one"},
            {"type":"tool_result","tool_use_id":"t2","content":"two"}]}});
        let (p, _) = parse("parallel", &[uses, results]);
        assert_eq!(p.entries.len(), 2);
        assert_eq!(p.entries[0].tool_name.as_deref(), Some("Read"));
        assert_eq!(p.entries[1].tool_name.as_deref(), Some("Grep"));
        assert_eq!(p.entries[0].byte_start, p.entries[1].byte_start);
    }

    #[test]
    fn every_attachment_goes_and_a_new_subtype_is_named() {
        let (p, c) = parse(
            "attach",
            &[
                json!({"type":"attachment","attachment":{"type":"hook_success","content":"x"}}),
                json!({"type":"attachment","attachment":{"type":"brand_new_thing"}}),
            ],
        );
        assert!(p.entries.is_empty());
        assert_eq!(excluded(&c, "attachment"), 2);
        assert_eq!(c.unknown_types.get("attachment_subtype:brand_new_thing").copied(), Some(1));
        assert_eq!(c.unknown_types.get("attachment_subtype:hook_success"), None);
    }

    #[test]
    fn the_fence_eats_everything_between_a_matching_pair_of_markers() {
        let id = "0199e5ef-9698-7cb3-8c23-3297e08d3c03";
        let (p, c) = parse(
            "fence",
            &[
                user_text("before the run"),
                json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":format!("lumberroom-ingest-begin:{id}")}]}}),
                user_text("inside, and never extracted"),
                json!({"type":"assistant","message":{"content":[{"type":"text","text":format!("done: lumberroom-ingest-end:{id}")}]}}),
                user_text("after the run"),
            ],
        );
        assert_eq!(p.entries.len(), 2);
        assert_eq!(p.entries[0].text, "before the run");
        assert_eq!(p.entries[1].text, "after the run");
        assert_eq!(excluded(&c, "ingest_fence"), 3);
        assert_eq!(c.fenced_entries, 3);
        assert!(!p.fence_open);
    }

    #[test]
    fn a_begin_marker_with_no_parseable_uuid_never_opens_a_fence() {
        // "4c1e" is not a uuid; a line that merely quotes the marker prefix must fall through to
        // ordinary parsing rather than swallowing the rest of the file.
        let (p, c) = parse(
            "fence-no-uuid",
            &[
                json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":"lumberroom-ingest-begin:4c1e"}]}}),
                user_text("not swallowed"),
            ],
        );
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].text, "not swallowed");
        assert!(!p.fence_open);
        assert_eq!(c.unknown_types.get("fence_marker:begin_no_uuid"), Some(&1));
    }

    #[test]
    fn a_fence_with_no_end_marker_is_reported_with_its_opening_offset() {
        let id = "0199e5ef-9698-7cb3-8c23-3297e08d3c03";
        let (p, c) = parse(
            "openfence",
            &[
                json!({"type":"assistant","message":{"content":[{"type":"text","text":format!("lumberroom-ingest-run:{id}")}]}}),
                user_text("swallowed"),
            ],
        );
        assert!(p.entries.is_empty());
        assert!(p.fence_open);
        assert_eq!(c.fences_unclosed, 1);
        assert_eq!(c.fenced_entries, 2);
        assert_eq!(p.fence_open_byte, Some(0), "the offset of the line that opened it");
    }

    #[test]
    fn a_close_with_a_different_uuid_leaves_a_real_fence_open() {
        let opened = "0199e5ef-9698-7cb3-8c23-3297e08d3c03";
        let other = "1199e5ef-9698-7cb3-8c23-3297e08d3c03";
        let (p, c) = parse(
            "fence-mismatch",
            &[
                json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":format!("lumberroom-ingest-begin:{opened}")}]}}),
                json!({"type":"assistant","message":{"content":[{"type":"text","text":format!("lumberroom-ingest-end:{other}")}]}}),
                user_text("still swallowed"),
            ],
        );
        assert!(p.entries.is_empty());
        assert!(p.fence_open);
        assert_eq!(c.unknown_types.get("fence_marker:close_mismatch"), Some(&1));
    }

    #[test]
    fn the_backstop_drops_a_digest_that_reached_the_classifier() {
        let (p, c) = parse(
            "backstop",
            &[
                user_text("<lumberroom-context>ns global: the owner prefers pgvector</lumberroom-context>"),
                user_text(
                    "Durable memory for this user, retrieved automatically at session start",
                ),
            ],
        );
        assert!(p.entries.is_empty());
        assert_eq!(excluded(&c, "backstop"), 2);
        assert_eq!(c.backstop.get("lumberroom_context").copied(), Some(1));
        assert_eq!(c.backstop.get("digest_preamble").copied(), Some(1));
    }

    #[test]
    fn a_sidechain_file_attributes_its_task_prompt_to_the_parent() {
        let (p, _) = parse_named(
            "agent-abc.jsonl",
            &[
                json!({"type":"user","uuid":"a1","sessionId":"s","message":{"content":"find every caller of nearest_ids"}}),
                json!({"type":"assistant","uuid":"a2","message":{"content":[{"type":"text","text":"found four"}]}}),
            ],
        );
        assert_eq!(p.entries.len(), 2);
        assert_eq!(p.entries[0].speaker, Speaker::MainModel);
        assert_eq!(p.entries[1].speaker, Speaker::Subagent);
        assert!(p.is_sidechain);
    }

    #[test]
    fn the_entry_flag_makes_a_subagent_provable_in_a_main_thread_file() {
        let (p, _) = parse(
            "inline",
            &[
                json!({"type":"assistant","uuid":"a1","isSidechain":true,"message":{"content":[{"type":"text","text":"sub says"}]}}),
            ],
        );
        assert_eq!(p.entries[0].speaker, Speaker::Subagent);
        assert!(p.is_sidechain);
    }

    #[test]
    fn harness_types_are_excluded_and_an_unknown_one_is_named() {
        let (p, c) = parse(
            "harness",
            &[
                json!({"type":"system","content":"x"}),
                json!({"type":"file-history-snapshot"}),
                json!({"type":"queue-operation"}),
                json!({"type":"teleport-beam"}),
            ],
        );
        assert!(p.entries.is_empty());
        assert_eq!(excluded(&c, "system"), 3);
        assert_eq!(excluded(&c, "unknown"), 1);
        assert_eq!(c.unknown_types.get("entry_type:teleport-beam").copied(), Some(1));
    }

    #[test]
    fn an_unparseable_line_is_counted_not_fatal() {
        let dir =
            std::env::temp_dir().join(format!("lumberroom-claude-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let body = "{not json\n{\"type\":\"user\",\"message\":{\"content\":\"real\"}}\n";
        std::fs::write(&path, body).unwrap();
        let mut c = PlanCounters::default();
        let p = parse_file(&path, 0, body.len() as i64, &limits(), &mut c).unwrap();
        assert_eq!(p.entries.len(), 1);
        assert_eq!(c.entries_seen, 2);
        assert_eq!(c.unknown_types.get("entry_type:unparseable_line").copied(), Some(1));
        assert_eq!(p.consumed, body.len() as i64);
    }
}

#[cfg(test)]
mod harness_text_in_the_owner_slot {
    use super::*;

    fn counted(entry: serde_json::Value) -> (Vec<ClassifiedEntry>, PlanCounters) {
        let mut counters = PlanCounters::default();
        let mut out = Vec::new();
        let mut names: HashMap<String, String> = HashMap::new();
        let limits = Limits::default();
        classify_entry(
            &entry,
            line_ctx(&entry, 0, 1),
            false,
            &limits,
            &mut counters,
            &mut names,
            &mut out,
        );
        (out, counters)
    }

    fn excluded(c: &PlanCounters, rule: &str) -> i32 {
        c.entries_excluded.get(rule).copied().unwrap_or(0)
    }

    #[test]
    fn a_compaction_summary_never_reaches_the_owner_slot() {
        let (out, c) = counted(serde_json::json!({
            "type": "user",
            "isCompactSummary": true,
            "message": { "content": "This session is being continued from a previous conversation. \
                                     The owner prefers pgvector over qdrant." }
        }));
        assert!(out.is_empty(), "a summary is a paraphrase and it is not the owner typing");
        assert_eq!(excluded(&c, "compact_summary"), 1);
    }

    #[test]
    fn the_head_catches_a_summary_that_carries_no_flag() {
        let (out, c) = counted(serde_json::json!({
            "type": "user",
            "message": { "content": "This session is being continued from a previous conversation \
                                     that ran out of context." }
        }));
        assert!(out.is_empty());
        assert_eq!(excluded(&c, "compact_summary"), 1);
    }

    #[test]
    fn a_task_notification_relaying_a_memory_search_never_reaches_the_owner_slot() {
        let (out, c) = counted(serde_json::json!({
            "type": "user",
            "message": { "content": "<task-notification>\n<task-id>x</task-id>\nrecalled from \
                                     memory: the owner keeps credentials in 1Password" }
        }));
        assert!(out.is_empty(), "a subagent's report is not the owner's word");
        assert_eq!(excluded(&c, "command_text"), 1);
    }

    #[test]
    fn a_user_entry_the_harness_sourced_is_not_the_owner() {
        let (out, c) = counted(serde_json::json!({
            "type": "user",
            "promptSource": "system",
            "message": { "content": "Continue where you left off." }
        }));
        assert!(out.is_empty());
        assert_eq!(excluded(&c, "prompt_source_not_user"), 1);
    }

    #[test]
    fn the_owner_typing_still_reaches_the_owner_slot() {
        let (out, _) = counted(serde_json::json!({
            "type": "user",
            "promptSource": "user",
            "message": { "content": "Use glm-5.3 for the ingest, not the flash models." }
        }));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].speaker, Speaker::OwnerTyped);
    }

    #[test]
    fn text_sitting_beside_a_tool_result_is_counted_rather_than_dropped() {
        let mut counters = PlanCounters::default();
        let mut out = Vec::new();
        let mut names: HashMap<String, String> = HashMap::new();
        names.insert("t1".into(), "Read".into());
        let entry = serde_json::json!({
            "type": "user",
            "message": { "content": [
                { "type": "text", "text": "stray machine text" },
                { "type": "tool_result", "tool_use_id": "t1", "content": [{"type":"text","text":"a file"}] }
            ]}
        });
        classify_entry(
            &entry,
            line_ctx(&entry, 0, 1),
            false,
            &Limits::default(),
            &mut counters,
            &mut names,
            &mut out,
        );
        assert_eq!(excluded(&counters, "text_beside_tool_result"), 1);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].speaker, Speaker::ToolReturned);
    }
}
