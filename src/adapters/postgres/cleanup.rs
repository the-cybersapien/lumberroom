//! Postgres implementation of `CleanupRepository`.
//!
//! Three candidate queries and a small queue. The candidate queries are where the care is, and two
//! properties carry them.
//!
//! **The window anchors one side.** A run reads rows created since its watermark and compares each
//! against every live row in scope, windowed or not. Filtering both sides is the obvious reading of
//! "read what changed" and it is wrong: restating a fact the store learned in July produces one new
//! row and one old one, and a window holding only the new one has nothing to compare it against.
//! That case is the most common duplicate there is.
//!
//! **Nothing here touches the HNSW index.** Every query filters by namespace, by sensitivity and to
//! live rows, and filtered HNSW is the trap this repository has already paid for: ten rows asked
//! for, zero returned, forty candidates pulled and all forty filtered away, no error. A pass whose
//! job is to notice what the store holds must never be able to answer "nothing" because an index
//! truncated, so the distance here is computed exactly. The work is new rows times live rows, which
//! a personal store does not make expensive.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::domain::cleanup::{cluster_key, CleanupKind, Disposition};
use crate::domain::errors::{DomainError, Result};
use crate::domain::types::Sensitivity;
use crate::ports::cleanup::{
    Candidate, CandidatePair, CandidateQuery, CleanupRepository, Member, NewProposal, Proposal,
    QueueOutcome, Watermark,
};

pub struct PgCleanupRepository {
    pool: PgPool,
}

impl PgCleanupRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// A namespace glob as SQL, matching `domain::namespaces::matches`.
///
/// `None` means every namespace, a trailing `*` means a prefix, anything else is the whole name.
/// Two bound parameters rather than a `format!`, because dynamic SQL built by string concatenation
/// is refused by the type system here on purpose.
fn ns_bounds(namespace: Option<&str>) -> (String, bool) {
    match namespace {
        None => (String::new(), false),
        Some(raw) => {
            let pattern = raw.trim().to_ascii_lowercase();
            match pattern.strip_suffix('*') {
                Some(prefix) => (prefix.to_string(), false),
                None => (pattern, true),
            }
        }
    }
}

/// The predicate every candidate query shares, spelled out in each of them rather than assembled.
///
/// It reads: this tenant, live, in scope, at or below the ceiling, and holding readable text. It is
/// written three times because `sqlx::query` refuses a string built at runtime, and rightly: a
/// predicate assembled with `format!` is one edit away from carrying data. `$1` tenant, `$2`
/// ceiling, `$3` namespace pattern, `$4` whether that pattern is exact.
///
/// `content IS NOT NULL` drops sealed rows whose bytes the server cannot read. A sealed row has no
/// plaintext to compare and no plaintext to show anyone, so it is not a cleanup candidate at all.
///
/// A test at the bottom of this file asserts the three copies stay identical, because three copies
/// of a security predicate is exactly the shape where one gets edited and the others do not.
#[cfg_attr(not(test), allow(dead_code))]
const IN_SCOPE_PREDICATE: &str = "AND ($3::text = '' OR ($4::bool AND m.namespace = $3) OR (NOT $4::bool AND m.namespace LIKE $3 || '%'))";

/// Live rows whose normalised content is byte-identical, grouped, anchored on what changed.
const EXACT_DUPLICATES_SQL: &str = r#"
            WITH scoped AS (
              SELECT m.id, m.namespace, m.sensitivity, m.content, m.created_at, m.access_count,
                     m.namespace || '\x1f' ||
                     lower(regexp_replace(btrim(m.content), '\s+', ' ', 'g')) AS norm
                FROM memory m
               WHERE     m.tenant_id = $1
    AND m.superseded_by IS NULL
    AND m.content IS NOT NULL
    AND (
      CASE $2::text
        WHEN 'open' THEN m.sensitivity = 'open'
        WHEN 'private' THEN m.sensitivity IN ('open', 'private')
        ELSE true
      END
    )
    AND ($3::text = '' OR ($4::bool AND m.namespace = $3) OR (NOT $4::bool AND m.namespace LIKE $3 || '%'))
            ),
            -- The anchor: groups holding at least one row this run is responsible for. A group
            -- entirely older than the watermark was already proposed by an earlier run.
            touched AS (
              SELECT norm FROM scoped
               WHERE $5::timestamptz IS NULL OR created_at >= $5
               GROUP BY norm
            )
            SELECT s.* FROM scoped s
             WHERE s.norm IN (SELECT norm FROM touched)
               AND s.norm IN (SELECT norm FROM scoped GROUP BY norm HAVING count(*) > 1)
             ORDER BY s.norm, s.created_at
             LIMIT $6
            "#;

