//! Transcript ingestion: the proposal queue, the watermarks, the run record and the emission check.
//!
//! Ingestion proposes and never writes. Every method here touches the ingest tables and the
//! emission table, and none of them reaches the memory store: a proposal becomes a memory only when
//! the owner approves it and the service calls `services::write::run`. The one exception the spec
//! allows is a `confirm` on an existing row, and that call belongs to the memory port rather than
//! to this one.
//!
//! Two invariants live in this contract rather than in a caller, because a caller that forgets them
//! loses data with no way back.
//!
//! **A watermark only ever moves forward.** `advance_watermark` is a `GREATEST`, so an older run
//! finishing last is a no-op rather than a rewind. A nightly Mode B run overlapping an interactive
//! one is an ordinary Tuesday, and a plain assignment would re-read and re-propose every byte
//! between the two ceilings.
//!
//! **A proposal's identity is its fingerprint, and speaker, quote and auto are frozen at first
//! insert.** Re-proposing adds a source row and changes nothing else. Upgrading `auto` on a later
//! arrival would let a row the owner is reading in the queue write itself while he reads it.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::domain::errors::Result;

/// Where a proposal sits. `written` carries a memory id, `rejected` blocks its fingerprint from
/// being proposed again, and `proposed` is everything the owner has yet to decide, including the
/// rows whose approval `write::run` refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalState {
    Proposed,
    Rejected,
    Written,
}

impl ProposalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Rejected => "rejected",
            Self::Written => "written",
        }
    }

    /// An unrecognised state is `None` rather than a default. Reading a row the schema forbids as
    /// `proposed` would put it back in front of the owner as an undecided fact.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "proposed" => Some(Self::Proposed),
            "rejected" => Some(Self::Rejected),
            "written" => Some(Self::Written),
            _ => None,
        }
    }
}

/// One transcript entry that stated the fact.
///
/// `source_key` is `file_path '#' entry_uuid` and it is what makes a re-post idempotent at the
/// source grain: the same entry read again by a later run inserts nothing.
#[derive(Debug, Clone, Serialize)]
pub struct ProposalSource {
    pub source_key: String,
    pub file_path: String,
    pub session_id: Option<String>,
    pub is_sidechain: bool,
    pub entry_uuid: Option<String>,
    /// The speaker of this source, which may be stronger than the frozen speaker on the parent row.
    /// `ingest show` reports the strongest across sources; nothing computes it onto the parent.
    pub speaker: String,
    pub observed_at: Option<DateTime<Utc>>,
    pub run_id: Uuid,
}

/// A fact an extractor proposed, on its way into the queue.
///
/// `auto` arrives already decided by the service, which checked the speaker and the substring claim
/// itself. The repository stores what it is given; the gate is one layer up and never on the wire.
#[derive(Debug, Clone)]
pub struct NewProposal {
    /// sha256 of the normalised content. The same function that produced every
    /// `recall_emission.content_sha256`, or the echo check can never match a proposal.
    pub fingerprint: String,
    pub content: String,
    pub namespace: String,
    pub tags: Vec<String>,
    pub supersedes: Option<Uuid>,
    pub speaker: String,
    pub quote: Option<String>,
    pub auto: bool,
    pub extractor: String,
    pub source: ProposalSource,
}

