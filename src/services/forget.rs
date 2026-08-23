//! Deleting, Phase 3 §4. `lumberroom forget <id>`, `lumberroom forget --query "..."`, and `memory_forget`.
//!
//! Phase 1 had no delete path at all, and this is the phase where sensitive content becomes
//! possible, so the two belong together.
//!
//! # Why every caller needs the delete flag
//!
//! The spec calls the CLI path unrestricted and the MCP path opt-in. This module requires
//! `may_delete` from both, because the thing that separates them is a request header the caller
//! chooses. `X-Invocation: cli` costs a model nothing to send, so treating it as an authorisation
//! boundary would make the MCP restriction decorative. The operator's own grant carries
//! `"mayDelete": true` and that is a one-line config change with a visible reason.
//!
//! # Why a query delete has a similarity floor
//!
//! A vector search always returns its nearest rows, however far away they are. Deleting the top N
//! of a fuzzy search would let "forget the old port number" take out the deploy notes sitting
//! beside it. Candidates below the floor are not offered at all, and the dry run prints exactly
//! what is in scope before anything goes.

use serde::Serialize;

use super::Ctx;
use crate::adapters::auth::{can_read, can_write, filter_readable};
use crate::domain::errors::{DomainError, Result};
use crate::domain::namespaces;
use crate::domain::types::{Memory, Sensitivity};
use crate::ports::{SearchQuery, Weights};

/// How much of a row is quoted back in a dry run. Enough to recognise the fact, short enough that a
/// confirmation prompt stays readable.
const PREVIEW_CHARS: usize = 140;

/// The most rows one query delete may take. A delete that removes forty rows because the phrasing
/// was loose is the failure this bound exists for; the caller can raise it per call, up to this.
const MAX_QUERY_DELETES: i64 = 25;

#[derive(Debug, Clone, Serialize)]
pub struct Doomed {
    pub id: String,
    pub namespace: String,
    pub sensitivity: Sensitivity,
    pub source_client: String,
    pub created_at: String,
    /// Truncated. The caller has already been checked against this row's ceiling.
    pub preview: String,
    /// Similarity to the query, on the query path only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ForgetOutcome {
    /// True when nothing was deleted and this is the list of what would be.
    pub dry_run: bool,
    pub count: usize,
    pub rows: Vec<Doomed>,
    /// What deletion means for the levels in this batch, one line each.
    pub consequences: Vec<String>,
    /// Rendered for a confirmation prompt.
    pub text: String,
}

pub async fn by_id(
    ctx: &Ctx,
    id: &str,
    reason: Option<&str>,
    dry_run: bool,
) -> Result<ForgetOutcome> {
    assert_may_delete(ctx)?;

    let uuid = uuid::Uuid::parse_str(id.trim())
        .map_err(|_| DomainError::validation(format!("{id:?} is not a uuid")))?;
    let row = ctx.repos.memories.find_by_id(ctx.tenant(), uuid).await?;

    // One message for "does not exist" and for "not yours", because naming a namespace the caller
    // cannot read tells it that namespace exists.
    let mut row = match row {
        Some(m) if deletable(ctx, &m) => m,
        _ => {
            return Err(DomainError::not_found(format!(
                "memory {id} does not exist or is not yours to delete"
            )))
        }
    };

    // Decrypt for the preview only, and ignore a failure: the dry run then says the row could not
    // be read, which is information, and the delete goes ahead either way.
    let _ = super::decrypt(ctx, vec![&mut row]).await;

    let doomed = vec![to_doomed(&row, None)];
    if dry_run {
        return Ok(outcome(true, doomed));
    }

    if !ctx.repos.memories.delete(ctx.tenant(), uuid).await? {
        // Lost a race with another delete. Nothing is wrong with the store, and the caller wanted
        // the row gone, which it is.
        return Ok(outcome(false, vec![]));
    }
    record(ctx, &doomed, reason);
    super::bootstrap::clear_cache();
    Ok(outcome(false, doomed))
}