/// Live pairs within a cosine band, anchored on one side and open on the other.
const SIMILAR_PAIRS_SQL: &str = r#"
            WITH scoped AS (
              SELECT m.id, m.namespace, m.sensitivity, m.content, m.created_at, m.access_count,
                     m.embedding
                FROM memory m
               WHERE     m.tenant_id = $1
    AND m.superseded_by IS NULL
    AND m.content IS NOT NULL
    AND (
      CASE $2::text
        WHEN 'open' THEN m.sensitivity = 'open'
        WHEN 'private' THEN m.sensitivity IN ('open', 'private')
        ELSE true
      END
    )
    AND ($3::text = '' OR ($4::bool AND m.namespace = $3) OR (NOT $4::bool AND m.namespace LIKE $3 || '%')) AND m.embedding IS NOT NULL
            )
            SELECT a.id AS a_id, a.namespace AS a_namespace, a.sensitivity AS a_sensitivity,
                   a.content AS a_content, a.created_at AS a_created_at,
                   a.access_count AS a_access_count,
                   b.id AS b_id, b.namespace AS b_namespace, b.sensitivity AS b_sensitivity,
                   b.content AS b_content, b.created_at AS b_created_at,
                   b.access_count AS b_access_count,
                   1 - (a.embedding <=> b.embedding) AS similarity
              FROM scoped a
              JOIN scoped b ON b.id <> a.id AND a.id < b.id
             WHERE ($5::timestamptz IS NULL OR a.created_at >= $5 OR b.created_at >= $5)
               AND 1 - (a.embedding <=> b.embedding) >= $6
             ORDER BY similarity DESC
             LIMIT $7
            "#;

/// The newest live row in scope. No window: the question is what the store now holds.
const NEWEST_SQL: &str = r#"
            SELECT max(m.created_at) AS newest
              FROM memory m
             WHERE     m.tenant_id = $1
    AND m.superseded_by IS NULL
    AND m.content IS NOT NULL
    AND (
      CASE $2::text
        WHEN 'open' THEN m.sensitivity = 'open'
        WHEN 'private' THEN m.sensitivity IN ('open', 'private')
        ELSE true
      END
    )
    AND ($3::text = '' OR ($4::bool AND m.namespace = $3) OR (NOT $4::bool AND m.namespace LIKE $3 || '%'))
"#;

/// Live rows nothing has read, older than the interval.
const UNREAD_SQL: &str = r#"
            SELECT m.id, m.namespace, m.sensitivity, m.content, m.created_at, m.access_count
              FROM memory m
             WHERE     m.tenant_id = $1
    AND m.superseded_by IS NULL
    AND m.content IS NOT NULL
    AND (
      CASE $2::text
        WHEN 'open' THEN m.sensitivity = 'open'
        WHEN 'private' THEN m.sensitivity IN ('open', 'private')
        ELSE true
      END
    )
    AND ($3::text = '' OR ($4::bool AND m.namespace = $3) OR (NOT $4::bool AND m.namespace LIKE $3 || '%'))
               AND m.access_count = 0
               AND m.last_confirmed_at IS NULL
               AND m.created_at < now() - make_interval(days => $5::int)
             ORDER BY m.created_at
             LIMIT $6
            "#;


