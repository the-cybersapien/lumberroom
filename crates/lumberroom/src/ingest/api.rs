//! The thirteen `/admin/ingest` routes, typed.
//!
//! Every field name here matches the server's, and the wire contract is snake_case throughout: a
//! rename on the domain side once turned every latency into `-ms` with nothing failing. Nothing in
//! this file hashes anything. The server hashes `content` with the function that produces a
//! proposal's fingerprint, and a client computing its own would be the second normaliser this
//! layer was already built wrong by once.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::client::{err, err_code, Client, Result};

/// A route answered with something other than 200. 403 keeps its own exit code because a client
/// without `may_ingest` is the common first failure and it is not a broken server.
fn ok(status: u16, body: Value, what: &str) -> Result<Value> {
    if status == 200 {
        return Ok(body);
    }
    let detail = body
        .get("detail")
        .or_else(|| body.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("no detail")
        .to_string();
    let code = if status == 401 || status == 403 { 2 } else { 1 };
    Err(err_code(format!("{what} failed: HTTP {status}, {detail}"), code))
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceReq {
    pub file_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub is_sidechain: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub run_id: Uuid,
}

/// One fact on its way into the queue.
///
/// `auto` is absent and always will be: the server checks the substring claim against `span_text`
/// rather than trusting the extractor, and a client that could set it would be approving its own
/// writes.
#[derive(Debug, Clone, Serialize)]
pub struct FactReq {
    pub content: String,
    pub namespace: String,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<Uuid>,
    pub speaker: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_text: Option<String>,
    pub source: SourceReq,
}

/// Internally tagged on `outcome`, so a report reads `{"outcome":"proposed","id":...,"auto":false}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum FactOutcome {
    Proposed { id: Uuid, auto: bool },
    Reinforced { id: Uuid },
    Blocked { id: Uuid },
    Confirmed { memory_id: Uuid },
    Refused { rule: String },
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PostReport {
    #[serde(default)]
    pub outcomes: Vec<FactOutcome>,
    pub proposals_new: i32,
    pub proposals_reinforced: i32,
    pub confirmations: i32,
    pub refused: i32,
    pub blocked: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmissionProbeReq {
    /// Send this. The server hashes it.
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmissionHit {
    pub content_sha256: String,
    pub memory_id: Uuid,
    pub tool: String,
    pub first_emitted_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Proposal {
    pub id: Uuid,
    pub fingerprint: String,
    pub content: String,
    pub namespace: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub supersedes: Option<Uuid>,
    pub speaker: String,
    pub quote: Option<String>,
    pub auto: bool,
    pub extractor: String,
    pub state: String,
    pub memory_id: Option<Uuid>,
    pub last_error: Option<String>,
    pub last_error_at: Option<chrono::DateTime<chrono::Utc>>,
    pub decided_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProposalSource {
    pub source_key: String,
    pub file_path: String,
    pub session_id: Option<String>,
    pub is_sidechain: bool,
    pub entry_uuid: Option<String>,
    pub speaker: String,
    pub observed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub run_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProposalDetail {
    pub proposal: Proposal,
    #[serde(default)]
    pub sources: Vec<ProposalSource>,
    pub strongest_speaker: Option<String>,
}

/// A refusal is a 200 carrying `refused`, never an error. The row stays at `proposed` with the
/// reason on it, and the owner reads the refusal in the queue rather than finding a row that
/// stopped moving.
#[derive(Debug, Clone, Deserialize)]
pub struct ApproveOutcome {
    pub id: Uuid,
    pub memory_id: Option<Uuid>,
    pub deduplicated: bool,
    pub refused: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ProposalFilter {
    pub state: Option<String>,
    pub run_id: Option<Uuid>,
    pub speaker: Option<String>,
    pub auto: Option<bool>,
    pub limit: Option<i64>,
}

impl ProposalFilter {
    fn query(&self) -> String {
        let mut parts: Vec<String> = vec![];
        if let Some(s) = &self.state {
            parts.push(format!("state={s}"));
        }
        if let Some(r) = &self.run_id {
            parts.push(format!("run_id={r}"));
        }
        if let Some(s) = &self.speaker {
            parts.push(format!("speaker={s}"));
        }
        if let Some(a) = self.auto {
            parts.push(format!("auto={a}"));
        }
        if let Some(l) = self.limit {
            parts.push(format!("limit={l}"));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("?{}", parts.join("&"))
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Watermark {
    pub file_path: String,
    pub session_id: Option<String>,
    pub is_sidechain: bool,
    pub byte_offset: i64,
    pub prefix_sha256: String,
    pub entries_seen: i64,
    pub skip_reason: Option<String>,
    pub skip_run_id: Option<Uuid>,
    pub fence_from: Option<i64>,
    pub fence_until: Option<i64>,
    pub fence_run_id: Option<Uuid>,
    pub last_run_id: Option<Uuid>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileAdvanceReq {
    pub file_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub is_sidechain: bool,
    pub plan_ceiling: i64,
    /// Hash of `[0, effective_offset)`, where the effective offset is the one the server will
    /// store: `min(unextracted_from)` capped at the ceiling, or the ceiling when nothing was held
    /// back. Hashing the ceiling on a held-back file makes the next run's prefix check fail.
    pub prefix_sha256: String,
    pub entries_seen: i64,
    pub unextracted_from: Vec<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HeldBack {
    pub file: String,
    pub held_at: i64,
    pub ceiling: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WatermarkReport {
    /// File and the offset stored now, which is not always the offset this run asked for.
    #[serde(default)]
    pub advanced: Vec<(String, i64)>,
    #[serde(default)]
    pub held_back: Vec<HeldBack>,
}

/// Every counter the run record carries. A field nobody counted is a zero rather than a guess.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RunTotals {
    pub files_seen: i32,
    pub files_skipped: Value,
    pub entries_seen: i64,
    pub entries_excluded: Value,
    pub unknown_types: Value,
    pub spans_cut: i32,
    pub chunks: i32,
    pub chunks_missing: i32,
    pub chunks_failed: i32,
    pub files_held_back: Value,
    pub fenced_entries: i32,
    pub fences_unclosed: i32,
    pub proposals_new: i32,
    pub proposals_reinforced: i32,
    pub confirmations: i32,
    pub traversal_capped: bool,
    pub artifact_sessions: Value,
}

pub async fn open_run(c: &Client, extractor: &str, scope: Value) -> Result<Uuid> {
    let (status, body) = c
        .http_request(
            reqwest::Method::POST,
            "/admin/ingest/runs",
            Some(json!({ "extractor": extractor, "scope": scope })),
        )
        .await?;
    let body = ok(status, body, "opening the run")?;
    body.get("run_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| err("the server opened a run and did not name it"))
}

/// Close a run and stamp its counters. The route that lets a later `plan` bound this run's fence.
pub async fn close_run(c: &Client, id: Uuid, totals: &RunTotals) -> Result<()> {
    let (status, body) = c
        .http_request(
            reqwest::Method::POST,
            &format!("/admin/ingest/runs/{id}/close"),
            Some(serde_json::to_value(totals).map_err(|e| err(e.to_string()))?),
        )
        .await?;
    ok(status, body, "closing the run").map(|_| ())
}

pub async fn run_report(c: &Client, id: Uuid) -> Result<Value> {
    let (status, body) = c.http_get(&format!("/admin/ingest/runs/{id}")).await?;
    ok(status, body, "reading the run report")
}

/// The credential tripwire, for a client that cannot run it in process. Rule names only, in the
/// order the texts arrived, `null` where nothing fired. The matched text never travels.
pub async fn scan(c: &Client, texts: &[String]) -> Result<Vec<Option<String>>> {
    let (status, body) = c
        .http_request(reqwest::Method::POST, "/admin/ingest/scan", Some(json!({ "texts": texts })))
        .await?;
    let body = ok(status, body, "the tripwire scan")?;
    let rules = body.get("rules").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    Ok(rules.into_iter().map(|v| v.as_str().map(|s| s.to_string())).collect())
}

pub async fn check_emissions(c: &Client, probes: &[EmissionProbeReq]) -> Result<Vec<EmissionHit>> {
    let (status, body) = c
        .http_request(
            reqwest::Method::POST,
            "/admin/ingest/emissions/check",
            Some(json!({ "probes": probes })),
        )
        .await?;
    let body = ok(status, body, "the emission check")?;
    serde_json::from_value(body.get("hits").cloned().unwrap_or(json!([])))
        .map_err(|e| err(format!("the emission check answered something unreadable: {e}")))
}

pub async fn post_proposals(c: &Client, extractor: &str, facts: &[FactReq]) -> Result<PostReport> {
    let (status, body) = c
        .http_request(
            reqwest::Method::POST,
            "/admin/ingest/proposals",
            Some(json!({ "extractor": extractor, "facts": facts })),
        )
        .await?;
    let body = ok(status, body, "posting proposals")?;
    serde_json::from_value(body)
        .map_err(|e| err(format!("the post report was unreadable: {e}")))
}

pub async fn list_proposals(c: &Client, filter: &ProposalFilter) -> Result<Vec<Proposal>> {
    let (status, body) =
        c.http_get(&format!("/admin/ingest/proposals{}", filter.query())).await?;
    let body = ok(status, body, "listing the queue")?;
    serde_json::from_value(body.get("proposals").cloned().unwrap_or(json!([])))
        .map_err(|e| err(format!("the queue listing was unreadable: {e}")))
}

pub async fn show_proposal(c: &Client, id: Uuid) -> Result<ProposalDetail> {
    let (status, body) = c.http_get(&format!("/admin/ingest/proposals/{id}")).await?;
    let body = ok(status, body, "reading a proposal")?;
    serde_json::from_value(body).map_err(|e| err(format!("the proposal was unreadable: {e}")))
}

pub async fn approve(c: &Client, id: Uuid) -> Result<ApproveOutcome> {
    let (status, body) = c
        .http_request(
            reqwest::Method::POST,
            &format!("/admin/ingest/proposals/{id}/approve"),
            Some(json!({})),
        )
        .await?;
    let body = ok(status, body, "approving a proposal")?;
    serde_json::from_value(body).map_err(|e| err(format!("the approval answer was unreadable: {e}")))
}

pub async fn reject(c: &Client, id: Uuid, reason: Option<&str>) -> Result<bool> {
    let (status, body) = c
        .http_request(
            reqwest::Method::POST,
            &format!("/admin/ingest/proposals/{id}/reject"),
            Some(json!({ "reason": reason })),
        )
        .await?;
    let body = ok(status, body, "rejecting a proposal")?;
    Ok(body.get("rejected").and_then(|v| v.as_bool()).unwrap_or(false))
}

pub async fn unreject(c: &Client, id: Uuid) -> Result<bool> {
    let (status, body) = c
        .http_request(
            reqwest::Method::POST,
            &format!("/admin/ingest/proposals/{id}/unreject"),
            Some(json!({})),
        )
        .await?;
    let body = ok(status, body, "unrejecting a proposal")?;
    Ok(body.get("unrejected").and_then(|v| v.as_bool()).unwrap_or(false))
}

pub async fn watermarks(c: &Client, skipped_only: bool) -> Result<Vec<Watermark>> {
    let path = if skipped_only {
        "/admin/ingest/watermarks?skipped=true"
    } else {
        "/admin/ingest/watermarks"
    };
    let (status, body) = c.http_get(path).await?;
    let body = ok(status, body, "reading the watermarks")?;
    serde_json::from_value(body.get("watermarks").cloned().unwrap_or(json!([])))
        .map_err(|e| err(format!("the watermarks were unreadable: {e}")))
}

pub async fn advance_watermarks(
    c: &Client,
    run_id: Uuid,
    files: &[FileAdvanceReq],
) -> Result<WatermarkReport> {
    let (status, body) = c
        .http_request(
            reqwest::Method::POST,
            "/admin/ingest/watermarks",
            Some(json!({ "run_id": run_id, "files": files })),
        )
        .await?;
    let body = ok(status, body, "advancing the watermarks")?;
    serde_json::from_value(body)
        .map_err(|e| err(format!("the watermark report was unreadable: {e}")))
}

pub async fn unskip(c: &Client, file_path: &str) -> Result<bool> {
    let (status, body) = c
        .http_request(
            reqwest::Method::POST,
            "/admin/ingest/watermarks/unskip",
            Some(json!({ "file_path": file_path })),
        )
        .await?;
    let body = ok(status, body, "clearing a skip")?;
    Ok(body.get("unskipped").and_then(|v| v.as_bool()).unwrap_or(false))
}