#[allow(clippy::too_many_arguments)]
pub async fn by_query(
    ctx: &Ctx,
    query: &str,
    requested: Option<Vec<String>>,
    limit: Option<i64>,
    min_similarity: Option<f64>,
    reason: Option<&str>,
    dry_run: bool,
) -> Result<ForgetOutcome> {
    assert_may_delete(ctx)?;

    let query = query.trim();
    if query.is_empty() {
        return Err(DomainError::validation("query cannot be empty"));
    }
    let limit = limit.unwrap_or(MAX_QUERY_DELETES).clamp(1, MAX_QUERY_DELETES);
    let floor = min_similarity.unwrap_or(ctx.cfg.quality.conflict_threshold);

    let asked = match requested {
        Some(list) if !list.is_empty() => {
            let mut out = list.iter().map(|n| namespaces::normalize(n)).collect::<Result<Vec<_>>>()?;
            namespaces::dedupe(&mut out);
            out
        }
        _ => namespaces::default_read_namespaces(&ctx.cfg.tenant_id, None)?,
    };
    let primary = filter_readable(&ctx.principal, &asked);
    if primary.is_empty() {
        return Ok(outcome(dry_run, vec![]));
    }

    let embedding = ctx.embedder.embed_query(query).await?;
    let mut hits = ctx
        .repos
        .memories
        .search(SearchQuery {
            tenant_id: ctx.cfg.tenant_id.clone(),
            primary,
            secondary: vec![],
            embedding,
            text: query.to_string(),
            limit,
            // Forgetting acts on what holds now. Deleting a fact as it stood in March would remove
            // the row that still stands today.
            as_of: None,
            weights: Weights {
                vector: ctx.cfg.search.vector_weight,
                lexical: ctx.cfg.search.lexical_weight,
                secondary_penalty: ctx.cfg.search.other_project_penalty,
                // No usage boost on a delete. A row being popular is not a reason to delete it, and
                // it is certainly not a reason to prefer it over the row the caller described.
                usage: 0.0,
            },
            include_superseded: true,
        })
        .await?;

    let _ = super::decrypt(ctx, hits.iter_mut().map(|h| &mut h.memory).collect()).await;

    // Superseded rows are included on purpose: forgetting a fact means forgetting the corrections
    // of it too, and a caller cleaning up after a mistake would otherwise be left with history
    // pointing at nothing.
    let doomed: Vec<Doomed> = hits
        .iter()
        .filter(|h| h.similarity >= floor)
        .filter(|h| deletable(ctx, &h.memory))
        .map(|h| to_doomed(&h.memory, Some(h.similarity)))
        .collect();

    if dry_run || doomed.is_empty() {
        return Ok(outcome(dry_run, doomed));
    }

    let mut gone = Vec::with_capacity(doomed.len());
    for row in doomed {
        let Ok(uuid) = uuid::Uuid::parse_str(&row.id) else { continue };
        match ctx.repos.memories.delete(ctx.tenant(), uuid).await {
            Ok(true) => gone.push(row),
            Ok(false) => {}
            // Partial success is reported as partial success. Rolling the whole batch back would
            // mean claiming rows still exist that do not.
            Err(e) => {
                tracing::error!(id = %row.id, error = %e.log_message(), "delete failed mid-batch");
                return Err(e);
            }
        }
    }
    record(ctx, &gone, reason);
    super::bootstrap::clear_cache();
    Ok(outcome(false, gone))
}

