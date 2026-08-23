//! The two-axis grant model. Namespace answers *whose facts*; sensitivity answers *how exposed*.
//!
//! One ceiling is not enough. Work notes and personal finance can both be `private` while a work
//! agent must see one and never the other, which is why a grant is a list of per-namespace
//! ceilings rather than a namespace list plus a global maximum (system PRD §4.5, Phase 3 spec §1).
//!
//! No I/O here, and nothing imports an adapter. The rules are pure so they can be tested as rules.

use serde::{Deserialize, Deserializer, Serialize};

use crate::domain::namespaces;
use crate::domain::types::Sensitivity;

/// One entry in a grant: a namespace glob and the highest sensitivity it admits.
///
/// A bare string deserialises to a ceiling of `open`. Defaulting to the lowest ceiling means a
/// grant written before the sensitivity axis existed never silently gains access when the axis
/// lands, which is the whole reason the migration is additive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NamespaceGrant {
    pub namespace: String,
    pub max: Sensitivity,
}

impl NamespaceGrant {
    pub fn open(namespace: impl Into<String>) -> Self {
        Self { namespace: namespace.into(), max: Sensitivity::Open }
    }

    pub fn new(namespace: impl Into<String>, max: Sensitivity) -> Self {
        Self { namespace: namespace.into(), max }
    }

    /// Everything, at every level. The Phase 1 default for the owner's own client.
    pub fn everything() -> Vec<Self> {
        vec![Self::new("*", Sensitivity::Sealed)]
    }
}

impl<'de> Deserialize<'de> for NamespaceGrant {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Object {
            namespace: String,
            #[serde(default)]
            max: Option<Sensitivity>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Either {
            Bare(String),
            Object(Object),
        }

        Ok(match Either::deserialize(d)? {
            Either::Bare(namespace) => NamespaceGrant { namespace, max: Sensitivity::Open },
            Either::Object(o) => {
                NamespaceGrant { namespace: o.namespace, max: o.max.unwrap_or(Sensitivity::Open) }
            }
        })
    }
}

/// A concrete namespace paired with the ceiling this caller holds for it. Resolved before the
/// query runs, so the filter is expressible in SQL and a row the caller may not see never enters
/// the caller's process memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceCeiling {
    pub namespace: String,
    pub max: Sensitivity,
}

/// The highest ceiling any matching pattern grants, or `None` when nothing matches.
///
/// Union rather than intersection across patterns: two entries matching one namespace is the
/// caller having been granted both, and the more generous one is what they were granted.
pub fn ceiling(grants: &[NamespaceGrant], namespace: &str) -> Option<Sensitivity> {
    grants.iter().filter(|g| namespaces::matches(&g.namespace, namespace)).map(|g| g.max).max()
}

/// The globs alone, for the paths that still reason about namespaces only.
pub fn patterns(grants: &[NamespaceGrant]) -> Vec<String> {
    grants.iter().map(|g| g.namespace.clone()).collect()
}

/// Narrow a requested namespace list to what the grant admits, carrying each ceiling through.
///
/// Reads narrow silently; a namespace the caller cannot reach is dropped rather than refused,
/// because a search that 403s because one namespace was out of reach is a search that never works.
pub fn resolve(grants: &[NamespaceGrant], requested: &[String]) -> Vec<NamespaceCeiling> {
    requested
        .iter()
        .filter_map(|ns| {
            ceiling(grants, ns).map(|max| NamespaceCeiling { namespace: ns.clone(), max })
        })
        .collect()
}

/// True when this grant admits `sensitivity` in `namespace`.
pub fn admits(grants: &[NamespaceGrant], namespace: &str, sensitivity: Sensitivity) -> bool {
    ceiling(grants, namespace).is_some_and(|max| sensitivity <= max)
}

/// Namespace-to-default classification, longest matching pattern first.
///
/// Classification is inferred by default and manual only by exception. The PRD is blunt about why:
/// if using the system means classifying every sentence, the system has failed at the product
/// level regardless of how well it works technically (§9).
#[derive(Debug, Clone, Default)]
pub struct SensitivityDefaults {
    rules: Vec<(String, Sensitivity)>,
}

impl SensitivityDefaults {
    /// Sorted longest-pattern-first on construction, so `credentials:*` beats `*` without the
    /// caller having to order the input.
    pub fn new(mut rules: Vec<(String, Sensitivity)>) -> Self {
        rules.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        Self { rules }
    }