#[derive(Debug, Clone, Serialize)]
pub struct Proposal {
    pub id: Uuid,
    pub fingerprint: String,
    pub content: String,
    pub namespace: String,
    pub tags: Vec<String>,
    pub supersedes: Option<Uuid>,
    pub speaker: String,
    pub quote: Option<String>,
    pub auto: bool,
    pub extractor: String,
    pub state: String,
    pub memory_id: Option<Uuid>,
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// What an insert did. The distinction is the whole report: `Created` is a new question for the
/// owner, `Existing` is the 808th sighting of a fact already in the queue, already written or
/// already refused.
#[derive(Debug, Clone)]
pub enum ProposalUpsert {
    Created(Proposal),
    Existing(Proposal),
}

impl ProposalUpsert {
    pub fn proposal(&self) -> &Proposal {
        match self {
            Self::Created(p) | Self::Existing(p) => p,
        }
    }
}

/// Filters for the queue read. Every field is optional and absent means unfiltered, which is how
/// the adapter answers them all with one statement and no generated SQL.
#[derive(Debug, Clone, Default)]
pub struct ProposalFilter {
    pub state: Option<String>,
    pub run_id: Option<Uuid>,
    pub speaker: Option<String>,
    pub auto: Option<bool>,
    pub limit: i64,
}

/// How far one file has been consumed, and why it is not being consumed further.
#[derive(Debug, Clone, Serialize)]
pub struct Watermark {
    pub file_path: String,
    pub session_id: Option<String>,
    pub is_sidechain: bool,
    /// Always a line boundary, always the bytes already extracted from. This is the number that
    /// loses transcript content when it moves too far.
    pub byte_offset: i64,
    /// Hash of bytes `[0, byte_offset)`. Catches the case the offset cannot: a file rewritten or
    /// truncated in place, where a silently shifted offset produces garbage spans forever.
    pub prefix_sha256: String,
    pub entries_seen: i64,
    pub skip_reason: Option<String>,
    pub skip_run_id: Option<Uuid>,
    pub fence_from: Option<i64>,
    pub fence_until: Option<i64>,
    pub fence_run_id: Option<Uuid>,
    pub last_run_id: Option<Uuid>,
    pub updated_at: DateTime<Utc>,
}

/// One file's advance, as the service computed it.
///
/// `byte_offset` is a target, not a command. The repository takes the larger of it and what is
/// stored, and `prefix_sha256`, `last_run_id` and `updated_at` move only when the offset did: they
/// describe whichever offset won, and a losing older run must leave no trace on a row it did not
/// advance.
#[derive(Debug, Clone)]
pub struct WatermarkAdvance {
    pub file_path: String,
    pub session_id: Option<String>,
    pub is_sidechain: bool,
    pub byte_offset: i64,
    pub prefix_sha256: String,
    pub entries_seen: i64,
    pub run_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct NewRun {
    pub id: Uuid,
    /// Roots, project filter and date window, as the CLI resolved them. Free-form because the scope
    /// of a run is a client concern and a column per option would be a migration per flag.
    pub scope: serde_json::Value,
    pub extractor: String,
}

/// The counters a finished run stamps on itself.
///
/// `Default` and then set what the run knows, because a closing run reports what it counted and a
/// field it did not count is a zero rather than a guess. Two of these are the ones people read:
/// `traversal_capped`, because silent partial coverage reads exactly like complete coverage, and
/// `files_held_back`, because it is the only place the owner learns bytes are still pending.
#[derive(Debug, Clone, Default)]
pub struct RunTotals {
    pub files_seen: i32,
    pub files_skipped: serde_json::Value,
    pub entries_seen: i64,
    pub entries_excluded: serde_json::Value,
    pub unknown_types: serde_json::Value,
    pub spans_cut: i32,
    pub chunks: i32,
    pub chunks_missing: i32,
    pub chunks_failed: i32,
    pub files_held_back: serde_json::Value,
    pub fenced_entries: i32,
    pub fences_unclosed: i32,
    pub proposals_new: i32,
    pub proposals_reinforced: i32,
    pub confirmations: i32,
    pub traversal_capped: bool,
    pub artifact_sessions: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunRecord {
    pub id: Uuid,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub scope: serde_json::Value,
    pub extractor: String,
    pub files_seen: i32,
    pub files_skipped: serde_json::Value,
    pub entries_seen: i64,
    pub entries_excluded: serde_json::Value,
    pub unknown_types: serde_json::Value,
    pub spans_cut: i32,
    pub chunks: i32,
    pub chunks_missing: i32,
    pub chunks_failed: i32,
    pub files_held_back: serde_json::Value,
    pub fenced_entries: i32,
    pub fences_unclosed: i32,
    pub proposals_new: i32,
    pub proposals_reinforced: i32,
    pub confirmations: i32,
    pub traversal_capped: bool,
    pub artifact_sessions: serde_json::Value,
}

/// One candidate fact, asking whether the store handed this content out before the transcript
/// recorded it.
#[derive(Debug, Clone)]
pub struct EmissionProbe {
    pub content_sha256: String,
    /// The source span's timestamp. The direction is the entire test: an emission at or before this
    /// moment is an echo, and one after it is a coincidence.
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmissionHit {
    pub content_sha256: String,
    pub memory_id: Uuid,
    pub tool: String,
    pub first_emitted_at: DateTime<Utc>,
}

#[async_trait]
pub trait IngestRepository: Send + Sync {
    /// Open a run and return its id. Every proposal source and every watermark advance names one,
    /// so a report can answer "what did this run touch" without a scan.
    async fn open_run(&self, tenant: &str, run: NewRun) -> Result<Uuid>;

    /// Stamp the counters and set `finished_at`. Closing the run is also what lets a later plan
    /// bound this run's fence, so a run that dies without closing leaves an open fence rather than
    /// a silently reopened one.
    async fn close_run(&self, tenant: &str, id: Uuid, totals: RunTotals) -> Result<()>;

    async fn run(&self, tenant: &str, id: Uuid) -> Result<Option<RunRecord>>;