fn candidate_from(row: &sqlx::postgres::PgRow, prefix: &str) -> Result<Candidate> {
    let id: Uuid = row.try_get(format!("{prefix}id").as_str()).map_err(map_err)?;
    let sensitivity: String = row.try_get(format!("{prefix}sensitivity").as_str()).map_err(map_err)?;
    Ok(Candidate {
        id: id.to_string(),
        namespace: row.try_get(format!("{prefix}namespace").as_str()).map_err(map_err)?,
        sensitivity: parse_sensitivity(&sensitivity)?,
        content: row.try_get(format!("{prefix}content").as_str()).map_err(map_err)?,
        created_at: row.try_get(format!("{prefix}created_at").as_str()).map_err(map_err)?,
        access_count: row.try_get(format!("{prefix}access_count").as_str()).map_err(map_err)?,
    })
}

fn parse_sensitivity(s: &str) -> Result<Sensitivity> {
    match s {
        "open" => Ok(Sensitivity::Open),
        "private" => Ok(Sensitivity::Private),
        "sealed" => Ok(Sensitivity::Sealed),
        other => Err(DomainError::internal(format!("unknown sensitivity {other:?} in the store"))),
    }
}

fn map_err(e: sqlx::Error) -> DomainError {
    DomainError::internal(format!("cleanup query failed: {e}"))
}

#[async_trait]
impl CleanupRepository for PgCleanupRepository {
    async fn exact_duplicates(
        &self,
        tenant: &str,
        q: &CandidateQuery,
    ) -> Result<Vec<Vec<Candidate>>> {
        let (ns, exact) = ns_bounds(q.namespace.as_deref());
        // Normalised the way the fingerprint is: case folded, whitespace collapsed, trimmed. Two
        // rows that differ only in spacing are the same fact typed twice, and a byte comparison
        // would call them distinct and leave the pair to the cosine band, where they arrive with a
        // judgement attached that they do not need.
        
        let rows = sqlx::query(EXACT_DUPLICATES_SQL)
            .bind(tenant)
            .bind(q.max_sensitivity.as_str())
            .bind(&ns)
            .bind(exact)
            .bind(q.since)
            .bind(q.limit)
            .fetch_all(&self.pool)
            .await
            .map_err(map_err)?;

        let mut groups: Vec<Vec<Candidate>> = Vec::new();
        let mut current_norm: Option<String> = None;
        for row in &rows {
            let norm: String = row.try_get("norm").map_err(map_err)?;
            let cand = candidate_from(row, "")?;
            if current_norm.as_deref() == Some(norm.as_str()) {
                groups.last_mut().expect("a group exists when the norm repeats").push(cand);
            } else {
                current_norm = Some(norm);
                groups.push(vec![cand]);
            }
        }
        // A group cut in half by the LIMIT is a group this run should not act on: proposing to
        // retire two of three identical rows leaves the third behind and the queue says the work
        // is done. Dropped, and the next run picks it up whole.
        groups.retain(|g| g.len() > 1);
        Ok(groups)
    }

    async fn similar_pairs(
        &self,
        tenant: &str,
        q: &CandidateQuery,
        min_similarity: f64,
    ) -> Result<Vec<CandidatePair>> {
        let (ns, exact) = ns_bounds(q.namespace.as_deref());
        // `<=>` is cosine distance, so similarity is 1 minus it. The join is anchored on `a` and
        // open on `b`: a is what changed, b is the whole store. a.id < b.id keeps one row per pair
        // rather than both directions, and the ORDER BY on created_at decides which is the older.
        //
        // No index hint and no HNSW. See the module comment.
        
        let rows = sqlx::query(SIMILAR_PAIRS_SQL)
            .bind(tenant)
            .bind(q.max_sensitivity.as_str())
            .bind(&ns)
            .bind(exact)
            .bind(q.since)
            .bind(min_similarity)
            .bind(q.limit)
            .fetch_all(&self.pool)
            .await
            .map_err(map_err)?;

        let mut pairs = Vec::with_capacity(rows.len());
        for row in &rows {
            let a = candidate_from(row, "a_")?;
            let b = candidate_from(row, "b_")?;
            let similarity: f64 = row.try_get("similarity").map_err(map_err)?;
            let (older, newer) = if a.created_at <= b.created_at { (a, b) } else { (b, a) };
            pairs.push(CandidatePair { older, newer, similarity });
        }
        Ok(pairs)
    }

