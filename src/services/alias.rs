//! Aliases: recording that two names denote the same thing, and expanding a query over them.
//!
//! The case that forced this is in the owner's store. Six rows describe one project under three
//! names, Warden then Quill then Lumen, linked by nothing, so a search for Lumen finds two of
//! the six. Supersession is the wrong tool: the Warden rows are true and about the same subject,
//! and retiring them would destroy history and hide facts that still hold.
//!
//! Retrieval treats this as query expansion rather than as a graph. One indexed read turns a name
//! into its group and the search runs over the group, which fixes every row already in the store.
//! Linking each memory to an entity would need every row rewritten and would help nothing until it
//! was.
//!
//! The repository arrives as an argument rather than on `Ctx`, following the same reasoning the
//! ingest port is held outside `services::Repos` for: recording an alias is an operator surface,
//! and the services that need it take the port.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

use super::Ctx;
use crate::adapters::auth::{assert_writable, can_read};
use crate::domain::errors::{DomainError, Result};
use crate::domain::namespaces;
use crate::domain::types::{Principal, Sensitivity};
use crate::ports::alias::{Alias, AliasRepository, NewAlias};

/// `manual` when the owner stated it, `derived` when something read it out of a fact. The table
/// carries the same two and nothing else.
const ORIGINS: [&str; 2] = ["manual", "derived"];

/// One alias row on the wire. snake_case, timestamps as RFC 3339 strings, the same shape every
/// other tool result in this server publishes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AliasRecord {
    pub namespace: String,
    pub alias: String,
    pub canonical: String,
    pub since: Option<String>,
    pub until: Option<String>,
    pub origin: String,
    pub created_at: String,
}

impl From<Alias> for AliasRecord {
    fn from(a: Alias) -> Self {
        Self {
            namespace: a.namespace,
            alias: a.alias,
            canonical: a.canonical,
            since: a.since.map(|t| t.to_rfc3339()),
            until: a.until.map(|t| t.to_rfc3339()),
            origin: a.origin,
            created_at: a.created_at.to_rfc3339(),
        }
    }
}

/// What a search should look for, given what the caller asked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Expansion {
    /// The known names the query text mentioned.
    pub matched: Vec<String>,
    /// Every name in every group those names belong to, the matched names included. Empty when the
    /// query mentions no name this store knows, which leaves the caller with the query it had.
    pub names: Vec<String>,
}

/// Record one name as another name for the same thing.
pub async fn put(
    ctx: &Ctx,
    aliases: &dyn AliasRepository,
    namespace: &str,
    alias: &str,
    canonical: &str,
    since: Option<chrono::DateTime<chrono::Utc>>,
    until: Option<chrono::DateTime<chrono::Utc>>,
    origin: Option<&str>,
) -> Result<AliasRecord> {
    let namespace = namespaces::normalize(namespace)?;
    assert_may_record(ctx, &namespace)?;
    let origin = parse_origin(origin)?;

    let stored = aliases
        .put(
            ctx.tenant(),
            NewAlias {
                namespace,
                alias: alias.to_string(),
                canonical: canonical.to_string(),
                since,
                until,
                origin,
            },
        )
        .await?;
    Ok(stored.into())
}

/// Every alias the caller may read, in one namespace or across the store.
pub async fn list(
    ctx: &Ctx,
    aliases: &dyn AliasRepository,
    namespace: Option<&str>,
) -> Result<Vec<AliasRecord>> {
    if let Some(ns) = namespace {
        let ns = namespaces::normalize(ns)?;
        if !readable(&ctx.principal, &ns) {
            // Reads narrow silently. A refusal here would tell a client which namespaces exist.
            return Ok(vec![]);
        }
        let rows = aliases.list(ctx.tenant(), Some(&ns)).await?;
        return Ok(rows.into_iter().map(AliasRecord::from).collect());
    }

    // The one place in this file where a filter runs over results instead of inside the query, and
    // the port is why: `list` takes a single namespace, and a grant is a set of globs that no
    // single argument expresses. An alias row carries a namespace and two names and no sensitivity
    // column, so the namespace is the whole grant and there is no second axis to leak on. Moving
    // this filter into the statement means widening the port to take the ceiling list, which the
    // memory and registry reads already do.
    let rows = aliases.list(ctx.tenant(), None).await?;
    Ok(rows
        .into_iter()
        .filter(|a| readable(&ctx.principal, &a.namespace))
        .map(AliasRecord::from)
        .collect())
}

/// Drop one name from its group. The facts that mention it stay, and stay searchable under it.
pub async fn forget(
    ctx: &Ctx,
    aliases: &dyn AliasRepository,
    namespace: &str,
    alias: &str,
) -> Result<bool> {
    let namespace = namespaces::normalize(namespace)?;
    assert_may_record(ctx, &namespace)?;
    aliases.forget(ctx.tenant(), &namespace, alias).await
}

