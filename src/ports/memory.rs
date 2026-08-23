//! The memory store. Everything a service needs from it, expressed without naming a database.

use async_trait::async_trait;
use std::collections::HashMap;

use crate::domain::errors::Result;
use crate::domain::policy::{NamespaceCeiling, NamespaceGrant};
use crate::domain::types::{ConflictCandidate, Memory, Sensitivity, SearchHit};

/// One subject's versions, oldest first, with what the caller was not shown.
///
/// The counts exist because a timeline that silently drops a version is a lie in the shape of an
/// answer. A reader can tell "these three versions are all there ever were" from "these three are
/// the ones you may see" only if the store says which it handed over.
///
/// Counts rather than ids or namespaces. A gap has to be visible to be honest, and naming the
/// namespace a withheld version sits in would map the grant the caller was refused.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Timeline {
    /// Oldest first, the anchor's own version among them.
    pub versions: Vec<Memory>,
    /// Versions on this chain the grant does not admit. Zero when the caller may read all of it,
    /// and zero for an empty timeline: an id the caller cannot read reveals nothing, not even a
    /// number.
    pub withheld: i64,
    /// The walk stopped at the depth cap, so the chain may run past one or both ends of
    /// `versions`. A chain exactly as long as the cap trips this too, which is the cheap side of
    /// the trade: the alternative walks one hop further to tell the two apart.
    pub depth_capped: bool,
}

impl Timeline {
    /// The one-row answer for a fact with no chain, and the fallback a caller uses when the walk
    /// returns nothing it may read.
    pub fn single(row: Memory) -> Self {
        Self { versions: vec![row], withheld: 0, depth_capped: false }
    }

    pub fn is_empty(&self) -> bool {
        self.versions.is_empty()
    }
}

/// One row handed out, on its way to `recall_emission`.
#[derive(Debug, Clone)]
pub struct Emission {
    /// `services::ingest::fingerprint` of the content as the client received it.
    pub content_sha256: String,
    pub memory_id: uuid::Uuid,
}

#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub tenant_id: String,
    /// Ranked first. Each namespace carries the ceiling this caller holds for it, so the
    /// sensitivity filter is part of the query rather than a pass over the results: a row a client
    /// may not see must never enter that client's process memory.
    pub primary: Vec<NamespaceCeiling>,
    /// Scanned at a penalty; empty in strict mode.
    pub secondary: Vec<NamespaceCeiling>,
    pub embedding: Vec<f32>,
    /// The raw text, for the lexical arm.
    pub text: String,
    pub limit: i64,
    pub weights: Weights,
    /// Live rows only by default. History stays queryable, which is what makes the decision log a
    /// side effect rather than a feature to build.
    pub include_superseded: bool,
    /// What held at this instant, on the valid-time axis: the `occurred_at` and `occurred_until`
    /// pair, never `created_at`. The other question, what the store believed at an instant, is
    /// transaction time and needs its own name; the spec reserves `believed_at` for it.
    ///
    /// Set, it overrides `include_superseded`. A row retired last week is exactly the row that
    /// answers a question about last month, so an as-of read applies no supersession filter and
    /// `include_superseded` decides nothing the period predicate has not already decided.
    ///
    /// Unset, the search is the one this server has always run, down to the statement text.
    ///
    /// Retired rows reach a caller through this field, so the caller checks
    /// `Principal::may_read_history` before setting it. Nothing below this line can: a repository
    /// holds no principal.
    pub as_of: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Copy)]
pub struct Weights {
    pub vector: f64,
    pub lexical: f64,
    pub secondary_penalty: f64,
    /// A small recency-and-use boost, capped low enough that it cannot outrank semantic relevance.
    pub usage: f64,
}