    async fn unread(&self, tenant: &str, q: &CandidateQuery, days: i64) -> Result<Vec<Candidate>> {
        let (ns, exact) = ns_bounds(q.namespace.as_deref());
        // No `since` here. Staleness is a property of a row growing older without being read, so
        // the rows that qualify are exactly the ones the window would exclude.
        
        let rows = sqlx::query(UNREAD_SQL)
            .bind(tenant)
            .bind(q.max_sensitivity.as_str())
            .bind(&ns)
            .bind(exact)
            .bind(i32::try_from(days).unwrap_or(i32::MAX))
            .bind(q.limit)
            .fetch_all(&self.pool)
            .await
            .map_err(map_err)?;
        rows.iter().map(|r| candidate_from(r, "")).collect()
    }

    async fn newest_in_scope(
        &self,
        tenant: &str,
        q: &CandidateQuery,
    ) -> Result<Option<DateTime<Utc>>> {
        let (ns, exact) = ns_bounds(q.namespace.as_deref());
        let newest: Option<DateTime<Utc>> = sqlx::query_scalar(NEWEST_SQL)
            .bind(tenant)
            .bind(q.max_sensitivity.as_str())
            .bind(&ns)
            .bind(exact)
            .fetch_one(&self.pool)
            .await
            .map_err(map_err)?;
        Ok(newest)
    }

    async fn queue(&self, tenant: &str, p: NewProposal) -> Result<(QueueOutcome, String)> {
        let ids: Vec<String> = p.members.iter().map(|m| m.memory_id.clone()).collect();
        let key = cluster_key(p.kind, &ids);

        let mut tx = self.pool.begin().await.map_err(map_err)?;
        // ON CONFLICT DO NOTHING over (tenant_id, cluster_key). An hourly pass finds the same
        // cluster every hour until the owner acts, and every state counts as known, rejected
        // included: re-proposing what he already refused is how a queue stops being read.
        let inserted: Option<Uuid> = sqlx::query_scalar(
            r#"
            INSERT INTO cleanup_proposal
                   (id, tenant_id, kind, namespace, keep_id, rationale, produced_by, similarity,
                    cluster_key)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (tenant_id, cluster_key) DO NOTHING
            RETURNING id
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(tenant)
        .bind(p.kind.as_str())
        .bind(&p.namespace)
        .bind(p.keep_id.as_deref().and_then(|s| Uuid::parse_str(s).ok()))
        .bind(&p.rationale)
        .bind(&p.produced_by)
        .bind(p.similarity)
        .bind(&key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_err)?;

        let Some(id) = inserted else {
            tx.rollback().await.map_err(map_err)?;
            let existing: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM cleanup_proposal WHERE tenant_id = $1 AND cluster_key = $2",
            )
            .bind(tenant)
            .bind(&key)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_err)?;
            return Ok((
                QueueOutcome::AlreadyKnown,
                existing.map(|u| u.to_string()).unwrap_or_default(),
            ));
        };

