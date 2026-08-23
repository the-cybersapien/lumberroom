//! `lumberroom ingest submit`. Deterministic on the way back.
//!
//! Step 7 is the one place this pipeline can lose data. A file advances to the first byte of the
//! earliest span that landed in a missing or failed chunk, and to the plan ceiling only when every
//! one of its spans came back.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use serde::Serialize;
use serde_json::json;

use crate::client::{err, Client, Result};
use crate::ingest::{
    api, prefix_sha256, runlock, ChunkOutput, ExtractedFact, RunPaths, RunState, Span, Speaker,
    Worklist,
};

/// Proposals per POST. The server deduplicates on fingerprint, so a batch that fails half way is
/// safe to send again: the survivors come back as reinforcements.
const BATCH: usize = 100;

#[derive(Debug, Clone)]
pub struct SubmitArgs {
    pub run_id: uuid::Uuid,
    pub dry_run: bool,
    /// Hold everything for review. The flag for a first run against an unfamiliar corpus.
    pub no_auto: bool,
    pub json: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SubmitReport {
    pub facts_seen: i32,
    pub written: i32,
    pub queued: i32,
    pub reinforced: i32,
    pub confirmed: i32,
    pub refused: i32,
    pub blocked: i32,
    pub chunks_missing: i32,
    pub chunks_failed: i32,
    pub files_held_back: i32,
    pub unknown_span_ids: i32,
}

/// What one file advances to, and which spans held it short.
///
/// `effective_offset` is the offset the server will store for this file. The client needs it to
/// hash the right prefix and it recomputes rather than trusting a round trip, because the hash has
/// to cover exactly the bytes the watermark ends up naming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHold {
    pub file_path: String,
    pub session_id: Option<String>,
    pub is_sidechain: bool,
    pub plan_ceiling: i64,
    pub entries_seen: i64,
    pub unextracted_from: Vec<i64>,
    pub effective_offset: i64,
}

impl FileHold {
    pub fn held_back(&self) -> bool {
        self.effective_offset < self.plan_ceiling
    }
}

/// The hold-back rule, as a function of the worklist and the chunks that never came back.
///
/// Three clauses, and the middle one is the reason the first version of this rule lost bytes. A
/// file with spans in chunks 398 through 405 has some of them extracted after a kill at chunk 400,
/// so a rule keyed on "all of this file's spans are missing" advanced it to the ceiling and the
/// bytes behind 401 to 405 were never planned again. A span that reached no chunk at all was read,
/// classified and excluded, so it holds nothing back: most of the corpus is that case and a rule
/// that stalled on it would freeze the watermark on almost every file forever.
pub fn hold_back_plan(w: &Worklist, unextracted_chunks: &BTreeSet<usize>) -> Vec<FileHold> {
    let mut chunk_of: HashMap<&str, usize> = HashMap::new();
    for chunk in &w.chunks {
        for id in &chunk.span_ids {
            chunk_of.insert(id.as_str(), chunk.index);
        }
    }

    // Keyed on the path alone. Two planned entries sharing a path both hold back, which is the
    // safe direction: a watermark that stops early costs a re-read, one that runs ahead costs the
    // bytes.
    let mut pending: HashMap<&str, Vec<i64>> = HashMap::new();
    for span in &w.spans {
        let held = match chunk_of.get(span.id.as_str()) {
            Some(index) => unextracted_chunks.contains(index),
            None => false,
        };
        if held {
            pending.entry(span.file_path.as_str()).or_default().push(span.byte_start);
        }
    }

    w.files
        .iter()
        .map(|f| {
            let unextracted_from = pending.get(f.file_path.as_str()).cloned().unwrap_or_default();
            let earliest = unextracted_from.iter().copied().filter(|b| *b >= 0).min();
            let effective_offset = match earliest {
                Some(byte) => byte.min(f.plan_ceiling),
                None => f.plan_ceiling,
            };
            FileHold {
                file_path: f.file_path.clone(),
                session_id: f.session_id.clone(),
                is_sidechain: f.is_sidechain,
                plan_ceiling: f.plan_ceiling,
                entries_seen: f.entries_seen,
                unextracted_from,
                effective_offset,
            }
        })
        .collect()
}

