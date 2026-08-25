//! Parsing a memory dump into proposals.
//!
//! The grammar is the one `dump_prompt` asks for, and every rule below exists because a real dump
//! did something. Two runs against ChatGPT on 25 August 2026 are the evidence. The dumps behind that
//! evidence stay off this repository, and so does any example drawn from them: a memory dump is a
//! person's private life in a text file, and a test fixture is a file that gets copied forever.
//!
//! ```text
//! SECTION
//! [YYYY-MM-DD] (stated) - One fact.
//! [unknown] (inferred) - Another.
//! COMPLETE
//! ```
//!
//! # What the parser refuses to trust
//!
//! **The completion marker.** A dump that runs out of room loses `MORE REMAINS` along with the line
//! it was writing: run one ended mid-word with neither marker present. So a missing marker is read
//! as truncation, and the last entry before it is dropped rather than stored, because a fact cut in
//! half is still a well-formed line and reads as complete to everything downstream.
//!
//! **A line it does not recognise.** Counted and reported, never skipped in silence. Phase 6 §4.3
//! settled that argument for the transcript walker and it applies here for the same reason: a
//! format change upstream has to surface as a number somebody reads.
//!
//! # Sensitivity is deliberately absent here
//!
//! A dump is not uniformly harmless. Read against a real one, `CAREER` came back as a compensation
//! history, `SETUP` held financial account details, and `IDENTITY` held years of health
//! measurements. Sections differ, and a per-section proposal is the obvious answer.
//!
//! It is not in this module, on purpose. Sensitivity is only worth proposing when the owner can act
//! on the proposal, which needs the console and the CLI to move a row between levels and between
//! namespaces, and that is its own piece of work. Half of it living here, with no way to change what
//! it decided, would be worse than none. Everything imports at the namespace default until then, and
//! the queue is the gate in the meantime.
//!
//! # What this cannot carry yet
//!
//! `ExtractedFact`, the shape `ingest submit` consumes, has no `occurred_at` and no `sensitivity`.
//! Both are the point of importing a dump: a dated line from a two-year-old account is exactly what
//! `occurred_at` exists for, and the paragraph above is why sensitivity cannot be left to a default.
//! `to_extracted_fact` is lossy on both, and wiring the dump into `submit` means widening that
//! contract first. Nothing here writes to the store.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ingest::ExtractedFact;

/// Where a section's facts belong.
///
/// A section this table does not name keeps its facts, counts them, and files them under `user:me`:
/// an unknown section means a drifted prompt or a model that improvised, and neither is a reason to
/// throw the owner's facts away.
fn route(section: &str) -> &'static str {
    match section {
        "INSTRUCTIONS" | "IDENTITY" | "CAREER" | "DECISIONS" | "PREFERENCES" => "user:me",
        "SETUP" => "global",
        // PROJECTS routes per entry in `namespace_for`, which needs the content to find the name.
        "PROJECTS" => "project",
        _ => "user:me",
    }
}