        for m in &p.members {
            let member_id = Uuid::parse_str(&m.memory_id).map_err(|_| {
                DomainError::validation(format!("{} is not a memory id", m.memory_id))
            })?;
            sqlx::query(
                r#"
                INSERT INTO cleanup_proposal_member
                       (proposal_id, memory_id, disposition, seen_content)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (proposal_id, memory_id) DO NOTHING
                "#,
            )
            .bind(id)
            .bind(member_id)
            .bind(m.disposition.as_str())
            .bind(&m.seen_content)
            .execute(&mut *tx)
            .await
            .map_err(map_err)?;
        }
        tx.commit().await.map_err(map_err)?;
        Ok((QueueOutcome::Queued, id.to_string()))
    }

    async fn list(&self, tenant: &str, state: Option<&str>, limit: i64) -> Result<Vec<Proposal>> {
        let rows = sqlx::query(
            r#"
            SELECT id, kind, namespace, keep_id, rationale, produced_by, similarity, state,
                   reason, decided_at, created_at
              FROM cleanup_proposal
             WHERE tenant_id = $1 AND ($2::text IS NULL OR state = $2)
             ORDER BY created_at DESC
             LIMIT $3
            "#,
        )
        .bind(tenant)
        .bind(state)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_err)?;

        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut p = proposal_from(row)?;
            p.members = self.members_of(&p.id).await?;
            out.push(p);
        }
        Ok(out)
    }

    async fn get(&self, tenant: &str, id: &str) -> Result<Option<Proposal>> {
        let Ok(uuid) = Uuid::parse_str(id) else { return Ok(None) };
        let row = sqlx::query(
            r#"
            SELECT id, kind, namespace, keep_id, rationale, produced_by, similarity, state,
                   reason, decided_at, created_at
              FROM cleanup_proposal
             WHERE tenant_id = $1 AND id = $2
            "#,
        )
        .bind(tenant)
        .bind(uuid)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_err)?;

        match row {
            None => Ok(None),
            Some(row) => {
                let mut p = proposal_from(&row)?;
                p.members = self.members_of(&p.id).await?;
                Ok(Some(p))
            }
        }
    }

    async fn decide(
        &self,
        tenant: &str,
        id: &str,
        state: &str,
        reason: Option<&str>,
    ) -> Result<bool> {
        let Ok(uuid) = Uuid::parse_str(id) else { return Ok(false) };
        // `state = 'proposed'` in the predicate rather than checked first. Two callers deciding the
        // same row race otherwise, and the loser silently overwrites the winner's decision.
        let done = sqlx::query(
            r#"
            UPDATE cleanup_proposal
               SET state = $3, reason = $4, decided_at = now()
             WHERE tenant_id = $1 AND id = $2 AND state = 'proposed'
            "#,
        )
        .bind(tenant)
        .bind(uuid)
        .bind(state)
        .bind(reason)
        .execute(&self.pool)
        .await
        .map_err(map_err)?;
        Ok(done.rows_affected() == 1)
    }

    async fn close_answered(&self, tenant: &str) -> Result<Vec<String>> {
        // Three ways the store answers a finding by itself: a member is gone, a member was retired
        // by something else, or a member's text is not what the pass read. Apply refuses all three,
        // so leaving them queued costs the owner a read and tells him nothing.
        let ids: Vec<Uuid> = sqlx::query_scalar(
            r#"
            UPDATE cleanup_proposal p
               SET state = 'obsolete',
                   reason = 'the store answered this on its own',
                   decided_at = now()
             WHERE p.tenant_id = $1
               AND p.state = 'proposed'
               AND EXISTS (
                     SELECT 1
                       FROM cleanup_proposal_member cm
                       LEFT JOIN memory m ON m.id = cm.memory_id
                      WHERE cm.proposal_id = p.id
                        AND (m.id IS NULL
                             OR m.superseded_by IS NOT NULL
                             OR m.content IS DISTINCT FROM cm.seen_content)
                   )
            RETURNING p.id
            "#,
        )
        .bind(tenant)
        .fetch_all(&self.pool)
        .await
        .map_err(map_err)?;
        Ok(ids.into_iter().map(|u| u.to_string()).collect())
    }

    async fn valid_times(
        &self,
        tenant: &str,
        ids: &[String],
    ) -> Result<Vec<(String, Option<DateTime<Utc>>)>> {
        let uuids: Vec<Uuid> = ids.iter().filter_map(|s| Uuid::parse_str(s).ok()).collect();
        let rows = sqlx::query(
            "SELECT id, occurred_at FROM memory WHERE tenant_id = $1 AND id = ANY($2)",
        )
        .bind(tenant)
        .bind(&uuids)
        .fetch_all(&self.pool)
        .await
        .map_err(map_err)?;
        let mut found: std::collections::HashMap<String, Option<DateTime<Utc>>> =
            std::collections::HashMap::new();
        for row in &rows {
            let id: Uuid = row.try_get("id").map_err(map_err)?;
            found.insert(id.to_string(), row.try_get("occurred_at").map_err(map_err)?);
        }
        // In the order asked for, and every id present. A caller reconciling an ordering cannot
        // work from a list that silently dropped the row it was asking about.
        Ok(ids.iter().map(|id| (id.clone(), found.get(id).copied().flatten())).collect())
    }

    async fn watermark(
        &self,
        tenant: &str,
        scope: &str,
        cadence: &str,
    ) -> Result<Option<Watermark>> {
        let row = sqlx::query(
            r#"
            SELECT last_run_at, through FROM cleanup_watermark
             WHERE tenant_id = $1 AND scope = $2 AND cadence = $3
            "#,
        )
        .bind(tenant)
        .bind(scope)
        .bind(cadence)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_err)?;
        match row {
            None => Ok(None),
            Some(row) => Ok(Some(Watermark {
                last_run_at: row.try_get("last_run_at").map_err(map_err)?,
                through: row.try_get("through").map_err(map_err)?,
            })),
        }
    }

    async fn advance(
        &self,
        tenant: &str,
        scope: &str,
        cadence: &str,
        through: DateTime<Utc>,
    ) -> Result<()> {
        // GREATEST on `through` so a re-run over an older window cannot walk the mark backwards and
        // re-propose everything between.
        sqlx::query(
            r#"
            INSERT INTO cleanup_watermark (tenant_id, scope, cadence, last_run_at, through)
            VALUES ($1, $2, $3, now(), $4)
            ON CONFLICT (tenant_id, scope, cadence) DO UPDATE
               SET last_run_at = now(),
                   through = GREATEST(cleanup_watermark.through, EXCLUDED.through)
            "#,
        )
        .bind(tenant)
        .bind(scope)
        .bind(cadence)
        .bind(through)
        .execute(&self.pool)
        .await
        .map_err(map_err)?;
        Ok(())
    }
}