/// A sealed item, by the client-computed HMAC of its name.
///
/// The server never held a key for this and cannot check what it is deleting, so there is nothing
/// to preview and nothing to recover. That is the level working as specified, not a gap.
pub async fn sealed_item(
    ctx: &Ctx,
    namespace: &str,
    key_hmac: &str,
    reason: Option<&str>,
    dry_run: bool,
) -> Result<ForgetOutcome> {
    assert_may_delete(ctx)?;

    let namespace = namespaces::normalize(namespace)?;
    if !can_write(&ctx.principal, &namespace, Sensitivity::Sealed) {
        return Err(DomainError::forbidden(format!(
            "client {} may not delete sealed items in {namespace}",
            ctx.principal.client
        )));
    }
    let key_hmac = key_hmac.trim();
    if key_hmac.is_empty() {
        return Err(DomainError::validation("key_hmac cannot be empty"));
    }

    let doomed = vec![Doomed {
        id: key_hmac.to_string(),
        namespace: namespace.clone(),
        sensitivity: Sensitivity::Sealed,
        source_client: String::new(),
        created_at: String::new(),
        preview: "(sealed; this server holds no key)".to_string(),
        similarity: None,
    }];
    if dry_run {
        return Ok(outcome(true, doomed));
    }

    let removed = ctx.sealed_store()?.delete(ctx.tenant(), &namespace, key_hmac).await?;
    if !removed {
        return Ok(outcome(false, vec![]));
    }
    record(ctx, &doomed, reason);
    super::bootstrap::clear_cache();
    Ok(outcome(false, doomed))
}

/// A model that can silently delete memories is a worse failure than one that hoards them, so the
/// capability is off unless the grant names it.
fn assert_may_delete(ctx: &Ctx) -> Result<()> {
    if ctx.principal.may_delete {
        return Ok(());
    }
    Err(DomainError::forbidden(format!(
        "client {} may not delete. Deleting is opt-in per client: set \"mayDelete\": true on the \
         grant.",
        ctx.principal.client
    )))
}

/// Deleting is a destructive write, so it needs the write grant at the row's stored level, and the
/// read grant too: a client that cannot see a row has no business removing it.
fn deletable(ctx: &Ctx, m: &Memory) -> bool {
    can_read(&ctx.principal, &m.namespace, m.sensitivity)
        && can_write(&ctx.principal, &m.namespace, m.sensitivity)
}

fn to_doomed(m: &Memory, similarity: Option<f64>) -> Doomed {
    Doomed {
        id: m.id.clone(),
        namespace: m.namespace.clone(),
        sensitivity: m.sensitivity,
        source_client: m.source_client.clone(),
        created_at: m.created_at.to_rfc3339(),
        preview: preview(&m.content),
        similarity,
    }
}

/// An empty preview means a private row that would not decrypt. Deleting it is still the right
/// answer, and more clearly so: a row nobody can read is exactly the kind of thing to remove.
fn preview(content: &str) -> String {
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return "(encrypted; this server could not read it)".to_string();
    }
    if flat.chars().count() <= PREVIEW_CHARS {
        return flat;
    }
    format!("{}…", flat.chars().take(PREVIEW_CHARS).collect::<String>())
}

/// Deletions are recorded so that "what happened to that fact" has an answer. The tool call itself
/// is recorded by the transport like every other call; this line is what names the rows, which a
/// per-call row cannot.
fn record(ctx: &Ctx, rows: &[Doomed], reason: Option<&str>) {
    for row in rows {
        tracing::warn!(
            id = %row.id,
            namespace = %row.namespace,
            sensitivity = %row.sensitivity,
            client = %ctx.principal.client,
            session = ctx.session_id.as_deref().unwrap_or("-"),
            reason = reason.unwrap_or("-"),
            "memory deleted"
        );
    }
}

fn outcome(dry_run: bool, rows: Vec<Doomed>) -> ForgetOutcome {
    let consequences = consequences(&rows);
    let text = render(dry_run, &rows, &consequences);
    ForgetOutcome { dry_run, count: rows.len(), rows, consequences, text }
}

