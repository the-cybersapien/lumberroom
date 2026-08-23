//! Postgres implementation of IngestRepository. The proposal queue, the watermarks, the run record
//! and the emission check.
//!
//! Nothing here writes to `memory`. The one row this adapter may point at is `ingest_proposal.
//! memory_id`, and it learns that id from the approval path after `services::write::run` returned
//! it.
//!
//! Three statements in this file carry the guarantees the rest of the phase rests on.
//!
//! `insert_proposal` conflicts to `DO NOTHING`, never to `DO UPDATE`. `speaker`, `quote` and `auto`
//! are frozen at first insert, so a fact already in the queue cannot gain the right to write itself
//! from a later arrival.
//!
//! `advance_watermark` takes `GREATEST` of the stored offset and the target, and everything that
//! describes the offset moves under the same condition. Two runs overlap on an ordinary Tuesday and
//! a plain assignment lets the older one drag the mark backwards.
//!
//! `emissions_matching` joins the probes in as arrays and applies the window in SQL. One probe
//! against a popular fact matches many rows, and filtering after the fetch turns a bounded read
//! into an unbounded one.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::domain::errors::Result;
use crate::ports::ingest::{
    EmissionHit, EmissionProbe, IngestRepository, NewProposal, NewRun, Proposal, ProposalFilter,
    ProposalSource, ProposalUpsert, RunRecord, RunTotals, Watermark, WatermarkAdvance,
};

pub struct PgIngestRepository {
    pool: PgPool,
}

impl PgIngestRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn proposal_from_row(r: &sqlx::postgres::PgRow) -> Proposal {
    Proposal {
        id: r.get("id"),
        fingerprint: r.get("fingerprint"),
        content: r.get("content"),
        namespace: r.get("namespace"),
        tags: r.get::<Vec<String>, _>("tags"),
        supersedes: r.get("supersedes"),
        speaker: r.get("speaker"),
        quote: r.get("quote"),
        auto: r.get("auto"),
        extractor: r.get("extractor"),
        state: r.get("state"),
        memory_id: r.get("memory_id"),
        last_error: r.get("last_error"),
        last_error_at: r.get("last_error_at"),
        decided_at: r.get("decided_at"),
        created_at: r.get("created_at"),
    }
}

fn source_from_row(r: &sqlx::postgres::PgRow) -> ProposalSource {
    ProposalSource {
        source_key: r.get("source_key"),
        file_path: r.get("file_path"),
        session_id: r.get("session_id"),
        is_sidechain: r.get("is_sidechain"),
        entry_uuid: r.get("entry_uuid"),
        speaker: r.get("speaker"),
        observed_at: r.get("observed_at"),
        run_id: r.get("run_id"),
    }
}

fn watermark_from_row(r: &sqlx::postgres::PgRow) -> Watermark {
    Watermark {
        file_path: r.get("file_path"),
        session_id: r.get("session_id"),
        is_sidechain: r.get("is_sidechain"),
        byte_offset: r.get("byte_offset"),
        prefix_sha256: r.get("prefix_sha256"),
        entries_seen: r.get("entries_seen"),
        skip_reason: r.get("skip_reason"),
        skip_run_id: r.get("skip_run_id"),
        fence_from: r.get("fence_from"),
        fence_until: r.get("fence_until"),
        fence_run_id: r.get("fence_run_id"),
        last_run_id: r.get("last_run_id"),
        updated_at: r.get("updated_at"),
    }
}