#[derive(Debug, Clone)]
pub struct NewMemory {
    pub tenant_id: String,
    /// Chosen by the caller rather than by the database.
    ///
    /// Encryption binds the row id in as associated data, so the id has to exist before the content
    /// is sealed. `None` lets the database generate one, which is the plaintext path.
    pub id: Option<uuid::Uuid>,
    pub namespace: String,
    /// Plaintext. Ignored, and never stored, when `sealed` is present.
    pub content: String,
    /// Computed over the plaintext and stored in the clear even for an encrypted row, because
    /// search has to work. That leaks the gist of a private fact to anyone holding the database,
    /// which is a defended and documented trade, not an oversight
    /// (docs/research/encryption-and-sensitivity.md).
    pub embedding: Vec<f32>,
    pub tags: Vec<String>,
    pub supersedes: Option<uuid::Uuid>,
    pub source_client: String,
    pub embedding_model: String,
    /// Resolved before it reaches here: the namespace default, raised if the caller asked for more.
    pub sensitivity: Sensitivity,
    /// Present when the service encrypted the content. The repository stores these bytes and holds
    /// no key: the service layer owns the KEK, and the store never sees a private row's plaintext.
    ///
    /// `ports` naming a type from `crypto` is deliberate. `SealedContent` is a pure data carrier
    /// with no I/O, and defining a second identical struct here to preserve a layering diagram
    /// would mean a conversion on every write and two places to get the field set wrong.
    pub sealed: Option<crate::crypto::envelope::SealedContent>,
    /// Valid time, when the caller knew it. The write path never guesses one: a date derived from
    /// content rather than stated by the owner is a guess stored as a fact, which is the pattern
    /// the credential tripwire exists to refuse.
    pub occurred_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Live rows in one namespace near a candidate embedding. Feeds both duplicate collapse and the
/// conflict candidates a write hands back.
#[derive(Debug, Clone)]
pub struct NeighbourQuery {
    pub tenant_id: String,
    pub namespace: String,
    pub embedding: Vec<f32>,
    /// Cosine similarity floor. Below it the row is not worth mentioning.
    pub min_similarity: f64,
    pub limit: i64,
    /// Ceiling for the caller, so a neighbour they may not read is never quoted back at them.
    pub max_sensitivity: Sensitivity,
}

/// One page of stored facts, newest first, for a reading surface.
///
/// Beside `list_for_export` rather than replacing it, because the two contracts agree on nothing.
/// The export is oldest first, offset paged and bounded by `EXPORT_MAX_SENSITIVITY`, so a vault
/// synced to a third party never carries private content. This one is newest first, keyset paged
/// and filtered on both policy axes inside the query, which is what a console page needs and what
/// the export must never gain.
#[derive(Debug, Clone)]
pub struct RecentQuery {
    pub tenant_id: String,
    /// Every namespace the caller may read, with its ceiling. Both axes run in the query, the same
    /// contract every other read path here holds to.
    pub readable: Vec<NamespaceCeiling>,
    /// One namespace, or all of `readable` when absent.
    pub namespace: Option<String>,
    /// Keyset cursor: rows strictly older than this `(created_at, id)`. Keyset rather than an
    /// offset because a write landing mid-read shifts every later offset by one and the reader
    /// sees a row twice or not at all.
    pub before: Option<(chrono::DateTime<chrono::Utc>, uuid::Uuid)>,
    pub limit: i64,
    /// Retired rows come back alongside the live ones, so a correction reads as a revision in
    /// place rather than as a row that vanished.
    pub include_superseded: bool,
}

/// What one namespace holds, filtered on both axes.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NamespaceSummary {
    pub namespace: String,
    pub live: i64,
    pub retired: i64,
    /// Live rows above `open`. The size of a namespace and its exposure are different questions.
    pub above_open: i64,
    pub last_write: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone)]
pub struct DigestQuery {
    pub tenant_id: String,
    pub user_namespace: String,
    pub project_namespace: Option<String>,
    /// Everything the caller may read, with its ceiling. Every subquery intersects with this;
    /// Phase 1 shipped a bug where the profile and project subqueries skipped the namespace filter,
    /// and the leak path in a memory system is the convenience surface, not the obvious one.
    pub readable: Vec<NamespaceCeiling>,
    pub profile_limit: i64,
    pub project_limit: i64,
    pub recent_limit: i64,
    pub registry_limit: i64,
    pub recent_days: i32,
}

#[derive(Debug, Clone, Default)]
pub struct DigestData {
    pub profile: Vec<Memory>,
    pub project_context: Vec<Memory>,
    pub recent: Vec<Memory>,
    pub registry: Vec<RegistrySummary>,
    pub memories_count: i64,
    pub registry_count: i64,
    pub by_namespace: HashMap<String, i64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RegistrySummary {
    pub namespace: String,
    pub kind: String,
    pub key: String,
    pub value: serde_json::Value,
}

/// Two live rows in one namespace close enough that one probably should have retired the other.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConflictPair {
    pub older: ConflictCandidate,
    pub newer: ConflictCandidate,
    pub similarity: f64,
}

/// The three numbers that say whether the store is decaying: a store read often and written rarely
/// is decaying, and one never read at all is dead weight (system PRD §8).
#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct Staleness {
    pub live_rows: i64,
    pub never_retrieved: i64,
    pub never_retrieved_pct: f64,
    pub median_age_days_retrieved: Option<f64>,
    pub superseded_rows: i64,
    pub oldest_never_retrieved_days: Option<f64>,
}