    /// The Phase 3 spec §2 table, used when the database has no rows yet.
    pub fn seeded() -> Self {
        Self::new(vec![
            ("*".into(), Sensitivity::Open),
            ("global".into(), Sensitivity::Open),
            ("project:*".into(), Sensitivity::Open),
            ("user:me".into(), Sensitivity::Open),
            ("personal:finance".into(), Sensitivity::Private),
            ("personal:health".into(), Sensitivity::Private),
            ("credentials:*".into(), Sensitivity::Sealed),
        ])
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn rules(&self) -> &[(String, Sensitivity)] {
        &self.rules
    }

    /// Absent any matching rule the answer is `open`, matching what every Phase 1 row already is.
    pub fn for_namespace(&self, namespace: &str) -> Sensitivity {
        self.rules
            .iter()
            .find(|(pattern, _)| namespaces::matches(pattern, namespace))
            .map(|(_, level)| *level)
            .unwrap_or(Sensitivity::Open)
    }

    /// What a write is stored at. A caller may raise the level above the namespace default and may
    /// never lower it: a tool that can downgrade classification is a tool that can leak by mistake.
    pub fn resolve_for_write(
        &self,
        namespace: &str,
        requested: Option<Sensitivity>,
    ) -> Sensitivity {
        let floor = self.for_namespace(namespace);
        match requested {
            Some(asked) if asked > floor => asked,
            _ => floor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grants(spec: &[(&str, Sensitivity)]) -> Vec<NamespaceGrant> {
        spec.iter().map(|(ns, max)| NamespaceGrant::new(*ns, *max)).collect()
    }

    #[test]
    fn a_bare_string_grant_means_open_only() {
        let g: Vec<NamespaceGrant> = serde_json::from_str(r#"["user:me","global"]"#).unwrap();
        assert_eq!(g[0], NamespaceGrant::new("user:me", Sensitivity::Open));
        assert_eq!(g[1].max, Sensitivity::Open);
    }

    #[test]
    fn an_object_grant_carries_its_ceiling() {
        let g: Vec<NamespaceGrant> =
            serde_json::from_str(r#"[{"namespace":"*","max":"sealed"}]"#).unwrap();
        assert_eq!(g[0].max, Sensitivity::Sealed);
    }

    #[test]
    fn an_object_without_a_max_still_means_open() {
        let g: Vec<NamespaceGrant> = serde_json::from_str(r#"[{"namespace":"user:me"}]"#).unwrap();
        assert_eq!(g[0].max, Sensitivity::Open);
    }

    #[test]
    fn the_two_grant_formats_mix_in_one_list() {
        let g: Vec<NamespaceGrant> =
            serde_json::from_str(r#"["global",{"namespace":"user:me","max":"private"}]"#).unwrap();
        assert_eq!(g[0].max, Sensitivity::Open);
        assert_eq!(g[1].max, Sensitivity::Private);
    }

    #[test]
    fn a_ceiling_admits_everything_at_or_below_it() {
        let g = grants(&[("user:me", Sensitivity::Private)]);
        assert!(admits(&g, "user:me", Sensitivity::Open));
        assert!(admits(&g, "user:me", Sensitivity::Private));
        assert!(!admits(&g, "user:me", Sensitivity::Sealed));
    }

    #[test]
    fn nothing_matching_means_no_access_rather_than_open_access() {
        let g = grants(&[("user:me", Sensitivity::Sealed)]);
        assert_eq!(ceiling(&g, "personal:finance"), None);
        assert!(!admits(&g, "personal:finance", Sensitivity::Open));
    }

    #[test]
    fn an_empty_grant_admits_nothing() {
        assert!(!admits(&[], "global", Sensitivity::Open));
        assert!(resolve(&[], &["global".to_string()]).is_empty());
    }

    #[test]
    fn overlapping_patterns_take_the_higher_ceiling() {
        let g = grants(&[("*", Sensitivity::Open), ("project:*", Sensitivity::Private)]);
        assert_eq!(ceiling(&g, "project:lumberroom"), Some(Sensitivity::Private));
        assert_eq!(ceiling(&g, "global"), Some(Sensitivity::Open));
    }

    #[test]
    fn resolve_drops_what_the_grant_does_not_reach_and_keeps_the_rest() {
        let g = grants(&[("user:me", Sensitivity::Private), ("global", Sensitivity::Open)]);
        let asked = vec!["user:me".into(), "global".into(), "personal:health".into()];
        let out = resolve(&g, &asked);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].max, Sensitivity::Private);
        assert_eq!(out[1].max, Sensitivity::Open);
    }

    #[test]
    fn the_longest_default_pattern_wins_regardless_of_input_order() {
        let d = SensitivityDefaults::new(vec![
            ("*".into(), Sensitivity::Open),
            ("credentials:*".into(), Sensitivity::Sealed),
        ]);
        assert_eq!(d.for_namespace("credentials:aws"), Sensitivity::Sealed);
        assert_eq!(d.for_namespace("global"), Sensitivity::Open);
    }

    #[test]
    fn an_unlisted_namespace_defaults_to_open() {
        assert_eq!(SensitivityDefaults::default().for_namespace("whatever"), Sensitivity::Open);
    }

    #[test]
    fn a_write_may_raise_its_level_above_the_default() {
        let d = SensitivityDefaults::seeded();
        assert_eq!(
            d.resolve_for_write("user:me", Some(Sensitivity::Private)),
            Sensitivity::Private
        );
    }

    #[test]
    fn a_write_may_never_lower_its_level_below_the_default() {
        let d = SensitivityDefaults::seeded();
        assert_eq!(
            d.resolve_for_write("personal:finance", Some(Sensitivity::Open)),
            Sensitivity::Private,
            "a tool that can downgrade classification is a tool that can leak by mistake"
        );
        assert_eq!(d.resolve_for_write("credentials:aws", None), Sensitivity::Sealed);
    }
}