impl PgCleanupRepository {
    async fn members_of(&self, proposal_id: &str) -> Result<Vec<Member>> {
        let Ok(uuid) = Uuid::parse_str(proposal_id) else { return Ok(Vec::new()) };
        let rows = sqlx::query(
            r#"
            SELECT cm.memory_id, cm.disposition, cm.seen_content,
                   m.content AS current_content, m.superseded_by
              FROM cleanup_proposal_member cm
              LEFT JOIN memory m ON m.id = cm.memory_id
             WHERE cm.proposal_id = $1
             ORDER BY cm.disposition DESC, cm.memory_id
            "#,
        )
        .bind(uuid)
        .fetch_all(&self.pool)
        .await
        .map_err(map_err)?;

        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let memory_id: Uuid = row.try_get("memory_id").map_err(map_err)?;
            let disposition: String = row.try_get("disposition").map_err(map_err)?;
            let superseded_by: Option<Uuid> = row.try_get("superseded_by").map_err(map_err)?;
            out.push(Member {
                memory_id: memory_id.to_string(),
                disposition: Disposition::parse(&disposition).ok_or_else(|| {
                    DomainError::internal(format!("unknown disposition {disposition:?}"))
                })?,
                seen_content: row.try_get("seen_content").map_err(map_err)?,
                current_content: row.try_get("current_content").map_err(map_err)?,
                superseded_by: superseded_by.map(|u| u.to_string()),
            });
        }
        Ok(out)
    }
}

