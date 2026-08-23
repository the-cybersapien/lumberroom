//! The queue commands: what the owner runs after a submit.
//!
//! `approve` takes several ids in one call and takes a `--run` filter, because a backfill queue
//! runs to hundreds of rows and a queue that costs one command per row is a queue the owner
//! abandons at row thirty.

use std::io::IsTerminal;

use crate::client::{err, Client, Result};
use crate::ingest::api::{self, Proposal, ProposalFilter};
use crate::{out, out_json, prompt};

/// Pad or truncate to an exact width. Local rather than `format::pad_end`: that one never
/// truncates, and every column here has a hard budget so one long speaker name cannot drag the
/// content column off the terminal.
fn fit(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.chars().take(width).collect()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}

fn short_id(id: &uuid::Uuid) -> String {
    id.to_string().chars().take(8).collect()
}

/// The first line of free text, for a prompt or a row: newlines inside a fact read as several
/// rows in a terminal table and break the count the owner is trying to verify.
fn first_line(s: &str) -> &str {
    s.split(['\n', '\r']).next().unwrap_or(s)
}

/// One row of `lumberroom ingest list`. Budgeted so a normal 80-column terminal holds it without
/// wrapping: id 8, state 9, speaker 11, auto 4, namespace clipped to 20, content fills the rest.
fn proposal_row(p: &Proposal) -> String {
    let auto = if p.auto { "auto" } else { "    " };
    let ns = fit(&p.namespace, 20);
    let content = first_line(&p.content);
    let budget = 100usize.saturating_sub(8 + 2 + 9 + 2 + 11 + 2 + 4 + 2 + 20 + 2);
    let content = fit(content, budget);
    format!(
        "{}  {}  {}  {auto}  {ns}  {content}",
        short_id(&p.id),
        fit(&p.state, 9),
        fit(&p.speaker, 11),
    )
}

/// `last_error`, when set, on its own line under the row it belongs to. A refusal is a thing the
/// owner reads, not a row that silently stopped moving (spec §2.3).
fn error_line(p: &Proposal) -> Option<String> {
    p.last_error.as_ref().map(|e| format!("    refused: {e}"))
}

/// `api::Proposal` derives `Deserialize` only, since the client never sends one back. `--json`
/// still needs a `Value`, built field by field rather than growing a `Serialize` impl this crate
/// does not own.
fn proposal_json(p: &Proposal) -> serde_json::Value {
    serde_json::json!({
        "id": p.id,
        "fingerprint": p.fingerprint,
        "content": p.content,
        "namespace": p.namespace,
        "tags": p.tags,
        "supersedes": p.supersedes,
        "speaker": p.speaker,
        "quote": p.quote,
        "auto": p.auto,
        "extractor": p.extractor,
        "state": p.state,
        "memory_id": p.memory_id,
        "last_error": p.last_error,
        "last_error_at": p.last_error_at,
        "decided_at": p.decided_at,
        "created_at": p.created_at,
    })
}

pub async fn list(c: &Client, filter: &ProposalFilter, json: bool) -> Result<()> {
    let proposals = api::list_proposals(c, filter).await?;
    if json {
        let rows: Vec<serde_json::Value> = proposals.iter().map(proposal_json).collect();
        out_json(&serde_json::Value::Array(rows));
        return Ok(());
    }
    if proposals.is_empty() {
        out("no proposals match");
        return Ok(());
    }
    for p in &proposals {
        out(&proposal_row(p));
        if let Some(line) = error_line(p) {
            out(&line);
        }
    }
    Ok(())
}

