//! Postgres implementation of RegistryRepository. Exact lookups, the versioned upsert, and aliases.
//!
//! The registry is the half fuzzy memory cannot answer, so every read here carries the same
//! sensitivity ceiling as a memory read. It holds credential locations: a registry list that
//! skipped the ceiling would leak more per row than a memory search does.

use async_trait::async_trait;
use sqlx::{PgPool, Row};

use crate::domain::canonical;
use crate::domain::errors::{DomainError, Result};
use crate::domain::policy::{NamespaceCeiling, NamespaceGrant};
use crate::domain::types::{RegistryEntry, Sensitivity};
use crate::ports::registry::{RegistryUpsert, RegistryVersion};
use crate::ports::{AliasOrigin, RegistryRepository, RegistryWrite};

pub struct PgRegistryRepository {
    pool: PgPool,
}

impl PgRegistryRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Exact key first, then the key read as a redirect.
///
/// The preference matters. A row stored under the name asked for answers for itself; an alias
/// pointing away from a key that exists is a contradiction, and resolving it in favour of the stored
/// row means adding an alias can never hide a fact.
///
/// One hop only. A chain of redirects means the canonical key itself was renamed, which is an
/// operator action that updates the alias rows rather than something to paper over at read time.
const GET_SQL: &str = r#"
    WITH candidate AS (
        SELECT r.namespace, r.kind, r.key, r.value, r.provenance, r.sensitivity, r.version,
               NULL::text AS resolved_from, 0 AS preference
          FROM registry r
         WHERE r.tenant_id = $1 AND r.namespace = $2 AND r.kind = $4 AND r.key = $5
           AND sensitivity_rank(r.sensitivity) <= sensitivity_rank($3)
        UNION ALL
        SELECT r.namespace, r.kind, r.key, r.value, r.provenance, r.sensitivity, r.version,
               a.alias_key AS resolved_from, 1 AS preference
          FROM registry_alias a
          JOIN registry r
            ON r.tenant_id = a.tenant_id AND r.namespace = a.namespace
           AND r.kind = a.kind AND r.key = a.canonical
         WHERE a.tenant_id = $1 AND a.namespace = $2 AND a.kind = $4 AND a.alias_key = $5
           AND sensitivity_rank(r.sensitivity) <= sensitivity_rank($3)
    )
    SELECT namespace, kind, key, value, provenance, sensitivity, version, resolved_from
      FROM candidate ORDER BY preference LIMIT 1
"#;

/// What a key used to hold, newest first, one key's worth.
///
/// The two arms and the preference ordering are `GET_SQL`'s, for the reason given there: a redirect
/// must never hide rows filed under the name the caller used. `answering` then picks one arm and
/// returns all of its rows, so an answer is one key's history rather than two keys' interleaved.
///
/// The ceiling filters on `h.sensitivity`, the level the row carried while it was current, and not
/// on the live row's. The live row may be reclassified, or gone: the archive outlives a delete on
/// purpose, so there is often no current row to ask. A value written at private and replaced by one
/// at open stays private here.
///
/// The exact arm losing to the redirect when the ceiling empties it is the same property `GET_SQL`
/// has. Both arms filter, so the fallthrough reveals nothing the caller could not read directly.
const HISTORY_SQL: &str = r#"
    WITH candidate AS (
        SELECT h.registry_id, h.namespace, h.kind, h.key, h.value, h.provenance, h.sensitivity,
               h.version, h.replaced_at, NULL::text AS resolved_from, 0 AS preference
          FROM registry_history h
         WHERE h.tenant_id = $1 AND h.namespace = $2 AND h.kind = $4 AND h.key = $5
           AND sensitivity_rank(h.sensitivity) <= sensitivity_rank($3)
        UNION ALL
        SELECT h.registry_id, h.namespace, h.kind, h.key, h.value, h.provenance, h.sensitivity,
               h.version, h.replaced_at, a.alias_key AS resolved_from, 1 AS preference
          FROM registry_alias a
          JOIN registry_history h
            ON h.tenant_id = a.tenant_id AND h.namespace = a.namespace
           AND h.kind = a.kind AND h.key = a.canonical
         WHERE a.tenant_id = $1 AND a.namespace = $2 AND a.kind = $4 AND a.alias_key = $5
           AND sensitivity_rank(h.sensitivity) <= sensitivity_rank($3)
    ),
    answering AS (SELECT min(preference) AS preference FROM candidate)
    SELECT c.registry_id, c.namespace, c.kind, c.key, c.value, c.provenance, c.sensitivity,
           c.version, c.replaced_at, c.resolved_from
      FROM candidate c
      JOIN answering w ON c.preference = w.preference
     ORDER BY c.replaced_at DESC, c.version DESC
     LIMIT $6