/// Words that open a sentence without naming anything. A `PROJECTS` line starting with one of these
/// has no slug to take, so the entry goes to `user:me` and the owner routes it in the queue.
const NOT_A_NAME: &[&str] = &["the", "a", "an", "my", "our", "this", "his", "her", "their", "its"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Stated,
    Inferred,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stated => "stated",
            Self::Inferred => "inferred",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DumpEntry {
    pub content: String,
    pub namespace: String,
    pub section: String,
    pub confidence: Confidence,
    /// `YYYY-MM-DD`, absent when the line said `[unknown]`.
    pub occurred_at: Option<String>,
    /// 1-based, so it matches what an editor shows the owner.
    pub line: usize,
}

impl DumpEntry {
    /// Lossy on `occurred_at`, which `ExtractedFact` has no field for. Saying so here beats dropping
    /// a date silently: 88 of the 119 entries in one real dump carried one, and a dated line out of
    /// an old account is exactly what valid time exists for.
    pub fn to_extracted_fact(&self, source: &str) -> ExtractedFact {
        ExtractedFact {
            content: self.content.clone(),
            namespace: self.namespace.clone(),
            tags: vec!["dump".to_string(), self.section.to_lowercase()],
            source_span_id: format!("{source}:{}", self.line),
            speaker: None,
            quote: None,
            confidence: Some(self.confidence.as_str().to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ending {
    Complete,
    MoreRemains,
    /// No marker at all, which means the answer was cut off.
    Truncated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DumpReport {
    pub entries: Vec<DumpEntry>,
    pub ending: Ending,
    pub per_section: BTreeMap<String, usize>,
    /// Lines that are neither blank, a section header, an entry, nor the marker, with line numbers.
    pub unrecognised: Vec<(usize, String)>,
    /// The entry dropped because the dump was truncated and it may be half a fact.
    pub dropped_partial: Option<String>,
    /// Entries whose date fell after the day of the run, with the date the line carried. The fact
    /// survives and its date does not, because a future `occurred_at` reads live and never reads
    /// as-of, so the fact would be visible in one query and denied in the other with nothing to
    /// say why. A model writing the wrong year and the owner mistyping one look identical here,
    /// and the owner is the only one who can tell them apart.
    pub future_dated: Vec<(usize, String)>,
}

impl DumpReport {
    pub fn needs_a_second_pass(&self) -> bool {
        !matches!(self.ending, Ending::Complete)
    }
}

fn is_section_header(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 4
        && t != "COMPLETE"
        && t != "MORE REMAINS"
        && t.chars().all(|c| c.is_ascii_uppercase() || c == ' ' || c == '&' || c == '-')
        && t.chars().any(|c| c.is_ascii_uppercase())
}

/// `[2026-08-18] (stated) - text` and the two ways each field is allowed to vary.
fn parse_entry(line: &str) -> Option<(Option<String>, Confidence, String)> {
    let t = line.trim();
    let rest = t.strip_prefix('[')?;
    let (date_raw, rest) = rest.split_once(']')?;
    let occurred_at = match date_raw {
        "unknown" => None,
        d if is_iso_date(d) => Some(d.to_string()),
        _ => return None,
    };

    let rest = rest.trim_start();
    // A marker is optional. Claude's exported memory files write bare bullets, and Anthropic's own
    // import prompt has no marker at all, so a line without one is `stated`: both formats mean
    // "this is something I stored" when they leave it off.
    let (confidence, rest) = if let Some(r) = rest.strip_prefix("(stated)") {
        (Confidence::Stated, r)
    } else if let Some(r) = rest.strip_prefix("(inferred)") {
        (Confidence::Inferred, r)
    } else {
        (Confidence::Stated, rest)
    };

    let content = rest.trim_start().strip_prefix('-')?.trim();
    if content.is_empty() {
        return None;
    }
    Some((occurred_at, confidence, content.to_string()))
}

fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..].iter().all(u8::is_ascii_digit)
}

/// A `PROJECTS` line opens with the project name, which the prompt asks for and which is the only
/// thing in a dump that can name a project namespace.
fn project_slug(content: &str) -> Option<String> {
    let first = content.split_whitespace().next()?;

    // Strip the possessive before anything else. Filtering punctuation first turns `Foo's` into
    // `foos`, and a real dump referred to the same project both ways inside one section: four
    // namespaces appeared where two belonged. Both apostrophes, because a model emits the curly one.
    let base = first.trim_end_matches(|c: char| c.is_ascii_punctuation() || c == '\u{2019}');
    let base = base
        .strip_suffix("'s")
        .or_else(|| base.strip_suffix("\u{2019}s"))
        .or_else(|| base.strip_suffix("'S"))
        .unwrap_or(base);

    // ASCII only, and anything else becomes a dash. `char::is_alphanumeric` accepts the whole
    // Unicode alphanumeric set, and the server does not: `domain::namespaces::valid_segment` wants
    // an ASCII alphanumeric first character and ASCII alphanumerics, `.`, `_` or `-` after it. A
    // project called `Café-Roaster` produced `project:café-roaster`, which every entry for that
    // project was then refused for, counted in with tripwire refusals, and lost with no way for the
    // owner to see why. This mirrors `domain::namespaces::project_slug`, which the client crate
    // cannot call across the workspace boundary.
    let mut cleaned = String::with_capacity(base.len());
    let mut last_dash = false;
    for c in base.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            cleaned.push(c);
            last_dash = false;
        } else if !last_dash {
            cleaned.push('-');
            last_dash = true;
        }
    }
    let cleaned = cleaned.trim_matches('-').chars().take(127).collect::<String>();

    // A leading digit or symbol is refused by `valid_segment` too, so a name that reduces to one is
    // no name at all.
    if cleaned.len() < 2
        || NOT_A_NAME.contains(&cleaned.as_str())
        || !cleaned.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(cleaned)
}

fn namespace_for(section: &str, content: &str) -> String {
    let base = route(section);
    if base == "project" {
        return match project_slug(content) {
            Some(slug) => format!("project:{slug}"),
            None => "user:me".to_string(),
        };
    }
    base.to_string()
}

pub fn parse(text: &str) -> DumpReport {
    parse_as_of(text, chrono::Utc::now().date_naive())
}

/// `parse` with the day supplied, so the future-date bound has a fixed clock in tests.
pub fn parse_as_of(text: &str, today: chrono::NaiveDate) -> DumpReport {
    let mut entries: Vec<DumpEntry> = Vec::new();
    let mut per_section: BTreeMap<String, usize> = BTreeMap::new();
    let mut unrecognised: Vec<(usize, String)> = Vec::new();
    let mut section = String::from("UNSECTIONED");
    let mut ending = Ending::Truncated;

    for (i, raw) in text.lines().enumerate() {
        let line_no = i + 1;
        let t = raw.trim();
        if t.is_empty() {
            continue;
        }
        if t == "COMPLETE" {
            ending = Ending::Complete;
            continue;
        }
        if t == "MORE REMAINS" {
            ending = Ending::MoreRemains;
            continue;
        }
        if is_section_header(t) {
            section = t.to_string();
            per_section.entry(section.clone()).or_insert(0);
            continue;
        }
        match parse_entry(t) {
            Some((occurred_at, confidence, content)) => {
                let namespace = namespace_for(&section, &content);
                *per_section.entry(section.clone()).or_insert(0) += 1;
                entries.push(DumpEntry {
                    content,
                    namespace,
                    section: section.clone(),
                    confidence,
                    occurred_at,
                    line: line_no,
                });
            }
            None => unrecognised.push((line_no, t.chars().take(80).collect())),
        }
    }

    // A truncated dump ends wherever the model ran out, and the last line it managed is as likely to
    // be half a sentence as a whole one. Half a fact is still well formed, so nothing downstream
    // would ever catch it.
    let dropped_partial = if matches!(ending, Ending::Truncated) {
        entries.pop().map(|e| {
            if let Some(n) = per_section.get_mut(&e.section) {
                *n = n.saturating_sub(1);
            }
            e.content
        })
    } else {
        None
    };

    // The date goes, the fact stays. Dropping the line would lose something the owner wrote; keeping
    // the date would store a fact dated next year that no as-of read can ever return.
    let mut future_dated = Vec::new();
    for e in &mut entries {
        let Some(raw) = e.occurred_at.clone() else { continue };
        let after_today =
            chrono::NaiveDate::parse_from_str(&raw, "%Y-%m-%d").map(|d| d > today).unwrap_or(false);
        if after_today {
            future_dated.push((e.line, raw));
            e.occurred_at = None;
        }
    }

    DumpReport { entries, ending, per_section, unrecognised, dropped_partial, future_dated }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Invented content, real shape. Every quirk here was observed in a live dump on 25 August
    /// 2026 and then rewritten with facts about nobody. A real dump is somebody's private life,
    /// so none of one reaches this repository, not as a fixture and not as an example in a comment.
    ///
    /// The quirks it reproduces, in order of appearance: a section with no entries, a bare line
    /// with no `(marker)`, both markers, `[unknown]` beside real dates, a `PROJECTS` line opening
    /// with a name and another opening with an article, and a stray line that is not an entry.
    const SHAPED: &str = "\
INSTRUCTIONS
[unknown] (stated) - Answer in short sentences.
[2026-01-02] - Check sources before quoting a number.
[2026-01-03] (inferred) - Prefers metric units.

IDENTITY
[1990-05-06] (stated) - Sam Rivers was born in Leeds.
[unknown] (inferred) - Sam Rivers lives near the coast.

CAREER
[2020-02-01] (inferred) - Sam Rivers joined a logistics firm as an analyst.

SETUP
[unknown] (stated) - The office laptop runs Debian 13.

EMPTYSECTION

PROJECTS
[2025-03-04] (stated) - Tidepool is a tide-table app for small boats.
[2025-06-07] (inferred) - The seabird survey was a weekend data-collection experiment.

DECISIONS
[2025-09-09] (stated) - Chose SQLite over Postgres for Tidepool because it ships as one file.

PREFERENCES
[unknown] (stated) - Prefers a dark terminal.
this line is not an entry at all
COMPLETE";

    fn find<'a>(r: &'a DumpReport, needle: &str) -> &'a DumpEntry {
        r.entries.iter().find(|e| e.content.contains(needle)).expect("entry not found")
    }

    #[test]
    fn every_entry_is_read_and_the_stray_line_is_reported_not_dropped() {
        let r = parse(SHAPED);
        assert_eq!(r.entries.len(), 11, "entries: {:?}", r.entries.len());
        assert_eq!(r.unrecognised.len(), 1);
        assert!(r.unrecognised[0].1.contains("not an entry"));
        assert_eq!(r.ending, Ending::Complete);
        assert!(r.dropped_partial.is_none());
    }

    #[test]
    fn a_line_with_no_marker_reads_as_stated() {
        let r = parse(SHAPED);
        assert_eq!(find(&r, "Check sources").confidence, Confidence::Stated);
        assert_eq!(find(&r, "Prefers metric").confidence, Confidence::Inferred);
    }

    #[test]
    fn a_date_after_the_run_loses_the_date_and_keeps_the_fact() {
        let text = "NOTES\n[2027-03-12] (stated) - the renewal moved to a yearly plan\n\
                    [2026-08-20] (stated) - the plan was monthly before that\nCOMPLETE\n";
        let today = chrono::NaiveDate::from_ymd_opt(2026, 8, 25).unwrap();
        let r = parse_as_of(text, today);

        assert_eq!(r.entries.len(), 2, "the fact survives, only its date goes");
        assert_eq!(find(&r, "renewal moved").occurred_at, None);
        assert_eq!(r.future_dated, vec![(2, "2027-03-12".to_string())]);
        // Today itself is not the future, and a past date is untouched.
        assert_eq!(find(&r, "monthly before").occurred_at.as_deref(), Some("2026-08-20"));
    }

    #[test]
    fn a_date_becomes_occurred_at_and_unknown_becomes_nothing() {
        let r = parse(SHAPED);
        assert_eq!(find(&r, "born in Leeds").occurred_at.as_deref(), Some("1990-05-06"));
        assert_eq!(find(&r, "near the coast").occurred_at, None);
    }

    #[test]
    fn sections_route_to_the_namespaces_they_belong_in() {
        let r = parse(SHAPED);
        assert_eq!(find(&r, "Answer in short").namespace, "user:me");
        assert_eq!(find(&r, "office laptop").namespace, "global");
        assert_eq!(find(&r, "Tidepool is a tide-table").namespace, "project:tidepool");
    }

    /// A project line that opens with an article names nothing, and inventing `project:the` would
    /// be worse than sending it to the queue in `user:me` for the owner to route.
    /// A real dump wrote about one project as both `Tidepool is` and `Tidepool's schema`, and the
    /// first version of this parser filed those under two namespaces.
    #[test]
    fn a_possessive_does_not_fork_a_project_into_two_namespaces() {
        let r = parse(
            "PROJECTS\n\
             [unknown] (stated) - Tidepool is a tide-table app.\n\
             [unknown] (stated) - Tidepool's schema is one table.\n\
             [unknown] (stated) - Tidepool\u{2019}s installer is a shell script.\n\
             COMPLETE",
        );
        assert_eq!(r.entries.len(), 3);
        for e in &r.entries {
            assert_eq!(e.namespace, "project:tidepool", "{} went to {}", e.content, e.namespace);
        }
    }

    /// The server refuses a namespace with a non-ASCII character in it, and it refuses it one entry
    /// at a time inside a counter that also holds tripwire refusals, so the loss is invisible.
    #[test]
    fn a_project_name_outside_ascii_still_produces_a_namespace_the_server_accepts() {
        let r = parse(
            "PROJECTS\n\
             [unknown] (stated) - Caf\u{e9}-Roaster tracks bean stock.\n\
             [unknown] (stated) - \u{5317}\u{4eac} is a mapping side project.\n\
             [unknown] (stated) - 2048-Solver plays the tile game.\n\
             COMPLETE",
        );
        let cafe = &r.entries[0];
        assert_eq!(cafe.namespace, "project:caf-roaster");
        for e in &r.entries {
            let seg = e.namespace.strip_prefix("project:").unwrap_or("me");
            assert!(
                seg.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()),
                "{} starts with something the server refuses",
                e.namespace
            );
            assert!(
                seg.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
                "{} holds a character the server refuses",
                e.namespace
            );
        }
        // A name that survives as nothing but punctuation is no name, and inventing one would file
        // the fact under a namespace the owner never chose.
        assert_eq!(r.entries[1].namespace, "user:me");
    }

    #[test]
    fn a_project_line_with_no_name_in_it_does_not_invent_a_namespace() {
        let r = parse(SHAPED);
        assert_eq!(find(&r, "seabird survey").namespace, "user:me");
    }

    #[test]
    fn an_unknown_section_keeps_its_facts_rather_than_dropping_them() {
        let r = parse("WEATHER\n[unknown] (stated) - It rained.\nCOMPLETE");
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0].namespace, "user:me");
        assert_eq!(r.entries[0].section, "WEATHER");
    }

    /// Run one on ChatGPT ended mid-word with neither marker. The half-written line is still a
    /// well-formed entry, so nothing downstream would catch it.
    #[test]
    fn a_dump_with_no_marker_is_truncated_and_gives_up_its_last_entry() {
        let cut = "IDENTITY\n[unknown] (stated) - A whole fact.\n[2026-01-01] (stated) - A fact cut in ha";
        let r = parse(cut);
        assert_eq!(r.ending, Ending::Truncated);
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0].content, "A whole fact.");
        assert_eq!(r.dropped_partial.as_deref(), Some("A fact cut in ha"));
        assert!(r.needs_a_second_pass());
        assert_eq!(r.per_section.get("IDENTITY"), Some(&1), "the count must lose it too");
    }

    #[test]
    fn more_remains_asks_for_a_second_pass_and_keeps_every_entry() {
        let r = parse("IDENTITY\n[unknown] (stated) - A whole fact.\nMORE REMAINS");
        assert_eq!(r.ending, Ending::MoreRemains);
        assert_eq!(r.entries.len(), 1);
        assert!(r.needs_a_second_pass());
        assert!(r.dropped_partial.is_none());
    }

    #[test]
    fn a_malformed_date_is_reported_rather_than_guessed() {
        let r = parse("IDENTITY\n[2026-13] (stated) - Not a date.\nCOMPLETE");
        assert_eq!(r.entries.len(), 0);
        assert_eq!(r.unrecognised.len(), 1);
    }

    #[test]
    fn the_extracted_fact_carries_the_section_and_confidence_and_says_where_it_came_from() {
        let r = parse(SHAPED);
        let f = find(&r, "Tidepool is a tide-table").to_extracted_fact("dump:example");
        assert_eq!(f.namespace, "project:tidepool");
        assert_eq!(f.confidence.as_deref(), Some("stated"));
        assert!(f.tags.contains(&"dump".to_string()));
        assert!(f.tags.contains(&"projects".to_string()));
        assert!(f.source_span_id.starts_with("dump:example:"));
    }

    #[test]
    fn an_empty_dump_reports_truncated_and_no_entries() {
        let r = parse("");
        assert_eq!(r.entries.len(), 0);
        assert_eq!(r.ending, Ending::Truncated);
        assert!(r.dropped_partial.is_none());
    }
}
