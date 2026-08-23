//! Names that denote the same thing.
//!
//! Supersession says a fact was replaced. This says two names are the same subject, which is a
//! different claim and needs a different table. A project renamed from Warden to Quill to Lumen
//! leaves every Warden fact true and about the same thing, so retiring them would destroy history
//! and hide facts that still hold.
//!
//! `registry_alias` already carries this shape for registry keys. This generalises it, and adds the
//! one thing that table lacks: valid time, so the store knows Warden was the current name until a
//! date rather than only that it was ever a name.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::domain::errors::Result;

/// One name for an entity, and when it was the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alias {
    pub namespace: String,
    /// Lowercased on the way in. Names are matched by a person typing one, not by exact bytes.
    pub alias: String,
    /// The name every alias in a group resolves to.
    pub canonical: String,
    /// Half-open, the same convention valid time uses on a memory: the alias was current from
    /// `since` and stopped being current at `until`.
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    /// `manual` when the owner stated it, `derived` when something read it out of a fact.
    pub origin: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewAlias {
    pub namespace: String,
    pub alias: String,
    pub canonical: String,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub origin: String,
}

#[async_trait]
pub trait AliasRepository: Send + Sync {
    async fn put(&self, tenant: &str, a: NewAlias) -> Result<Alias>;

    /// Every name that denotes whatever `name` denotes, including `name` itself.
    ///
    /// This is the whole retrieval story. Expanding a query over the group costs one indexed read
    /// and fixes every row already in the store, where linking each memory to an entity would need
    /// every row rewritten and would help nothing until it was.
    async fn group(&self, tenant: &str, namespace: &str, name: &str) -> Result<Vec<String>>;

    async fn list(&self, tenant: &str, namespace: Option<&str>) -> Result<Vec<Alias>>;

    async fn forget(&self, tenant: &str, namespace: &str, alias: &str) -> Result<bool>;
}