"#;

/// The write, with the overwrite guard in the statement.
///
/// `$9` is the caller's replace ceiling. A row stored above it is not touched, which keeps an
/// open-ceiling writer from declassifying a private slot and from learning its version. The guard
/// sits on the conflict arm only: an insert into an empty slot has nothing to overwrite.
const UPSERT_SQL: &str = r#"
    INSERT INTO registry (tenant_id, namespace, kind, key, value, provenance,
                          sensitivity, review_after)
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
    ON CONFLICT (tenant_id, namespace, kind, key)
    DO UPDATE SET value        = EXCLUDED.value,
                  provenance   = EXCLUDED.provenance,
                  sensitivity  = EXCLUDED.sensitivity,
                  review_after = EXCLUDED.review_after,
                  version      = registry.version + 1
          WHERE sensitivity_rank(registry.sensitivity) <= sensitivity_rank($9)
    RETURNING id, version
"#;

fn entry_from_row(r: &sqlx::postgres::PgRow) -> RegistryEntry {
    RegistryEntry {
        namespace: r.get("namespace"),
        kind: r.get("kind"),
        key: r.get("key"),
        value: r.get::<serde_json::Value, _>("value"),
        provenance: serde_json::from_value(r.get::<serde_json::Value, _>("provenance"))
            .unwrap_or_default(),
        // An unrecognised level reads as the most restrictive. The queries filter on
        // sensitivity_rank, which admits an unknown level under no ceiling, so this is the second
        // line of the same rule rather than the only one.
        sensitivity: Sensitivity::parse(r.get::<&str, _>("sensitivity"))
            .unwrap_or(Sensitivity::Sealed),
        version: r.get("version"),
        // Present only on the read that resolves aliases. A redirect the caller can see is the
        // difference between preventing a naming mess and cleaning one up later.
        resolved_from: r.try_get::<Option<String>, _>("resolved_from").ok().flatten(),
    }
}

fn version_from_row(r: &sqlx::postgres::PgRow) -> RegistryVersion {
    RegistryVersion {
        registry_id: r.get::<uuid::Uuid, _>("registry_id").to_string(),
        namespace: r.get("namespace"),
        kind: r.get("kind"),
        key: r.get("key"),
        value: r.get::<serde_json::Value, _>("value"),
        provenance: serde_json::from_value(r.get::<serde_json::Value, _>("provenance"))
            .unwrap_or_default(),
        // Same rule as `entry_from_row`: a level this build does not recognise reads as the most
        // restrictive. The archive carries no CHECK constraint, so an old row can hold a word a
        // later vocabulary dropped.
        sensitivity: Sensitivity::parse(r.get::<&str, _>("sensitivity"))
            .unwrap_or(Sensitivity::Sealed),
        version: r.get("version"),
        replaced_at: r.get::<chrono::DateTime<chrono::Utc>, _>("replaced_at"),
        resolved_from: r.try_get::<Option<String>, _>("resolved_from").ok().flatten(),
    }
}

