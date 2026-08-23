//! Namespace rules. Ported from the TypeScript, including its tests.
//!
//! Five shapes: `global`, `user:<id>`, `project:<slug>`, `personal:<slug>`, `credentials:<slug>`.
//! Grants are namespace globs so a client can be denied a namespace without touching the
//! authorization path.
//!
//! `personal:*` and `credentials:*` exist here because the classification table classifies them.
//! A shape the sensitivity table calls private or sealed but that validation refuses is a rule that
//! can never fire, which leaves every reachable namespace classified open: the exact failure the
//! system PRD calls fatal. `work:*` stays on the roadmap because nothing classifies it yet, and a
//! shape with no rule behind it buys nothing.

use crate::domain::errors::{DomainError, Result};

/// Accepts a namespace or explains why it was refused. The refusal text reaches the caller, so
/// it names every valid shape rather than saying "invalid".
pub fn normalize(input: &str) -> Result<String> {
    let ns = input.trim().to_ascii_lowercase();
    if is_valid(&ns) {
        Ok(ns)
    } else {
        Err(DomainError::validation(format!(
            "invalid namespace {input:?}. Use 'global', 'user:<id>', 'project:<slug>', \
             'personal:<slug>' or 'credentials:<slug>'."
        )))
    }
}

fn is_valid(ns: &str) -> bool {
    if ns == "global" {
        return true;
    }
    if let Some(rest) = ns.strip_prefix("user:") {
        return valid_segment(rest, 64, false);
    }
    if let Some(rest) = ns.strip_prefix("project:") {
        return valid_segment(rest, 128, true);
    }
    // `personal:finance` and `credentials:aws` name an area of life or a system, not a path, so
    // they take the `user:` rules: bounded at 64 and no slash. A slug here that could contain `/`
    // would let `credentials:aws/prod` and `credentials:aws` look like one place to a reader while
    // the store treats them as two.
    if let Some(rest) = ns.strip_prefix("personal:") {
        return valid_segment(rest, 64, false);
    }
    if let Some(rest) = ns.strip_prefix("credentials:") {
        return valid_segment(rest, 64, false);
    }
    false
}

/// Whether this namespace holds content the client encrypts before it leaves the client.
///
/// The trap: `memory_write` takes plaintext. The classification table sends `credentials:*` to
/// `sealed` and `MAX_WRITE_SENSITIVITY` stops at `private`, so the ceiling check already refuses
/// such a write on a default install. That check reads an operator-editable table, and an operator
/// who replaces the table with rules of their own drops the `credentials:*` row with it, at which
/// point a plaintext credential would be stored at open and stemmed into the lexical index.
/// The shape itself is the caller's stated intent, so it is checked separately and cannot be
/// configured away.
///
/// This is not a validation rule. `lumberroom seal` writes ciphertext to these namespaces through the
/// sealed store, and the registry records where a credential lives. Only the plaintext path refuses.
pub fn requires_client_sealing(namespace: &str) -> bool {
    namespace.trim().to_ascii_lowercase().starts_with("credentials:")
}

