//! Transcript ingestion, the client half.
//!
//! The server holds the proposal queue and the watermarks. Everything that touches a transcript
//! happens here, on the machine that owns the files, and reaches the server over the thirteen
//! routes under `/admin/ingest`. That split is the whole reason this is a binary: the server can
//! run anywhere and never sees a byte of a transcript it was not handed.
//!
//! Read `docs/specs/phase-6-ingestion.md` for the design. Ignore the spec on where the code lives:
//! sections 3 and 13 name `bin/lumberroom.mjs` and predate the workspace split.
//!
//! Three stages, and the split is copied from graphify. `plan` is deterministic and walks the
//! filesystem. Extraction is judgment and runs either as dispatched subagents (Mode A) or as calls
//! to a provider (Mode B). `submit` is deterministic again on the way back.

pub mod api;
pub mod batch;
pub mod claude;
pub mod codex;
pub mod extract;
pub mod plan;
pub mod prompt;
pub mod provider;
pub mod queue;
pub mod reader;
pub mod run;
pub mod runlock;
pub mod spans;
pub mod submit;
pub mod walk;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::client::{err, Result};

/// Which transcript format a file is in. The two parsers share span cutting and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Claude,
    Codex,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// Six values, and every span carries exactly one (spec §5).
///
/// Auto-approval rests on `OwnerTyped` alone, and the substring check in §2.4 runs on top of it,
/// so a misclassification here is the one that costs the owner a fact he never said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speaker {
    OwnerTyped,
    MainModel,
    Subagent,
    ToolReturned,
    HookInjected,
    System,
}

impl Speaker {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OwnerTyped => "owner_typed",
            Self::MainModel => "main_model",
            Self::Subagent => "subagent",
            Self::ToolReturned => "tool_returned",
            Self::HookInjected => "hook_injected",
            Self::System => "system",
        }
    }

    /// Whether a span with this speaker is offered to an extractor at all. `ToolReturned` is the
    /// bulk of the corpus by bytes and it is where the credentials are, so it is off unless the
    /// owner asks for it.
    pub fn reaches_extraction(self, include_tool_output: bool) -> bool {
        match self {
            Self::OwnerTyped | Self::MainModel | Self::Subagent => true,
            Self::ToolReturned => include_tool_output,
            Self::HookInjected | Self::System => false,
        }
    }
}

/// One contiguous run of entries sharing a speaker.
///
/// `byte_start` is load-bearing beyond bookkeeping: the hold-back rule in §8.3 advances a file to
/// the first byte of its earliest span that never came back extracted, so losing this number loses
/// transcript bytes with no recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    /// Stable within a run and referenced by the extractor's output. Shape: `s<index>`.
    pub id: String,
    pub file_path: String,
    pub session_id: Option<String>,
    pub is_sidechain: bool,
    pub source: Source,
    pub entry_uuids: Vec<String>,
    pub byte_start: i64,
    pub byte_end: i64,
    pub speaker: Speaker,
    pub tool_name: Option<String>,
    pub timestamp: Option<DateTime<Utc>>,
    pub cwd: Option<String>,
    pub text: String,
}

/// The extractor's view of a span. Byte offsets and paths stay out of the prompt: they are not
/// evidence about the owner and they cost tokens on every chunk.
#[derive(Debug, Clone, Serialize)]
pub struct ChunkSpan {
    pub id: String,
    pub speaker: &'static str,
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    pub text: String,
}

