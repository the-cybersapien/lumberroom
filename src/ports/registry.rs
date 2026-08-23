//! The registry: exact structured facts with a canonical key and provenance.
//!
//! Not "the box runs Ubuntu" recalled fuzzily, but `machines.desktop.os = Ubuntu 26.04`, confirmed,
//! on a date, superseding an older value. Aliases are part of the port because a rejected key that
//! becomes a redirect is the difference between preventing a mess and cleaning one up.

use async_trait::async_trait;
use serde::Serialize;

use crate::domain::errors::Result;
use crate::domain::policy::NamespaceCeiling;
use crate::domain::types::{Provenance, RegistryEntry, Sensitivity};

#[derive(Debug, Clone)]
pub struct RegistryWrite {
    pub tenant_id: String,
    pub namespace: String,
    pub kind: String,
    pub key: String,
    pub value: serde_json::Value,
    pub provenance: Provenance,
    pub sensitivity: Sensitivity,
    /// Per-kind expectation of how fast this fact goes stale. A host ages slowly; a model route ages
    /// fast because routing preferences change monthly. Expiry marks a row for review, never removes it.
    pub review_after: Option<chrono::DateTime<chrono::Utc>>,
}

/// A value this key used to hold, and the moment it stopped holding it.
///
/// Every field is the row as it was when it was current, including `sensitivity`. A key
/// reclassified from private to open does not declassify what it held before, so a reader's ceiling
/// is checked against the archived level rather than the level the live row carries today.
#[derive(Debug, Clone, Serialize)]
pub struct RegistryVersion {
    /// The live row this value belonged to. A key deleted and written again gets a new id under the
    /// same name, which is the one signal that tells a rewrite apart from a fresh start.
    pub registry_id: String,
    pub namespace: String,
    pub kind: String,
    pub key: String,
    pub value: serde_json::Value,
    pub provenance: Provenance,
    pub sensitivity: Sensitivity,
    /// The version this value was, not the version that replaced it.
    pub version: i32,
    pub replaced_at: chrono::DateTime<chrono::Utc>,
    /// Set when the key asked for was an alias, the same way `get` reports a redirect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_from: Option<String>,
}

/// Where the alias came from, so a hand-written mapping is distinguishable from one the server
/// inferred after refusing a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasOrigin {
    Manual,
    RejectedWrite,
    Migration,
}

impl AliasOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::RejectedWrite => "rejected-write",
            Self::Migration => "migration",
        }
    }
}

#[async_trait]
pub trait RegistryRepository: Send + Sync {
    /// Resolves an alias transparently and reports which key answered, so a redirect is visible.
    /// The ceiling is applied in the query, per namespace, the same as every other read path.
    async fn get(
        &self,
        tenant: &str,
        namespace: &str,
        max_sensitivity: Sensitivity,
        kind: &str,
        key: &str,
    ) -> Result<Option<RegistryEntry>>;

    /// Returns the row id and the version this write produced.
    ///
    /// An implementation owes the caller one thing beyond the write: the value it replaces has to
    /// survive somewhere, in the same transaction as the replacement. A registry value overwritten
    /// in place is the one loss in this system with nothing left on disk, and a partial archive that
    /// can fail on its own is worse than none, because the gaps are undetectable. Postgres does it
    /// with an append-only table and a trigger. The contract here is the durability; an
    /// implementation picks its own mechanism. Read it back with `history`.
    async fn upsert(&self, w: RegistryWrite) -> Result<(String, i32)>;

    /// What this key used to hold, newest first, at most `limit` rows.
    ///
    /// Replaced values only. The current value is a `get` away, and an implementation archives on
    /// replacement, so the live value is absent here by construction rather than by a filter.
    ///
    /// Aliases resolve, with the exact key preferred, the same rule `get` follows. A caller that
    /// read a value through a redirect and then asked what it used to be must not be told nothing
    /// is known about a key that answered a moment ago. One key answers, never a merge of two: the
    /// exact key wins when it has rows the caller may see, otherwise the redirect answers and
    /// `resolved_from` says so. An alias can therefore never hide history filed under the name the
    /// caller used.
    ///
    /// `max_sensitivity` is the caller's ceiling for `namespace` and it filters inside the query.
    /// The registry holds credential locations, and a location that was replaced is the shape most
    /// worth hiding: the value that replaced it may sit at open while the one before it named a
    /// vault the caller has no grant over.
    ///
    /// Retired values are the whole content of the answer, so a caller checks
    /// `Principal::may_read_history` before asking. Nothing below this line can: a repository holds
    /// no principal.
    async fn history(
        &self,
        tenant: &str,
        namespace: &str,
        max_sensitivity: Sensitivity,
        kind: &str,
        key: &str,
        limit: i64,
    ) -> Result<Vec<RegistryVersion>>;

    async fn add_alias(
        &self,
        tenant: &str,
        namespace: &str,
        kind: &str,
        alias_key: &str,
        canonical: &str,
        origin: AliasOrigin,
    ) -> Result<()>;

    async fn resolve_alias(
        &self,
        tenant: &str,
        namespace: &str,
        kind: &str,
        alias_key: &str,
    ) -> Result<Option<String>>;

    /// Everything the caller may read, for the digest and for `lumberroom registry list`.
    async fn list(&self, tenant: &str, readable: &[NamespaceCeiling]) -> Result<Vec<RegistryEntry>>;

    async fn delete(&self, tenant: &str, namespace: &str, kind: &str, key: &str) -> Result<bool>;

    /// Past its review_after. Marked for a human to look at, never expired automatically.
    async fn due_for_review(&self, tenant: &str, limit: i64) -> Result<Vec<RegistryEntry>>;

    /// Free-form keys still in the store, for the one-time hand migration to the canonical scheme.
    async fn non_canonical(&self, tenant: &str) -> Result<Vec<RegistryEntry>>;
}
