//! Turning parsed dump entries into proposals, and everything that happens on the way.
//!
//! Four passes, in this order, and the order is the point:
//!
//! 1. **Dedupe inside the dump.** Only inside. The server already refuses a proposal whose content
//!    it holds, answering `reinforced` for a proposal it has and `confirmed` for a memory it has,
//!    so client-side deduplication against the store would be a second implementation of a rule
//!    that already exists one layer down and could disagree with it.
//! 2. **Structural junk.** `junk::deterministic`, which decides nothing about meaning.
//! 3. **The credential tripwire.** Server side, over content only, before anything is posted. A
//!    dump is an assistant reading its notes aloud, and notes hold keys.
//! 4. **Build the requests.** Speaker `main_model`, and the entry's date onto `observed_at`.
//!
//! # Why the date rides on the source rather than the fact
//!
//! `approve` fills a memory's `occurred_at` from `min(observed_at)` across a proposal's sources, and
//! it reaches the write path through `run_observed`, which is exempt from the near-now fence. So a
//! line dated two years ago arrives as valid time with no new column, no migration, and no change to
//! the request shape. The dump's `[YYYY-MM-DD]` is read as midnight UTC, because a date is all the
//! precision the dump has and inventing a time inside the day would be inventing evidence.

use chrono::{NaiveDate, TimeZone, Utc};
use uuid::Uuid;

use crate::client::{err, Client, Result};
use crate::import::dump::{DumpEntry, DumpReport};
use crate::import::junk;
use crate::ingest::api;

/// A dump entry is the assistant's answer about what it stored, which is not the owner typing it.
/// Auto-approval rests on `owner_typed` plus a substring check against a frozen span; a dump has no
/// span, and claiming the stronger speaker to reach the weaker gate would be a lie about provenance
/// that survives in the store forever.
pub const DUMP_SPEAKER: &str = "main_model";

#[derive(Debug, Clone)]
pub struct Dropped {
    pub content: String,
    pub why: String,
}

#[derive(Debug, Default)]
pub struct PlanReport {
    pub kept: Vec<DumpEntry>,
    pub duplicates: Vec<Dropped>,
    pub structural: Vec<Dropped>,
    pub refused: Vec<Dropped>,
    /// Dropped because a model called them junk and the owner agreed. Separate from the structural
    /// list on purpose: one is a rule anybody can check, the other is a judgement somebody made.
    pub judged: Vec<Dropped>,
}

impl PlanReport {
    pub fn dropped_total(&self) -> usize {
        self.duplicates.len() + self.structural.len() + self.refused.len() + self.judged.len()
    }
}

/// Passes one and two. Deterministic, offline, and safe to run before the owner has decided
/// anything.
pub fn sift(entries: Vec<DumpEntry>) -> PlanReport {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut report = PlanReport::default();

    for entry in entries {
        let key = junk::dedupe_key(&entry.content);
        if !seen.insert(key) {
            report
                .duplicates
                .push(Dropped { content: entry.content, why: "already in this dump".to_string() });
            continue;
        }
        if let Some(reason) = junk::deterministic(&entry.content) {
            report
                .structural
                .push(Dropped { content: entry.content, why: reason.as_str().to_string() });
            continue;
        }
        report.kept.push(entry);
    }
    report
}

/// Pass three. The tripwire runs on the server so one implementation decides it, and it answers rule
/// names in arrival order with `null` where nothing fired. The matched text never travels, which is
/// the whole reason a refusal here names a rule and never a value.
pub async fn tripwire(c: &Client, report: &mut PlanReport) -> Result<()> {
    if report.kept.is_empty() {
        return Ok(());
    }
    let texts: Vec<String> = report.kept.iter().map(|e| e.content.clone()).collect();
    let rules = api::scan(c, &texts).await?;
    if rules.len() != texts.len() {
        // A zip over a short answer pairs verdicts with the wrong facts, and the wrong fact here is
        // a credential going into the queue with a clean bill.
        return Err(err(format!(
            "the tripwire scan answered {} rules for {} facts",
            rules.len(),
            texts.len()
        )));
    }

    let mut kept = Vec::with_capacity(report.kept.len());
    for (entry, rule) in std::mem::take(&mut report.kept).into_iter().zip(rules) {
        match rule {
            Some(rule) => report.refused.push(Dropped { content: entry.content, why: rule }),
            None => kept.push(entry),
        }
    }
    report.kept = kept;
    Ok(())
}