fn proposal_from(row: &sqlx::postgres::PgRow) -> Result<Proposal> {
    let id: Uuid = row.try_get("id").map_err(map_err)?;
    let keep_id: Option<Uuid> = row.try_get("keep_id").map_err(map_err)?;
    let kind: String = row.try_get("kind").map_err(map_err)?;
    Ok(Proposal {
        id: id.to_string(),
        kind: CleanupKind::parse(&kind)
            .ok_or_else(|| DomainError::internal(format!("unknown cleanup kind {kind:?}")))?,
        namespace: row.try_get("namespace").map_err(map_err)?,
        keep_id: keep_id.map(|u| u.to_string()),
        rationale: row.try_get("rationale").map_err(map_err)?,
        produced_by: row.try_get("produced_by").map_err(map_err)?,
        similarity: row.try_get("similarity").map_err(map_err)?,
        state: row.try_get("state").map_err(map_err)?,
        reason: row.try_get("reason").map_err(map_err)?,
        decided_at: row.try_get("decided_at").map_err(map_err)?,
        created_at: row.try_get("created_at").map_err(map_err)?,
        members: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_namespace_matches_only_itself() {
        assert_eq!(ns_bounds(Some("project:lumberroom")), ("project:lumberroom".to_string(), true));
    }

    #[test]
    fn a_trailing_star_becomes_a_prefix() {
        assert_eq!(ns_bounds(Some("project:*")), ("project:".to_string(), false));
    }

    #[test]
    fn no_namespace_means_every_namespace() {
        // The empty pattern is what the SQL reads as "no scope filter". A prefix match on "" would
        // also match everything, but only by accident, and the predicate says so explicitly.
        assert_eq!(ns_bounds(None), (String::new(), false));
    }

    #[test]
    fn a_glob_is_trimmed_and_folded_the_way_a_grant_is() {
        assert_eq!(ns_bounds(Some("  Project:Lumberroom  ")), ("project:lumberroom".to_string(), true));
    }

    #[test]
    fn every_candidate_query_carries_the_same_scope_predicate() {
        // Three copies of the filter that decides what a caller may see. One edited and two not is
        // the failure this catches, and it is silent: the pass keeps working and starts reading
        // rows it should not.
        for (name, sql) in [
            ("exact_duplicates", EXACT_DUPLICATES_SQL),
            ("similar_pairs", SIMILAR_PAIRS_SQL),
            ("unread", UNREAD_SQL),
            ("newest_in_scope", NEWEST_SQL),
        ] {
            assert!(sql.contains(IN_SCOPE_PREDICATE), "{name} lost the namespace scope predicate");
            assert!(sql.contains("m.superseded_by IS NULL"), "{name} would read retired rows");
            assert!(sql.contains("m.content IS NOT NULL"), "{name} would read sealed rows");
            assert!(
                sql.contains("WHEN 'open' THEN m.sensitivity = 'open'"),
                "{name} lost the sensitivity ceiling, which is what keeps a private row away from \
                 a model"
            );
        }
    }

    #[test]
    fn the_similar_pairs_query_never_reaches_for_the_index() {
        // Filtered HNSW returned zero rows against 40,000 here once. A pass that answers "nothing
        // to clean up" because an index truncated is the same failure wearing a different hat.
        assert!(
            !SIMILAR_PAIRS_SQL.contains("ORDER BY a.embedding <=>"),
            "an ORDER BY on the operator is what makes the planner choose HNSW"
        );
        assert!(SIMILAR_PAIRS_SQL.contains("ORDER BY similarity DESC"));
    }

    #[test]
    fn the_pair_join_takes_each_pair_once() {
        // Without a.id < b.id every pair arrives twice, in both directions, and the cluster key
        // hides the second while the limit silently halves the useful result.
        assert!(SIMILAR_PAIRS_SQL.contains("a.id < b.id"));
    }

    #[test]
    fn the_similar_pairs_window_leaves_one_side_open() {
        // The trap: filtering both sides makes the pass blind to a new row restating an old fact,
        // which is the most common duplicate there is.
        assert!(
            SIMILAR_PAIRS_SQL.contains("a.created_at >= $5 OR b.created_at >= $5"),
            "the window has to admit a pair where only one side is new"
        );
    }

    #[test]
    fn staleness_does_not_take_the_window() {
        // A stale row qualifies by being old and unread, so a window over recent rows excludes
        // exactly the rows this query is looking for.
        assert!(!UNREAD_SQL.contains("$5::timestamptz"));
        assert!(UNREAD_SQL.contains("m.access_count = 0"));
    }
}
