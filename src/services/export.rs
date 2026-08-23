//! The Obsidian mirror, Phase 4 §5. One markdown note per fact, so the whole store is browsable
//! without an AI tool in the loop.
//!
//! **One way only.** The database is the record of truth and the vault is a window onto it. Nothing
//! reads edits back out. Two-way sync is a separate decision with a conflict model attached, not an
//! extension of this.
//!
//! **This module renders; it does not write.** It returns files as data and a manifest of the paths
//! it owns. The writer's contract is in [`ExportResult::manifest`], and the one rule in it is that
//! the writer never deletes: a row that left the database becomes a tombstone note. A tool that
//! deletes files in a personal vault gets one chance to be wrong.
//!
//! **Sensitivity bounds it.** `sealed` cannot be exported at all, and `private` content in a vault
//! synced to a third party defeats the encryption it was given, so the ceiling defaults to whatever
//! `EXPORT_MAX_SENSITIVITY` says and can only be narrowed by the caller, never widened.

use serde::Serialize;
use std::collections::HashMap;

use super::Ctx;
use crate::adapters::auth::can_read;
use crate::domain::errors::{DomainError, Result};
use crate::domain::types::{Memory, RegistryEntry, Sensitivity};

/// Everything this export owns lives under one folder, so the writer can never touch a note the
/// owner wrote by hand.
pub const ROOT: &str = "lumberroom";

/// Rows per page out of the repository. The export is a batch job and this only bounds memory.
const PAGE: i64 = 500;

/// Total rows one export will render. A vault with fifty thousand notes is not browsable anyway, and
/// an unbounded loop against a growing table is how a cron job becomes an incident.
const MAX_ROWS: i64 = 20_000;

/// Longest slug taken from content. Long enough to recognise, short enough for a file listing.
const SLUG_CHARS: usize = 48;

#[derive(Debug, Clone, Serialize)]
pub struct ExportFile {
    /// Relative to the vault root, always beginning with `lumberroom/`.
    pub path: String,
    pub contents: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportResult {
    /// The ceiling actually applied, after the config bound and the caller's request.
    pub max_sensitivity: Sensitivity,
    pub memories: usize,
    pub registry: usize,
    /// What this caller may read and this run left out anyway: the private rows that would not open,
    /// and the registry entries above the export ceiling. Reported rather than silently missing,
    /// because a vault that is quietly incomplete is worse than one that says what it left out.
    ///
    /// **Rows outside the grant are not counted here, and counting them was a leak.** This number is
    /// published, `list_for_export` takes no grant, and the difference between the page it returns and
    /// the page that survives `can_read` is the number of rows the caller may not see. A client
    /// granted `user:me` learned how much of the store its grant excludes, one number per export, with
    /// no attacker in it.
    ///
    /// Memory rows above the ceiling contribute nothing today: `list_for_export` takes the same
    /// ceiling and applies it in SQL, so the pass below it is belt and braces rather than a category.
    pub excluded: usize,
    pub files: Vec<ExportFile>,
    /// Every path this run produced.
    ///
    /// The writer's contract: write each file in `files`, then compare the export folder against
    /// this list. A file under `lumberroom/` that is not in the manifest belongs to a row that left the
    /// database. Replace its body with [`tombstone`] and never unlink it.
    pub manifest: Vec<String>,
}

/// Render the vault.
///
/// `max_sensitivity` may narrow the configured ceiling and never widens it, so turning the export on
/// for private content is a deployment decision rather than a per-call argument.
pub async fn run(
    ctx: &Ctx,
    max_sensitivity: Option<&str>,
    limit: Option<i64>,
) -> Result<ExportResult> {
    let configured = ctx.cfg.quality.export_max_sensitivity;
    let ceiling = match max_sensitivity.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => {
            let asked = Sensitivity::parse(raw).ok_or_else(|| {
                DomainError::validation(format!(
                    "sensitivity {raw:?} is not one of open, private, sealed"
                ))
            })?;
            asked.min(configured)
        }
        None => configured,
    };
    // Belt and braces. `sealed` content lives in another table and has no plaintext to render, so a
    // configuration that asked for it is a configuration error rather than an instruction.
    let ceiling = ceiling.min(Sensitivity::Private);

