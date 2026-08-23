//! registry_get(kind, key, namespace?, project?) -> value, and the write that keeps keys canonical.
//!
//! Exact lookup, no fuzziness. With no namespace given the read walks the read set most-specific
//! first, so a project override beats a global default, and reports which namespace answered. Each
//! step of the walk carries that namespace's ceiling, so precedence never becomes a way around the
//! sensitivity axis.
//!
//! # Why the write path is strict and the read path is not
//!
//! Six writers without a scheme produce `desktop.gpu`, `machines.desktop.gpu` and
//! `hardware.desktop.gpu` for one fact. Preventing that beats cleaning it up, so a write must name
//! a canonical key or be refused.
//!
//! Rejection alone makes it worse. A model that gets rejected invents a second variant rather than
//! the canonical one, so every rejected guess is recorded as a redirect. The same wrong guess then
//! resolves on the next attempt instead of becoming a duplicate fact. That is why the read path is
//! lenient: it normalises what it can, then lets the alias table answer for the rest.

use serde::Serialize;

use super::Ctx;
use crate::adapters::auth::{assert_writable, filter_readable};
use crate::domain::errors::{DomainError, Result};
use crate::domain::types::{Principal, Provenance, Sensitivity};
use crate::domain::{canonical, namespaces, policy, tripwire};
use crate::ports::registry::{RegistryUpsert, RegistryVersion};

/// How long a fact of each kind stays trustworthy without being looked at.
///
/// A host ages slowly: the box keeps its GPU. A model route ages fast because routing preferences
/// change monthly, and a stale route is a wrong answer rather than an old one. Expiry marks a row
/// for review and never removes it, so being wrong here costs a line in `lumberroom review` (Phase 4 §3).
///
/// A const table rather than configuration. The vocabulary is five words and closed, so a setting
/// here would be five settings nobody ever edits; the moment `KINDS` grows this grows with it.
const REVIEW_AFTER_DAYS: &[(&str, i64)] = &[
    ("host", 365),
    ("service", 180),
    // A credential reference outlives a rotation but not a migration.
    ("credential-ref", 90),
    ("model-route", 30),
    ("dataset", 180),
];

/// For a kind not in the table. Six months: long enough not to be noise, short enough that a fact
/// nobody has looked at in half a year gets a second glance.
const DEFAULT_REVIEW_AFTER_DAYS: i64 = 180;