/// Pair each fact with the span it names. A fact naming a span the planner never cut gets nothing:
/// a model that invents a span id has invented its evidence too.
pub fn resolve_spans<'a>(
    facts: &[ExtractedFact],
    spans: &'a [Span],
) -> (Vec<(ExtractedFact, &'a Span)>, i32) {
    let index: HashMap<&str, &Span> = spans.iter().map(|s| (s.id.as_str(), s)).collect();
    let mut kept = Vec::with_capacity(facts.len());
    let mut unknown = 0;
    for fact in facts {
        match index.get(fact.source_span_id.as_str()) {
            Some(span) => kept.push((fact.clone(), *span)),
            None => unknown += 1,
        }
    }
    (kept, unknown)
}

/// What the proposal rows record as their author. Mode A leaves `provider` unset and is the agent.
pub fn extractor_name(state: &RunState) -> String {
    let provider = match state.provider.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => p,
        None => return "agent:claude-code".to_string(),
    };
    if provider.starts_with("provider:") || provider.starts_with("agent:") {
        return provider.to_string();
    }
    match state.model.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
        Some(model) => format!("provider:{provider}/{model}"),
        None => format!("provider:{provider}"),
    }
}

fn read_chunk(paths: &RunPaths, index: usize) -> Option<ChunkOutput> {
    let raw = std::fs::read_to_string(paths.chunk_out(index)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// One line of a fact, for a dry run and for a refusal the owner has to recognise.
fn first_line(content: &str, width: usize) -> String {
    let line = content.lines().next().unwrap_or("");
    if line.chars().count() <= width {
        return line.to_string();
    }
    let cut: String = line.chars().take(width).collect();
    format!("{cut}...")
}

/// Merge, screen, post, advance, approve, report.
///
/// The tripwire runs before a proposal exists. The emission check runs before the insert, so
/// content the store itself emitted becomes a confirmation rather than a fact the owner is asked to
/// re-approve. Both run again on the server, and a client that skipped either changes nothing.
pub async fn run(c: &Client, args: &SubmitArgs) -> Result<SubmitReport> {
    // Held to the end of the function, so it outlives the end marker. Binding this to `_` instead
    // of `_lock` drops it here and leaves the run unlocked.
    let _lock = runlock::acquire(&runlock::holder(&format!("submit {}", args.run_id)))?;

    let paths = RunPaths::new(args.run_id)?;
    let worklist = paths.read_worklist()?;
    let state = paths.read_state();
    let extractor = extractor_name(&state);

    // A chunk `extract` recorded as failed writes no output file, so counting it missing as well
    // would double it and push an ordinary run of 429s past the abort threshold below.
    let failed: BTreeSet<usize> = state.failed.iter().map(|f| f.index).collect();
    let mut missing: BTreeSet<usize> = BTreeSet::new();
    let mut merged: Vec<ExtractedFact> = vec![];
    for chunk in &worklist.chunks {
        match read_chunk(&paths, chunk.index) {
            Some(out) => merged.extend(out.facts),
            None => {
                if !failed.contains(&chunk.index) {
                    missing.insert(chunk.index);
                }
            }
        }
    }

    if missing.len() * 2 > worklist.chunks.len() {
        let names: Vec<String> = missing.iter().take(10).map(|i| i.to_string()).collect();
        return Err(err(format!(
            "{} of {} chunk files are missing for run {}, starting at {}. That is a crashed \
             extractor rather than a quiet corpus, so nothing was posted and no watermark moved. \
             Read state.json, then run `lumberroom ingest extract --run {} --retry-failed`",
            missing.len(),
            worklist.chunks.len(),
            args.run_id,
            names.join(", "),
            args.run_id,
        )));
    }

    let mut report = SubmitReport {
        facts_seen: merged.len() as i32,
        chunks_missing: missing.len() as i32,
        chunks_failed: failed.len() as i32,
        ..Default::default()
    };

    let (resolved, unknown) = resolve_spans(&merged, &worklist.spans);
    report.unknown_span_ids = unknown;

    // The tripwire, one call for the batch. A fact that trips is dropped before a proposal can
    // exist, and only the rule name is ever printed or written down.
    let mut tripwire_rules: BTreeMap<String, i32> = BTreeMap::new();
    let mut survivors: Vec<(ExtractedFact, &Span)> = vec![];
    if resolved.is_empty() {
        survivors = resolved;
    } else {
        let texts: Vec<String> = resolved.iter().map(|(f, _)| f.content.clone()).collect();
        let rules = api::scan(c, &texts).await?;
        if rules.len() != texts.len() {
            return Err(err(format!(
                "the tripwire scan answered {} rules for {} facts. A zip over that would let a \
                 fact past the tripwire, so nothing was posted",
                rules.len(),
                texts.len()
            )));
        }
        for ((fact, span), rule) in resolved.into_iter().zip(rules) {
            match rule {
                Some(name) => {
                    *tripwire_rules.entry(name).or_insert(0) += 1;
                    report.refused += 1;
                }
                None => survivors.push((fact, span)),
            }
        }
    }

    // The read-only half of the anti-loop check. The server answers one bit per probe in the
    // order sent, so this client could tell which fact echoed; it only counts them for the
    // report, posts everything, and the proposal handler runs the same check again, turning an
    // echo into a confirmation.
    let mut emission_hits = 0;
    if !survivors.is_empty() {
        let probes: Vec<api::EmissionProbeReq> = survivors
            .iter()
            .map(|(f, span)| api::EmissionProbeReq {
                content: f.content.clone(),
                observed_at: span.timestamp,
            })
            .collect();
        emission_hits =
            api::check_emissions(c, &probes).await?.iter().filter(|e| **e).count() as i32;
    }

    let requests: Vec<api::FactReq> = survivors
        .iter()
        .map(|(fact, span)| {
            let owner_typed = span.speaker == Speaker::OwnerTyped;
            api::FactReq {
                content: fact.content.clone(),
                namespace: fact.namespace.clone(),
                tags: fact.tags.clone(),
                // An offline extractor does not get to retire a live fact. The owner supersedes by
                // hand or not at all.
                supersedes: None,
                // The span's speaker, never the extractor's claim about it.
                speaker: span.speaker.as_str().to_string(),
                quote: if owner_typed { fact.quote.clone() } else { None },
                // What the server checks the substring claim against, which is why `auto` is not a
                // request field.
                span_text: if owner_typed { Some(span.text.clone()) } else { None },
                source: api::SourceReq {
                    file_path: span.file_path.clone(),
                    entry_uuid: span.entry_uuids.first().cloned(),
                    source_key: None,
                    session_id: span.session_id.clone(),
                    is_sidechain: span.is_sidechain,
                    speaker: Some(span.speaker.as_str().to_string()),
                    observed_at: span.timestamp,
                    run_id: args.run_id,
                },
            }
        })
        .collect();

    if args.dry_run {
        report.confirmed = emission_hits;
        if args.json {
            let body = serde_json::to_string_pretty(&report)
                .map_err(|e| err(format!("could not serialise the report: {e}")))?;
            println!("{body}");
            return Ok(report);
        }
        println!("dry run for {}, extractor {extractor}", args.run_id);
        println!(
            "{} facts seen, {} unknown span ids, {} refused by the tripwire, {emission_hits} \
             already emitted",
            report.facts_seen, report.unknown_span_ids, report.refused
        );
        for req in &requests {
            println!("  [{}] {} {}", req.namespace, req.speaker, first_line(&req.content, 96));
        }
        for (rule, count) in &tripwire_rules {
            println!("  tripwire {rule} x{count}");
        }
        println!("would post {} facts. Nothing advanced, nothing closed", requests.len());
        return Ok(report);
    }

    let mut post = api::PostReport::default();
    for batch in requests.chunks(BATCH) {
        let r = api::post_proposals(c, &extractor, batch).await?;
        post.proposals_new += r.proposals_new;
        post.proposals_reinforced += r.proposals_reinforced;
        post.confirmations += r.confirmations;
        post.refused += r.refused;
        post.blocked += r.blocked;
        post.outcomes.extend(r.outcomes);
    }
    report.reinforced = post.proposals_reinforced;
    report.confirmed = post.confirmations;
    report.blocked = post.blocked;
    report.refused += post.refused;

    let mut server_rules: BTreeMap<String, i32> = BTreeMap::new();
    for outcome in &post.outcomes {
        if let api::FactOutcome::Refused { rule } = outcome {
            *server_rules.entry(rule.clone()).or_insert(0) += 1;
        }
    }

    // The hold-back. A chunk that failed and a chunk that never arrived hold back the same bytes.
    let mut unextracted: BTreeSet<usize> = missing.clone();
    unextracted.extend(failed.iter().copied());
    let holds = hold_back_plan(&worklist, &unextracted);

    let mut advances: Vec<api::FileAdvanceReq> = vec![];
    let mut unhashable: Vec<(String, String)> = vec![];
    for hold in &holds {
        let upto = hold.effective_offset.max(0) as u64;
        match prefix_sha256(Path::new(&hold.file_path), upto) {
            Ok(hash) => advances.push(api::FileAdvanceReq {
                file_path: hold.file_path.clone(),
                session_id: hold.session_id.clone(),
                is_sidechain: hold.is_sidechain,
                plan_ceiling: hold.plan_ceiling,
                // The hash covers the offset the server will store. Hashing the ceiling on a
                // held-back file fails that file's own prefix check next run, and the file gets
                // re-read from zero.
                prefix_sha256: hash,
                entries_seen: hold.entries_seen,
                unextracted_from: hold.unextracted_from.clone(),
            }),
            // A file deleted since the plan. Leaving its watermark alone is the safe direction.
            Err(e) => unhashable.push((hold.file_path.clone(), e.message)),
        }
    }

    let watermarks = if advances.is_empty() {
        api::WatermarkReport::default()
    } else {
        api::advance_watermarks(c, args.run_id, &advances).await?
    };
    report.files_held_back = watermarks.held_back.len() as i32;

    let held_back_json = json!(watermarks
        .held_back
        .iter()
        .map(|h| json!({ "file": h.file, "held_at": h.held_at, "ceiling": h.ceiling }))
        .collect::<Vec<_>>());

    let counters = &worklist.counters;
    let totals = api::RunTotals {
        files_seen: counters.files_seen,
        files_skipped: json!(counters.files_skipped),
        entries_seen: counters.entries_seen,
        entries_excluded: json!(counters.entries_excluded),
        unknown_types: json!(counters.unknown_types),
        spans_cut: counters.spans_cut,
        chunks: counters.chunks,
        chunks_missing: report.chunks_missing,
        chunks_failed: report.chunks_failed,
        files_held_back: held_back_json,
        fenced_entries: counters.fenced_entries,
        fences_unclosed: counters.fences_unclosed,
        proposals_new: post.proposals_new,
        proposals_reinforced: post.proposals_reinforced,
        confirmations: post.confirmations,
        traversal_capped: counters.traversal_capped,
        // Nobody on this side counts the sessions a Mode A run dispatched, so this is empty rather
        // than a guess.
        artifact_sessions: json!([]),
    };
    api::close_run(c, args.run_id, &totals).await?;

    let mut approval_refusals: Vec<(uuid::Uuid, String)> = vec![];
    let mut deduplicated = 0;
    let mut approvals_left = 0;
    if !args.no_auto {
        let auto: Vec<uuid::Uuid> = post
            .outcomes
            .iter()
            .filter_map(|o| match o {
                api::FactOutcome::Proposed { id, auto } if *auto => Some(*id),
                _ => None,
            })
            .collect();
        for (done, id) in auto.iter().copied().enumerate() {
            // A dead server must not cost the report and the end marker. The watermarks have
            // already moved and the run is closed, so an unfenced transcript is the only damage
            // left to do, and the rows stay queued for `ingest approve --run`.
            let answer = match api::approve(c, id).await {
                Ok(a) => a,
                Err(e) => {
                    approval_refusals.push((id, e.message));
                    approvals_left = auto.len() - done - 1;
                    break;
                }
            };
            match (&answer.refused, answer.memory_id) {
                // A refusal is a 200. The row stays queued with the reason on it, and the batch
                // keeps going.
                (Some(rule), _) => approval_refusals.push((id, rule.clone())),
                (None, Some(_)) => {
                    report.written += 1;
                    if answer.deduplicated {
                        deduplicated += 1;
                    }
                }
                (None, None) => {
                    approval_refusals.push((id, "approved and named no memory".to_string()))
                }
            }
        }
    }
    report.queued = (post.proposals_new - report.written).max(0);

    if args.json {
        let body = serde_json::to_string_pretty(&report)
            .map_err(|e| err(format!("could not serialise the report: {e}")))?;
        println!("{body}");
    } else {
        println!("run {}, extractor {extractor}", args.run_id);
        println!("facts seen {}", report.facts_seen);
        println!("written {} ({deduplicated} already in the store)", report.written);
        println!("queued {}", report.queued);
        println!("reinforced {}", report.reinforced);
        println!("confirmed {}", report.confirmed);
        println!("refused {}", report.refused);
        println!("blocked {}", report.blocked);
        println!("unknown span ids {}", report.unknown_span_ids);
        println!(
            "chunks missing {}, chunks failed {}",
            report.chunks_missing, report.chunks_failed
        );
        println!("files held back {}", report.files_held_back);
        for h in &watermarks.held_back {
            println!(
                "  {} held at {} of {}, {} bytes pending",
                h.file,
                h.held_at,
                h.ceiling,
                (h.ceiling - h.held_at).max(0)
            );
        }
        for (path, reason) in &unhashable {
            println!("  {path} kept its watermark: {reason}");
        }
        for (rule, count) in &tripwire_rules {
            println!("  tripwire refused {count}: {rule}");
        }
        for (rule, count) in &server_rules {
            println!("  server refused {count}: {rule}");
        }
        for (id, reason) in &approval_refusals {
            println!("  approval refused {id}: {reason}");
        }
        if approvals_left > 0 {
            println!(
                "  {approvals_left} approvals were not attempted. Run `lumberroom ingest approve \
                 --run {}` when the server answers again",
                args.run_id
            );
        }
        if !approval_refusals.is_empty() {
            println!(
                "those rows stay queued. `lumberroom ingest list --state proposed` shows them"
            );
        }
    }

    // Last, always. This closes the run's conversation in the transcript the owner is sitting in,
    // so the next plan fences everything above it.
    println!("{}{}", crate::ingest::FENCE_END, args.run_id);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{ChunkRef, PlanCounters, PlannedFile, Source};

    fn span(id: &str, file: &str, byte_start: i64) -> Span {
        Span {
            id: id.to_string(),
            file_path: file.to_string(),
            session_id: Some("sess".to_string()),
            is_sidechain: false,
            source: Source::Claude,
            entry_uuids: vec![format!("{id}-uuid")],
            byte_start,
            byte_end: byte_start + 10,
            speaker: Speaker::OwnerTyped,
            tool_name: None,
            timestamp: None,
            cwd: None,
            text: format!("text of {id}"),
        }
    }

    fn file(path: &str, ceiling: i64) -> PlannedFile {
        PlannedFile {
            file_path: path.to_string(),
            session_id: Some("sess".to_string()),
            is_sidechain: false,
            source: Source::Claude,
            byte_start: 0,
            plan_ceiling: ceiling,
            entries_seen: 4,
            prefix_mismatch: false,
        }
    }

    fn worklist(files: Vec<PlannedFile>, spans: Vec<Span>, chunks: Vec<ChunkRef>) -> Worklist {
        Worklist {
            run_id: uuid::Uuid::nil(),
            created_at: chrono::Utc::now(),
            scope: json!({}),
            include_tool_output: false,
            files,
            spans,
            chunks,
            counters: PlanCounters::default(),
        }
    }

    fn chunk(index: usize, ids: &[&str]) -> ChunkRef {
        ChunkRef { index, span_ids: ids.iter().map(|s| s.to_string()).collect() }
    }

    fn holds_for(w: &Worklist, unextracted: &[usize]) -> Vec<FileHold> {
        hold_back_plan(w, &unextracted.iter().copied().collect())
    }

    #[test]
    fn a_file_with_no_surviving_spans_still_advances_to_the_ceiling() {
        // Every byte was read, classified and excluded. Most of the corpus is this case.
        let w = worklist(vec![file("/a.jsonl", 9000)], vec![], vec![]);
        let holds = holds_for(&w, &[0, 1, 2]);
        assert_eq!(holds[0].effective_offset, 9000);
        assert!(holds[0].unextracted_from.is_empty());
        assert!(!holds[0].held_back());
    }

    #[test]
    fn a_span_that_reached_no_chunk_holds_nothing_back() {
        let w = worklist(vec![file("/a.jsonl", 9000)], vec![span("s1", "/a.jsonl", 100)], vec![]);
        let holds = holds_for(&w, &[0]);
        assert_eq!(holds[0].effective_offset, 9000);
    }

    #[test]
    fn a_fully_extracted_file_advances_to_the_ceiling() {
        let w = worklist(
            vec![file("/a.jsonl", 9000)],
            vec![span("s1", "/a.jsonl", 100), span("s2", "/a.jsonl", 400)],
            vec![chunk(0, &["s1"]), chunk(1, &["s2"])],
        );
        let holds = holds_for(&w, &[]);
        assert_eq!(holds[0].effective_offset, 9000);
        assert!(holds[0].unextracted_from.is_empty());
    }

    #[test]
    fn a_partly_extracted_file_stops_at_its_earliest_unextracted_span() {
        // The kill-at-chunk-400 shape: chunk 1 came back, chunks 2 and 3 did not, and the file
        // must stop at the first byte behind chunk 2 rather than run to the ceiling.
        let w = worklist(
            vec![file("/a.jsonl", 9000), file("/b.jsonl", 500)],
            vec![
                span("s1", "/a.jsonl", 100),
                span("s2", "/a.jsonl", 4000),
                span("s3", "/a.jsonl", 6000),
                span("s4", "/b.jsonl", 20),
            ],
            vec![chunk(1, &["s1"]), chunk(2, &["s3"]), chunk(3, &["s2", "s4"])],
        );
        let holds = holds_for(&w, &[2, 3]);

        assert_eq!(holds[0].file_path, "/a.jsonl");
        assert_eq!(holds[0].effective_offset, 4000);
        assert!(holds[0].held_back());
        let mut pending = holds[0].unextracted_from.clone();
        pending.sort_unstable();
        assert_eq!(pending, vec![4000, 6000]);

        // The second file's only span rode the same failed chunk.
        assert_eq!(holds[1].effective_offset, 20);
        assert!(holds[1].held_back());
    }

    #[test]
    fn an_unextracted_span_past_the_ceiling_caps_at_the_ceiling() {
        // The plan ceiling froze before this span's bytes, so the watermark may not pass it.
        let w = worklist(
            vec![file("/a.jsonl", 300)],
            vec![span("s1", "/a.jsonl", 900)],
            vec![chunk(0, &["s1"])],
        );
        let holds = holds_for(&w, &[0]);
        assert_eq!(holds[0].effective_offset, 300);
        assert!(!holds[0].held_back());
    }

    fn fact(span_id: &str) -> ExtractedFact {
        ExtractedFact {
            content: format!("a fact from {span_id}"),
            namespace: "user:me".to_string(),
            tags: vec![],
            source_span_id: span_id.to_string(),
            speaker: Some("owner_typed".to_string()),
            quote: None,
            confidence: None,
        }
    }

    #[test]
    fn a_fact_naming_a_span_the_planner_never_cut_is_dropped() {
        let spans = vec![span("s1", "/a.jsonl", 0), span("s2", "/a.jsonl", 50)];
        let facts = vec![fact("s2"), fact("s99"), fact("s1"), fact("")];
        let (kept, unknown) = resolve_spans(&facts, &spans);
        assert_eq!(unknown, 2);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].1.id, "s2");
        assert_eq!(kept[1].1.id, "s1");
    }

    #[test]
    fn the_extractor_names_the_agent_when_no_provider_ran() {
        let mut state = RunState::new(uuid::Uuid::nil());
        assert_eq!(extractor_name(&state), "agent:claude-code");

        state.provider = Some("openai".to_string());
        assert_eq!(extractor_name(&state), "provider:openai");

        state.model = Some("gpt-5-mini".to_string());
        assert_eq!(extractor_name(&state), "provider:openai/gpt-5-mini");

        state.provider = Some("provider:anthropic/claude".to_string());
        assert_eq!(extractor_name(&state), "provider:anthropic/claude");

        state.provider = Some("  ".to_string());
        assert_eq!(extractor_name(&state), "agent:claude-code");
    }

    #[test]
    fn a_first_line_keeps_its_own_width() {
        assert_eq!(first_line("one\ntwo", 40), "one");
        assert_eq!(first_line("abcdef", 3), "abc...");
        assert_eq!(first_line("", 8), "");
    }
}