/// Pass four. `source_key` carries the dump's own name and line so a proposal in the queue can be
/// traced back to the line it came from, which is the only provenance a dump has.
pub fn requests(entries: &[DumpEntry], source: &str, run_id: Uuid) -> Vec<api::FactReq> {
    entries
        .iter()
        .map(|e| api::FactReq {
            content: e.content.clone(),
            namespace: e.namespace.clone(),
            tags: vec!["dump".to_string(), e.section.to_lowercase()],
            // An import does not retire a live fact. The owner supersedes by hand or not at all.
            supersedes: None,
            speaker: DUMP_SPEAKER.to_string(),
            // Both are the auto-approval path, and a dump has no span to check a quote against.
            quote: None,
            span_text: None,
            source: api::SourceReq {
                file_path: source.to_string(),
                entry_uuid: None,
                source_key: Some(format!("{source}#{}", e.line)),
                session_id: None,
                is_sidechain: false,
                speaker: Some(DUMP_SPEAKER.to_string()),
                observed_at: e.occurred_at.as_deref().and_then(midnight_utc),
                run_id,
            },
        })
        .collect()
}

/// `YYYY-MM-DD` at midnight UTC. A dump carries a date and no time, and picking an hour inside the
/// day would put precision into the store that the evidence never had.
fn midnight_utc(date: &str) -> Option<chrono::DateTime<Utc>> {
    let d = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0)?).into()
}

/// The scope recorded on the run, so the queue can say what this import was pointed at.
pub fn scope(source: &str, parsed: &DumpReport) -> serde_json::Value {
    serde_json::json!({
        "source": "memory-dump",
        "file": source,
        "entries_parsed": parsed.entries.len(),
        "ending": format!("{:?}", parsed.ending),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::dump;

    fn parse(text: &str) -> Vec<DumpEntry> {
        dump::parse(text).entries
    }

    #[test]
    fn a_repeat_inside_one_dump_is_dropped_once_and_named() {
        let r = sift(parse(
            "CAREER\n\
             [2022-06-01] (stated) - Sam left the firm for health reasons.\n\
             DECISIONS\n\
             [2022-06-01] (stated) - Sam left the firm for health reasons\n\
             COMPLETE",
        ));
        assert_eq!(r.kept.len(), 1);
        assert_eq!(r.duplicates.len(), 1);
        assert_eq!(r.kept[0].section, "CAREER", "the first occurrence is the one kept");
    }

    #[test]
    fn structural_junk_is_dropped_with_its_rule_and_real_facts_are_not() {
        let r = sift(parse(
            "PREFERENCES\n\
             [unknown] (stated) - ...\n\
             [unknown] (stated) - Prefers a dark terminal.\n\
             COMPLETE",
        ));
        assert_eq!(r.kept.len(), 1);
        assert_eq!(r.structural.len(), 1);
        assert_eq!(r.structural[0].why, "no words");
    }

    #[test]
    fn a_dated_line_becomes_an_observation_at_midnight_and_an_undated_one_stays_empty() {
        let entries = parse(
            "IDENTITY\n\
             [1990-05-06] (stated) - Sam Rivers was born in Leeds.\n\
             [unknown] (stated) - Sam Rivers lives near the coast.\n\
             COMPLETE",
        );
        let reqs = requests(&entries, "dump.txt", Uuid::nil());
        assert_eq!(reqs[0].source.observed_at.unwrap().to_rfc3339(), "1990-05-06T00:00:00+00:00");
        assert!(reqs[1].source.observed_at.is_none());
    }

    /// Claiming `owner_typed` would reach the auto-approval gate on a false claim about who said it.
    #[test]
    fn a_dump_never_claims_the_owner_typed_it_and_never_sends_a_span() {
        let entries =
            parse("IDENTITY\n[unknown] (stated) - Sam Rivers lives near the coast.\nCOMPLETE");
        let reqs = requests(&entries, "dump.txt", Uuid::nil());
        assert_eq!(reqs[0].speaker, "main_model");
        assert!(reqs[0].quote.is_none());
        assert!(
            reqs[0].span_text.is_none(),
            "a span_text would offer the server a substring to check"
        );
    }

    #[test]
    fn every_request_can_be_traced_back_to_the_line_it_came_from() {
        let entries =
            parse("SETUP\n[unknown] (stated) - The office laptop runs Debian 13.\nCOMPLETE");
        let reqs = requests(&entries, "mydump.txt", Uuid::nil());
        assert_eq!(reqs[0].source.source_key.as_deref(), Some("mydump.txt#2"));
        assert_eq!(reqs[0].source.file_path, "mydump.txt");
    }
}