#[async_trait]
pub trait MemoryRepository: Send + Sync {
    async fn search(&self, q: SearchQuery) -> Result<Vec<SearchHit>>;
    async fn insert(&self, m: NewMemory) -> Result<Memory>;

    /// Exact-content match, used for duplicate collapse. Only reaches plaintext rows: an encrypted
    /// row has no content column to compare, so private duplicates are caught by similarity alone.
    async fn find_exact(
        &self,
        tenant: &str,
        namespace: &str,
        content: &str,
    ) -> Result<Option<Memory>>;

    async fn find_by_id(&self, tenant: &str, id: uuid::Uuid) -> Result<Option<Memory>>;

    /// One round trip. The bootstrap latency budget depends on it staying that way.
    async fn digest(&self, q: DigestQuery) -> Result<DigestData>;

    /// Namespace to row count, **pre-policy**: no grant and no ceiling are applied to either half.
    ///
    /// The one read here that takes no policy argument, and it stays that way because a glob grant
    /// resolves against concrete names. Nothing can ask for "the namespaces I may read" until
    /// something has listed the namespaces that exist, so discovery runs first and the grant runs
    /// over its result. Giving this method a ceiling would make it two queries or move glob
    /// matching into SQL, and policy in the adapter is worse than a documented contract here.
    ///
    /// **The contract: neither the counts nor the names may reach a response as they stand.**
    ///
    /// The counts include rows above the caller's ceiling. The digest's `by_namespace` arm is the
    /// filtered count and is what an inventory is built from; a digest that took its inventory from
    /// here told a client granted `*` at open that `personal:finance` holds one row, refusing the
    /// content and publishing the name and the number.
    ///
    /// The names are safe to feed `filter_readable` and nothing else. Surviving that call means the
    /// namespace axis admits the name, which is not the same as the caller being able to read
    /// anything there: a ceiling of open over a namespace holding private rows publishes a name the
    /// second axis refused. Publish a name only once a both-axes count has put a row behind it.
    async fn namespace_counts(&self, tenant: &str) -> Result<HashMap<String, i64>>;

    /// One page of facts, newest first, filtered on both axes inside the query.
    ///
    /// Takes the ceilings for the reason `namespace_counts` does not: this one returns content, so
    /// the grant has to be part of the plan rather than a pass over the rows.
    async fn recent(&self, q: RecentQuery) -> Result<Vec<Memory>>;

    /// Per-namespace counts and the last write, for a reader deciding where to look.
    ///
    /// Both axes, so a namespace whose rows all sit above the caller's ceiling is absent rather
    /// than present at zero. An entry at zero is the same disclosure with a smaller number on it.
    async fn namespace_summary(
        &self,
        tenant: &str,
        readable: &[NamespaceCeiling],
    ) -> Result<Vec<NamespaceSummary>>;

    /// Random sample of stored content the caller may read, for the recall monitor.
    ///
    /// Takes the ceilings because the sample reaches a response: the report quotes the opening
    /// characters of every probe it measured, and `/admin/recall` asks for authentication and
    /// nothing else. Sampling the whole tenant read open rows out of namespaces the caller's grant
    /// never reached, to any client that could hold a token.
    async fn sample_content(
        &self,
        tenant: &str,
        readable: &[NamespaceCeiling],
        n: i64,
    ) -> Result<Vec<String>>;

    /// Nearest ids by embedding. `exact` forces a sequential scan for ground truth.
    async fn nearest_ids(
        &self,
        tenant: &str,
        embedding: &[f32],
        k: i64,
        exact: bool,
    ) -> Result<Vec<String>>;

    /// Live rows near this embedding, for dedupe bands and conflict candidates.
    async fn neighbours(&self, q: NeighbourQuery) -> Result<Vec<ConflictCandidate>>;

    /// Retire `old` in favour of `new`, writing the link on both rows in one transaction.
    ///
    /// Must reject a target that is already superseded, and must reject a cycle: a two-row cycle
    /// makes both rows invisible, which is data loss dressed as a correction.
    async fn supersede(&self, tenant: &str, old: uuid::Uuid, new: uuid::Uuid) -> Result<()>;

    /// Walk the chain to the row that is live now, so a rejection can name the current head.
    async fn supersession_head(&self, tenant: &str, id: uuid::Uuid) -> Result<Option<Memory>>;