impl From<&Span> for ChunkSpan {
    fn from(s: &Span) -> Self {
        Self {
            id: s.id.clone(),
            speaker: s.speaker.as_str(),
            session_id: s.session_id.clone(),
            timestamp: s.timestamp,
            cwd: s.cwd.clone(),
            tool_name: s.tool_name.clone(),
            text: s.text.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRef {
    pub index: usize,
    pub span_ids: Vec<String>,
}

/// A file this run read, and how far it read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedFile {
    pub file_path: String,
    pub session_id: Option<String>,
    pub is_sidechain: bool,
    pub source: Source,
    /// Where the watermark stood when the walk started.
    pub byte_start: i64,
    /// Frozen at plan start. A live transcript grows all day and nothing beyond this was read.
    pub plan_ceiling: i64,
    pub entries_seen: i64,
    /// The stored prefix hash disagreed, so the file was rewritten in place and re-read from zero.
    pub prefix_mismatch: bool,
}

/// Counted by the rule that made each exclusion. An exclusion with no counter is an exclusion
/// nobody finds, so every map here is printed by `plan` and stamped onto the run record.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanCounters {
    pub files_seen: i32,
    /// reason to count: `sensitive_path`, `symlink`, `unparseable`, `skipped`, `capped`.
    pub files_skipped: std::collections::BTreeMap<String, i32>,
    pub entries_seen: i64,
    /// rule to count: `attachment`, `tool_result`, `memory_tool`, `system`, `sensitive`,
    /// `ingest_fence`, `developer`, `environment_context`, `duplicate_agent_message`.
    pub entries_excluded: std::collections::BTreeMap<String, i32>,
    /// `entry_type:<name>` and `attachment_subtype:<name>`, so a Claude Code release that adds one
    /// gets noticed rather than silently dropped.
    pub unknown_types: std::collections::BTreeMap<String, i32>,
    /// One per speaker value, before the extraction filter.
    pub speakers: std::collections::BTreeMap<String, i32>,
    /// E3, split by which token fired. Only the preamble one can fire against the historical corpus.
    pub backstop: std::collections::BTreeMap<String, i32>,
    pub spans_cut: i32,
    pub chunks: i32,
    pub fenced_entries: i32,
    pub fences_unclosed: i32,
    /// `INGEST_MAX_FILES` or `INGEST_MAX_ENTRIES` fired. Silent partial coverage of a corpus reads
    /// exactly like complete coverage.
    pub traversal_capped: bool,
}

impl PlanCounters {
    pub fn skip(&mut self, reason: &str) {
        *self.files_skipped.entry(reason.to_string()).or_insert(0) += 1;
    }
    pub fn exclude(&mut self, rule: &str) {
        *self.entries_excluded.entry(rule.to_string()).or_insert(0) += 1;
    }
    pub fn unknown(&mut self, kind: &str, name: &str) {
        *self.unknown_types.entry(format!("{kind}:{name}")).or_insert(0) += 1;
    }
    pub fn speaker(&mut self, s: Speaker) {
        *self.speakers.entry(s.as_str().to_string()).or_insert(0) += 1;
    }
}

/// What `plan` writes and `submit` reads back. The single artifact tying a run together.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worklist {
    pub run_id: uuid::Uuid,
    pub created_at: DateTime<Utc>,
    /// Roots, project filter and date window as the client resolved them. Free-form on the wire.
    pub scope: serde_json::Value,
    pub include_tool_output: bool,
    pub files: Vec<PlannedFile>,
    pub spans: Vec<Span>,
    pub chunks: Vec<ChunkRef>,
    pub counters: PlanCounters,
}

/// One fact an extractor produced. The shape the §8.2 prompt asks for, parsed leniently: a model
/// that omits `tags` or `confidence` produced a usable fact and should not fail the chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedFact {
    pub content: String,
    pub namespace: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub source_span_id: String,
    #[serde(default)]
    pub speaker: Option<String>,
    #[serde(default)]
    pub quote: Option<String>,
    #[serde(default)]
    pub confidence: Option<String>,
}

/// `out/chunk-NN.json`. An empty `facts` with a `<no-facts/>` refusal is a correct answer and the
/// common one: most chunks are ordinary work with nothing durable in them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChunkOutput {
    #[serde(default)]
    pub facts: Vec<ExtractedFact>,
    #[serde(default)]
    pub refusal: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedChunk {
    pub index: usize,
    /// `http_429`, `timeout`, `unparseable`, `refused`. Never the provider's key or the response
    /// body beyond its first 200 characters.
    pub reason: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub requests: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
}