#[async_trait]
impl IngestRepository for PgIngestRepository {
    async fn open_run(&self, tenant: &str, run: NewRun) -> Result<Uuid> {
        let row = sqlx::query(
            "INSERT INTO ingest_run (id, tenant_id, scope, extractor)
             VALUES ($1, $2, $3, $4)
             RETURNING id",
        )
        .bind(run.id)
        .bind(tenant)
        .bind(&run.scope)
        .bind(&run.extractor)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get("id"))
    }

    /// One statement, so a report that reads the run while it closes sees either the open row or
    /// the finished one and never a half-stamped mixture.
    async fn close_run(&self, tenant: &str, id: Uuid, totals: RunTotals) -> Result<()> {
        sqlx::query(
            "UPDATE ingest_run
                SET finished_at          = now(),
                    files_seen           = $3,
                    files_skipped        = $4,
                    entries_seen         = $5,
                    entries_excluded     = $6,
                    unknown_types        = $7,
                    spans_cut            = $8,
                    chunks               = $9,
                    chunks_missing       = $10,
                    chunks_failed        = $11,
                    files_held_back      = $12,
                    fenced_entries       = $13,
                    fences_unclosed      = $14,
                    proposals_new        = $15,
                    proposals_reinforced = $16,
                    confirmations        = $17,
                    traversal_capped     = $18,
                    artifact_sessions    = $19
              WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant)
        .bind(id)
        .bind(totals.files_seen)
        .bind(json_or(totals.files_skipped, serde_json::json!({})))
        .bind(totals.entries_seen)
        .bind(json_or(totals.entries_excluded, serde_json::json!({})))
        .bind(json_or(totals.unknown_types, serde_json::json!({})))
        .bind(totals.spans_cut)
        .bind(totals.chunks)
        .bind(totals.chunks_missing)
        .bind(totals.chunks_failed)
        .bind(json_or(totals.files_held_back, serde_json::json!([])))
        .bind(totals.fenced_entries)
        .bind(totals.fences_unclosed)
        .bind(totals.proposals_new)
        .bind(totals.proposals_reinforced)
        .bind(totals.confirmations)
        .bind(totals.traversal_capped)
        .bind(json_or(totals.artifact_sessions, serde_json::json!([])))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn run(&self, tenant: &str, id: Uuid) -> Result<Option<RunRecord>> {
        let row = sqlx::query(
            "SELECT id, started_at, finished_at, scope, extractor, files_seen, files_skipped,
                    entries_seen, entries_excluded, unknown_types, spans_cut, chunks,
                    chunks_missing, chunks_failed, files_held_back, fenced_entries,
                    fences_unclosed, proposals_new, proposals_reinforced, confirmations,
                    traversal_capped, artifact_sessions
               FROM ingest_run
              WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| RunRecord {
            id: r.get("id"),
            started_at: r.get("started_at"),
            finished_at: r.get("finished_at"),
            scope: r.get("scope"),
            extractor: r.get("extractor"),
            files_seen: r.get("files_seen"),
            files_skipped: r.get("files_skipped"),
            entries_seen: r.get("entries_seen"),
            entries_excluded: r.get("entries_excluded"),
            unknown_types: r.get("unknown_types"),
            spans_cut: r.get("spans_cut"),
            chunks: r.get("chunks"),
            chunks_missing: r.get("chunks_missing"),
            chunks_failed: r.get("chunks_failed"),
            files_held_back: r.get("files_held_back"),
            fenced_entries: r.get("fenced_entries"),
            fences_unclosed: r.get("fences_unclosed"),
            proposals_new: r.get("proposals_new"),
            proposals_reinforced: r.get("proposals_reinforced"),
            confirmations: r.get("confirmations"),
            traversal_capped: r.get("traversal_capped"),
            artifact_sessions: r.get("artifact_sessions"),
        }))
    }

    /// Insert the fact, or attach a source to the row that already holds its fingerprint.
    ///
    /// `DO NOTHING` rather than `DO UPDATE`, which is the difference between a queue and a store
    /// that rewrites itself under the reader. The source row goes in either way and carries its own
    /// speaker, so `show` can report the strongest speaker across sources without any of them
    /// touching the frozen value on the parent.
    ///
    /// Both writes share one transaction: a source row whose proposal insert rolled back would name
    /// a proposal nobody can read, and a proposal with no source cannot be traced back to a file.
    async fn insert_proposal(&self, tenant: &str, proposal: NewProposal) -> Result<ProposalUpsert> {
        let mut tx = self.pool.begin().await?;

        let inserted = sqlx::query(
            "INSERT INTO ingest_proposal
                 (id, tenant_id, fingerprint, content, namespace, tags, supersedes, speaker,
                  quote, auto, extractor, state)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'proposed')
             ON CONFLICT (tenant_id, fingerprint) DO NOTHING
             RETURNING id, fingerprint, content, namespace, tags, supersedes, speaker, quote,
                       auto, extractor, state, memory_id, last_error, last_error_at, decided_at,
                       created_at",
        )
        .bind(Uuid::new_v4())
        .bind(tenant)
        .bind(&proposal.fingerprint)
        .bind(&proposal.content)
        .bind(&proposal.namespace)
        .bind(&proposal.tags)
        .bind(proposal.supersedes)
        .bind(&proposal.speaker)
        .bind(proposal.quote.as_deref())
        .bind(proposal.auto)
        .bind(&proposal.extractor)
        .fetch_optional(&mut *tx)
        .await?;

        let (row, created) = match inserted {
            Some(r) => (r, true),
            None => {
                let existing = sqlx::query(
                    "SELECT id, fingerprint, content, namespace, tags, supersedes, speaker, quote,
                            auto, extractor, state, memory_id, last_error, last_error_at,
                            decided_at, created_at
                       FROM ingest_proposal
                      WHERE tenant_id = $1 AND fingerprint = $2",
                )
                .bind(tenant)
                .bind(&proposal.fingerprint)
                .fetch_one(&mut *tx)
                .await?;
                (existing, false)
            }
        };

        let stored = proposal_from_row(&row);
        let source = proposal.source;

        sqlx::query(
            "INSERT INTO ingest_proposal_source
                 (proposal_id, source_key, file_path, session_id, is_sidechain, entry_uuid,
                  speaker, observed_at, run_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (proposal_id, source_key) DO NOTHING",
        )
        .bind(stored.id)
        .bind(&source.source_key)
        .bind(&source.file_path)
        .bind(source.session_id.as_deref())
        .bind(source.is_sidechain)
        .bind(source.entry_uuid.as_deref())
        .bind(&source.speaker)
        .bind(source.observed_at)
        .bind(source.run_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(match created {
            true => ProposalUpsert::Created(stored),
            false => ProposalUpsert::Existing(stored),
        })
    }

    /// Every filter is a null-guarded bind rather than a clause appended to a string. A `format!`
    /// here would be the one place in this codebase where a query is assembled from values.
    async fn list_proposals(&self, tenant: &str, filter: ProposalFilter) -> Result<Vec<Proposal>> {
        let rows = sqlx::query(
            "SELECT p.id, p.fingerprint, p.content, p.namespace, p.tags, p.supersedes, p.speaker,
                    p.quote, p.auto, p.extractor, p.state, p.memory_id, p.last_error,
                    p.last_error_at, p.decided_at, p.created_at
               FROM ingest_proposal p
              WHERE p.tenant_id = $1
                AND ($2::text IS NULL OR p.state = $2)
                AND ($3::bool IS NULL OR p.auto = $3)
                AND ($4::text IS NULL OR p.speaker = $4)
                AND ($5::uuid IS NULL OR EXISTS (
                      SELECT 1 FROM ingest_proposal_source s
                       WHERE s.proposal_id = p.id AND s.run_id = $5))
              ORDER BY p.created_at DESC, p.id
              LIMIT $6",
        )
        .bind(tenant)
        .bind(filter.state.as_deref())
        .bind(filter.auto)
        .bind(filter.speaker.as_deref())
        .bind(filter.run_id)
        .bind(filter.limit.clamp(1, 1000))
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(proposal_from_row).collect())
    }

    async fn proposal(&self, tenant: &str, id: Uuid) -> Result<Option<Proposal>> {
        let row = sqlx::query(
            "SELECT id, fingerprint, content, namespace, tags, supersedes, speaker, quote, auto,
                    extractor, state, memory_id, last_error, last_error_at, decided_at, created_at
               FROM ingest_proposal
              WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(proposal_from_row))
    }

    /// Joined back through the parent so a source cannot be read across tenants by id alone.
    async fn proposal_sources(&self, tenant: &str, id: Uuid) -> Result<Vec<ProposalSource>> {
        let rows = sqlx::query(
            "SELECT s.source_key, s.file_path, s.session_id, s.is_sidechain, s.entry_uuid,
                    s.speaker, s.observed_at, s.run_id
               FROM ingest_proposal_source s
               JOIN ingest_proposal p ON p.id = s.proposal_id
              WHERE p.tenant_id = $1 AND s.proposal_id = $2
              ORDER BY s.observed_at NULLS LAST, s.source_key",
        )
        .bind(tenant)
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(source_from_row).collect())
    }

    /// Clears the last refusal along with the state. A written row that still displayed the error
    /// from an earlier failed approval would read as a write that did not happen.
    async fn mark_written(&self, tenant: &str, id: Uuid, memory_id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE ingest_proposal
                SET state = 'written', memory_id = $3, decided_at = now(),
                    last_error = NULL, last_error_at = NULL
              WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant)
        .bind(id)
        .bind(memory_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The state deliberately does not move. A refused proposal stays in the queue with the reason
    /// attached, because the owner reading why is the point.
    async fn mark_error(&self, tenant: &str, id: Uuid, message: &str) -> Result<()> {
        sqlx::query(
            "UPDATE ingest_proposal
                SET last_error = $3, last_error_at = now()
              WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant)
        .bind(id)
        .bind(message)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn reject(&self, tenant: &str, id: Uuid) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE ingest_proposal
                SET state = 'rejected', decided_at = now()
              WHERE tenant_id = $1 AND id = $2 AND state = 'proposed'",
        )
        .bind(tenant)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// `decided_at` stays where the rejection put it, so the queue can show what was undone.
    async fn unreject(&self, tenant: &str, id: Uuid) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE ingest_proposal
                SET state = 'proposed'
              WHERE tenant_id = $1 AND id = $2 AND state = 'rejected'",
        )
        .bind(tenant)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    async fn watermark(&self, tenant: &str, file_path: &str) -> Result<Option<Watermark>> {
        let row = sqlx::query(
            "SELECT file_path, session_id, is_sidechain, byte_offset, prefix_sha256, entries_seen,
                    skip_reason, skip_run_id, fence_from, fence_until, fence_run_id, last_run_id,
                    updated_at
               FROM ingest_watermark
              WHERE tenant_id = $1 AND file_path = $2",
        )
        .bind(tenant)
        .bind(file_path)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(watermark_from_row))
    }

    async fn watermarks(&self, tenant: &str, skipped_only: bool) -> Result<Vec<Watermark>> {
        let rows = sqlx::query(
            "SELECT file_path, session_id, is_sidechain, byte_offset, prefix_sha256, entries_seen,
                    skip_reason, skip_run_id, fence_from, fence_until, fence_run_id, last_run_id,
                    updated_at
               FROM ingest_watermark
              WHERE tenant_id = $1
                AND (NOT $2 OR skip_reason IS NOT NULL)
              ORDER BY updated_at DESC, file_path",
        )
        .bind(tenant)
        .bind(skipped_only)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(watermark_from_row).collect())
    }

    /// The monotonic advance, and the one statement in this phase that can lose transcript bytes.
    ///
    /// `GREATEST` on the offset, and every column that describes the offset moves only when the
    /// offset did: the prefix hash belongs to whichever offset won, and a losing older run leaves
    /// no `last_run_id` and no fresh `updated_at` on a row it did not move. The returned value is
    /// what is stored afterwards, which is what a caller has to report rather than the number it
    /// asked for.
    async fn advance_watermark(&self, tenant: &str, advance: WatermarkAdvance) -> Result<i64> {
        let row = sqlx::query(
            "INSERT INTO ingest_watermark
                 (tenant_id, file_path, session_id, is_sidechain, byte_offset, prefix_sha256,
                  entries_seen, last_run_id, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now())
             ON CONFLICT (tenant_id, file_path) DO UPDATE
                SET byte_offset   = GREATEST(ingest_watermark.byte_offset, EXCLUDED.byte_offset),
                    prefix_sha256 = CASE
                                      WHEN EXCLUDED.byte_offset > ingest_watermark.byte_offset
                                      THEN EXCLUDED.prefix_sha256
                                      ELSE ingest_watermark.prefix_sha256 END,
                    last_run_id   = CASE
                                      WHEN EXCLUDED.byte_offset > ingest_watermark.byte_offset
                                      THEN EXCLUDED.last_run_id
                                      ELSE ingest_watermark.last_run_id END,
                    updated_at    = CASE
                                      WHEN EXCLUDED.byte_offset > ingest_watermark.byte_offset
                                      THEN now()
                                      ELSE ingest_watermark.updated_at END,
                    session_id    = COALESCE(EXCLUDED.session_id, ingest_watermark.session_id),
                    is_sidechain  = EXCLUDED.is_sidechain,
                    entries_seen  = GREATEST(ingest_watermark.entries_seen, EXCLUDED.entries_seen)
             RETURNING byte_offset",
        )
        .bind(tenant)
        .bind(&advance.file_path)
        .bind(advance.session_id.as_deref())
        .bind(advance.is_sidechain)
        .bind(advance.byte_offset.max(0))
        .bind(&advance.prefix_sha256)
        .bind(advance.entries_seen.max(0))
        .bind(advance.run_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get("byte_offset"))
    }

    /// The first reason wins. A file skipped as this run's own artifact and later skipped again for
    /// something else keeps the first explanation, which is the one that says why it stopped.
    async fn set_skip(
        &self,
        tenant: &str,
        file_path: &str,
        reason: &str,
        run_id: Uuid,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO ingest_watermark (tenant_id, file_path, skip_reason, skip_run_id)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (tenant_id, file_path) DO UPDATE
                SET skip_reason = COALESCE(ingest_watermark.skip_reason, EXCLUDED.skip_reason),
                    skip_run_id = CASE
                                    WHEN ingest_watermark.skip_reason IS NULL
                                    THEN EXCLUDED.skip_run_id
                                    ELSE ingest_watermark.skip_run_id END",
        )
        .bind(tenant)
        .bind(file_path)
        .bind(reason)
        .bind(run_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn clear_skip(&self, tenant: &str, file_path: &str) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE ingest_watermark
                SET skip_reason = NULL, skip_run_id = NULL, updated_at = now()
              WHERE tenant_id = $1 AND file_path = $2 AND skip_reason IS NOT NULL",
        )
        .bind(tenant)
        .bind(file_path)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    async fn record_emission(
        &self,
        tenant: &str,
        content_sha256: &str,
        memory_id: Uuid,
        tool: &str,
        session_id: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO recall_emission
                 (tenant_id, content_sha256, memory_id, tool, session_id)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (tenant_id, content_sha256, memory_id, tool) DO UPDATE
                SET last_emitted_at = now(),
                    emit_count      = recall_emission.emit_count + 1,
                    session_id      = COALESCE(EXCLUDED.session_id, recall_emission.session_id)",
        )
        .bind(tenant)
        .bind(content_sha256)
        .bind(memory_id)
        .bind(tool)
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The probes join in as two parallel arrays, which is the only form `unnest` takes and the
    /// only way to apply each probe's own window without a statement per fact.
    ///
    /// `first_emitted_at <= observed_at + slack` is the direction that makes a match an echo: the
    /// store handed the content out before the transcript recorded it. The lower bound keeps a fact
    /// emitted last year out of a span written today, which would be a coincidence rather than a
    /// loop. `DISTINCT` because two probes in one batch can carry the same hash.
    async fn emissions_matching(
        &self,
        tenant: &str,
        probes: &[EmissionProbe],
        slack_secs: f64,
        window_secs: f64,
    ) -> Result<Vec<EmissionHit>> {
        if probes.is_empty() {
            return Ok(vec![]);
        }
        let hashes: Vec<String> = probes.iter().map(|p| p.content_sha256.clone()).collect();
        let observed: Vec<DateTime<Utc>> = probes.iter().map(|p| p.observed_at).collect();

        let rows = sqlx::query(
            "SELECT DISTINCT e.content_sha256, e.memory_id, e.tool, e.first_emitted_at
               FROM recall_emission e
               JOIN unnest($2::text[], $3::timestamptz[]) AS p(hash, observed_at)
                 ON e.content_sha256 = p.hash
              WHERE e.tenant_id = $1
                AND e.first_emitted_at <= p.observed_at + make_interval(secs => $4::float8)
                AND e.first_emitted_at >= p.observed_at - make_interval(secs => $5::float8)",
        )
        .bind(tenant)
        .bind(&hashes)
        .bind(&observed)
        .bind(slack_secs)
        .bind(window_secs)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|r| EmissionHit {
                content_sha256: r.get("content_sha256"),
                memory_id: r.get("memory_id"),
                tool: r.get("tool"),
                first_emitted_at: r.get("first_emitted_at"),
            })
            .collect())
    }
}

/// A jsonb column with a NOT NULL default takes the default's shape when the caller counted
/// nothing. `RunTotals::default()` leaves these as JSON null, and a null in `files_skipped` would
/// read as "the counter broke" rather than "nothing was skipped".
fn json_or(value: serde_json::Value, fallback: serde_json::Value) -> serde_json::Value {
    match value.is_null() {
        true => fallback,
        false => value,
    }
}