    let cap = limit.unwrap_or(MAX_ROWS).clamp(1, MAX_ROWS);
    let mut rows: Vec<Memory> = Vec::new();
    let mut offset = 0i64;
    loop {
        let page = ctx
            .repos
            .memories
            .list_for_export(ctx.tenant(), ceiling, PAGE.min(cap - offset), offset)
            .await?;
        let got = page.len() as i64;
        rows.extend(page);
        offset += got;
        if got < PAGE || offset >= cap {
            break;
        }
    }

    // The repository bounded by sensitivity; the grant is this caller's and is checked here, because
    // `list_for_export` takes no ceilings and an export run by a restricted client must not mirror
    // more than that client can read.
    //
    // Counted in two steps, and the order is the point. `excluded` is published, so nothing outside
    // the grant may reach it: one length taken before the grant filter and one after would publish
    // the size of everything the grant excludes, which is the disclosure the digest inventory made
    // with a namespace name attached. The ceiling pass below is belt and braces against a repository
    // that ignored the ceiling it was handed, so it normally adds zero.
    rows.retain(|m| can_read(&ctx.principal, &m.namespace, m.sensitivity));
    let before = rows.len();
    rows.retain(|m| m.sensitivity <= ceiling);
    let mut excluded = before - rows.len();

    // Private rows arrive without their plaintext. A row that will not open is left out rather than
    // written as an empty note: a vault full of blank files is worse than a vault that is short.
    let unopened = super::decrypt(ctx, rows.iter_mut().collect()).await;
    if !unopened.is_empty() {
        rows.retain(|m| !unopened.contains(&m.id));
        excluded += unopened.len();
    }

    let entries = ctx.repos.registry.list(ctx.tenant(), &readable(ctx).await?).await?;
    let registry: Vec<RegistryEntry> = entries
        .into_iter()
        .filter(|e| {
            let keep = e.sensitivity <= ceiling;
            if !keep {
                excluded += 1;
            }
            keep
        })
        .collect();

    // Note names first, so a `supersedes` link can point at a real file instead of a bare uuid.
    let names: HashMap<String, String> =
        rows.iter().map(|m| (m.id.clone(), memory_stem(m))).collect();

    let mut files: Vec<ExportFile> = Vec::with_capacity(rows.len() + registry.len() + 1);
    for m in &rows {
        files.push(ExportFile { path: memory_path(m), contents: memory_note(m, &names) });
    }
    for e in &registry {
        files.push(ExportFile { path: registry_path(e), contents: registry_note(e) });
    }
    files.push(ExportFile {
        path: format!("{ROOT}/index.md"),
        contents: index_note(&rows, &registry, ceiling),
    });

    // Deterministic order, so two runs over an unchanged store produce byte-identical output and a
    // vault in git shows an empty diff.
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest = files.iter().map(|f| f.path.clone()).collect();

    Ok(ExportResult {
        max_sensitivity: ceiling,
        memories: rows.len(),
        registry: registry.len(),
        excluded,
        files,
        manifest,
    })
}

/// Every namespace this caller may read, with its ceiling.
///
/// Built from the namespaces that hold memories plus the default read set, because no port
/// enumerates registry namespaces on their own. A registry entry in a namespace holding no memories
/// and outside the defaults is therefore missed by the export, which is a gap worth naming rather
/// than papering over: it is the same list the digest inventory is built from.
async fn readable(ctx: &Ctx) -> Result<Vec<crate::domain::policy::NamespaceCeiling>> {
    let counts = ctx.repos.memories.namespace_counts(ctx.tenant()).await?;
    let mut all: Vec<String> = counts.into_keys().collect();
    all.extend(crate::domain::namespaces::default_read_namespaces(&ctx.cfg.tenant_id, None)?);
    all.sort();
    crate::domain::namespaces::dedupe(&mut all);
    Ok(crate::adapters::auth::filter_readable(&ctx.principal, &all))
}