/// `state.json`. Written after every chunk so a killed run resumes where it stopped rather than
/// paying for the whole corpus twice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunState {
    pub run_id: uuid::Uuid,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub done: Vec<usize>,
    pub failed: Vec<FailedChunk>,
    pub usage: Usage,
    /// The batch this run was submitted as, when it went out through Mode C. It lives here because
    /// the synchronous path rewrites this whole file after every chunk: without the field, one
    /// `extract --retry-failed` erases the batch id while the results sit on a provider for another
    /// thirty days with nothing local knowing what to ask for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<serde_json::Value>,
    pub updated_at: DateTime<Utc>,
}

impl RunState {
    pub fn new(run_id: uuid::Uuid) -> Self {
        Self {
            run_id,
            provider: None,
            model: None,
            done: vec![],
            failed: vec![],
            usage: Usage::default(),
            batch: None,
            updated_at: Utc::now(),
        }
    }
}

/// Where a run's files live. One directory per run, swept after `INGEST_RUN_RETENTION_DAYS`.
///
/// Under the state directory rather than the config directory: these are working files, they can be
/// large, and deleting the lot must never cost the owner a credential.
#[derive(Debug, Clone)]
pub struct RunPaths {
    pub root: PathBuf,
    pub run_id: uuid::Uuid,
}

impl RunPaths {
    pub fn new(run_id: uuid::Uuid) -> Result<Self> {
        Ok(Self { root: runs_dir()?.join(run_id.to_string()), run_id })
    }

    pub fn worklist(&self) -> PathBuf {
        self.root.join("worklist.json")
    }
    pub fn state(&self) -> PathBuf {
        self.root.join("state.json")
    }
    pub fn spans_dir(&self) -> PathBuf {
        self.root.join("spans")
    }
    pub fn out_dir(&self) -> PathBuf {
        self.root.join("out")
    }
    pub fn chunk_in(&self, index: usize) -> PathBuf {
        self.spans_dir().join(format!("chunk-{index:02}.json"))
    }
    pub fn chunk_out(&self, index: usize) -> PathBuf {
        self.out_dir().join(format!("chunk-{index:02}.json"))
    }

    pub fn create(&self) -> Result<()> {
        std::fs::create_dir_all(self.spans_dir())
            .and_then(|_| std::fs::create_dir_all(self.out_dir()))
            .map_err(|e| err(format!("could not create the run directory: {e}")))
    }

    pub fn read_worklist(&self) -> Result<Worklist> {
        let raw = std::fs::read_to_string(self.worklist()).map_err(|e| {
            err(format!("no worklist for run {}: {e}. Run `lumberroom ingest plan` first", self.run_id))
        })?;
        serde_json::from_str(&raw).map_err(|e| err(format!("worklist.json is unreadable: {e}")))
    }

    pub fn read_state(&self) -> RunState {
        std::fs::read_to_string(self.state())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_else(|| RunState::new(self.run_id))
    }

    pub fn write_state(&self, state: &RunState) -> Result<()> {
        write_json(&self.state(), state)
    }
}

/// The state directory. `LUMBERROOM_STATE_DIR` wins, then `~/.local/state/lumberroom`.
pub fn state_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("LUMBERROOM_STATE_DIR") {
        if !dir.trim().is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    let home = std::env::var("HOME").map_err(|_| err("HOME is not set, so there is no state directory"))?;
    Ok(PathBuf::from(home).join(".local/state/lumberroom"))
}

pub fn ingest_dir() -> Result<PathBuf> {
    Ok(state_dir()?.join("ingest"))
}

pub fn runs_dir() -> Result<PathBuf> {
    Ok(ingest_dir()?.join("runs"))
}

