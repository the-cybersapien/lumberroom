//! The Postgres adapter. The only module in the service that contains SQL.

mod alias;
mod cleanup;
mod ingest;
mod memory;
mod oauth;
mod registry;
mod sealed;
mod tool_calls;

pub use alias::PgAliasRepository;
pub use cleanup::PgCleanupRepository;
pub use ingest::PgIngestRepository;
pub use memory::PgMemoryRepository;
pub use oauth::PgOauthStore;
pub use registry::PgRegistryRepository;
pub use sealed::PgSealedRepository;
pub use tool_calls::PgToolCallRepository;

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use std::sync::Arc;
use std::time::Duration;

use crate::domain::errors::{DomainError, Result};
use crate::ports::{
    CleanupRepository, IngestRepository, MemoryRepository, OauthStore, RegistryRepository,
    SealedRepository, ToolCallRepository,
};

#[derive(Clone)]
pub struct Repositories {
    pub memories: Arc<dyn MemoryRepository>,
    pub registry: Arc<dyn RegistryRepository>,
    pub tool_calls: Arc<dyn ToolCallRepository>,
    pub sealed: Arc<dyn SealedRepository>,
    pub oauth: Arc<dyn OauthStore>,
    /// Held beside the rest rather than inside `services::Repos`: ingestion is an operator surface
    /// with no tool behind it, and the services that use it take the port as an argument.
    pub ingest: Arc<dyn IngestRepository>,
    /// Same reasoning as `ingest`, and the same shape: a queue an operator reads, no tool, and the
    /// service takes the port as an argument.
    pub cleanup: Arc<dyn CleanupRepository>,
}

pub async fn connect(database_url: &str) -> Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        // A pathological query must not hold a connection until the client gives up.
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                sqlx::query("SET statement_timeout = '30s'").execute(conn).await?;
                Ok(())
            })
        })
        .connect(database_url)
        .await
        .map_err(|e| DomainError::unavailable("cannot reach the database").with_source(e))
}

pub fn repositories(pool: &PgPool, search: &crate::config::SearchConfig) -> Repositories {
    Repositories {
        memories: Arc::new(PgMemoryRepository::new(pool.clone()).with_search(search)),
        registry: Arc::new(PgRegistryRepository::new(pool.clone())),
        tool_calls: Arc::new(PgToolCallRepository::new(pool.clone())),
        sealed: Arc::new(PgSealedRepository::new(pool.clone())),
        oauth: Arc::new(PgOauthStore::new(pool.clone())),
        ingest: Arc::new(PgIngestRepository::new(pool.clone())),
        cleanup: Arc::new(PgCleanupRepository::new(pool.clone())),
    }
}

pub async fn migrate(pool: &PgPool) -> Result<()> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .map_err(|e| DomainError::internal("migration failed").with_source(e))?;
    Ok(())
}

/// What the boot check found in `kek_state`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KekCheck {
    /// Nothing was recorded, and this key's id and fingerprint now are. First boot with encryption
    /// configured, and the only state in which an unrecognised key is not a problem.
    Recorded,
    /// The recorded fingerprint is this key's. Encrypted rows already written can be read.
    Matches,
    /// A different key. Every row already sealed was sealed under the recorded one, so encrypted
    /// writes have to stay refused: writing under this key would produce rows nobody can read and
    /// leave the store holding two families of ciphertext with one label.
    Mismatch { recorded_kek_id: String },
}