#[derive(Debug, Serialize)]
pub struct RegistryGetResult {
    pub found: bool,
    pub kind: String,
    pub key: String,
    pub namespace: Option<String>,
    pub value: serde_json::Value,
    pub provenance: Option<Provenance>,
    pub sensitivity: Option<Sensitivity>,
    pub version: Option<i32>,
    /// Set when the key asked for was an alias, so a redirect is visible rather than silent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_from: Option<String>,
    pub searched: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RegistryHistoryResult {
    pub kind: String,
    /// The key the versions are filed under, which is the canonical one when a redirect answered.
    pub key: String,
    /// The namespace that held versions, or `None` when none of the searched ones did.
    pub namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_from: Option<String>,
    pub searched: Vec<String>,
    pub entries: Vec<RegistryVersion>,
}

/// Versions returned when the caller names no limit.
///
/// A registry key rewritten twenty times is already a key worth looking at by hand. This bounds one
/// page; the archive keeps every version regardless.
const DEFAULT_HISTORY_LIMIT: i64 = 20;

/// The ceiling on one history read. A caller asking for more is asking for an export, and
/// `lumberroom export` is the tool that exists for that.
const MAX_HISTORY_LIMIT: i64 = 200;

#[derive(Debug, Serialize)]
pub struct RegistrySetResult {
    pub id: String,
    pub namespace: String,
    pub kind: String,
    /// The key as stored, which is not always the key that was sent.
    pub key: String,
    pub sensitivity: Sensitivity,
    pub version: i32,
    pub review_after: String,
}

pub async fn get(
    ctx: &Ctx,
    kind: &str,
    key: &str,
    namespace: Option<&str>,
    project: Option<&str>,
) -> Result<RegistryGetResult> {
    let asked_key = key.trim();
    if asked_key.is_empty() {
        return Err(DomainError::validation("key cannot be empty"));
    }
    // Lenient on read. A kind or key the write path would refuse may still be exactly what an
    // older row or an alias is filed under, and a reader that refuses to look is no use to anybody.
    let kind = canonical::validate_kind(kind).unwrap_or_else(|_| kind.trim().to_ascii_lowercase());
    let lookup = canonical::validate_key(asked_key).unwrap_or_else(|_| asked_key.to_string());

    let order = match namespace {
        Some(ns) => vec![namespaces::normalize(ns)?],
        None => precedence(&ctx.cfg.tenant_id, project)?,
    };
    let searched = filter_readable(&ctx.principal, &order);

    for ceiling in &searched {
        let found = ctx
            .repos
            .registry
            .get(ctx.tenant(), &ceiling.namespace, ceiling.max, &kind, &lookup)
            .await?;
        if let Some(entry) = found {
            // Two ways the answer can come from a different key than the one asked for: the
            // repository followed an alias, or normalisation rewrote the key before the lookup.
            // Both are redirects and both are reported.
            let resolved_from = entry
                .resolved_from
                .clone()
                .or_else(|| (lookup != asked_key).then(|| asked_key.to_string()));
            return Ok(RegistryGetResult {
                found: true,
                kind,
                key: entry.key,
                namespace: Some(ceiling.namespace.clone()),
                value: entry.value,
                provenance: Some(entry.provenance),
                sensitivity: Some(entry.sensitivity),
                version: Some(entry.version),
                resolved_from,
                searched: names(&searched),
            });
        }
    }

    Ok(RegistryGetResult {
        found: false,
        kind,
        key: asked_key.to_string(),
        namespace: None,
        value: serde_json::Value::Null,
        provenance: None,
        sensitivity: None,
        version: None,
        resolved_from: None,
        searched: names(&searched),
    })
}

/// Versions this key used to hold, newest first.
///
/// The current value is not in the answer. The archive fills on replacement, so what is here is
/// what this key stopped holding, and `get` is one call away for what it holds now.
///
/// # Alias resolution
///
/// It resolves, the same way `get` does, with the exact key preferred and one key answering. A
/// caller that read a value through a redirect and then asked what it used to be would otherwise be
/// told nothing is known about a key that answered a moment earlier, which is the failure this
/// system treats as the worst one it has. `resolved_from` reports the redirect, so a reader never
/// has to guess which key the rows came from.
///
/// # Why the capability is checked before the grant
///
/// A grant over what a key holds is not a grant over what it used to hold. `may_read_history` is
/// off by default, and the registry is where that matters most: an old credential location is
/// exactly the shape that gets replaced rather than deleted, and the vault it named may still hold
/// the secret. The refusal comes first so a client without the capability learns nothing from the
/// shape of the answer, not even which namespaces were searched.
pub async fn history(
    ctx: &Ctx,
    kind: &str,
    key: &str,
    namespace: Option<&str>,
    project: Option<&str>,
    limit: Option<i64>,
) -> Result<RegistryHistoryResult> {
    assert_may_read_history(&ctx.principal)?;

    let asked_key = key.trim();
    if asked_key.is_empty() {
        return Err(DomainError::validation("key cannot be empty"));
    }
    // Lenient, for the reason `get` is lenient: a value archived years ago may be filed under a
    // kind or key the write path would refuse today, and those rows are the ones worth reading.
    let kind = canonical::validate_kind(kind).unwrap_or_else(|_| kind.trim().to_ascii_lowercase());
    let lookup = canonical::validate_key(asked_key).unwrap_or_else(|_| asked_key.to_string());
    let limit = resolve_limit(limit)?;

    let order = match namespace {
        Some(ns) => vec![namespaces::normalize(ns)?],
        None => precedence(&ctx.cfg.tenant_id, project)?,
    };
    let searched = filter_readable(&ctx.principal, &order);

    for ceiling in &searched {
        let entries = ctx
            .repos
            .registry
            .history(ctx.tenant(), &ceiling.namespace, ceiling.max, &kind, &lookup, limit)
            .await?;
        // The first namespace holding versions answers, so precedence works the same here as it
        // does for a value: a project override's history beats a global default's.
        if !entries.is_empty() {
            let resolved_from = entries[0]
                .resolved_from
                .clone()
                .or_else(|| (lookup != asked_key).then(|| asked_key.to_string()));
            return Ok(RegistryHistoryResult {
                kind,
                key: entries[0].key.clone(),
                namespace: Some(ceiling.namespace.clone()),
                resolved_from,
                searched: names(&searched),
                entries,
            });
        }
    }

    Ok(RegistryHistoryResult {
        kind,
        key: asked_key.to_string(),
        namespace: None,
        resolved_from: None,
        searched: names(&searched),
        entries: vec![],
    })
}

/// Write a registry entry under a canonical key.
///
/// The sensitivity axis on a registry row controls who may read it and nothing else: migration 008
/// gave `memory` encryption columns and did not give them to `registry`, so a private registry value
/// is protected by the grant and not by a key. `sealed` is therefore refused outright rather than
/// stored in the clear under a name that promises otherwise.
pub async fn set(
    ctx: &Ctx,
    namespace: &str,
    kind: &str,
    key: &str,
    value: &serde_json::Value,
    sensitivity: Option<&str>,
    conv_id: Option<&str>,
) -> Result<RegistrySetResult> {
    // The registry holds credential locations and machine facts that other tools act on, so
    // writing to it is an operator action rather than something a model does in passing.
    if !ctx.principal.registry_write {
        return Err(DomainError::forbidden(format!(
            "client {} may not write to the registry",
            ctx.principal.client
        )));
    }

    let namespace = namespaces::normalize(namespace)?;
    let kind = canonical::validate_kind(kind)?;

    let resolved = ctx.cfg.policy.defaults.resolve_for_write(&namespace, parse_level(sensitivity)?);
    if resolved >= Sensitivity::Sealed {
        return Err(DomainError::validation(format!(
            "{namespace} classifies at sealed and the registry stores values in the clear. \
             Store the secret itself in the sealed store and keep a credential-ref here pointing \
             at it."
        )));
    }
    if resolved > ctx.cfg.policy.max_write_sensitivity {
        return Err(DomainError::validation(format!(
            "{namespace} classifies content at {resolved} and the registry accepts up to {}.",
            ctx.cfg.policy.max_write_sensitivity
        )));
    }
    assert_writable(&ctx.principal, &namespace, resolved)?;

    // The same backstop `memory_write` runs, for the same reason. A registry value at open is
    // plaintext every reader of the namespace sees, and the registry is where a credential gets
    // written when somebody means to write where it lives. Only at open: a private value is
    // behind the grant already. The scan runs over the JSON text, so a secret inside a nested
    // object is found as readily as a bare string.
    if ctx.cfg.policy.tripwire && resolved == Sensitivity::Open {
        if let Some(finding) = tripwire::scan(&value.to_string()) {
            tracing::warn!(
                client = %ctx.principal.client,
                namespace = %namespace,
                rule = %finding.rule,
                "credential tripwire refused a registry write at open"
            );
            return Err(DomainError::validation(format!(
                "this value matches the {} pattern and will not be stored in the registry at \
                 open. Store the secret with `lumberroom seal <key> --namespace credentials:<slug>` \
                 and keep a credential-ref here that names where it lives. The matched text is \
                 not repeated here on purpose.",
                finding.rule
            )));
        }
    }

    // The level this caller may overwrite in this namespace. `assert_writable` has passed, so a
    // ceiling exists.
    let replace_ceiling =
        policy::ceiling(&ctx.principal.write, &namespace).unwrap_or(Sensitivity::Open);

    let stored_key = match canonical::validate_key(key) {
        Ok(k) => k,
        Err(rejection) => {
            // Record the redirect before answering. The caller is about to retry with the
            // suggestion, and every other writer that reaches for the same wrong name should land
            // on the right row rather than inventing a third variant.
            if let Some(suggestion) = canonical::suggest_key(key) {
                if let Err(e) = ctx
                    .repos
                    .registry
                    .add_alias(
                        ctx.tenant(),
                        &namespace,
                        &kind,
                        key.trim(),
                        &suggestion,
                        crate::ports::AliasOrigin::RejectedWrite,
                    )
                    .await
                {
                    // The rejection still stands. Losing the alias costs a redirect, not the row.
                    tracing::warn!(
                        error = %e.log_message(),
                        rejected = key,
                        "could not record a rejected key as an alias"
                    );
                }
            }
            return Err(rejection);
        }
    };

    let provenance = Provenance {
        source_client: ctx.principal.client.clone(),
        conv_id: conv_id.map(str::to_string),
        confidence: 1.0,
        // A registry write is an operator action by the grant check above, so it is confirmed by
        // construction. A model-authored fact goes to memory, not here.
        user_confirmed: true,
        valid_from: chrono::Utc::now().to_rfc3339(),
    };
    let review_after = chrono::Utc::now() + chrono::Duration::days(review_after_days(&kind));

    let written = ctx
        .repos
        .registry
        .upsert(crate::ports::RegistryWrite {
            tenant_id: ctx.cfg.tenant_id.clone(),
            namespace: namespace.clone(),
            kind: kind.clone(),
            key: stored_key.clone(),
            value: value.clone(),
            provenance,
            sensitivity: resolved,
            replace_ceiling,
            review_after: Some(review_after),
        })
        .await?;
    let (id, version) = match written {
        RegistryUpsert::Written { id, version } => (id, version),
        // The slot holds something above this caller's ceiling. The message names neither the
        // level nor the value; "may not replace" is all the caller is owed, and it is the same
        // sentence whether the stored level is private or sealed.
        RegistryUpsert::Refused => {
            return Err(DomainError::forbidden(format!(
                "client {} may not replace what {namespace} holds under {kind}/{stored_key}",
                ctx.principal.client
            )))
        }
    };

    super::bootstrap::clear_cache();

    Ok(RegistrySetResult {
        id,
        namespace,
        kind,
        key: stored_key,
        sensitivity: resolved,
        version,
        review_after: review_after.to_rfc3339(),
    })
}

/// The capability check, split out so it can be exercised without standing up a `Ctx`.
///
/// The message names the client and says nothing about the key. A client without the capability
/// should not learn whether a key it cannot read the history of even exists.
fn assert_may_read_history(principal: &Principal) -> Result<()> {
    if principal.may_read_history {
        return Ok(());
    }
    Err(DomainError::forbidden(format!(
        "client {} may not read values the registry no longer holds",
        principal.client
    )))
}

/// A missing limit takes the default; an oversized one is trimmed; zero and below are refused.
///
/// Refused rather than clamped upward, because a caller that computed its way to zero has a bug and
/// an empty answer here reads as "this key has never been rewritten". Silently answering that about
/// a key with versions is the failure mode this whole read path exists to avoid.
fn resolve_limit(requested: Option<i64>) -> Result<i64> {
    match requested {
        None => Ok(DEFAULT_HISTORY_LIMIT),
        Some(n) if n < 1 => {
            Err(DomainError::validation(format!("limit {n} asks for no rows; ask for at least 1")))
        }
        Some(n) => Ok(n.min(MAX_HISTORY_LIMIT)),
    }
}

fn parse_level(raw: Option<&str>) -> Result<Option<Sensitivity>> {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    Sensitivity::parse(s).map(Some).ok_or_else(|| {
        DomainError::validation(format!("sensitivity {s:?} is not one of open, private, sealed"))
    })
}

fn review_after_days(kind: &str) -> i64 {
    REVIEW_AFTER_DAYS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, days)| *days)
        .unwrap_or(DEFAULT_REVIEW_AFTER_DAYS)
}

