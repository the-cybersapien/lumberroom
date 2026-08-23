//! The recursive walk over transcript roots.
//!
//! The layout is nested and it moved once already: 576 subagent files sit under
//! `<project>/<session-uuid>/subagents/` and `<project>/<session-uuid>/subagents/workflows/wf_*/`,
//! and the research this was designed from describes a flat directory. So the walk recurses to an
//! unbounded depth and refuses symlinks at every level rather than only at the top.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::client::{err, Result};
use crate::ingest::{Limits, PlanCounters, Source};

#[derive(Debug, Clone)]
pub struct WalkOpts {
    /// Match against the `cwd` field on the first entry carrying one, as a path or a slug.
    pub project: Option<String>,
    /// File mtime plus the first timestamp inside.
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    pub max_files: usize,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: PathBuf,
    pub source: Source,
    /// From the basename for a subagent file, from the first entry otherwise. A session id does not
    /// identify a file: one session id spans nine files in this corpus.
    pub session_id: Option<String>,
    pub is_sidechain: bool,
    pub size: u64,
}

/// Components that never reach an extractor, matched case-insensitively as a substring of any one
/// path component. A project slug encodes the whole cwd with dashes, so `secrets` in the directory
/// name is the handle that catches `~/secrets/app`.
const SENSITIVE: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".kube",
    "secrets",
    "credentials",
    "id_rsa",
    ".env",
    ".pem",
    ".p12",
    "keychain",
];

/// The roots for a source. `~/.claude/projects` and `~/.codex/sessions`.
///
/// The two overrides are how a test points this at a fixture tree. They take one path each.
pub fn roots(source: Source) -> Result<Vec<PathBuf>> {
    let (var, suffix) = match source {
        Source::Claude => ("LUMBERROOM_CLAUDE_ROOT", ".claude/projects"),
        Source::Codex => ("LUMBERROOM_CODEX_ROOT", ".codex/sessions"),
    };
    if let Ok(dir) = std::env::var(var) {
        if !dir.trim().is_empty() {
            return Ok(vec![PathBuf::from(dir.trim())]);
        }
    }
    let home = std::env::var("HOME")
        .map_err(|_| err(format!("HOME is not set, so {var} has to name the transcript root")))?;
    Ok(vec![PathBuf::from(home).join(suffix)])
}

/// Walk, filter and sort. Sets `traversal_capped` on the counters when `max_files` fires, because
/// silent partial coverage of a corpus reads exactly like complete coverage.
pub fn walk(
    roots: &[PathBuf],
    source: Source,
    opts: &WalkOpts,
    limits: &Limits,
    counters: &mut PlanCounters,
) -> Result<Vec<Candidate>> {
    let mut found: Vec<(PathBuf, SystemTime, u64)> = vec![];
    for root in roots {
        descend(root, opts, counters, &mut found);
    }

    // Newest first, and the path breaks a tie so two runs over one corpus plan the same order.
    found.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let cap = opts.max_files.min(limits.max_files).max(1);
    let mut out: Vec<Candidate> = vec![];
    for (i, (path, _, size)) in found.iter().enumerate() {
        if out.len() >= cap {
            counters.traversal_capped = true;
            *counters.files_skipped.entry("capped".to_string()).or_insert(0) +=
                (found.len() - i) as i32;
            break;
        }
        let header = peek_header(path);
        let (session_id, cwd, first_ts) = header.unwrap_or((None, None, None));

        if let Some(want) = &opts.project {
            if !project_matches(want, cwd.as_deref(), path) {
                continue;
            }
        }
        // The mtime half of `--since` already ran in `descend`. This is the half that needs the
        // first entry: a file touched today can hold a conversation from March.
        if let (Some(since), Some(ts)) = (opts.since, first_ts) {
            if ts < since {
                continue;
            }
        }

        let sidechain = is_sidechain_path(path);
        out.push(Candidate {
            path: path.clone(),
            source,
            session_id: session_id.or_else(|| session_from_path(path)),
            is_sidechain: sidechain,
            size: *size,
        });
    }

    counters.files_seen += out.len() as i32;
    Ok(out)
}

/// One directory level. Every entry goes through `symlink_metadata`, so a symlink is refused
/// whatever it points at and a link loop cannot exist.
fn descend(
    dir: &Path,
    opts: &WalkOpts,
    counters: &mut PlanCounters,
    out: &mut Vec<(PathBuf, SystemTime, u64)>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            counters.skip("unreadable");
            continue;
        };
        if meta.file_type().is_symlink() {
            counters.skip("symlink");
            continue;
        }
        if meta.is_dir() {
            // Only the component being entered, never the whole path. Testing the whole path would
            // refuse every file under a root that happens to sit below a directory named `secrets`.
            if component_is_sensitive(&path) {
                counters.skip("sensitive_path");
                continue;
            }
            descend(&path, opts, counters, out);
            continue;
        }
        if !meta.is_file() || path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if component_is_sensitive(&path) {
            counters.skip("sensitive_path");
            continue;
        }
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if let Some(since) = opts.since {
            // Out of the date window is not a skip. Counting every file the owner did not ask for
            // turns a one-week run into a report that reads like a corpus-wide failure.
            if chrono::DateTime::<chrono::Utc>::from(mtime) < since {
                continue;
            }
        }
        out.push((path, mtime, meta.len()));
    }
}