    /// Insert a proposal, or attach a source row to the one that already holds this fingerprint.
    ///
    /// Never updates `speaker`, `quote` or `auto` on an existing row. The caller reads the returned
    /// state to decide what happened: `proposed` is a reinforcement, `written` means the memory
    /// wants a `confirm`, and `rejected` means the owner already answered this question and the
    /// content stays blocked.
    async fn insert_proposal(&self, tenant: &str, proposal: NewProposal) -> Result<ProposalUpsert>;

    /// The queue read. Newest first, bounded by `limit`, filtered by whichever fields are set.
    async fn list_proposals(&self, tenant: &str, filter: ProposalFilter) -> Result<Vec<Proposal>>;

    async fn proposal(&self, tenant: &str, id: Uuid) -> Result<Option<Proposal>>;

    /// Every source that stated this fact, oldest first. This is the answer to "have I already
    /// counted this", and it is an exact one rather than a similarity guess.
    async fn proposal_sources(&self, tenant: &str, id: Uuid) -> Result<Vec<ProposalSource>>;

    /// Record the memory `write::run` returned. Only the approval path calls this, and it is the
    /// only place a proposal ever learns a memory id.
    async fn mark_written(&self, tenant: &str, id: Uuid, memory_id: Uuid) -> Result<()>;

    /// Record a refusal and leave the row at `proposed`.
    ///
    /// The message is a rule name and a reason, never the matched text: the queue is read in a
    /// terminal, copied into reports and pasted into transcripts, and a secret echoed here travels
    /// with it. A refused proposal stays visible in the queue precisely so the refusal is a thing
    /// the owner reads rather than a row that silently stopped moving.
    async fn mark_error(&self, tenant: &str, id: Uuid, message: &str) -> Result<()>;

    /// `false` when the row was not at `proposed`, so a double reject is not a second decision.
    async fn reject(&self, tenant: &str, id: Uuid) -> Result<bool>;

    /// Return a rejected row to `proposed`, sources intact.
    ///
    /// Permanent and irreversible are different claims. A rejection blocks its fingerprint forever,
    /// which makes `reject` against the wrong uuid a fact nobody can propose again, and a queue
    /// read at speed is exactly where that typo happens. `decided_at` stays on the row so the
    /// earlier rejection is still visible after the undo.
    async fn unreject(&self, tenant: &str, id: Uuid) -> Result<bool>;

    async fn watermark(&self, tenant: &str, file_path: &str) -> Result<Option<Watermark>>;

    /// Every watermark, or only the skipped ones. The skipped list is what the owner reads before
    /// wondering why a project produced nothing.
    async fn watermarks(&self, tenant: &str, skipped_only: bool) -> Result<Vec<Watermark>>;

    /// Move one file's mark forward and return the offset that survived.
    ///
    /// The return value is the stored offset after the write, which is the target when this run won
    /// and the older, larger value when it did not. A caller that assumed its own number was stored
    /// would report progress the store did not make.
    async fn advance_watermark(&self, tenant: &str, advance: WatermarkAdvance) -> Result<i64>;

    /// Stamp a file as skipped, once. A second stamp does not overwrite the first: the reason a
    /// file was first held is the one worth keeping, and the run that set it is the audit trail
    /// `unskip` is checked against.
    async fn set_skip(
        &self,
        tenant: &str,
        file_path: &str,
        reason: &str,
        run_id: Uuid,
    ) -> Result<()>;

    /// Clear one file's skip. By hand only, which is the whole design: a skip that expired on its
    /// own would let a run eat its own output the next night.
    async fn clear_skip(&self, tenant: &str, file_path: &str) -> Result<bool>;

    /// What the store handed out, for the tools that hand it out.
    ///
    /// Upserts on `(tenant, content_sha256, memory_id, tool)`, bumping `last_emitted_at` and
    /// `emit_count`, so a fact read a thousand times stays one row. `first_emitted_at` never moves,
    /// because the check compares against the first time the store could have caused the echo.
    async fn record_emission(
        &self,
        tenant: &str,
        content_sha256: &str,
        memory_id: Uuid,
        tool: &str,
        session_id: Option<&str>,
    ) -> Result<()>;

    /// The anti-loop check. Hits for the probes whose content the store emitted first.
    ///
    /// Tenant-wide on the hash, never on a session id. `Ctx.session_id` is set only by a header
    /// nothing sends, and the server's id space and a transcript's `sessionId` are minted by
    /// different processes, so a session-keyed check would fire never.
    ///
    /// The window belongs in the query rather than in a pass over the results: one probe against a
    /// popular fact can match many rows, and filtering after the fetch turns a bounded read into an
    /// unbounded one.
    async fn emissions_matching(
        &self,
        tenant: &str,
        probes: &[EmissionProbe],
        slack_secs: f64,
        window_secs: f64,
    ) -> Result<Vec<EmissionHit>>;
}