/// Serialise pretty and write. Every file this module writes is one the owner may open.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| err(format!("could not create {}: {e}", parent.display()))) ?;
    }
    let body = serde_json::to_vec_pretty(value)
        .map_err(|e| err(format!("could not serialise {}: {e}", path.display())))?;
    std::fs::write(path, body).map_err(|e| err(format!("could not write {}: {e}", path.display())))
}

/// Hex sha256 of a byte range starting at zero. The watermark's `prefix_sha256` and nothing else.
pub fn prefix_sha256(path: &Path, upto: u64) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .map_err(|e| err(format!("could not open {}: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut left = upto;
    let mut buf = vec![0u8; 64 * 1024];
    while left > 0 {
        let want = buf.len().min(left as usize);
        let read = file
            .read(&mut buf[..want])
            .map_err(|e| err(format!("could not read {}: {e}", path.display())))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
        left -= read as u64;
    }
    Ok(hex_lower(&hasher.finalize()))
}

pub fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Tunables. Every one of these is read here and nowhere else, the same rule `src/config.rs`
/// carries on the server side.
pub struct Limits {
    pub span_chars: usize,
    pub chunk_spans: usize,
    pub chunk_chars: usize,
    pub max_line_bytes: usize,
    pub max_files: usize,
    pub max_entries: u64,
    pub retention_days: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            span_chars: env_usize("INGEST_SPAN_CHARS", 6000),
            chunk_spans: env_usize("INGEST_CHUNK_SPANS", 40),
            chunk_chars: env_usize("INGEST_CHUNK_CHARS", 24_000),
            // A 96MB transcript exists in this corpus. A line longer than this is skipped and
            // counted rather than buffered.
            max_line_bytes: env_usize("INGEST_MAX_LINE_BYTES", 8 * 1024 * 1024),
            max_files: env_usize("INGEST_MAX_FILES", 5000),
            max_entries: env_usize("INGEST_MAX_ENTRIES", 2_000_000) as u64,
            retention_days: env_usize("INGEST_RUN_RETENTION_DAYS", 7) as u64,
        }
    }
}

pub fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

/// Tool-name prefixes whose calls and results never reach an extractor. A `memory_write` argument
/// is content already in the store, so it can only produce a duplicate of a row that exists.
pub fn memory_tool_prefixes() -> Vec<String> {
    let mut out = vec!["mcp__lumberroom__".to_string(), "mcp__agentmemory__".to_string()];
    if let Ok(extra) = std::env::var("INGEST_MEMORY_TOOL_PREFIXES") {
        out.extend(extra.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
    }
    out
}

pub fn is_memory_tool(name: &str) -> bool {
    memory_tool_prefixes().iter().any(|p| name.starts_with(p.as_str()))
}

/// E3, the text-level backstop. Returns the token that fired.
///
/// E1 should have caught every one of these. A backstop that never fires is the evidence that E1
/// works, which is why each token is counted separately rather than as one total.
pub fn backstop_token(text: &str) -> Option<&'static str> {
    const PREAMBLE: &str = "Durable memory for this user, retrieved automatically at session start";
    if text.contains("<lumberroom-context>") {
        return Some("lumberroom_context");
    }
    if text.contains("<agentmemory-context") {
        return Some("agentmemory_context");
    }
    if text.contains(PREAMBLE) {
        return Some("digest_preamble");
    }
    None
}

/// The fence markers §7.3 writes around an ingest conversation. Scanned before every exclusion,
/// whatever the entry's type, because an ingest run that eats its own conversation grows a store
/// out of its own output.
pub const FENCE_BEGIN: &str = "lumberroom-ingest-begin:";
pub const FENCE_END: &str = "lumberroom-ingest-end:";
pub const FENCE_RUN: &str = "lumberroom-ingest-run:";