/// Read the first few entries for the fields the walk cannot see from the outside.
///
/// Returns the session id, the cwd and the first timestamp. Bounded to the head of the file: this
/// runs once per candidate and the corpus holds a 96.2MB transcript.
pub fn peek_header(
    path: &Path,
) -> Option<(Option<String>, Option<String>, Option<chrono::DateTime<chrono::Utc>>)> {
    const HEAD_BYTES: u64 = 256 * 1024;
    const HEAD_LINES: usize = 8;

    let mut session_id: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut first_ts: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut seen = 0usize;

    // The callback stops the read by returning an error, which is the only brake `for_each_line`
    // offers. Reading 256KB of every one of 5000 files to answer three questions is the cost of
    // not having one.
    let _ = crate::ingest::reader::for_each_line(path, 0, HEAD_BYTES, 1024 * 1024, |line, _, _| {
        seen += 1;
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if session_id.is_none() {
                session_id = string_field(&v, "sessionId");
            }
            if cwd.is_none() {
                cwd = string_field(&v, "cwd");
            }
            if first_ts.is_none() {
                first_ts = string_field(&v, "timestamp")
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                    .map(|t| t.with_timezone(&chrono::Utc));
            }
        }
        let done = seen >= HEAD_LINES
            || (session_id.is_some() && cwd.is_some() && first_ts.is_some());
        if done {
            Err(err("peek complete"))
        } else {
            Ok(())
        }
    });

    if session_id.is_none() && cwd.is_none() && first_ts.is_none() && seen == 0 {
        return None;
    }
    Some((session_id, cwd, first_ts))
}

fn string_field(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string())
}

/// `--project` accepts a path or a slug. Claude Code encodes the cwd into the project directory
/// name by replacing every separator with a dash, so both forms have to reach the same file.
fn project_matches(want: &str, cwd: Option<&str>, path: &Path) -> bool {
    let want = want.trim().trim_end_matches('/');
    if want.is_empty() {
        return true;
    }
    let wanted_slug = slug(want);
    if let Some(cwd) = cwd {
        let cwd = cwd.trim_end_matches('/');
        if cwd == want || cwd.ends_with(want) || slug(cwd) == wanted_slug {
            return true;
        }
    }
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s == want || s == wanted_slug || slug(&s) == wanted_slug
    })
}

fn slug(s: &str) -> String {
    s.chars().map(|c| if c == '/' || c == '.' || c == '_' { '-' } else { c }).collect()
}

fn is_sidechain_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with("agent-"))
        .unwrap_or(false)
}

/// A last resort when the first entry carries no `sessionId`. For a subagent file the parent
/// session is the `<session-uuid>` directory above `subagents/`; for a main thread it is the stem.
fn session_from_path(path: &Path) -> Option<String> {
    if is_sidechain_path(path) {
        let mut dir = path.parent();
        while let Some(d) = dir {
            if d.file_name().and_then(|n| n.to_str()) == Some("subagents") {
                return d
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string());
            }
            dir = d.parent();
        }
        return None;
    }
    path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string())
}

fn component_is_sensitive(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return false };
    let lowered = name.to_lowercase();
    SENSITIVE.iter().any(|m| lowered.contains(m))
}