/// What deletion means, per level present in this batch. Said out loud because the three levels
/// differ in what survives, and the operator confirming a delete is the person who needs to know.
fn consequences(rows: &[Doomed]) -> Vec<String> {
    let mut out = Vec::new();
    if rows.iter().any(|r| r.sensitivity == Sensitivity::Open) {
        out.push(
            "open: the row goes. A plaintext backup taken before now still contains it.".to_string(),
        );
    }
    if rows.iter().any(|r| r.sensitivity == Sensitivity::Private) {
        out.push(
            "private: the wrapped DEK goes with the row, so the ciphertext in any older backup is \
             already unreadable."
                .to_string(),
        );
    }
    if rows.iter().any(|r| r.sensitivity == Sensitivity::Sealed) {
        out.push(
            "sealed: this removes the only copy. The server holds no key and cannot help recover it."
                .to_string(),
        );
    }
    out
}

/// The dry run's whole job: print exactly what would go, in a form somebody can say yes to.
fn render(dry_run: bool, rows: &[Doomed], consequences: &[String]) -> String {
    if rows.is_empty() {
        return if dry_run {
            "Nothing matches. Nothing would be deleted.".to_string()
        } else {
            "Nothing matched. Nothing was deleted.".to_string()
        };
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "{} {} {}:",
        if dry_run { "Would delete" } else { "Deleted" },
        rows.len(),
        if rows.len() == 1 { "row" } else { "rows" }
    ));
    for r in rows {
        let similarity = match r.similarity {
            Some(s) => format!(" {s:.2}"),
            None => String::new(),
        };
        lines.push(format!(
            "- {} [{}, {}]{}  {}",
            r.id, r.namespace, r.sensitivity, similarity, r.preview
        ));
    }
    for line in consequences {
        lines.push(String::new());
        lines.push(line.clone());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doomed(id: &str, level: Sensitivity) -> Doomed {
        Doomed {
            id: id.into(),
            namespace: "user:me".into(),
            sensitivity: level,
            source_client: "mac".into(),
            created_at: "2026-08-19T10:00:00+00:00".into(),
            preview: "The port is 8080".into(),
            similarity: Some(0.93),
        }
    }

    #[test]
    fn a_dry_run_names_every_row_it_would_take() {
        let rows = vec![doomed("a", Sensitivity::Open), doomed("b", Sensitivity::Open)];
        let text = render(true, &rows, &consequences(&rows));
        assert!(text.starts_with("Would delete 2 rows:"));
        assert!(text.contains("- a [user:me, open] 0.93  The port is 8080"));
        assert!(text.contains("- b ["));
    }

    #[test]
    fn a_dry_run_over_nothing_says_so_rather_than_printing_an_empty_list() {
        assert!(render(true, &[], &[]).contains("Nothing would be deleted"));
        assert!(render(false, &[], &[]).contains("Nothing was deleted"));
    }

    #[test]
    fn the_private_consequence_names_the_dek_going_with_the_row() {
        let rows = vec![doomed("a", Sensitivity::Private)];
        assert!(consequences(&rows)[0].contains("wrapped DEK goes with the row"));
    }

    #[test]
    fn the_sealed_consequence_says_the_copy_is_the_only_one() {
        let rows = vec![doomed("a", Sensitivity::Sealed)];
        assert!(consequences(&rows)[0].contains("only copy"));
    }

    #[test]
    fn one_line_per_level_present_and_none_for_a_level_that_is_not() {
        let rows = vec![
            doomed("a", Sensitivity::Open),
            doomed("b", Sensitivity::Open),
            doomed("c", Sensitivity::Private),
        ];
        let lines = consequences(&rows);
        assert_eq!(lines.len(), 2);
        assert!(!lines.iter().any(|l| l.starts_with("sealed")));
    }

    #[test]
    fn a_preview_is_flattened_and_bounded() {
        assert_eq!(preview("two\n  lines"), "two lines");
        let long = preview(&"x".repeat(500));
        assert_eq!(long.chars().count(), PREVIEW_CHARS + 1, "one char for the ellipsis");
    }
}
