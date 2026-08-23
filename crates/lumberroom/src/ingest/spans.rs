//! Span cutting and chunking, shared by both parsers.
//!
//! A parser decides what survives and who said it. This decides how the survivors are grouped, and
//! it is the same rule for both formats.

use crate::ingest::{ChunkRef, Limits, Source, Span, Speaker};

/// What separates two entries inside one span.
const JOIN: &str = "\n\n";

/// One entry that survived every exclusion, with its speaker decided.
#[derive(Debug, Clone)]
pub struct ClassifiedEntry {
    pub uuid: Option<String>,
    pub speaker: Speaker,
    pub tool_name: Option<String>,
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
    pub cwd: Option<String>,
    pub byte_start: i64,
    pub byte_end: i64,
    pub text: String,
}

/// A contiguous run of entries sharing a speaker, capped at `INGEST_SPAN_CHARS`.
///
/// `next_index` is threaded through so span ids stay unique across every file in a run: the
/// extractor references them by id alone and a collision silently reattributes a fact.
pub fn cut(
    entries: &[ClassifiedEntry],
    file_path: &str,
    session_id: Option<&str>,
    is_sidechain: bool,
    source: Source,
    include_tool_output: bool,
    limits: &Limits,
    next_index: &mut usize,
) -> Vec<Span> {
    let cap = limits.span_chars.max(1);
    let mut out: Vec<Span> = vec![];
    let mut group: Vec<&ClassifiedEntry> = vec![];
    let mut group_chars = 0usize;

    // A tool name change breaks a span even when the speaker holds: one span carrying a `WebFetch`
    // result under a `Read` label attributes a fact to a file that was never read.
    fn breaks(prev: &ClassifiedEntry, next: &ClassifiedEntry) -> bool {
        prev.speaker != next.speaker || prev.tool_name != next.tool_name
    }

    for entry in entries {
        if !entry.speaker.reaches_extraction(include_tool_output) {
            continue;
        }
        if entry.text.trim().is_empty() {
            continue;
        }
        let chars = entry.text.chars().count();
        // The join separator counts. Leaving it out lets a group land two characters over the cap
        // and get windowed into a second span mid-sentence.
        let added = if group.is_empty() { chars } else { chars + JOIN.chars().count() };
        let split = match group.last() {
            None => false,
            Some(prev) => breaks(prev, entry) || group_chars + added > cap,
        };
        if split {
            flush(&group, file_path, session_id, is_sidechain, source, cap, next_index, &mut out);
            group.clear();
            group.push(entry);
            group_chars = chars;
            continue;
        }
        group.push(entry);
        group_chars += added;
    }
    flush(&group, file_path, session_id, is_sidechain, source, cap, next_index, &mut out);
    out
}

/// Turn one accumulated group into spans. A single entry longer than the cap is windowed rather
/// than dropped or truncated: losing the tail of a long design message loses the decision in it.
#[allow(clippy::too_many_arguments)]
fn flush(
    group: &[&ClassifiedEntry],
    file_path: &str,
    session_id: Option<&str>,
    is_sidechain: bool,
    source: Source,
    cap: usize,
    next_index: &mut usize,
    out: &mut Vec<Span>,
) {
    let Some(first) = group.first() else { return };
    let last = group[group.len() - 1];
    let text = group.iter().map(|e| e.text.as_str()).collect::<Vec<_>>().join(JOIN);
    let uuids: Vec<String> = group.iter().filter_map(|e| e.uuid.clone()).collect();

    for window in windows(&text, cap) {
        let id = format!("s{}", *next_index);
        *next_index += 1;
        out.push(Span {
            id,
            file_path: file_path.to_string(),
            session_id: session_id.map(|s| s.to_string()),
            is_sidechain,
            source,
            entry_uuids: uuids.clone(),
            byte_start: first.byte_start,
            byte_end: last.byte_end,
            speaker: first.speaker,
            tool_name: first.tool_name.clone(),
            timestamp: first.timestamp,
            cwd: first.cwd.clone(),
            text: window,
        });
    }
}