/// `lumberroom ingest <sub>`. One subcommand table, so a flag renamed here is renamed once.
pub async fn dispatch(
    c: &crate::client::Client,
    args: &crate::args::Args,
    sub: &str,
) -> Result<()> {
    use crate::client::err as e;

    let json = args.present("json");
    let yes = args.present("yes");

    // A run id is either --run or the first positional after the subcommand. Both, because the
    // owner copies the id out of the plan output and appends it without thinking about which.
    let run_id = |pos: usize| -> Result<uuid::Uuid> {
        let raw = args
            .value_any(&["run", "run-id"])
            .or_else(|| args.positional_at(pos))
            .ok_or_else(|| e("this needs a run id: pass --run <id>"))?;
        uuid::Uuid::parse_str(raw.trim()).map_err(|_| e(format!("{raw:?} is not a run id")))
    };

    match sub {
        "plan" => {
            let a = plan::PlanArgs {
                source: args.value("source").unwrap_or("claude").to_string(),
                project: args.value("project").map(str::to_string),
                since: args.value("since").map(str::to_string),
                max_files: args.value("max-files").and_then(|v| v.parse().ok()),
                include_tool_output: args.present("include-tool-output"),
                json,
            };
            plan::run(c, &a).await.map(|_| ())
        }
        "extract" => {
            let a = extract::ExtractArgs {
                run_id: run_id(2)?,
                provider: args.value("provider").unwrap_or("zai").to_string(),
                model: args.value("model").map(str::to_string),
                base_url: args.value("base-url").map(str::to_string),
                concurrency: args.value("concurrency").and_then(|v| v.parse().ok()).unwrap_or(4),
                rpm: args.value("rpm").and_then(|v| v.parse().ok()),
                timeout_secs: args.value("timeout").and_then(|v| v.parse().ok()).unwrap_or(120),
                dry_run: args.present("dry-run"),
                retry_failed: args.present("retry-failed"),
                yes,
            };
            // A batch is one request that a provider answers hours later, so it takes the same
            // spans and the same prompt down a different path. The flags pick the stage rather
            // than a mode: submit, ask, collect.
            if args.present("batch") || args.present("batch-status") || args.present("batch-fetch") {
                let b = batch::BatchArgs {
                    run_id: a.run_id,
                    provider: a.provider.clone(),
                    model: a.model.clone(),
                    base_url: a.base_url.clone(),
                    timeout_secs: a.timeout_secs,
                    dry_run: a.dry_run,
                    retry_failed: a.retry_failed,
                    yes: a.yes,
                    action: if args.present("batch-status") {
                        batch::BatchAction::Status
                    } else if args.present("batch-fetch") {
                        batch::BatchAction::Fetch
                    } else {
                        batch::BatchAction::Advance
                    },
                };
                return batch::run(&b).await.map(|_| ());
            }
            extract::run(&a).await.map(|_| ())
        }
        "submit" => {
            let a = submit::SubmitArgs {
                run_id: run_id(2)?,
                dry_run: args.present("dry-run"),
                no_auto: args.present("no-auto"),
                json,
            };
            submit::run(c, &a).await.map(|_| ())
        }
        "run" => {
            let a = run::RunArgs {
                source: args.value("source").unwrap_or("claude").to_string(),
                project: args.value("project").map(str::to_string),
                since: args.value("since").map(str::to_string),
                max_files: args.value("max-files").and_then(|v| v.parse().ok()),
                include_tool_output: args.present("include-tool-output"),
                provider: args.value("provider").unwrap_or("zai").to_string(),
                model: args.value("model").map(str::to_string),
                base_url: args.value("base-url").map(str::to_string),
                concurrency: args.value("concurrency").and_then(|v| v.parse().ok()).unwrap_or(4),
                timeout_secs: args.value("timeout").and_then(|v| v.parse().ok()).unwrap_or(120),
                no_auto: args.present("no-auto"),
                dry_run: args.present("dry-run"),
                yes: args.present("yes"),
                rpm: args.value("rpm").and_then(|v| v.parse().ok()),
                json,
                help: args.present("help"),
            };
            run::run(c, &a).await.map(|_| ())
        }
        "keys" => {
            let action = args.positional_at(2).unwrap_or("");
            if action != "set" {
                return Err(e("keys takes one action: lumberroom ingest keys set <provider>"));
            }
            let name = args.positional_at(3).ok_or_else(|| {
                e("keys set needs a provider: openai, anthropic, openrouter, zai or custom")
            })?;
            let path = crate::config::config_path(&crate::config::ProcessEnv);
            provider::keys_set(name, path, || {
                // The key arrives on stdin and never as an argument: `ps` shows every argument of a
                // running process, and an interactive shell writes the command into its history.
                if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                    crate::prompt("paste the key, then press enter: ");
                }
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                Ok(line)
            })
        }
        "list" => {
            let filter = api::ProposalFilter {
                state: args.value("state").map(str::to_string),
                run_id: args.value_any(&["run", "run-id"]).and_then(|v| uuid::Uuid::parse_str(v).ok()),
                speaker: args.value("speaker").map(str::to_string),
                auto: if args.present("auto") { Some(true) } else { None },
                limit: args.value("limit").and_then(|v| v.parse().ok()),
            };
            queue::list(c, &filter, json).await
        }
        "show" => {
            let raw = args
                .positional_at(2)
                .ok_or_else(|| e("show needs a proposal id"))?;
            let id = uuid::Uuid::parse_str(raw.trim())
                .map_err(|_| e(format!("{raw:?} is not a proposal id")))?;
            queue::show(c, id, json).await
        }
        "approve" => {
            let run = args.value_any(&["run", "run-id"]).map(|v| {
                uuid::Uuid::parse_str(v).map_err(|_| e(format!("{v:?} is not a run id")))
            });
            let run = match run {
                Some(r) => Some(r?),
                None => None,
            };
            let ids: Vec<uuid::Uuid> = args.positional.iter().skip(2)
                .filter_map(|s| uuid::Uuid::parse_str(s.trim()).ok())
                .collect();
            if ids.is_empty() && run.is_none() {
                return Err(e("approve needs one or more proposal ids, or --run <id>"));
            }
            queue::approve(c, &ids, run, args.value("speaker"), yes, json).await
        }
        "reject" => {
            let raw = args.positional_at(2).ok_or_else(|| e("reject needs a proposal id"))?;
            let id = uuid::Uuid::parse_str(raw.trim())
                .map_err(|_| e(format!("{raw:?} is not a proposal id")))?;
            queue::reject(c, id, args.value("reason"), yes).await
        }
        "unreject" => {
            let raw = args.positional_at(2).ok_or_else(|| e("unreject needs a proposal id"))?;
            let id = uuid::Uuid::parse_str(raw.trim())
                .map_err(|_| e(format!("{raw:?} is not a proposal id")))?;
            queue::unreject(c, id).await
        }
        "watermarks" => queue::watermarks(c, args.present("skipped"), json).await,
        "unskip" => {
            let path = args.positional_at(2).ok_or_else(|| e("unskip needs a file path"))?;
            queue::unskip(c, path).await
        }
        "clean" => {
            let id = args
                .value_any(&["run", "run-id"])
                .or_else(|| args.positional_at(2))
                .and_then(|v| uuid::Uuid::parse_str(v.trim()).ok());
            let all = args.present("all");
            if id.is_none() && !all {
                return Err(e("clean needs --run <id> or --all"));
            }
            let n = queue::clean(id, all)?;
            crate::out(&format!("removed {n} run director{}", if n == 1 { "y" } else { "ies" }));
            Ok(())
        }
        other => Err(e(format!(
            "unknown ingest subcommand {other}. Try: plan, extract, submit, run, keys, list, show, approve, \
             reject, unreject, watermarks, unskip, clean"
        ))),
    }
}