    /// One subject's whole supersession chain, oldest first, each row carrying its valid period.
    ///
    /// This is what reads back as "the port was 8080, and since 20 August it is 8787". A search
    /// answers what holds now and an as-of search answers what held at one instant; neither can
    /// show the sequence, because the live filter hides every row but the last and an as-of read
    /// returns one slice.
    ///
    /// `id` may sit anywhere in the chain. The walk runs both ways from it, so a caller holding the
    /// first version and a caller holding the current one get the same timeline.
    ///
    /// Retired rows are the entire content of the answer, so the caller checks
    /// `Principal::may_read_history` before asking.
    ///
    /// **The whole grant arrives, globs and ceilings, rather than one namespace and one ceiling.**
    /// A supersession may retire a row in one namespace in favour of a row in another, so the
    /// namespaces on a chain are unknown until the walk has run and cannot be resolved to concrete
    /// ceilings in advance. Scoping the walk to a single namespace stopped it at the first
    /// boundary and reported a short history as a complete one. The walk now crosses, and every
    /// row is admitted by the same rule `policy::admits` states: some granted pattern matches the
    /// row's namespace and its ceiling reaches the row's level. Both axes still run inside the
    /// query, so a version this caller may not read never enters this process.
    ///
    /// An id the caller may not read gives an empty timeline with no gap reported, which is what
    /// `search` answers about the same row. Learning that a fact exists is the disclosure this
    /// refuses.
    async fn subject_history(
        &self,
        tenant: &str,
        grants: &[NamespaceGrant],
        id: uuid::Uuid,
    ) -> Result<Timeline>;

    /// Record that these rows were actually returned in a result. Fire and forget and batched by
    /// contract: a search must not turn into a write storm, and it must not pay for this.
    fn touch_accessed(&self, tenant: &str, ids: Vec<uuid::Uuid>);

    /// Record what the store handed a client, so content it emitted cannot come back to it as a
    /// new fact.
    ///
    /// Same contract as `touch_accessed`: batched, fire and forget, a failure is a log line. A read
    /// that waited on this or failed because of it would trade the failure this layer exists to
    /// catch for a worse one.
    ///
    /// The hash arrives already computed, and that is the point. It has to be
    /// `services::ingest::fingerprint` of the same content, because a second normaliser gives the
    /// echo check a hash that can never meet a proposal's, which is how the earlier version of this
    /// layer was built unreachable.
    fn record_emissions(
        &self,
        tenant: &str,
        tool: &'static str,
        session_id: Option<String>,
        rows: Vec<Emission>,
    );

    /// Repetition is confirmation. Set when a write restates a fact rather than contradicting it.
    async fn confirm(&self, tenant: &str, id: uuid::Uuid) -> Result<()>;

    /// Hard delete. For a private row the wrapped DEK goes with it, so ciphertext in any older
    /// backup is already unreadable.
    async fn delete(&self, tenant: &str, id: uuid::Uuid) -> Result<bool>;

    /// Live rows older than a threshold that were never retrieved. The review queue, not a reaper.
    async fn stale(&self, tenant: &str, older_than_days: i32, limit: i64) -> Result<Vec<Memory>>;

    async fn staleness(&self, tenant: &str) -> Result<Staleness>;

    /// Near-duplicate live pairs, for `lumberroom review`. Computed on demand rather than recorded at
    /// write time: a stored queue drifts out of step with the store it describes, and this runs by
    /// hand rather than on the hot path.
    async fn conflicts(
        &self,
        tenant: &str,
        min_similarity: f64,
        limit: i64,
    ) -> Result<Vec<ConflictPair>>;

    /// The ciphertext columns for rows the caller already holds, so the service can decrypt them.
    ///
    /// Batched, because the shape that needs it is a search result rather than a single row.
    /// Deliberately separate from the read paths: one that returned plaintext and ciphertext
    /// together would make it possible to hand a caller ciphertext in a text field by mistake.
    ///
    /// The third element is the id of the key that wrapped the row's DEK. A row wrapped by a key
    /// this deployment no longer holds must fail to open rather than appear empty.
    async fn sealed_batch(
        &self,
        tenant: &str,
        ids: &[uuid::Uuid],
    ) -> Result<Vec<(uuid::Uuid, crate::crypto::envelope::SealedContent, Option<String>)>>;

    async fn sealed_one(
        &self,
        tenant: &str,
        id: uuid::Uuid,
    ) -> Result<Option<(crate::crypto::envelope::SealedContent, Option<String>)>>;

    /// Everything the export may see, oldest first. Bounded by sensitivity, because private content
    /// in a vault synced to a third party defeats the encryption it was given.
    async fn list_for_export(
        &self,
        tenant: &str,
        max_sensitivity: Sensitivity,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Memory>>;
}