/// The names a search should look for, given the caller's query text.
///
/// Whole words only, matched against names this store already holds. What that misses, plainly: a
/// plural, a misspelling, a possessive that is not split on the apostrophe, a name nobody recorded,
/// and any sentence that refers to the thing without naming it. A pronoun expands nothing.
///
/// Pulling entity names out of free text with a model would catch those and is refused here for the
/// reason it is refused everywhere else in this system: it puts a guess on the retrieval path,
/// where a wrong guess expands a search into a different subject and the caller cannot see it
/// happen. A name the owner recorded is a fact. Everything here is derived from that fact.
pub async fn resolve(
    ctx: &Ctx,
    aliases: &dyn AliasRepository,
    query: &str,
    namespace: &str,
) -> Result<Expansion> {
    let namespace = namespaces::normalize(namespace)?;
    if !readable(&ctx.principal, &namespace) {
        return Ok(Expansion::default());
    }
    // One read for the namespace, then the matching in memory. An owner's alias table is tens of
    // rows: a query per candidate name would be tens of round trips in front of every search.
    let rows = aliases.list(ctx.tenant(), Some(&namespace)).await?;
    Ok(expand(&rows, query))
}

/// Recording an alias is an operator act, so it sits behind the same capability a registry write
/// does. An alias steers retrieval for every client at once: a model that could record one could
/// point a name at a subject of its choosing and change what every later search returns.
fn assert_may_record(ctx: &Ctx, namespace: &str) -> Result<()> {
    let stored_at = ctx.cfg.policy.defaults.resolve_for_write(namespace, None);
    gate(&ctx.principal, namespace, stored_at, ctx.cfg.policy.max_write_sensitivity)
}

/// The write check, split out from `Ctx` so a test can state a principal and nothing else.
fn gate(
    principal: &Principal,
    namespace: &str,
    stored_at: Sensitivity,
    ceiling: Sensitivity,
) -> Result<()> {
    if !principal.registry_write {
        return Err(DomainError::forbidden(format!(
            "client {} may not record an alias",
            principal.client
        )));
    }
    if stored_at >= Sensitivity::Sealed {
        return Err(DomainError::validation(format!(
            "{namespace} classifies at sealed and an alias is a name stored in the clear. \
             Record the alias in a namespace that classifies below sealed."
        )));
    }
    if stored_at > ceiling {
        return Err(DomainError::validation(format!(
            "{namespace} classifies content at {stored_at} and this server accepts up to {ceiling}."
        )));
    }
    assert_writable(principal, namespace, stored_at)
}

/// An alias row has no sensitivity of its own, so `open` is the level the read is checked at.
fn readable(principal: &Principal, namespace: &str) -> bool {
    can_read(principal, namespace, Sensitivity::Open)
}

fn parse_origin(raw: Option<&str>) -> Result<String> {
    let Some(o) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok("manual".to_string());
    };
    let o = o.to_ascii_lowercase();
    if ORIGINS.contains(&o.as_str()) {
        Ok(o)
    } else {
        Err(DomainError::validation(format!("origin {o:?} is not one of manual, derived")))
    }
}

/// Canonical name to every name in its group, the canonical name included.
fn groups_from_rows(rows: &[Alias]) -> BTreeMap<String, BTreeSet<String>> {
    let mut groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in rows {
        let group = groups.entry(row.canonical.clone()).or_default();
        group.insert(row.canonical.clone());
        group.insert(row.alias.clone());
    }
    groups
}

/// The matching, with no I/O in it, so the rule is testable on its own.
fn expand(rows: &[Alias], query: &str) -> Expansion {
    let groups = groups_from_rows(rows);
    let mut canonical_of: BTreeMap<&str, &str> = BTreeMap::new();
    for (canonical, group) in &groups {
        for name in group {
            canonical_of.insert(name.as_str(), canonical.as_str());
        }
    }

    let words = tokenize(query);
    let mut matched = BTreeSet::new();
    let mut names = BTreeSet::new();
    for (name, canonical) in &canonical_of {
        if !mentions(&words, &tokenize(name)) {
            continue;
        }
        matched.insert((*name).to_string());
        if let Some(group) = groups.get(*canonical) {
            names.extend(group.iter().cloned());
        }
    }

    Expansion { matched: matched.into_iter().collect(), names: names.into_iter().collect() }
}