pub async fn show(c: &Client, id: uuid::Uuid, json: bool) -> Result<()> {
    let detail = api::show_proposal(c, id).await?;
    if json {
        let sources: Vec<serde_json::Value> = detail
            .sources
            .iter()
            .map(|s| {
                serde_json::json!({
                    "source_key": s.source_key,
                    "file_path": s.file_path,
                    "session_id": s.session_id,
                    "is_sidechain": s.is_sidechain,
                    "entry_uuid": s.entry_uuid,
                    "speaker": s.speaker,
                    "observed_at": s.observed_at,
                    "run_id": s.run_id,
                })
            })
            .collect();
        out_json(&serde_json::json!({
            "proposal": proposal_json(&detail.proposal),
            "sources": sources,
            "strongest_speaker": detail.strongest_speaker,
        }));
        return Ok(());
    }
    let p = &detail.proposal;
    out(&format!("fact: {}", p.content));
    out(&format!("namespace: {}", p.namespace));
    out(&format!("tags: {}", p.tags.join(", ")));
    out(&format!("speaker: {} (auto: {})", p.speaker, p.auto));
    // The frozen speaker is the one `write::run` will see. A stronger one arriving later on a
    // second source only shows up here, which is how the owner learns the fact he is looking at
    // was also typed by him somewhere, even though the row itself never upgrades (spec §2.3).
    if let Some(strongest) = &detail.strongest_speaker {
        if strongest != &p.speaker {
            out(&format!("strongest speaker across sources: {strongest}"));
        }
    }
    if let Some(quote) = &p.quote {
        out(&format!("quote: {quote}"));
    }
    if let Some(err) = &p.last_error {
        out(&format!("last error: {err}"));
    }
    out(&format!("sources ({}):", detail.sources.len()));
    for s in &detail.sources {
        let session = s.session_id.as_deref().unwrap_or("-");
        out(&format!("  {} [{}] session {session}", s.file_path, s.speaker));
    }
    Ok(())
}

/// Reads a yes/no line from stdin. `None` when stdin is not a terminal: a batch job under cron has
/// nothing to answer with, and blocking on a read that will never resolve is worse than refusing.
fn confirm(question: &str) -> Option<bool> {
    if !std::io::stdin().is_terminal() {
        return None;
    }
    prompt(question);
    let mut buf = String::new();
    if std::io::stdin().read_line(&mut buf).is_err() {
        return None;
    }
    Some(buf.trim().eq_ignore_ascii_case("yes"))
}

fn approve_outcome_line(id: uuid::Uuid, outcome: &Result<api::ApproveOutcome>) -> String {
    match outcome {
        Ok(o) if o.deduplicated => {
            format!("  {} deduplicated ({})", short_id(&id), o.memory_id.map(|m| m.to_string()).unwrap_or_default())
        }
        Ok(o) => match (&o.refused, o.memory_id) {
            (Some(reason), _) => format!("  {} refused: {reason}", short_id(&id)),
            (None, Some(memory_id)) => format!("  {} written {memory_id}", short_id(&id)),
            (None, None) => format!("  {} written", short_id(&id)),
        },
        Err(e) => format!("  {} failed: {}", short_id(&id), e.message),
    }
}

/// Ids, or a run filter with an optional speaker. The filtered form prints every row it will
/// approve, counts them, and asks once unless `--yes`.
pub async fn approve(
    c: &Client,
    ids: &[uuid::Uuid],
    run: Option<uuid::Uuid>,
    speaker: Option<&str>,
    yes: bool,
    json: bool,
) -> Result<()> {
    let targets: Vec<uuid::Uuid> = if let Some(run_id) = run {
        let filter = ProposalFilter {
            run_id: Some(run_id),
            speaker: speaker.map(str::to_string),
            state: Some("proposed".to_string()),
            ..Default::default()
        };
        let proposals = api::list_proposals(c, &filter).await?;
        if proposals.is_empty() {
            out("no proposed rows match that run");
            return Ok(());
        }
        for p in &proposals {
            out(&proposal_row(p));
        }
        out(&format!("{} row(s) will be approved", proposals.len()));
        if !yes {
            match confirm("Approve the rows above? Type \"yes\" to confirm: ") {
                Some(true) => {}
                Some(false) => {
                    out("aborted, nothing approved");
                    return Ok(());
                }
                None => {
                    return Err(err(
                        "refusing to approve without --yes: stdin is not a terminal to confirm on",
                    ));
                }
            }
        }
        proposals.into_iter().map(|p| p.id).collect()
    } else {
        if ids.is_empty() {
            return Err(err("usage: lumberroom ingest approve <id>... | --run <id> [--speaker s] [--yes]"));
        }
        ids.to_vec()
    };

    let mut outcomes = Vec::with_capacity(targets.len());
    for id in &targets {
        let outcome = api::approve(c, *id).await;
        if !json {
            out(&approve_outcome_line(*id, &outcome));
        }
        outcomes.push((*id, outcome));
    }

    let succeeded = outcomes.iter().filter(|(_, o)| o.is_ok()).count();
    if json {
        let rows: Vec<serde_json::Value> = outcomes
            .iter()
            .map(|(id, o)| match o {
                Ok(outcome) => serde_json::json!({
                    "id": id,
                    "memory_id": outcome.memory_id,
                    "deduplicated": outcome.deduplicated,
                    "refused": outcome.refused,
                }),
                Err(e) => serde_json::json!({ "id": id, "error": e.message }),
            })
            .collect();
        out_json(&serde_json::Value::Array(rows));
    } else {
        out(&format!("{succeeded} of {} approved", targets.len()));
    }

    // One refusal does not stop a batch, but a batch that refused every single row is a batch
    // that told the owner nothing worked and deserves a non-zero exit to say so.
    if succeeded == 0 && !targets.is_empty() {
        return Err(err(format!("all {} approvals failed or were refused", targets.len())));
    }
    Ok(())
}