fn names(ceilings: &[crate::domain::policy::NamespaceCeiling]) -> Vec<String> {
    ceilings.iter().map(|c| c.namespace.clone()).collect()
}

/// Most specific first: a project override should beat a global default.
fn precedence(tenant: &str, project: Option<&str>) -> Result<Vec<String>> {
    let base = namespaces::default_read_namespaces(tenant, project)?;
    let project_ns = match project {
        Some(p) if !p.trim().is_empty() => Some(namespaces::project_namespace(p)?),
        _ => None,
    };
    let mut order = Vec::with_capacity(base.len());
    if let Some(p) = &project_ns {
        order.push(p.clone());
    }
    order.extend(base.into_iter().filter(|ns| Some(ns) != project_ns.as_ref()));
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_route_ages_faster_than_a_host() {
        assert!(review_after_days("model-route") < review_after_days("host"));
        assert_eq!(review_after_days("model-route"), 30);
        assert_eq!(review_after_days("host"), 365);
    }

    #[test]
    fn every_canonical_kind_has_an_expectation() {
        for kind in canonical::KINDS {
            assert!(
                REVIEW_AFTER_DAYS.iter().any(|(k, _)| k == kind),
                "kind {kind} has no review expectation, so it would silently take the default"
            );
        }
    }

    #[test]
    fn an_unknown_kind_takes_the_default_rather_than_never_ageing() {
        assert_eq!(review_after_days("something-new"), DEFAULT_REVIEW_AFTER_DAYS);
    }

    #[test]
    fn the_project_namespace_is_walked_before_the_defaults() {
        let order = precedence("me", Some("lumberroom")).unwrap();
        assert_eq!(order[0], "project:lumberroom");
        assert_eq!(order.iter().filter(|ns| *ns == "project:lumberroom").count(), 1);
    }

    #[test]
    fn a_client_without_the_capability_cannot_read_history_at_all() {
        let mut p = Principal::empty("browser");
        p.read = crate::domain::policy::NamespaceGrant::everything();
        p.registry_write = true;
        // A grant over the whole store, at sealed, and it still does not reach what a key used to
        // hold. That is the point of the axis being separate.
        assert!(assert_may_read_history(&p).is_err());

        p.may_read_history = true;
        assert!(assert_may_read_history(&p).is_ok());
    }

    #[test]
    fn the_refusal_names_the_client_and_not_the_key() {
        let err = assert_may_read_history(&Principal::empty("browser")).unwrap_err();
        let message = format!("{err}");
        assert!(message.contains("browser"), "{message}");
    }

    #[test]
    fn a_history_read_is_bounded_whether_or_not_the_caller_bounds_it() {
        assert_eq!(resolve_limit(None).unwrap(), DEFAULT_HISTORY_LIMIT);
        assert_eq!(resolve_limit(Some(1)).unwrap(), 1);
        assert_eq!(resolve_limit(Some(MAX_HISTORY_LIMIT + 1)).unwrap(), MAX_HISTORY_LIMIT);
        assert_eq!(resolve_limit(Some(i64::MAX)).unwrap(), MAX_HISTORY_LIMIT);
    }

    #[test]
    fn a_limit_of_zero_is_refused_rather_than_answered_with_nothing() {
        assert!(resolve_limit(Some(0)).is_err());
        assert!(resolve_limit(Some(-5)).is_err());
    }

    #[test]
    fn an_unrecognised_sensitivity_is_refused_on_a_registry_write() {
        assert!(parse_level(Some("secret")).is_err());
        assert_eq!(parse_level(Some("private")).unwrap(), Some(Sensitivity::Private));
        assert_eq!(parse_level(None).unwrap(), None);
    }
}