/// Namespaces and ceilings as two parallel arrays, the only form the `unnest` join takes.
fn split_ceilings(ceilings: &[NamespaceCeiling]) -> (Vec<String>, Vec<String>) {
    let mut namespaces = Vec::with_capacity(ceilings.len());
    let mut maxima = Vec::with_capacity(ceilings.len());
    for c in ceilings {
        namespaces.push(c.namespace.clone());
        maxima.push(c.max.as_str().to_string());
    }
    (namespaces, maxima)
}

#[async_trait]
impl RegistryRepository for PgRegistryRepository {
    async fn get(
        &self,
        tenant: &str,
        namespace: &str,
        max_sensitivity: Sensitivity,
        kind: &str,
        key: &str,
    ) -> Result<Option<RegistryEntry>> {
        let row = sqlx::query(GET_SQL)
            .bind(tenant)
            .bind(namespace)
            .bind(max_sensitivity.as_str())
            .bind(kind)
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(entry_from_row))
    }

    /// The version is the audit trail: a fact rewritten five times says so, and the provenance of
    /// the current value is the provenance of the write that set it.
    ///
    /// The value this statement overwrites survives in `registry_history`, written by the
    /// `registry_archive` trigger from migration `20260821000012`. The archive sits in the database
    /// rather than in this statement so a writer that is not this adapter, an owner in psql above
    /// all, cannot replace a value without leaving the old one behind. Read that migration before
    /// changing anything here: the trigger is the reason this can stay one statement.
    ///
    /// The `WHERE` on the `DO UPDATE` is the overwrite guard. A stored level above the caller's
    /// replace ceiling makes the update a no-op, `RETURNING` yields no row, and that absence is
    /// the refusal. `fetch_optional` rather than `fetch_one` for that reason: the guard biting is
    /// an answer, not a missing row.
    async fn upsert(&self, w: RegistryWrite) -> Result<RegistryUpsert> {
        let row = sqlx::query(UPSERT_SQL)
            .bind(&w.tenant_id)
            .bind(&w.namespace)
            .bind(&w.kind)
            .bind(&w.key)
            .bind(&w.value)
            .bind(serde_json::to_value(&w.provenance).unwrap_or(serde_json::Value::Null))
            .bind(w.sensitivity.as_str())
            .bind(w.review_after)
            .bind(w.replace_ceiling.as_str())
            .fetch_optional(&self.pool)
            .await?;

        Ok(match row {
            Some(row) => RegistryUpsert::Written {
                id: row.get::<uuid::Uuid, _>("id").to_string(),
                version: row.get("version"),
            },
            None => RegistryUpsert::Refused,
        })
    }

    /// The reader for what the trigger has been writing since migration `20260821000012`.
    ///
    /// `replaced_at` defaults to `now()`, which is transaction start time, so two upserts to one key
    /// inside one transaction land on the same timestamp. The version breaks that tie, and without
    /// it the pair comes back in whatever order the scan produced.
    async fn history(
        &self,
        tenant: &str,
        namespace: &str,
        max_sensitivity: Sensitivity,
        kind: &str,
        key: &str,
        limit: i64,
    ) -> Result<Vec<RegistryVersion>> {
        let rows = sqlx::query(HISTORY_SQL)
            .bind(tenant)
            .bind(namespace)
            .bind(max_sensitivity.as_str())
            .bind(kind)
            .bind(key)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(version_from_row).collect())
    }

    /// A rejected guess becomes a redirect. That is what makes rejection survivable: a model handed
    /// a bare error invents a third variant rather than the canonical name.
    async fn add_alias(
        &self,
        tenant: &str,
        namespace: &str,
        kind: &str,
        alias_key: &str,
        canonical_key: &str,
        origin: AliasOrigin,
    ) -> Result<()> {
        if alias_key == canonical_key {
            return Err(DomainError::validation("an alias cannot point at itself"));
        }
        // An inferred redirect must not overwrite a hand-written one. The owner mapping a key by
        // hand is a decision; a model's rejected guess is a guess, and the guess losing is correct.
        sqlx::query(
            "INSERT INTO registry_alias (tenant_id, namespace, kind, alias_key, canonical, origin)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (tenant_id, namespace, kind, alias_key)
             DO UPDATE SET canonical = EXCLUDED.canonical, origin = EXCLUDED.origin
                     WHERE EXCLUDED.origin <> 'rejected-write'",
        )
        .bind(tenant)
        .bind(namespace)
        .bind(kind)
        .bind(alias_key)
        .bind(canonical_key)
        .bind(origin.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn resolve_alias(
        &self,
        tenant: &str,
        namespace: &str,
        kind: &str,
        alias_key: &str,
    ) -> Result<Option<String>> {
        let canonical_key = sqlx::query_scalar::<_, String>(
            "SELECT canonical FROM registry_alias
              WHERE tenant_id = $1 AND namespace = $2 AND kind = $3 AND alias_key = $4",
        )
        .bind(tenant)
        .bind(namespace)
        .bind(kind)
        .bind(alias_key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(canonical_key)
    }

    /// Everything the caller may read, with the ceiling applied per namespace rather than once for
    /// the whole list: work notes and personal finance can both be private while a work agent may
    /// see one and never the other.
    async fn list(
        &self,
        tenant: &str,
        readable: &[NamespaceCeiling],
    ) -> Result<Vec<RegistryEntry>> {
        if readable.is_empty() {
            return Ok(vec![]);
        }
        let (namespaces, maxima) = split_ceilings(readable);
        let rows = sqlx::query(
            "SELECT r.namespace, r.kind, r.key, r.value, r.provenance, r.sensitivity, r.version
               FROM registry r
               JOIN (
                     SELECT namespace, min(sensitivity_rank(max)) AS max_rank
                       FROM unnest($2::text[], $3::text[]) AS g(namespace, max)
                      GROUP BY namespace
                    ) rg ON rg.namespace = r.namespace
              WHERE r.tenant_id = $1
                AND sensitivity_rank(r.sensitivity) <= rg.max_rank
              ORDER BY r.namespace, r.kind, r.key",
        )
        .bind(tenant)
        .bind(&namespaces)
        .bind(&maxima)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(entry_from_row).collect())
    }

    /// Aliases pointing at the deleted key are left alone. A dangling redirect resolves to nothing,
    /// which reads as "no such fact"; deleting the redirect as well would lose the record that the
    /// name was ever in use, and that record is why the alias table exists.
    async fn delete(&self, tenant: &str, namespace: &str, kind: &str, key: &str) -> Result<bool> {
        let done = sqlx::query(
            "DELETE FROM registry
              WHERE tenant_id = $1 AND namespace = $2 AND kind = $3 AND key = $4",
        )
        .bind(tenant)
        .bind(namespace)
        .bind(kind)
        .bind(key)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(done > 0)
    }

    /// Past its review date. Marked for a human, never expired automatically: a host entry ages
    /// slowly and a model route ages fast, and neither becomes false on a schedule.
    async fn due_for_review(
        &self,
        tenant: &str,
        limit: i64,
        reader: &[NamespaceGrant],
    ) -> Result<Vec<RegistryEntry>> {
        let (g_prefix, g_exact, g_max) = crate::adapters::postgres::cleanup::grant_arrays(reader);
        let rows = sqlx::query(
            "SELECT namespace, kind, key, value, provenance, sensitivity, version
               FROM registry
              WHERE tenant_id = $1 AND review_after IS NOT NULL AND review_after < now()
                AND EXISTS (
                      SELECT 1
                        FROM unnest($3::text[], $4::bool[], $5::text[]) AS g(prefix, exact, max)
                       WHERE CASE WHEN g.exact THEN registry.namespace = g.prefix
                                  ELSE left(registry.namespace, length(g.prefix)) = g.prefix END
                         AND sensitivity_rank(g.max) >= sensitivity_rank(registry.sensitivity)
                    )
              ORDER BY review_after ASC
              LIMIT $2",
        )
        .bind(tenant)
        .bind(limit)
        .bind(&g_prefix)
        .bind(&g_exact)
        .bind(&g_max)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(entry_from_row).collect())
    }

    /// Keys that would be rejected if they were written today, for the one-time hand migration.
    ///
    /// Judged in Rust rather than in SQL. The scheme is a closed vocabulary, a typo distance and a
    /// segment count; a SQL approximation of it would disagree with the rule the write path applies,
    /// and a migration list that disagrees with the validator is worse than no list. No ceiling
    /// argument: this is an operator command over the owner's own store, and a key it could not see
    /// is a key that would never get migrated.
    async fn non_canonical(&self, tenant: &str) -> Result<Vec<RegistryEntry>> {
        let rows = sqlx::query(
            "SELECT namespace, kind, key, value, provenance, sensitivity, version
               FROM registry WHERE tenant_id = $1 ORDER BY namespace, kind, key",
        )
        .bind(tenant)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(entry_from_row).filter(|e| !canonical::is_canonical(&e.key)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_alias_read_prefers_the_key_that_was_asked_for() {
        // The exact-key arm carries preference 0 and the redirect arm 1, and the statement orders
        // on it: an alias can never hide a row stored under the name the caller used.
        let exact = GET_SQL.find("0 AS preference").expect("exact arm");
        let alias = GET_SQL.find("1 AS preference").expect("redirect arm");
        assert!(exact < alias);
        assert!(GET_SQL.contains("ORDER BY preference LIMIT 1"));
    }

    #[test]
    fn both_arms_of_the_alias_read_apply_the_ceiling() {
        assert_eq!(
            GET_SQL.matches("sensitivity_rank(r.sensitivity) <= sensitivity_rank($3)").count(),
            2
        );
    }

    #[test]
    fn both_arms_of_the_history_read_apply_the_ceiling() {
        // The one edit that would turn this read into a leak is dropping the filter from the
        // redirect arm, where it is easy to miss because the alias table carries no sensitivity of
        // its own. A history row can name a vault the value that replaced it no longer names.
        assert_eq!(
            HISTORY_SQL.matches("sensitivity_rank(h.sensitivity) <= sensitivity_rank($3)").count(),
            2
        );
    }

    #[test]
    fn the_history_read_filters_on_the_archived_level() {
        // Joining to `registry` for a live sensitivity would declassify by reclassification, and
        // would answer nothing at all for a key that has since been deleted.
        assert!(!HISTORY_SQL.contains("JOIN registry r"));
        assert!(!HISTORY_SQL.contains("sensitivity_rank(r.sensitivity)"));
    }

    #[test]
    fn the_history_read_prefers_the_key_that_was_asked_for() {
        let exact = HISTORY_SQL.find("0 AS preference").expect("exact arm");
        let alias = HISTORY_SQL.find("1 AS preference").expect("redirect arm");
        assert!(exact < alias);
        // One arm answers. Joining on the winning preference rather than taking a global LIMIT is
        // what stops two keys' versions interleaving into one timeline that never existed.
        assert!(HISTORY_SQL.contains("min(preference) AS preference"));
        assert!(HISTORY_SQL.contains("JOIN answering w ON c.preference = w.preference"));
    }

    #[test]
    fn history_comes_back_newest_first_and_bounded() {
        assert!(HISTORY_SQL.contains("ORDER BY c.replaced_at DESC, c.version DESC"));
        // Bound, never interpolated. A limit is the one number a caller controls here.
        assert!(HISTORY_SQL.contains("LIMIT $6"));
    }

    #[test]
    fn the_history_read_stays_inside_one_tenant() {
        assert_eq!(HISTORY_SQL.matches("h.tenant_id = $1").count(), 1);
        assert_eq!(HISTORY_SQL.matches("a.tenant_id = $1").count(), 1);
        assert!(HISTORY_SQL.contains("h.tenant_id = a.tenant_id"));
    }

    #[test]
    fn a_ceiling_list_becomes_two_arrays_in_the_same_order() {
        let ceilings = vec![
            NamespaceCeiling { namespace: "credentials:aws".into(), max: Sensitivity::Sealed },
            NamespaceCeiling { namespace: "global".into(), max: Sensitivity::Open },
        ];
        let (namespaces, maxima) = split_ceilings(&ceilings);
        assert_eq!(namespaces, vec!["credentials:aws", "global"]);
        assert_eq!(maxima, vec!["sealed", "open"]);
    }

    /// The archive is a trigger, so nothing in this file fails when it is dropped. These read the
    /// migration text instead. They are cheap and they cover the three edits that would turn the
    /// stop-loss back into silent loss.
    const HISTORY_MIGRATION: &str =
        include_str!("../../../migrations/20260821000012_registry_history.sql");

    #[test]
    fn the_archive_fires_after_every_update_of_a_registry_row() {
        assert!(HISTORY_MIGRATION.contains("AFTER UPDATE ON registry"));
        // A WHEN clause would let an UPDATE that leaves the version alone destroy a value without
        // recording it, which is the failure this table exists to prevent.
        assert!(!HISTORY_MIGRATION.contains("WHEN ("));
    }

    #[test]
    fn the_archive_leaves_deletes_alone() {
        // Deleting a registry row is the owner asking for a credential location to be gone. The
        // full history design decides whether that archives; this migration must not decide it by
        // accident.
        assert!(!HISTORY_MIGRATION.contains("OR DELETE"));
        assert!(!HISTORY_MIGRATION.contains("AFTER DELETE"));
        assert!(!HISTORY_MIGRATION.contains("BEFORE DELETE"));
    }

    #[test]
    fn the_history_table_outlives_the_rows_it_records() {
        // A foreign key back to `registry` either cascades the history away or blocks the delete.
        assert!(!HISTORY_MIGRATION.contains("REFERENCES registry"));
        assert!(HISTORY_MIGRATION.contains("registry_history_key"));
        assert!(HISTORY_MIGRATION.contains("replaced_at DESC"));
    }

    #[test]
    fn the_write_refuses_to_replace_a_row_stored_above_the_callers_ceiling() {
        assert!(UPSERT_SQL
            .contains("WHERE sensitivity_rank(registry.sensitivity) <= sensitivity_rank($9)"));
        // The guard belongs to the update arm. On the insert arm there is no stored level to
        // compare and the clause would refuse every first write.
        let guard = UPSERT_SQL.find("WHERE sensitivity_rank").unwrap();
        assert!(UPSERT_SQL.find("DO UPDATE").unwrap() < guard);
    }

    const HISTORY_LEVEL_MIGRATION: &str =
        include_str!("../../../migrations/20260823000020_registry_history_level.sql");

    #[test]
    fn the_archive_records_a_replaced_value_at_the_higher_of_the_two_levels() {
        // Raising a key's level must not leave its previous value readable at the old one: the
        // value the owner just classified is, more often than not, the value that was there.
        assert!(HISTORY_LEVEL_MIGRATION
            .contains("sensitivity_rank(OLD.sensitivity) >= sensitivity_rank(NEW.sensitivity)"));
        assert!(HISTORY_LEVEL_MIGRATION.contains("UPDATE registry_history"));
        assert!(HISTORY_LEVEL_MIGRATION
            .contains("CREATE OR REPLACE FUNCTION registry_archive_old_value()"));
        assert!(!HISTORY_LEVEL_MIGRATION.contains("WHEN ("));
    }

    #[test]
    fn the_archived_row_carries_the_version_it_was() {
        // Without the old version a history row cannot be placed in the sequence, and "value at
        // version 3" is the question this table answers.
        assert!(HISTORY_MIGRATION.contains("OLD.version"));
        assert!(HISTORY_MIGRATION.contains("OLD.value"));
        assert!(HISTORY_MIGRATION.contains("OLD.provenance"));
        assert!(HISTORY_MIGRATION.contains("OLD.sensitivity"));
    }
}