/// Asks for confirmation printing the first line of the content, unless `--yes`. A rejection blocks
/// its fingerprint forever and a queue read at speed is where the wrong uuid gets typed.
pub async fn reject(c: &Client, id: uuid::Uuid, reason: Option<&str>, yes: bool) -> Result<()> {
    if !yes {
        let detail = api::show_proposal(c, id).await?;
        out(&format!("  {} {}", short_id(&id), first_line(&detail.proposal.content)));
        match confirm("Reject the row above? Type \"yes\" to confirm: ") {
            Some(true) => {}
            Some(false) => {
                out("aborted, nothing rejected");
                return Ok(());
            }
            None => {
                return Err(err(
                    "refusing to reject without --yes: stdin is not a terminal to confirm on",
                ));
            }
        }
    }
    let rejected = api::reject(c, id, reason).await?;
    if rejected {
        out(&format!("rejected {}", short_id(&id)));
    } else {
        out(&format!("{} was not rejected (already decided?)", short_id(&id)));
    }
    Ok(())
}

pub async fn unreject(c: &Client, id: uuid::Uuid) -> Result<()> {
    let unrejected = api::unreject(c, id).await?;
    if unrejected {
        out(&format!("{} returned to proposed", short_id(&id)));
    } else {
        out(&format!("{} was not rejected, nothing to undo", short_id(&id)));
    }
    Ok(())
}

fn watermark_row(w: &api::Watermark) -> String {
    let reason = w.skip_reason.as_deref().unwrap_or("-");
    format!(
        "{}  offset {}  entries {}  skip: {reason}",
        w.file_path, w.byte_offset, w.entries_seen
    )
}

pub async fn watermarks(c: &Client, skipped_only: bool, json: bool) -> Result<()> {
    let rows = api::watermarks(c, skipped_only).await?;
    if json {
        let values: Vec<serde_json::Value> = rows
            .iter()
            .map(|w| {
                serde_json::json!({
                    "file_path": w.file_path,
                    "session_id": w.session_id,
                    "is_sidechain": w.is_sidechain,
                    "byte_offset": w.byte_offset,
                    "prefix_sha256": w.prefix_sha256,
                    "entries_seen": w.entries_seen,
                    "skip_reason": w.skip_reason,
                    "skip_run_id": w.skip_run_id,
                    "fence_from": w.fence_from,
                    "fence_until": w.fence_until,
                    "fence_run_id": w.fence_run_id,
                    "last_run_id": w.last_run_id,
                    "updated_at": w.updated_at,
                })
            })
            .collect();
        out_json(&serde_json::Value::Array(values));
        return Ok(());
    }
    if rows.is_empty() {
        out(if skipped_only { "no skipped files" } else { "no watermarks yet" });
        return Ok(());
    }
    for w in &rows {
        out(&watermark_row(w));
    }
    Ok(())
}