/// Split on character boundaries, never bytes. A cut inside a multi-byte character would panic on
/// a slice and mangle the text on anything else.
fn windows(text: &str, cap: usize) -> Vec<String> {
    let mut out = vec![];
    let mut current = String::new();
    let mut count = 0usize;
    for c in text.chars() {
        current.push(c);
        count += 1;
        if count == cap {
            out.push(std::mem::take(&mut current));
            count = 0;
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Up to 40 spans or 24,000 characters, whichever comes first, and spans from one session stay
/// together so a chunk reads as a conversation.
pub fn chunk(spans: &[Span], limits: &Limits) -> Vec<ChunkRef> {
    let max_spans = limits.chunk_spans.max(1);
    let max_chars = limits.chunk_chars.max(1);
    let mut out: Vec<ChunkRef> = vec![];
    let mut current: Vec<String> = vec![];
    let mut chars = 0usize;
    let mut session: Option<String> = None;

    for span in spans {
        let n = span.text.chars().count();
        let session_changed =
            !current.is_empty() && session.as_deref() != span.session_id.as_deref();
        let full = current.len() >= max_spans || (!current.is_empty() && chars + n > max_chars);
        if session_changed || full {
            out.push(ChunkRef { index: out.len(), span_ids: std::mem::take(&mut current) });
            chars = 0;
        }
        if current.is_empty() {
            session = span.session_id.clone();
        }
        current.push(span.id.clone());
        chars += n;
    }
    if !current.is_empty() {
        out.push(ChunkRef { index: out.len(), span_ids: current });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(speaker: Speaker, text: &str, start: i64, end: i64) -> ClassifiedEntry {
        ClassifiedEntry {
            uuid: Some(format!("u{start}")),
            speaker,
            tool_name: None,
            timestamp: None,
            cwd: None,
            byte_start: start,
            byte_end: end,
            text: text.to_string(),
        }
    }

    fn limits(span_chars: usize, chunk_spans: usize, chunk_chars: usize) -> Limits {
        Limits {
            span_chars,
            chunk_spans,
            chunk_chars,
            max_line_bytes: 1024,
            max_files: 100,
            max_entries: 1000,
            retention_days: 7,
        }
    }

    fn run(entries: &[ClassifiedEntry], l: &Limits, tool_output: bool) -> Vec<Span> {
        let mut i = 0;
        cut(entries, "/f.jsonl", Some("sess"), false, Source::Claude, tool_output, l, &mut i)
    }

    #[test]
    fn consecutive_entries_of_one_speaker_join() {
        let l = limits(6000, 40, 24_000);
        let spans = run(
            &[
                entry(Speaker::OwnerTyped, "one", 0, 10),
                entry(Speaker::OwnerTyped, "two", 10, 20),
                entry(Speaker::MainModel, "three", 20, 30),
            ],
            &l,
            false,
        );
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "one\n\ntwo");
        assert_eq!(spans[0].byte_start, 0);
        assert_eq!(spans[0].byte_end, 20);
        assert_eq!(spans[0].entry_uuids, vec!["u0".to_string(), "u10".to_string()]);
        assert_eq!(spans[1].speaker, Speaker::MainModel);
    }

    #[test]
    fn ids_stay_unique_across_files() {
        let l = limits(6000, 40, 24_000);
        let mut i = 0;
        let a = cut(
            &[entry(Speaker::OwnerTyped, "a", 0, 1)],
            "/a.jsonl",
            None,
            false,
            Source::Claude,
            false,
            &l,
            &mut i,
        );
        let b = cut(
            &[entry(Speaker::OwnerTyped, "b", 0, 1)],
            "/b.jsonl",
            None,
            false,
            Source::Claude,
            false,
            &l,
            &mut i,
        );
        assert_eq!(a[0].id, "s0");
        assert_eq!(b[0].id, "s1");
        assert_eq!(i, 2);
    }

    #[test]
    fn the_char_cap_splits_a_run() {
        let l = limits(12, 40, 24_000);
        let spans = run(
            &[
                entry(Speaker::MainModel, "aaaaa", 0, 5),
                entry(Speaker::MainModel, "bbbbb", 5, 10),
                entry(Speaker::MainModel, "ccccc", 10, 15),
            ],
            &l,
            false,
        );
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "aaaaa\n\nbbbbb");
        assert_eq!(spans[1].text, "ccccc");
    }

    #[test]
    fn a_single_over_long_entry_is_windowed_not_lost() {
        let l = limits(4, 40, 24_000);
        let spans = run(&[entry(Speaker::MainModel, "abcdefghij", 0, 20)], &l, false);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].text, "abcd");
        assert_eq!(spans[2].text, "ij");
        assert!(spans.iter().all(|s| s.byte_start == 0 && s.byte_end == 20));
    }

    #[test]
    fn tool_output_is_dropped_unless_asked_for() {
        let l = limits(6000, 40, 24_000);
        let mut e = entry(Speaker::ToolReturned, "stdout", 0, 5);
        e.tool_name = Some("Read".into());
        assert!(run(&[e.clone()], &l, false).is_empty());
        assert_eq!(run(&[e], &l, true).len(), 1);
    }

    #[test]
    fn a_tool_name_change_breaks_a_span() {
        let l = limits(6000, 40, 24_000);
        let mut a = entry(Speaker::ToolReturned, "one", 0, 5);
        a.tool_name = Some("Read".into());
        let mut b = entry(Speaker::ToolReturned, "two", 5, 9);
        b.tool_name = Some("WebFetch".into());
        let spans = run(&[a, b], &l, true);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].tool_name.as_deref(), Some("Read"));
        assert_eq!(spans[1].tool_name.as_deref(), Some("WebFetch"));
    }

    #[test]
    fn blank_entries_never_become_spans() {
        let l = limits(6000, 40, 24_000);
        let spans = run(
            &[entry(Speaker::OwnerTyped, "   \n ", 0, 5), entry(Speaker::OwnerTyped, "real", 5, 9)],
            &l,
            false,
        );
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "real");
        assert_eq!(spans[0].byte_start, 5);
    }

    fn span_of(id: &str, session: Option<&str>, text: &str) -> Span {
        Span {
            id: id.to_string(),
            file_path: "/f.jsonl".into(),
            session_id: session.map(|s| s.to_string()),
            is_sidechain: false,
            source: Source::Claude,
            entry_uuids: vec![],
            byte_start: 0,
            byte_end: 1,
            speaker: Speaker::MainModel,
            tool_name: None,
            timestamp: None,
            cwd: None,
            text: text.to_string(),
        }
    }

    #[test]
    fn chunking_stops_at_the_span_cap() {
        let l = limits(6000, 2, 24_000);
        let spans: Vec<Span> = (0..5).map(|i| span_of(&format!("s{i}"), Some("a"), "x")).collect();
        let chunks = chunk(&spans, &l);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].span_ids, vec!["s0".to_string(), "s1".to_string()]);
        assert_eq!(chunks[2].span_ids, vec!["s4".to_string()]);
        assert_eq!(chunks[2].index, 2);
    }

    #[test]
    fn chunking_stops_at_the_char_cap() {
        let l = limits(6000, 40, 10);
        let spans = vec![
            span_of("s0", Some("a"), "aaaaa"),
            span_of("s1", Some("a"), "bbbbb"),
            span_of("s2", Some("a"), "ccccc"),
        ];
        let chunks = chunk(&spans, &l);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].span_ids.len(), 2);
    }

    #[test]
    fn a_session_change_starts_a_new_chunk() {
        let l = limits(6000, 40, 24_000);
        let spans = vec![
            span_of("s0", Some("a"), "x"),
            span_of("s1", Some("b"), "y"),
            span_of("s2", Some("b"), "z"),
        ];
        let chunks = chunk(&spans, &l);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].span_ids, vec!["s0".to_string()]);
        assert_eq!(chunks[1].span_ids, vec!["s1".to_string(), "s2".to_string()]);
    }
}