/// First character alphanumeric, then a bounded run of the allowed set. `/` is permitted in
/// project slugs so a nested path can round-trip.
fn valid_segment(s: &str, max: usize, allow_slash: bool) -> bool {
    if s.is_empty() || s.len() > max {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    s.chars().all(|c| {
        c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' || (allow_slash && c == '/')
    })
}

/// Turns a directory path, repo name or free text into a project slug.
pub fn project_slug(input: &str) -> Result<String> {
    let trimmed = input.trim().trim_end_matches('/');
    let base = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let mut slug = String::with_capacity(base.len());
    let mut last_dash = false;
    for c in base.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            slug.push(c);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-').chars().take(127).collect::<String>();
    if slug.is_empty() {
        return Err(DomainError::validation(format!(
            "cannot derive a project slug from {input:?}"
        )));
    }
    Ok(slug)
}

pub fn project_namespace(input: &str) -> Result<String> {
    if input.starts_with("project:") {
        normalize(input)
    } else {
        Ok(format!("project:{}", project_slug(input)?))
    }
}

pub fn user_namespace(tenant_id: &str) -> String {
    format!("user:{tenant_id}")
}

/// PRD §5: the default set is the user namespace and global, plus the active project.
pub fn default_read_namespaces(tenant_id: &str, project: Option<&str>) -> Result<Vec<String>> {
    let mut list = vec![user_namespace(tenant_id), "global".to_string()];
    if let Some(p) = project {
        if !p.trim().is_empty() {
            list.push(project_namespace(p)?);
        }
    }
    dedupe(&mut list);
    Ok(list)
}

pub fn dedupe(list: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    list.retain(|item| seen.insert(item.clone()));
}

/// Trailing-wildcard glob. `*` matches everything, `project:*` matches any project.
pub fn matches(pattern: &str, namespace: &str) -> bool {
    let p = pattern.trim().to_ascii_lowercase();
    if p == "*" {
        return true;
    }
    match p.strip_suffix('*') {
        Some(prefix) => namespace.starts_with(prefix),
        None => p == namespace,
    }
}

pub fn allowed<'a>(patterns: &[String], namespaces: &'a [String]) -> Vec<String> {
    namespaces
        .iter()
        .filter(|ns| patterns.iter().any(|p| matches(p, ns)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_three_phase_one_shapes() {
        assert_eq!(normalize("global").unwrap(), "global");
        assert_eq!(normalize("user:me").unwrap(), "user:me");
        assert_eq!(normalize("project:memory-engine").unwrap(), "project:memory-engine");
    }

    /// The regression this pair guards: the classification table and migration 004 both send
    /// `personal:finance`, `personal:health` and `credentials:*` above open, and validation used to
    /// refuse all three. Every rule above open was therefore unreachable and every reachable
    /// namespace classified open.
    #[test]
    fn accepts_the_shapes_the_classification_table_classifies() {
        assert_eq!(normalize("personal:finance").unwrap(), "personal:finance");
        assert_eq!(normalize("personal:health").unwrap(), "personal:health");
        assert_eq!(normalize("credentials:aws").unwrap(), "credentials:aws");
        assert_eq!(normalize("  Credentials:AWS  ").unwrap(), "credentials:aws");
    }

    #[test]
    fn holds_the_new_shapes_to_the_same_slug_rules() {
        assert!(normalize("personal:").is_err());
        assert!(normalize("credentials:").is_err());
        assert!(normalize("personal:-finance").is_err(), "must start alphanumeric");
        assert!(normalize("credentials:has spaces").is_err());
        assert!(normalize("credentials:aws/prod").is_err(), "no slash outside project slugs");
        assert!(normalize(&format!("personal:{}", "a".repeat(65))).is_err());
        assert!(normalize(&format!("personal:{}", "a".repeat(64))).is_ok());
    }

    #[test]
    fn only_credentials_namespaces_demand_client_side_sealing() {
        assert!(requires_client_sealing("credentials:aws"));
        assert!(requires_client_sealing("  Credentials:AWS  "), "checked after normalisation too");
        assert!(!requires_client_sealing("personal:finance"));
        assert!(!requires_client_sealing("global"));
        assert!(!requires_client_sealing("project:credentials"));
    }

    /// A grant pattern from the docs has to reach the namespaces the shapes now admit, or the
    /// console's "add personal:* to chatgpt's read list" is a grant that matches nothing.
    #[test]
    fn the_documented_grant_patterns_match_the_new_shapes() {
        assert!(matches("personal:*", "personal:finance"));
        assert!(matches("credentials:*", "credentials:aws"));
        assert!(!matches("personal:*", "project:personal"));
    }

    #[test]
    fn lowercases_and_trims() {
        assert_eq!(normalize("  User:Me  ").unwrap(), "user:me");
    }

    #[test]
    fn rejects_everything_else() {
        for bad in [
            "", "users:me", "user:", "project:", "user:me;drop table memory",
            "project:has spaces", "global extra",
        ] {
            assert!(normalize(bad).is_err(), "should have rejected {bad:?}");
        }
    }

    #[test]
    fn takes_the_last_path_segment() {
        assert_eq!(project_slug("/Users/example/work/acme/memoryEngine").unwrap(), "memoryengine");
        assert_eq!(project_slug("/Users/example/work/acme/memoryEngine/").unwrap(), "memoryengine");
    }

    #[test]
    fn slugifies_punctuation_and_casing() {
        assert_eq!(project_slug("My Project (v2)").unwrap(), "my-project-v2");
    }

    #[test]
    fn passes_an_existing_namespace_through() {
        assert_eq!(project_namespace("project:already-slugged").unwrap(), "project:already-slugged");
    }

    #[test]
    fn refuses_input_with_nothing_usable() {
        assert!(project_slug("///").is_err());
    }

    #[test]
    fn default_set_is_user_and_global() {
        assert_eq!(default_read_namespaces("me", None).unwrap(), vec!["user:me", "global"]);
    }

    #[test]
    fn default_set_adds_the_active_project() {
        assert_eq!(
            default_read_namespaces("me", Some("/tmp/warden")).unwrap(),
            vec!["user:me", "global", "project:warden"]
        );
    }

    #[test]
    fn default_set_does_not_duplicate() {
        assert_eq!(default_read_namespaces("me", Some("project:warden")).unwrap().len(), 3);
    }

    #[test]
    fn star_matches_everything() {
        assert!(matches("*", "project:anything"));
    }

    #[test]
    fn trailing_wildcard_is_a_prefix() {
        assert!(matches("project:*", "project:warden"));
        assert!(!matches("project:*", "user:me"));
    }

    #[test]
    fn otherwise_exact() {
        assert!(matches("user:me", "user:me"));
        assert!(!matches("user:me", "user:meredith"));
    }

    #[test]
    fn filters_a_requested_set_to_what_is_granted() {
        let patterns = vec!["user:me".to_string(), "global".to_string()];
        let requested = vec!["user:me".into(), "global".into(), "project:x".into()];
        assert_eq!(allowed(&patterns, &requested), vec!["user:me", "global"]);
    }
}