/// Paths whose contents never reach an extractor, refused before the file is opened.
pub fn is_sensitive_path(path: &std::path::Path) -> bool {
    path.components().any(|c| {
        let lowered = c.as_os_str().to_string_lossy().to_lowercase();
        SENSITIVE.iter().any(|m| lowered.contains(m))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("lumberroom-walk-{}-{}-{}", std::process::id(), name, line!()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn opts() -> WalkOpts {
        WalkOpts { project: None, since: None, max_files: 100 }
    }

    fn limits() -> Limits {
        Limits {
            span_chars: 6000,
            chunk_spans: 40,
            chunk_chars: 24_000,
            max_line_bytes: 1024 * 1024,
            max_files: 5000,
            max_entries: 1000,
            retention_days: 7,
        }
    }

    #[test]
    fn sensitive_components_are_refused() {
        assert!(is_sensitive_path(Path::new("/home/a/.ssh/notes.jsonl")));
        assert!(is_sensitive_path(Path::new("/home/a/-Users-a-secrets/x.jsonl")));
        assert!(is_sensitive_path(Path::new("/h/a/KeyChain/x.jsonl")));
        assert!(is_sensitive_path(Path::new("/h/a/p/id_rsa.pub.jsonl")));
        assert!(is_sensitive_path(Path::new("/h/a/p/thing.env.jsonl")));
        assert!(!is_sensitive_path(Path::new("/home/a/.claude/projects/p/s.jsonl")));
        assert!(!is_sensitive_path(Path::new("/home/a/environment/s.jsonl")));
    }

    #[test]
    fn a_symlink_is_refused_and_counted() {
        let dir = tmp("symlink");
        let real = dir.join("real.jsonl");
        std::fs::write(&real, b"{\"sessionId\":\"s1\"}\n").unwrap();
        std::os::unix::fs::symlink(&real, dir.join("link.jsonl")).unwrap();

        let mut counters = PlanCounters::default();
        let found = walk(&[dir.clone()], Source::Claude, &opts(), &limits(), &mut counters).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, real);
        assert_eq!(counters.files_skipped.get("symlink").copied(), Some(1));
    }

    #[test]
    fn a_symlinked_directory_is_refused() {
        let dir = tmp("symdir");
        let inner = dir.join("real");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("a.jsonl"), b"{}\n").unwrap();
        std::os::unix::fs::symlink(&inner, dir.join("mirror")).unwrap();

        let mut counters = PlanCounters::default();
        let found = walk(&[dir], Source::Claude, &opts(), &limits(), &mut counters).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(counters.files_skipped.get("symlink").copied(), Some(1));
    }

    #[test]
    fn the_walk_reaches_nested_subagent_files() {
        let dir = tmp("nested");
        let deep = dir.join("proj/9f/subagents/workflows/wf_1");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("agent-abc.jsonl"), b"{}\n").unwrap();
        std::fs::write(dir.join("proj/9f.jsonl"), b"{}\n").unwrap();

        let mut counters = PlanCounters::default();
        let found = walk(&[dir], Source::Claude, &opts(), &limits(), &mut counters).unwrap();
        assert_eq!(found.len(), 2);
        let agent = found.iter().find(|c| c.is_sidechain).unwrap();
        assert_eq!(agent.session_id.as_deref(), Some("9f"));
        let main = found.iter().find(|c| !c.is_sidechain).unwrap();
        assert_eq!(main.session_id.as_deref(), Some("9f"));
        assert_eq!(counters.files_seen, 2);
    }

    #[test]
    fn a_sensitive_directory_is_never_entered() {
        let dir = tmp("sensitive");
        let bad = dir.join("-Users-a-secrets");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("a.jsonl"), b"{}\n").unwrap();
        std::fs::write(dir.join("ok.jsonl"), b"{}\n").unwrap();

        let mut counters = PlanCounters::default();
        let found = walk(&[dir], Source::Claude, &opts(), &limits(), &mut counters).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(counters.files_skipped.get("sensitive_path").copied(), Some(1));
    }

    #[test]
    fn the_file_cap_reports_itself() {
        let dir = tmp("cap");
        for i in 0..4 {
            std::fs::write(dir.join(format!("f{i}.jsonl")), b"{}\n").unwrap();
        }
        let mut counters = PlanCounters::default();
        let mut o = opts();
        o.max_files = 2;
        let found = walk(&[dir], Source::Claude, &o, &limits(), &mut counters).unwrap();
        assert_eq!(found.len(), 2);
        assert!(counters.traversal_capped);
        assert_eq!(counters.files_skipped.get("capped").copied(), Some(2));
    }

    #[test]
    fn the_root_override_wins() {
        std::env::set_var("LUMBERROOM_CLAUDE_ROOT", "/tmp/lumberroom-fixture-root");
        assert_eq!(roots(Source::Claude).unwrap(), vec![PathBuf::from("/tmp/lumberroom-fixture-root")]);
        std::env::remove_var("LUMBERROOM_CLAUDE_ROOT");
    }

    #[test]
    fn peek_reads_the_header_fields() {
        let dir = tmp("peek");
        let path = dir.join("s.jsonl");
        std::fs::write(
            &path,
            b"{\"type\":\"system\"}\n{\"sessionId\":\"abc\",\"cwd\":\"/w/p\",\"timestamp\":\"2026-08-01T10:00:00.000Z\"}\n",
        )
        .unwrap();
        let (session, cwd, ts) = peek_header(&path).unwrap();
        assert_eq!(session.as_deref(), Some("abc"));
        assert_eq!(cwd.as_deref(), Some("/w/p"));
        assert_eq!(ts.unwrap().to_rfc3339(), "2026-08-01T10:00:00+00:00");
    }

    #[test]
    fn the_project_filter_takes_a_path_or_a_slug() {
        let p = Path::new("/h/.claude/projects/-Users-a-work-warden/9f.jsonl");
        assert!(project_matches("/Users/a/work/warden", Some("/Users/a/work/warden"), p));
        assert!(project_matches("-Users-a-work-warden", None, p));
        assert!(!project_matches("/Users/a/work/other", Some("/Users/a/work/warden"), p));
    }
}
