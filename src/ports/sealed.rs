//! Sealed items. Client-encrypted blobs this server cannot read, by construction.
//!
//! Keyed by an HMAC of the canonical name computed client-side, so the server cannot enumerate what
//! is stored either. Not searchable, retrievable only by exact key. That is the level's whole point
//! and pretending otherwise would be the failure mode.

use async_trait::async_trait;

use crate::domain::errors::Result;
use crate::domain::types::SealedItem;

#[async_trait]
pub trait SealedRepository: Send + Sync {
    async fn put(
        &self,
        tenant: &str,
        namespace: &str,
        key_hmac: &str,
        ciphertext: &[u8],
        alg: &str,
        source_client: &str,
    ) -> Result<()>;

    /// Exact key only, within the namespaces the caller may reach.
    async fn get(
        &self,
        tenant: &str,
        namespaces: &[String],
        key_hmac: &str,
    ) -> Result<Option<SealedItem>>;

    /// Deleting the row removes the only copy: the server cannot help recover it.
    async fn delete(&self, tenant: &str, namespace: &str, key_hmac: &str) -> Result<bool>;

    /// Count per namespace, for the digest inventory. The count is all that can honestly be shown.
    async fn counts(&self, tenant: &str, namespaces: &[String]) -> Result<Vec<(String, i64)>>;

    /// Namespaces holding at least one sealed item.
    ///
    /// The digest builds its readable set from the namespaces the memory table knows, and a
    /// `credentials:*` namespace holds sealed items and nothing else, so it never appeared there and
    /// the sealed inventory never asked about it. Names only, and the caller still applies the
    /// sealed ceiling before showing any of them: a client that cannot reach a namespace must not
    /// learn it exists.
    async fn namespaces(&self, tenant: &str) -> Result<Vec<String>>;
}
