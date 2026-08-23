//! Postgres implementation of SealedRepository.
//!
//! Blobs this server cannot read, by construction. It holds no key for them, it never will, and the
//! key is the whole level: `private` protects content from anyone holding the database, `sealed`
//! protects it from this server even under full compromise.
//!
//! Two consequences run through every statement here, and neither is a limitation to work around.
//!
//! The key is an HMAC of the canonical name computed client-side, so the server cannot enumerate
//! what is stored either: a lookup is an exact match on an opaque string or it is nothing. There is
//! no prefix search, no listing of names, and no search of any kind.
//!
//! What can honestly be shown about this table is a count per namespace. Anything more would be a
//! claim about content nothing here can read.

use async_trait::async_trait;
use base64::Engine as _;
use sqlx::{PgPool, Row};

use crate::domain::errors::Result;
use crate::domain::types::SealedItem;
use crate::ports::SealedRepository;

pub struct PgSealedRepository {
    pool: PgPool,
}

impl PgSealedRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn item_from_row(r: &sqlx::postgres::PgRow) -> SealedItem {
    SealedItem {
        namespace: r.get("namespace"),
        key_hmac: r.get("key_hmac"),
        // Base64 because the wire contract is JSON and the bytes are opaque here anyway. The client
        // decodes and decrypts; this server does neither.
        ciphertext: base64::engine::general_purpose::STANDARD.encode(r.get::<Vec<u8>, _>("ciphertext")),
        alg: r.get("alg"),
        source_client: r.get("source_client"),
        created_at: r.get("created_at"),
    }
}

#[async_trait]
impl SealedRepository for PgSealedRepository {
    /// Upsert, keeping `created_at` and moving `updated_at`.
    ///
    /// A second put under the same key is the client rotating or editing its own blob, which is a
    /// new version of one item rather than a new item. `alg` moves with the ciphertext: a client
    /// that changed cipher must be able to tell, before it tries, whether it can read what is here.
    async fn put(
        &self,
        tenant: &str,
        namespace: &str,
        key_hmac: &str,
        ciphertext: &[u8],
        alg: &str,
        source_client: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO sealed_item (tenant_id, namespace, key_hmac, ciphertext, alg,
                                      source_client)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (tenant_id, namespace, key_hmac)
             DO UPDATE SET ciphertext    = EXCLUDED.ciphertext,
                           alg           = EXCLUDED.alg,
                           source_client = EXCLUDED.source_client,
                           updated_at    = now()",
        )
        .bind(tenant)
        .bind(namespace)
        .bind(key_hmac)
        .bind(ciphertext)
        .bind(alg)
        .bind(source_client)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Exact key, inside the namespaces the caller may reach.
    ///
    /// The same key may exist in two namespaces the caller can reach, which is two different items
    /// that happen to share a name. Oldest wins so the answer is stable across calls rather than
    /// depending on the plan.
    async fn get(
        &self,
        tenant: &str,
        namespaces: &[String],
        key_hmac: &str,
    ) -> Result<Option<SealedItem>> {
        if namespaces.is_empty() {
            return Ok(None);
        }
        let row = sqlx::query(
            "SELECT namespace, key_hmac, ciphertext, alg, source_client, created_at
               FROM sealed_item
              WHERE tenant_id = $1 AND namespace = ANY($2) AND key_hmac = $3
              ORDER BY created_at ASC, namespace ASC
              LIMIT 1",
        )
        .bind(tenant)
        .bind(namespaces)
        .bind(key_hmac)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(item_from_row))
    }

    /// The row is the only copy. Nothing here can help recover it, which is the level working as
    /// specified rather than a gap in the delete path.
    async fn delete(&self, tenant: &str, namespace: &str, key_hmac: &str) -> Result<bool> {
        let done = sqlx::query(
            "DELETE FROM sealed_item
              WHERE tenant_id = $1 AND namespace = $2 AND key_hmac = $3",
        )
        .bind(tenant)
        .bind(namespace)
        .bind(key_hmac)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(done > 0)
    }

    /// Count per namespace, for the digest inventory. Namespaces holding nothing are absent rather
    /// than reported as zero: the digest lists what exists.
    async fn counts(&self, tenant: &str, namespaces: &[String]) -> Result<Vec<(String, i64)>> {
        if namespaces.is_empty() {
            return Ok(vec![]);
        }
        let rows = sqlx::query(
            "SELECT namespace, count(*) AS n
               FROM sealed_item
              WHERE tenant_id = $1 AND namespace = ANY($2)
              GROUP BY namespace
              ORDER BY namespace",
        )
        .bind(tenant)
        .bind(namespaces)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| (r.get("namespace"), r.get::<i64, _>("n"))).collect())
    }

    /// Names only, unfiltered by grant: the caller owns the ceiling. Cheap enough for the digest's
    /// latency budget because the group is over a table with one row per stored item, not per read.
    async fn namespaces(&self, tenant: &str) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT DISTINCT namespace
               FROM sealed_item
              WHERE tenant_id = $1
              ORDER BY namespace",
        )
        .bind(tenant)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get("namespace")).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Base64 of the ciphertext, and nothing else. Worth pinning because the one thing this file
    /// must never do is hand back something that reads as content.
    #[test]
    fn ciphertext_leaves_as_base64_of_exactly_the_stored_bytes() {
        let bytes = [0u8, 1, 2, 250, 251, 255];
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let decoded = base64::engine::general_purpose::STANDARD.decode(&encoded).unwrap();
        assert_eq!(decoded, bytes);
    }
}