/// Lowercase runs of alphanumerics. Splitting on everything else is what makes "Warden's" and
/// "(warden)" both match the recorded name, and it is why a possessive costs nothing here.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// Whether the query says this name, as whole words and in order.
///
/// A contiguous run, so a two-word name matches only where both words sit together. Substring
/// matching would make "quill" fire on "tranquillity", which expands a search into a subject nobody
/// asked about.
fn mentions(words: &[String], name: &[String]) -> bool {
    if name.is_empty() || name.len() > words.len() {
        return false;
    }
    words.windows(name.len()).any(|w| w == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::policy::NamespaceGrant;

    fn row(alias: &str, canonical: &str) -> Alias {
        Alias {
            namespace: "project:lumen".into(),
            alias: alias.into(),
            canonical: canonical.into(),
            since: None,
            until: None,
            origin: "manual".into(),
            created_at: chrono::Utc::now(),
        }
    }

    /// The owner's case: Warden was renamed to Quill and Quill to Lumen, and the store holds
    /// facts under all three. A query naming any one of them has to search for all three.
    #[test]
    fn a_three_name_chain_resolves_to_all_three() {
        let rows = vec![row("warden", "lumen"), row("quill", "lumen")];
        let all = vec!["lumen".to_string(), "quill".to_string(), "warden".to_string()];

        for asked in ["warden", "quill", "lumen"] {
            let e = expand(&rows, &format!("what port does {asked} run on"));
            assert_eq!(e.matched, vec![asked.to_string()], "matched for {asked}");
            assert_eq!(e.names, all, "expansion for {asked}");
        }
    }

    #[test]
    fn a_query_naming_nothing_known_expands_to_nothing() {
        let rows = vec![row("warden", "lumen")];
        let e = expand(&rows, "what port does the database run on");
        assert!(e.matched.is_empty());
        // The caller substitutes this into a search and keeps its own query text, so empty here
        // means "add nothing" rather than "search for nothing".
        assert!(e.names.is_empty());
    }

    #[test]
    fn a_name_is_matched_whatever_case_the_query_typed_it_in() {
        let rows = vec![row("warden", "lumen")];
        let e = expand(&rows, "Is WARDEN still deployed?");
        assert_eq!(e.matched, vec!["warden".to_string()]);
    }

    #[test]
    fn a_name_inside_a_longer_word_does_not_fire() {
        // The reason matching is on whole words. "quill" inside "tranquillity" would expand this
        // query into a project nobody mentioned.
        let rows = vec![row("quill", "lumen")];
        assert!(expand(&rows, "read about tranquillity").matched.is_empty());
        assert!(expand(&rows, "read quill's notes").matched.contains(&"quill".to_string()));
    }

    #[test]
    fn a_two_word_name_matches_only_where_both_words_sit_together() {
        let rows = vec![row("project warden", "lumen")];
        assert!(expand(&rows, "the project warden shipped").matched.len() == 1);
        assert!(expand(&rows, "the project that warden shipped").matched.is_empty());
    }

    #[test]
    fn two_groups_named_in_one_query_both_expand() {
        let rows = vec![row("warden", "lumen"), row("lumberroom", "memoryengine")];
        let e = expand(&rows, "does warden use lumberroom");
        assert_eq!(e.matched, vec!["lumberroom".to_string(), "warden".to_string()]);
        assert_eq!(
            e.names,
            vec![
                "lumberroom".to_string(),
                "lumen".to_string(),
                "memoryengine".to_string(),
                "warden".to_string()
            ]
        );
    }

    fn operator(write: Vec<NamespaceGrant>) -> Principal {
        let mut p = Principal::empty("console");
        p.registry_write = true;
        p.write = write;
        p
    }

    #[test]
    fn a_client_without_the_registry_capability_cannot_record_an_alias() {
        // `Principal::empty` has registry_write false, which is the default every client starts at.
        let refused =
            gate(&Principal::empty("chatgpt"), "user:me", Sensitivity::Open, Sensitivity::Private);
        assert!(refused.is_err());
        assert!(refused.unwrap_err().client_message().contains("may not record an alias"));
    }

    #[test]
    fn the_capability_alone_does_not_reach_a_namespace_the_grant_excludes() {
        let p = operator(vec![NamespaceGrant::new("user:me", Sensitivity::Open)]);
        assert!(gate(&p, "user:me", Sensitivity::Open, Sensitivity::Private).is_ok());
        assert!(gate(&p, "project:lumen", Sensitivity::Open, Sensitivity::Private).is_err());
    }

    #[test]
    fn a_sealed_namespace_takes_no_aliases() {
        let p = operator(vec![NamespaceGrant::new("*", Sensitivity::Sealed)]);
        let refused = gate(&p, "credentials:aws", Sensitivity::Sealed, Sensitivity::Sealed);
        assert!(refused.is_err());
        assert!(refused.unwrap_err().client_message().contains("stored in the clear"));
    }

    #[test]
    fn an_origin_outside_the_vocabulary_is_refused() {
        assert_eq!(parse_origin(None).unwrap(), "manual");
        assert_eq!(parse_origin(Some(" Derived ")).unwrap(), "derived");
        assert!(parse_origin(Some("rejected-write")).is_err());
    }

    #[test]
    fn a_group_carries_its_canonical_name_even_when_no_row_names_it_first() {
        let groups = groups_from_rows(&[row("warden", "lumen")]);
        let group = groups.get("lumen").expect("group keyed by canonical");
        assert!(group.contains("lumen"));
        assert!(group.contains("warden"));
    }
}