pub async fn unskip(c: &Client, file_path: &str) -> Result<()> {
    let cleared = api::unskip(c, file_path).await?;
    if cleared {
        out(&format!("cleared the skip on {file_path}"));
    } else {
        out(&format!("{file_path} had no skip to clear"));
    }
    Ok(())
}

/// Delete a run directory and its spans. The property that makes a first run safe to abandon.
///
/// Every path built here is joined under `runs_dir()`, never taken from the caller as a full path,
/// so a run id cannot walk this out of the directory it is scoped to.
pub fn clean(run_id: Option<uuid::Uuid>, all: bool) -> Result<usize> {
    let base = crate::ingest::runs_dir()?;
    if let Some(id) = run_id {
        let path = base.join(id.to_string());
        if !path.exists() {
            return Ok(0);
        }
        std::fs::remove_dir_all(&path)
            .map_err(|e| err(format!("could not delete {}: {e}", path.display())))?;
        return Ok(1);
    }
    if !all {
        return Err(err("usage: lumberroom ingest clean <run-id> | --all"));
    }
    if !base.exists() {
        return Ok(0);
    }
    let mut deleted = 0;
    let entries = std::fs::read_dir(&base)
        .map_err(|e| err(format!("could not read {}: {e}", base.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| err(format!("could not read a run directory: {e}")))?;
        let path = entry.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path)
                .map_err(|e| err(format!("could not delete {}: {e}", path.display())))?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn sample(id: uuid::Uuid, state: &str, auto: bool, content: &str) -> Proposal {
        Proposal {
            id,
            fingerprint: "f".into(),
            content: content.to_string(),
            namespace: "user:me".into(),
            tags: vec![],
            supersedes: None,
            speaker: "owner_typed".into(),
            quote: None,
            auto,
            extractor: "agent:claude-code".into(),
            state: state.to_string(),
            memory_id: None,
            last_error: None,
            last_error_at: None,
            decided_at: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn a_row_carries_the_short_id_state_speaker_and_auto_marker() {
        let id = uuid::Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let p = sample(id, "proposed", true, "the owner runs arm64");
        let row = proposal_row(&p);
        assert!(row.starts_with("11111111  proposed   owner_typed  auto"), "{row}");
        assert!(row.contains("user:me"), "{row}");
        assert!(row.contains("the owner runs arm64"), "{row}");
    }

    #[test]
    fn a_non_auto_row_leaves_the_marker_blank() {
        let id = uuid::Uuid::new_v4();
        let p = sample(id, "proposed", false, "content");
        let row = proposal_row(&p);
        assert!(row.contains("       "), "{row}");
    }

    #[test]
    fn long_content_is_truncated_rather_than_wrapped() {
        let id = uuid::Uuid::new_v4();
        let long = "x".repeat(500);
        let p = sample(id, "proposed", false, &long);
        let row = proposal_row(&p);
        assert!(row.len() < long.len() + 60, "row was not truncated: {} chars", row.len());
    }

    #[test]
    fn a_multiline_fact_shows_only_its_first_line_in_a_row() {
        let id = uuid::Uuid::new_v4();
        let p = sample(id, "proposed", false, "line one\nline two");
        let row = proposal_row(&p);
        assert!(row.contains("line one"), "{row}");
        assert!(!row.contains("line two"), "{row}");
    }

    #[test]
    fn an_error_line_is_only_printed_when_last_error_is_set() {
        let id = uuid::Uuid::new_v4();
        let mut p = sample(id, "proposed", false, "content");
        assert!(error_line(&p).is_none());
        p.last_error = Some("credential_tripwire".into());
        let line = error_line(&p).unwrap();
        assert!(line.contains("credential_tripwire"), "{line}");
        assert!(line.starts_with("    refused:"), "{line}");
    }

    #[test]
    fn fit_pads_short_strings_and_truncates_long_ones() {
        assert_eq!(fit("ab", 5), "ab   ");
        assert_eq!(fit("abcdefgh", 5), "abcde");
    }

    #[test]
    fn short_id_is_the_first_eight_characters() {
        let id = uuid::Uuid::parse_str("abcdefab-0000-0000-0000-000000000000").unwrap();
        assert_eq!(short_id(&id), "abcdefab");
    }
}