/// What a note becomes when its row is gone.
///
/// The frontmatter keeps the id so the note is still findable, and the body says what happened. A
/// deleted file would take any backlink in the vault with it.
pub fn tombstone(path: &str, id: &str, deleted_at: &str) -> ExportFile {
    let contents = format!(
        "---\nlumberroom_id: {}\nlumberroom_status: deleted\ndeleted_noticed_at: {}\n---\n\n\
         This fact is no longer in the store. The note is kept so links to it still resolve; \
         lumberroom never deletes a file in a vault.\n",
        yaml(id),
        yaml(deleted_at)
    );
    ExportFile { path: path.to_string(), contents }
}

// -- paths -------------------------------------------------------------------------------------
//
// Deterministic and stable: the same row renders to the same path on every run, which is what makes
// the export idempotent. The short id is the last segment so two facts that slugify identically
// cannot collide, and it is short because the whole point is a readable file listing.

fn memory_path(m: &Memory) -> String {
    format!("{ROOT}/memory/{}/{}.md", slug(&m.namespace), memory_stem(m))
}

fn memory_stem(m: &Memory) -> String {
    let date = m.created_at.format("%Y-%m-%d");
    let body = slug(&m.content);
    let short = short_id(&m.id);
    if body.is_empty() {
        format!("{date}-{short}")
    } else {
        format!("{date}-{body}-{short}")
    }
}

/// The namespace is in the path because the registry's whole precedence model is the same kind and
/// key existing in `global` and in `project:x` at once. Without it the two collapse onto one file and
/// whichever sorts last silently wins.
fn registry_path(e: &RegistryEntry) -> String {
    format!("{ROOT}/registry/{}/{}/{}.md", slug(&e.kind), slug(&e.namespace), slug(&e.key))
}

fn short_id(id: &str) -> String {
    id.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect()
}

/// Lowercase, ascii, dash-joined. Non-ascii characters are dropped rather than transliterated:
/// guessing at a romanisation would make the filename unstable across a library upgrade, and the
/// short id already guarantees uniqueness.
fn slug(text: &str) -> String {
    let mut out = String::with_capacity(SLUG_CHARS);
    let mut dash = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.extend(ch.to_lowercase());
            if out.chars().count() >= SLUG_CHARS {
                break;
            }
        } else {
            dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

// -- notes -------------------------------------------------------------------------------------

fn memory_note(m: &Memory, names: &HashMap<String, String>) -> String {
    let mut fm = Vec::new();
    fm.push(format!("lumberroom_id: {}", yaml(&m.id)));
    fm.push(format!("namespace: {}", yaml(&m.namespace)));
    fm.push(format!("sensitivity: {}", m.sensitivity));
    fm.push(format!("source_client: {}", yaml(&m.source_client)));
    fm.push(format!("created_at: {}", yaml(&m.created_at.to_rfc3339())));
    // Provenance is the half fuzzy memory cannot answer, so it goes in the frontmatter rather than
    // being left to whoever reads the body.
    fm.push(format!("confirmed: {}", m.last_confirmed_at.is_some()));
    if let Some(t) = m.last_confirmed_at {
        fm.push(format!("last_confirmed_at: {}", yaml(&t.to_rfc3339())));
    }
    fm.push(format!("access_count: {}", m.access_count));
    if !m.tags.is_empty() {
        fm.push(format!(
            "tags: [{}]",
            m.tags.iter().map(|t| yaml(t)).collect::<Vec<_>>().join(", ")
        ));
    }
    // Wikilinks mean the decision log is navigable in the graph view, which is the one thing
    // Obsidian does better than a database.
    if let Some(old) = &m.supersedes {
        fm.push(format!("supersedes: {}", link(old, names)));
    }
    if let Some(new) = &m.superseded_by {
        fm.push(format!("superseded_by: {}", link(new, names)));
        fm.push("lumberroom_status: superseded".to_string());
    }

    let mut body = String::new();
    if m.superseded_by.is_some() {
        // The note stays because history is the point, and it says so at the top so a reader
        // skimming the vault does not act on a retired fact.
        body.push_str("> Retired. A later fact replaced this one.\n\n");
    }
    body.push_str(m.content.trim());
    body.push('\n');

    format!("---\n{}\n---\n\n{body}", fm.join("\n"))
}

fn registry_note(e: &RegistryEntry) -> String {
    let value = serde_json::to_string_pretty(&e.value).unwrap_or_else(|_| e.value.to_string());
    let fm = vec![
        format!("kind: {}", yaml(&e.kind)),
        format!("key: {}", yaml(&e.key)),
        format!("namespace: {}", yaml(&e.namespace)),
        format!("sensitivity: {}", e.sensitivity),
        format!("version: {}", e.version),
        format!("source_client: {}", yaml(&e.provenance.source_client)),
        format!("confirmed: {}", e.provenance.user_confirmed),
        format!("valid_from: {}", yaml(&e.provenance.valid_from)),
    ];
    // Fenced, because a registry value is JSON and Obsidian would otherwise render braces as text
    // it has opinions about.
    format!("---\n{}\n---\n\n```json\n{value}\n```\n", fm.join("\n"))
}

fn index_note(rows: &[Memory], registry: &[RegistryEntry], ceiling: Sensitivity) -> String {
    let mut by_namespace: HashMap<&str, usize> = HashMap::new();
    for m in rows {
        *by_namespace.entry(m.namespace.as_str()).or_default() += 1;
    }
    let mut namespaces: Vec<(&&str, &usize)> = by_namespace.iter().collect();
    namespaces.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));

    let mut lines = vec![
        "---\nlumberroom_generated: true\n---\n".to_string(),
        "# lumberroom".to_string(),
        String::new(),
        format!(
            "{} memories and {} registry entries, up to sensitivity {ceiling}. \
             Generated from the database, which is the record of truth: edits here are not read back.",
            rows.len(),
            registry.len()
        ),
    ];
    if !namespaces.is_empty() {
        lines.push(String::new());
        lines.push("## Namespaces".to_string());
        for (ns, n) in namespaces {
            lines.push(format!("- `{ns}`: {n}"));
        }
    }
    lines.push(String::new());
    lines.join("\n")
}