/// Compare the live KEK against the one this store was built with, and record it on first sight.
///
/// Step 4 of the Phase 3 migration order is the one that can strand data, which is why it is a
/// stored row rather than a note in a runbook: the fingerprint is an HMAC of a fixed label under the
/// key, so it identifies the key without being derived from it in a way that helps an attacker.
///
/// The comparison is a plain equality. The fingerprint is computed locally and compared against a
/// local row with no attacker-supplied input on either side, so there is no secret-dependent timing
/// to hide here, unlike the presented-token comparison in `adapters::auth::token`.
///
/// SQL for the KEK state lives in this module because all SQL lives in this module. The composition
/// root calls this at boot and sets `services::Ctx::kek_verified` from the answer.
pub async fn verify_kek(
    pool: &PgPool,
    tenant: &str,
    kek_id: &str,
    fingerprint: &str,
    provider: &str,
) -> Result<KekCheck> {
    let recorded = sqlx::query("SELECT kek_id, fingerprint FROM kek_state WHERE tenant_id = $1")
        .bind(tenant)
        .fetch_optional(pool)
        .await?;

    let Some(row) = recorded else {
        // Boot is single-process, so this is not a race in practice. ON CONFLICT DO NOTHING keeps
        // it from being one anyway, and a losing insert falls through to the next boot's compare.
        sqlx::query(
            "INSERT INTO kek_state (tenant_id, kek_id, fingerprint, provider)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (tenant_id) DO NOTHING",
        )
        .bind(tenant)
        .bind(kek_id)
        .bind(fingerprint)
        .bind(provider)
        .execute(pool)
        .await?;
        return Ok(KekCheck::Recorded);
    };

    let stored_fingerprint: String = row.get("fingerprint");
    if stored_fingerprint != fingerprint {
        return Ok(KekCheck::Mismatch { recorded_kek_id: row.get("kek_id") });
    }

    // Same key, so the id and provider are labels for it and may have been renamed in config
    // between boots. verified_at moves, which is what makes "when did this last work" answerable.
    sqlx::query(
        "UPDATE kek_state SET kek_id = $2, provider = $3, verified_at = now()
          WHERE tenant_id = $1",
    )
    .bind(tenant)
    .bind(kek_id)
    .bind(provider)
    .execute(pool)
    .await?;
    Ok(KekCheck::Matches)
}

/// The operator-editable classification table, read once at boot.
///
/// Read here rather than through `RegistryRepository`: this is policy, not registry data, and a
/// request-path port is the wrong home for a question asked once per process. It sits beside
/// `verify_kek` for the same reason that one does. The SQL stays in this module because all SQL
/// stays in this module.
///
/// Returned unsorted and unresolved. `SensitivityDefaults::new` orders longest-pattern-first, and
/// `config::resolve_sensitivity_defaults` decides whether these rows win at all.
pub async fn sensitivity_defaults(
    pool: &PgPool,
    tenant: &str,
) -> Result<Vec<(String, crate::domain::types::Sensitivity)>> {
    use crate::domain::types::Sensitivity;

    let rows =
        sqlx::query("SELECT pattern, sensitivity FROM sensitivity_default WHERE tenant_id = $1")
            .bind(tenant)
            .fetch_all(pool)
            .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let pattern: String = row.get("pattern");
        let level: String = row.get("sensitivity");
        match Sensitivity::parse(&level) {
            Some(level) => out.push((pattern, level)),
            // A CHECK constraint on the column forbids this, so reaching it means the constraint was
            // dropped. Skip the row and say so rather than refusing to boot: one unreadable rule
            // must not lock the operator out of the store it classifies.
            None => tracing::warn!(
                pattern = %pattern,
                level = %level,
                "sensitivity_default row has an unknown level; ignoring it"
            ),
        }
    }
    Ok(out)
}

/// The embedding column width is fixed in SQL and the embedder is configurable, so a mismatch
/// would produce a confusing error on every write. Fail loudly at boot instead.
pub async fn assert_embedding_dim(pool: &PgPool, expected: usize) -> Result<usize> {
    let row = sqlx::query(
        "SELECT format_type(a.atttypid, a.atttypmod) AS type
           FROM pg_attribute a
           JOIN pg_class c ON c.oid = a.attrelid
          WHERE c.relname = 'memory' AND a.attname = 'embedding' AND a.attnum > 0",
    )
    .fetch_optional(pool)
    .await?;

    let ty: String = row
        .ok_or_else(|| {
            DomainError::internal("memory.embedding column not found — did migrations run?")
        })?
        .get("type");

    let actual: usize =
        ty.trim_start_matches("vector(").trim_end_matches(')').parse().map_err(|_| {
            DomainError::internal(format!("cannot read embedding dimension from {ty:?}"))
        })?;

    if actual != expected {
        return Err(DomainError::internal(format!(
            "embedding dimension mismatch: memory.embedding is {ty} but EMBED_DIM={expected}. \
             Change EMBED_DIM back, or migrate the column."
        )));
    }
    Ok(actual)
}