/// A wikilink when the target is in this export, the bare id when it is not: a link to a note that
/// was never written is a broken link, and a uuid at least says what to look for.
fn link(id: &str, names: &HashMap<String, String>) -> String {
    match names.get(id) {
        Some(stem) => format!("\"[[{stem}]]\""),
        None => yaml(id),
    }
}

/// A double-quoted YAML scalar. Quoting everything is what stops a value beginning with `[`, `{`,
/// `*` or `%` from being parsed as structure by whatever reads the frontmatter next.
fn yaml(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' | '\r' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn memory(id: &str, content: &str) -> Memory {
        Memory {
            occurred_at: None,
            occurred_until: None,
            id: id.into(),
            namespace: "user:me".into(),
            content: content.into(),
            tags: vec!["preference".into()],
            source_client: "claude-code-mac".into(),
            embedding_model: Some("hash".into()),
            sensitivity: Sensitivity::Open,
            supersedes: None,
            superseded_by: None,
            superseded_at: None,
            access_count: 3,
            last_accessed_at: None,
            last_confirmed_at: None,
            created_at: chrono::Utc.with_ymd_and_hms(2026, 8, 19, 9, 0, 0).unwrap(),
        }
    }

    fn entry(namespace: &str) -> RegistryEntry {
        RegistryEntry {
            namespace: namespace.into(),
            kind: "host".into(),
            key: "machines.desktop.os".into(),
            value: serde_json::json!("Ubuntu 26.04"),
            provenance: crate::domain::types::Provenance {
                source_client: "claude-code-mac".into(),
                conv_id: None,
                confidence: 1.0,
                user_confirmed: true,
                valid_from: "2026-08-19".into(),
            },
            sensitivity: Sensitivity::Open,
            version: 2,
            resolved_from: None,
        }
    }

    #[test]
    fn the_same_registry_key_in_two_namespaces_gets_two_files() {
        assert_ne!(
            registry_path(&entry("global")),
            registry_path(&entry("project:lumberroom")),
            "the registry's precedence model is the same key in two namespaces at once"
        );
        assert_eq!(
            registry_path(&entry("global")),
            "lumberroom/registry/host/global/machines-desktop-os.md"
        );
    }

    #[test]
    fn a_registry_note_carries_its_provenance_and_fences_the_value() {
        let note = registry_note(&entry("global"));
        assert!(note.contains("version: 2"));
        assert!(note.contains("source_client: \"claude-code-mac\""));
        assert!(note.contains("```json"));
    }

    #[test]
    fn a_note_path_is_deterministic_and_lives_under_one_folder() {
        let m = memory("0f4a1b2c-dead-beef-0000-111122223333", "Dana prefers TypeScript");
        let path = memory_path(&m);
        assert_eq!(
            path,
            "lumberroom/memory/user-me/2026-08-19-dana-prefers-typescript-0f4a1b2c.md"
        );
        assert_eq!(path, memory_path(&m), "two runs must agree");
        assert!(path.starts_with("lumberroom/"));
    }

    #[test]
    fn two_facts_that_slugify_the_same_do_not_collide() {
        let a = memory("aaaaaaaa-0000-0000-0000-000000000000", "The port is 8080");
        let b = memory("bbbbbbbb-0000-0000-0000-000000000000", "The port is 8787");
        assert_ne!(memory_path(&a), memory_path(&b));
    }

    #[test]
    fn a_fact_with_no_sluggable_content_still_gets_a_path() {
        let m = memory("aaaaaaaa-0000-0000-0000-000000000000", "→ ✓ ←");
        assert_eq!(memory_path(&m), "lumberroom/memory/user-me/2026-08-19-aaaaaaaa.md");
    }

    #[test]
    fn frontmatter_carries_provenance() {
        let note = memory_note(&memory("abc", "Dana prefers TypeScript"), &HashMap::new());
        assert!(note.starts_with("---\n"));
        assert!(note.contains("source_client: \"claude-code-mac\""));
        assert!(note.contains("namespace: \"user:me\""));
        assert!(note.contains("sensitivity: open"));
        assert!(note.contains("confirmed: false"));
        assert!(note.contains("tags: [\"preference\"]"));
        assert!(note.ends_with("Dana prefers TypeScript\n"));
    }

    #[test]
    fn supersedes_renders_as_a_wikilink_when_the_target_is_in_the_export() {
        let mut m = memory("bbb", "The port is 8787");
        m.supersedes = Some("aaa".into());
        let names =
            HashMap::from([("aaa".to_string(), "2026-03-02-the-port-is-8080-aaa".to_string())]);
        assert!(
            memory_note(&m, &names).contains("supersedes: \"[[2026-03-02-the-port-is-8080-aaa]]\"")
        );
    }

    #[test]
    fn supersedes_falls_back_to_the_id_rather_than_a_broken_link() {
        let mut m = memory("bbb", "The port is 8787");
        m.supersedes = Some("aaa".into());
        assert!(memory_note(&m, &HashMap::new()).contains("supersedes: \"aaa\""));
    }

    #[test]
    fn a_retired_note_says_so_at_the_top_and_is_still_written() {
        let mut m = memory("aaa", "The port is 8080");
        m.superseded_by = Some("bbb".into());
        let note = memory_note(&m, &HashMap::new());
        assert!(note.contains("lumberroom_status: superseded"));
        assert!(note.contains("> Retired."));
    }

    #[test]
    fn a_tombstone_keeps_the_id_and_never_implies_the_file_was_removed() {
        let t = tombstone("lumberroom/memory/user-me/x.md", "abc", "2026-08-19T00:00:00Z");
        assert_eq!(t.path, "lumberroom/memory/user-me/x.md");
        assert!(t.contents.contains("lumberroom_status: deleted"));
        assert!(t.contents.contains("never deletes a file"));
    }

    #[test]
    fn a_yaml_value_cannot_break_out_of_its_quotes() {
        assert_eq!(yaml("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(yaml("a\nb"), "\"a b\"");
        assert_eq!(yaml("[not, a, list]"), "\"[not, a, list]\"");
    }

    #[test]
    fn a_slug_collapses_punctuation_runs_and_trims_the_edges() {
        assert_eq!(slug("  The port -- is 8080!  "), "the-port-is-8080");
        assert_eq!(slug("user:me"), "user-me");
        assert_eq!(slug("credential-ref"), "credential-ref");
        assert!(slug(&"word ".repeat(50)).chars().count() <= SLUG_CHARS + 4);
    }
}
