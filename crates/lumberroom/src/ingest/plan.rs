//! `lumberroom ingest plan`. Deterministic, and it advances nothing.
//!
//! The watermark moves in `submit`, so a run that dies between the two re-plans the same bytes
//! instead of losing them.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::json;

use crate::client::{err, Client, Result};
use crate::ingest::{
    api, claude, codex, prefix_sha256, runlock, runs_dir, spans, walk, write_json, ChunkSpan,
    Limits, PlanCounters, PlannedFile, RunPaths, Source, Span, Speaker, Worklist, FENCE_BEGIN,
};

#[derive(Debug, Clone)]
pub struct PlanArgs {
    /// `claude`, `codex` or `all`.
    pub source: String,
    pub project: Option<String>,
    /// `7d`, `48h`, or an ISO date.
    pub since: Option<String>,
    pub max_files: Option<usize>,
    pub include_tool_output: bool,
    pub json: bool,
}

/// Walk, exclude, classify, cut, chunk, write. Prints the exclusion table, which is the point of a
/// first run: every count on it is a rule the owner can check.
pub async fn run(c: &Client, args: &PlanArgs) -> Result<Worklist> {
    let limits = Limits::default();
    let sources = sources_for(&args.source)?;
    let since = match args.since.as_deref() {
        Some(raw) => Some(parse_since(raw)?),
        None => None,
    };

    let lock = runlock::acquire(&runlock::holder("plan"))?;
    let swept = sweep_old_runs(limits.retention_days)?;

    // Verbatim as the owner gave them. The run record is the only place a later reader can learn
    // what this run was pointed at, and a normalised copy would answer a different question.
    let scope = json!({
        "source": args.source,
        "project": args.project,
        "since": args.since,
        "max_files": args.max_files,
        "include_tool_output": args.include_tool_output,
    });

    // "pending" because nothing has extracted anything yet. `extract` and the skill each rewrite
    // it with the extractor that actually ran.
    let run_id = api::open_run(c, "pending", scope.clone()).await?;

    // The fence marker goes out before any other line, including in --json mode. The transcript of
    // the session the owner is sitting in has to record it, or the next run reads this run's own
    // conversation back as material.
    println!("{FENCE_BEGIN}{run_id}");
    lock.note(&format!("plan run {run_id} pid {}", std::process::id()))?;
    if swept > 0 {
        println!("swept {swept} run directories older than {} days", limits.retention_days);
    }

    // One call for the whole corpus. Per file would be 685 round trips on this machine.
    let marks = api::watermarks(c, false).await?;
    let by_path: BTreeMap<&str, &api::Watermark> =
        marks.iter().map(|w| (w.file_path.as_str(), w)).collect();

    let mut counters = PlanCounters::default();
    let opts = walk::WalkOpts {
        project: args.project.clone(),
        since,
        max_files: args.max_files.unwrap_or(limits.max_files),
    };
    let mut candidates = vec![];
    for source in &sources {
        let roots = walk::roots(*source)?;
        candidates.extend(walk::walk(&roots, *source, &opts, &limits, &mut counters)?);
    }

    let mut files: Vec<PlannedFile> = vec![];
    let mut all_spans: Vec<Span> = vec![];
    // Threaded across every file so span ids stay unique for the whole run. The extractor
    // references a span by id alone and a collision reattributes a fact to another session.
    let mut next_index: usize = 0;
    let mut entries_budget: u64 = 0;

    for cand in &candidates {
        let file_path = cand.path.to_string_lossy().to_string();

        if entries_budget >= limits.max_entries {
            counters.traversal_capped = true;
            counters.skip("capped");
            continue;
        }

        let mark = by_path.get(file_path.as_str()).copied();
        if mark.and_then(|w| w.skip_reason.as_ref()).is_some() {
            counters.skip("skipped");
            continue;
        }

        let mut byte_start = mark.map(|w| w.byte_offset).unwrap_or(0).max(0);
        let mut prefix_mismatch = false;
        if byte_start > 0 {
            let stored = mark.map(|w| w.prefix_sha256.as_str()).unwrap_or("");
            match prefix_sha256(&cand.path, byte_start as u64) {
                Ok(seen) if seen == stored => {}
                Ok(_) => {
                    // A file rewritten or truncated in place. Say it out loud: a silently shifted
                    // offset produces garbage spans forever, and re-reading is cheap by comparison.
                    println!("prefix hash mismatch, re-reading from zero: {file_path}");
                    byte_start = 0;
                    prefix_mismatch = true;
                }
                Err(e) => {
                    println!("could not hash {file_path}: {}", e.message);
                    counters.skip("unparseable");
                    continue;
                }
            }
        }

        // Frozen here. A live transcript grows all day and nothing beyond this offset is read,
        // whatever the file has become by the time the parser reaches it.
        let ceiling = cand.size as i64;
        if ceiling <= byte_start {
            // Not grown since the last run. No rule excluded it, so no counter moves.
            continue;
        }

        let parsed = match cand.source {
            Source::Claude => {
                claude::parse_file(&cand.path, byte_start, ceiling, &limits, &mut counters)
            }
            Source::Codex => {
                codex::parse_file(&cand.path, byte_start, ceiling, &limits, &mut counters)
            }
        };
        let parsed = match parsed {
            Ok(p) => p,
            Err(e) => {
                println!("could not parse {file_path}: {}", e.message);
                counters.skip("unparseable");
                continue;
            }
        };

        // The parser already counted these onto `counters`. Adding them again here doubled the
        // "entries seen" line while every exclusion stayed right, which reads as thousands of
        // entries falling through with no rule naming them.
        entries_budget += parsed.entries_seen.max(0) as u64;

        let session_id = cand.session_id.clone().or_else(|| parsed.session_id.clone());
        let is_sidechain = cand.is_sidechain || parsed.is_sidechain;
        all_spans.extend(spans::cut(
            &parsed.entries,
            &file_path,
            session_id.as_deref(),
            is_sidechain,
            cand.source,
            args.include_tool_output,
            &limits,
            &mut next_index,
        ));

        // `consumed`, not the stat ceiling. A trailing fragment with no newline is not a complete
        // entry, so the parser stops below the ceiling and those bytes belong to the next run.
        // Advancing to the ceiling instead would step over them and lose the entry they become.
        //
        // A fence still open at EOF holds the watermark at the byte it opened at instead of
        // `consumed`: every entry from there on was excluded as fenced content and never turned
        // into a span, so advancing past it anyway would drop those bytes for good the moment a
        // real run closes the fence on a later line this walk never reached. Holding back costs
        // one re-read of the fenced region; advancing costs the region.
        let read_to = match parsed.fence_open_byte {
            Some(open_at) if parsed.fence_open => open_at.clamp(byte_start, ceiling),
            _ => parsed.consumed.clamp(byte_start, ceiling),
        };
        files.push(PlannedFile {
            file_path,
            session_id,
            is_sidechain,
            source: cand.source,
            byte_start,
            plan_ceiling: read_to,
            entries_seen: parsed.entries_seen,
            prefix_mismatch,
        });
    }

    // The plan-time tripwire scan. Everything below this point leaves the machine: `extract`
    // posts each chunk's span text to a provider (z.ai or OpenRouter for Mode B, a local subagent
    // reading the same chunk file for Mode A). A span carrying a credential has to be caught and
    // dropped here, before chunking, not later at `submit` where the scan runs on the facts a
    // provider already handed back and the secret has already left the machine. Batched by bytes
    // rather than in one request: the server caps a body at 1 MiB, and a week of transcripts at
    // the default span size is several times that.
    if !all_spans.is_empty() {
        let texts: Vec<String> = all_spans.iter().map(|s| s.text.clone()).collect();
        let mut rules = Vec::with_capacity(texts.len());
        for batch in scan_batches(&texts, SCAN_BATCH_BYTES) {
            let answered = api::scan(c, batch).await?;
            if answered.len() != batch.len() {
                return Err(err(format!(
                    "the tripwire scan (POST /admin/ingest/scan) answered {} rules for {} spans. A \
                     zip over that would let a span reach a provider unscanned, so nothing was \
                     written for run {run_id}",
                    answered.len(),
                    batch.len(),
                )));
            }
            rules.extend(answered);
        }
        all_spans = apply_tripwire(all_spans, rules, run_id, &mut counters)?;
    }

    counters.spans_cut = all_spans.len() as i32;
    let chunks = spans::chunk(&all_spans, &limits);
    counters.chunks = chunks.len() as i32;
    // Everything the walk touched: the files this run planned plus every file a rule refused.
    counters.files_seen =
        files.len() as i32 + counters.files_skipped.values().copied().sum::<i32>();

    let worklist = Worklist {
        run_id,
        created_at: Utc::now(),
        scope,
        include_tool_output: args.include_tool_output,
        files,
        spans: all_spans,
        chunks,
        counters,
    };

    let paths = RunPaths::new(run_id)?;
    paths.create()?;
    write_json(&paths.worklist(), &worklist)?;

    let by_id: BTreeMap<&str, &Span> = worklist.spans.iter().map(|s| (s.id.as_str(), s)).collect();
    for chunk in &worklist.chunks {
        let projected: Vec<ChunkSpan> = chunk
            .span_ids
            .iter()
            .filter_map(|id| by_id.get(id.as_str()).map(|s| ChunkSpan::from(*s)))
            .collect();
        write_json(&paths.chunk_in(chunk.index), &projected)?;
    }

    if args.json {
        let report = json!({
            "run_id": run_id,
            "worklist": paths.worklist().display().to_string(),
            "counters": worklist.counters,
        });
        println!("{}", serde_json::to_string_pretty(&report).map_err(|e| err(e.to_string()))?);
    } else {
        print_table(&worklist);
        println!();
        println!("run {run_id}");
        println!("spans in {}", paths.spans_dir().display());
        println!("next  lumberroom ingest extract --run {run_id}   (Mode B, a provider extracts)");
        println!("      or dispatch one subagent per chunk       (Mode A, the skill does this)");
        println!("then  lumberroom ingest submit --run {run_id}");
    }

    // Held to here on purpose. The lock covers the whole walk, and dropping it earlier hands a
    // second run the same growing files halfway through this one's reads.
    drop(lock);
    Ok(worklist)
}

/// Drop every span the tripwire named a rule for, counting each onto `counters` and refusing
/// outright rather than guessing when the scan answered a different number of rules than spans.
/// How much span text one scan request carries. Half the server's 1 MiB body cap, which leaves
/// room for the JSON framing and escaping around the text.
const SCAN_BATCH_BYTES: usize = 512 * 1024;

/// Consecutive slices of `texts`, each under `max_bytes` of text unless a single span is larger
/// than that on its own, in which case it travels alone and the server's cap is the one that
/// refuses it. Slices rather than copies: the texts are already owned once.
fn scan_batches(texts: &[String], max_bytes: usize) -> Vec<&[String]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut bytes = 0;
    for (i, t) in texts.iter().enumerate() {
        if i > start && bytes + t.len() > max_bytes {
            out.push(&texts[start..i]);
            start = i;
            bytes = 0;
        }
        bytes += t.len();
    }
    if start < texts.len() {
        out.push(&texts[start..]);
    }
    out
}

fn apply_tripwire(
    spans: Vec<Span>,
    rules: Vec<Option<String>>,
    run_id: uuid::Uuid,
    counters: &mut PlanCounters,
) -> Result<Vec<Span>> {
    if rules.len() != spans.len() {
        return Err(err(format!(
            "the tripwire scan (POST /admin/ingest/scan) answered {} rules for {} spans. A zip \
             over that would let a span reach a provider unscanned, so nothing was written for \
             run {run_id}",
            rules.len(),
            spans.len(),
        )));
    }
    let mut kept = Vec::with_capacity(spans.len());
    for (span, rule) in spans.into_iter().zip(rules) {
        match rule {
            Some(name) => counters.tripwire(&name),
            None => kept.push(span),
        }
    }
    Ok(kept)
}

/// The table `plan` prints. Nothing on it is a total the reader has to compute.
pub fn print_table(w: &Worklist) {
    print!("{}", render_table(w));
}

/// Split out from `print_table` so a test can read what the owner reads.
fn render_table(w: &Worklist) -> String {
    let c = &w.counters;
    let mut out = String::new();

    let artifacts = c.files_skipped.get("ingest_artifact").copied().unwrap_or(0);
    let named: Vec<(String, i32)> = sorted(&c.files_skipped)
        .into_iter()
        .filter(|(k, _)| k != "ingest_artifact")
        .map(|(k, n)| (k.replace('_', " "), n))
        .collect();
    let skipped: i32 = c.files_skipped.values().copied().sum();
    let detail = if named.is_empty() {
        String::new()
    } else {
        format!(
            " ({})",
            named.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", ")
        )
    };
    row(
        &mut out,
        "files",
        &format!(
            "{} seen, {} skipped{detail}",
            commas(c.files_seen as i64),
            commas(skipped as i64)
        ),
    );
    // Held back is `submit`'s number and it is always zero here, which is the point: this command
    // reads files and moves no watermark.
    let plural = if artifacts == 1 { "artifact" } else { "artifacts" };
    cont(&mut out, &format!("{} ingest {plural}, 0 held back", commas(artifacts as i64)));
    let rewound = w.files.iter().filter(|f| f.prefix_mismatch).count();
    if rewound > 0 {
        cont(&mut out, &format!("{rewound} re-read from zero after a prefix hash mismatch"));
    }

    let excluded: i32 = c.entries_excluded.values().copied().sum();
    row(
        &mut out,
        "entries",
        &format!("{} seen, {} excluded", commas(c.entries_seen), commas(excluded as i64)),
    );
    for line in dotted(&sorted(&c.entries_excluded), 3) {
        cont(&mut out, &line);
    }

    let order = [
        Speaker::OwnerTyped,
        Speaker::MainModel,
        Speaker::Subagent,
        Speaker::ToolReturned,
        Speaker::HookInjected,
        Speaker::System,
    ];
    let speakers: Vec<(String, i32)> = order
        .iter()
        .filter_map(|s| {
            c.speakers.get(s.as_str()).filter(|n| **n > 0).map(|n| (s.as_str().to_string(), *n))
        })
        .collect();
    let speaker_lines = dotted(&speakers, 3);
    if speaker_lines.is_empty() {
        row(&mut out, "speakers", "none");
    } else {
        row(&mut out, "speakers", &speaker_lines[0]);
        for line in &speaker_lines[1..] {
            cont(&mut out, line);
        }
    }

    row(
        &mut out,
        "spans",
        &format!("{} cut into {} chunks", commas(c.spans_cut as i64), commas(c.chunks as i64)),
    );
    let tripwire_dropped: i32 = c.spans_dropped_tripwire.values().copied().sum();
    if tripwire_dropped > 0 {
        row(
            &mut out,
            "tripwire",
            &format!("{} spans refused before extraction", commas(tripwire_dropped as i64)),
        );
        for line in dotted(&sorted(&c.spans_dropped_tripwire), 3) {
            cont(&mut out, &line);
        }
    }
    row(
        &mut out,
        "fences",
        &format!(
            "{} entries dropped, {} closed without an end marker",
            commas(c.fenced_entries as i64),
            commas(c.fences_unclosed as i64)
        ),
    );

    let entry_types = group(&c.unknown_types, "entry_type");
    let subtypes = group(&c.unknown_types, "attachment_subtype");
    row(
        &mut out,
        "unknown",
        &format!(
            "entry types: {}   attachment subtypes: {}",
            listing(&entry_types),
            listing(&subtypes)
        ),
    );

    // E1 should have caught every one of these, so a line here is a parser bug rather than a
    // statistic. It prints only when one fired.
    let backstop = sorted(&c.backstop);
    if !backstop.is_empty() {
        row(&mut out, "backstop", &listing(&backstop));
    }

    if c.traversal_capped {
        out.push('\n');
        out.push_str(
            "COVERAGE IS PARTIAL. INGEST_MAX_FILES or INGEST_MAX_ENTRIES fired, so this\n",
        );
        out.push_str(
            "run read part of the corpus. Raise the limit and re-plan to read the rest.\n",
        );
    }
    out
}

/// Sweep run directories older than `INGEST_RUN_RETENTION_DAYS`.
pub fn sweep_old_runs(retention_days: u64) -> Result<usize> {
    sweep_dir(&runs_dir()?, retention_days)
}

/// Split out for the test. Nothing outside `runs_dir()` is ever passed here, and a symlink under
/// it is skipped rather than followed: `remove_dir_all` through a link deletes the target.
fn sweep_dir(dir: &std::path::Path, retention_days: u64) -> Result<usize> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        // No runs directory yet is the state of a fresh machine, not a failure.
        Err(_) => return Ok(0),
    };
    let retention = std::time::Duration::from_secs(retention_days * 24 * 60 * 60);
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_dir() {
            continue;
        }
        // A clock that ran backwards leaves an mtime in the future. Age zero keeps that directory.
        let age = meta
            .modified()
            .ok()
            .and_then(|m| std::time::SystemTime::now().duration_since(m).ok())
            .unwrap_or_default();
        if age >= retention && std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

fn sources_for(arg: &str) -> Result<Vec<Source>> {
    match arg {
        "claude" => Ok(vec![Source::Claude]),
        "codex" => Ok(vec![Source::Codex]),
        "all" => Ok(vec![Source::Claude, Source::Codex]),
        other => Err(err(format!("unknown source `{other}`. Use claude, codex or all"))),
    }
}

/// `7d`, `48h`, `30m`, or an ISO-8601 date. Anything else is refused: a `--since` nobody parsed
/// silently widens the window to the whole corpus.
pub fn parse_since(input: &str) -> Result<DateTime<Utc>> {
    let raw = input.trim();
    let refuse = || {
        err(format!(
            "cannot read `{raw}` as a date. Use 7d, 48h, 30m, 2026-08-01 or a full ISO-8601 instant"
        ))
    };

    if let Some(unit) = raw.chars().last() {
        if matches!(unit, 'd' | 'h' | 'm') {
            let count = &raw[..raw.len() - unit.len_utf8()];
            if !count.is_empty() && count.chars().all(|ch| ch.is_ascii_digit()) {
                let n: i64 = count.parse().map_err(|_| refuse())?;
                // Bounded so the multiplication below cannot overflow chrono's range.
                if n > 100_000 {
                    return Err(err(format!("`{raw}` is further back than the corpus goes")));
                }
                let seconds = match unit {
                    'd' => n * 86_400,
                    'h' => n * 3_600,
                    _ => n * 60,
                };
                return Utc::now()
                    .checked_sub_signed(Duration::seconds(seconds))
                    .ok_or_else(|| refuse());
            }
        }
    }

    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        let midnight = date.and_hms_opt(0, 0, 0).ok_or_else(|| refuse())?;
        return Ok(Utc.from_utc_datetime(&midnight));
    }
    Err(refuse())
}

fn row(out: &mut String, label: &str, text: &str) {
    out.push_str(&format!("{label:<10} {text}\n"));
}

fn cont(out: &mut String, text: &str) {
    out.push_str(&format!("{:<10} {text}\n", ""));
}

/// Largest first, so the rule doing the most work reads first. Ties break by name to keep two
/// runs over the same corpus comparable line by line.
fn sorted(map: &BTreeMap<String, i32>) -> Vec<(String, i32)> {
    let mut pairs: Vec<(String, i32)> =
        map.iter().filter(|(_, n)| **n > 0).map(|(k, n)| (k.clone(), *n)).collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs
}

fn dotted(pairs: &[(String, i32)], per_line: usize) -> Vec<String> {
    pairs
        .chunks(per_line.max(1))
        .map(|group| {
            group
                .iter()
                .map(|(k, n)| format!("{k} {}", commas(*n as i64)))
                .collect::<Vec<_>>()
                .join(" \u{00b7} ")
        })
        .collect()
}

fn group(map: &BTreeMap<String, i32>, kind: &str) -> Vec<(String, i32)> {
    let prefix = format!("{kind}:");
    let picked: BTreeMap<String, i32> = map
        .iter()
        .filter_map(|(k, n)| k.strip_prefix(&prefix).map(|name| (name.to_string(), *n)))
        .collect();
    sorted(&picked)
}

fn listing(pairs: &[(String, i32)]) -> String {
    if pairs.is_empty() {
        return "0".to_string();
    }
    pairs
        .iter()
        .map(|(k, n)| format!("{k} {}", commas(*n as i64)))
        .collect::<Vec<_>>()
        .join(" \u{00b7} ")
}

/// 91204 reads as 91,204. The owner reads these numbers against each other.
fn commas(n: i64) -> String {
    let negative = n < 0;
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if negative {
        format!("-{out}")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::ChunkRef;

    #[test]
    fn since_accepts_the_three_relative_forms() {
        let now = Utc::now();
        let d = parse_since("7d").unwrap();
        assert!((now - d).num_hours() >= 167 && (now - d).num_hours() <= 169);
        let h = parse_since("48h").unwrap();
        assert!((now - h).num_hours() >= 47 && (now - h).num_hours() <= 49);
        let m = parse_since("30m").unwrap();
        assert!((now - m).num_minutes() >= 29 && (now - m).num_minutes() <= 31);
        // Whitespace from a shell wrapper is not a different date.
        assert!(parse_since("  7d  ").is_ok());
    }

    #[test]
    fn since_accepts_iso_dates_and_instants() {
        let day = parse_since("2026-08-01").unwrap();
        assert_eq!(day.to_rfc3339(), "2026-08-01T00:00:00+00:00");
        let instant = parse_since("2026-08-01T12:30:00Z").unwrap();
        assert_eq!(instant.to_rfc3339(), "2026-08-01T12:30:00+00:00");
    }

    #[test]
    fn since_refuses_anything_it_cannot_read() {
        for bad in ["", "yesterday", "7", "d7", "-3d", "7w", "2026-13-01", "7 d", "1.5h"] {
            assert!(parse_since(bad).is_err(), "`{bad}` should have been refused");
        }
    }

    #[test]
    fn sources_are_the_three_the_flag_documents() {
        assert_eq!(sources_for("claude").unwrap(), vec![Source::Claude]);
        assert_eq!(sources_for("codex").unwrap(), vec![Source::Codex]);
        assert_eq!(sources_for("all").unwrap(), vec![Source::Claude, Source::Codex]);
        assert!(sources_for("gemini").is_err());
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1_000), "1,000");
        assert_eq!(commas(91_204), "91,204");
        assert_eq!(commas(1_234_567), "1,234,567");
    }

    fn worklist() -> Worklist {
        let mut c = PlanCounters::default();
        c.files_seen = 184;
        for (reason, n) in [("sensitive_path", 4), ("symlink", 1), ("unparseable", 1)] {
            c.files_skipped.insert(reason.to_string(), n);
        }
        c.entries_seen = 91_204;
        for (rule, n) in [
            ("attachment", 41_882),
            ("tool_result", 38_004),
            ("memory_tool", 1_317),
            ("system", 5_910),
            ("sensitive", 218),
            ("ingest_fence", 96),
        ] {
            c.entries_excluded.insert(rule.to_string(), n);
        }
        c.speakers.insert("owner_typed".into(), 619);
        c.speakers.insert("main_model".into(), 2_701);
        c.speakers.insert("subagent".into(), 553);
        c.unknown_types.insert("attachment_subtype:nested_memory".into(), 12);
        c.spans_cut = 1_204;
        c.chunks = 34;
        c.fenced_entries = 96;
        Worklist {
            run_id: uuid::Uuid::nil(),
            created_at: Utc::now(),
            scope: json!({}),
            include_tool_output: false,
            files: vec![],
            spans: vec![],
            chunks: vec![ChunkRef { index: 0, span_ids: vec![] }],
            counters: c,
        }
    }

    #[test]
    fn the_table_names_every_exclusion_by_its_rule() {
        let text = render_table(&worklist());
        assert!(text.contains("184 seen, 6 skipped"), "{text}");
        assert!(text.contains("sensitive path 4, symlink 1, unparseable 1"), "{text}");
        assert!(text.contains("91,204 seen, 87,427 excluded"), "{text}");
        assert!(text.contains("attachment 41,882"), "{text}");
        assert!(text.contains("memory_tool 1,317"), "{text}");
        assert!(text.contains("ingest_fence 96"), "{text}");
        assert!(text.contains("owner_typed 619"), "{text}");
        assert!(text.contains("1,204 cut into 34 chunks"), "{text}");
        assert!(text.contains("attachment subtypes: nested_memory 12"), "{text}");
        assert!(text.contains("entry types: 0"), "{text}");
        // Every exclusion the counters carry appears somewhere on the table.
        for rule in ["attachment", "tool_result", "memory_tool", "system", "sensitive"] {
            assert!(text.contains(rule), "{rule} is missing from:\n{text}");
        }
        assert!(!text.contains("COVERAGE IS PARTIAL"), "{text}");
    }

    #[test]
    fn a_capped_traversal_says_so_loudly() {
        let mut w = worklist();
        w.counters.traversal_capped = true;
        assert!(render_table(&w).contains("COVERAGE IS PARTIAL"));
    }

    #[test]
    fn a_prefix_mismatch_reaches_the_table() {
        let mut w = worklist();
        w.files.push(PlannedFile {
            file_path: "/tmp/a.jsonl".into(),
            session_id: None,
            is_sidechain: false,
            source: Source::Claude,
            byte_start: 0,
            plan_ceiling: 10,
            entries_seen: 1,
            prefix_mismatch: true,
        });
        assert!(render_table(&w).contains("1 re-read from zero after a prefix hash mismatch"));
    }

    #[test]
    fn the_sweep_keeps_fresh_runs_and_removes_expired_ones() {
        let dir = std::env::temp_dir().join(format!("lumberroom-sweep-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("run-a")).unwrap();
        std::fs::create_dir_all(dir.join("run-b")).unwrap();
        std::fs::write(dir.join("loose.json"), b"{}").unwrap();

        assert_eq!(sweep_dir(&dir, 7).unwrap(), 0);
        assert!(dir.join("run-a").exists());

        // Retention of zero makes every directory expired, which is the only age this test can
        // produce without reaching for a crate that sets mtimes.
        assert_eq!(sweep_dir(&dir, 0).unwrap(), 2);
        assert!(!dir.join("run-a").exists());
        assert!(dir.join("loose.json").exists(), "the sweep deletes directories and nothing else");

        assert_eq!(sweep_dir(&dir.join("absent"), 0).unwrap(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn span(id: &str, text: &str) -> Span {
        Span {
            id: id.to_string(),
            file_path: "/tmp/t.jsonl".to_string(),
            session_id: None,
            is_sidechain: false,
            source: Source::Claude,
            entry_uuids: vec![],
            byte_start: 0,
            byte_end: text.len() as i64,
            speaker: Speaker::OwnerTyped,
            tool_name: None,
            timestamp: None,
            cwd: None,
            text: text.to_string(),
        }
    }

    #[test]
    fn scan_batches_stay_under_the_byte_ceiling_and_cover_every_span_in_order() {
        let texts: Vec<String> = (0..10).map(|i| format!("{i}").repeat(300)).collect();
        let batches = scan_batches(&texts, 1000);
        assert!(batches.len() > 1, "ten spans of 300 bytes do not fit one 1000-byte batch");
        for b in &batches {
            assert!(b.iter().map(String::len).sum::<usize>() <= 1000, "a batch over the ceiling");
        }
        let joined: Vec<&String> = batches.iter().flat_map(|b| b.iter()).collect();
        assert_eq!(joined.len(), texts.len());
        assert!(joined.iter().zip(&texts).all(|(a, b)| *a == b), "order is what the zip relies on");
    }

    #[test]
    fn a_span_larger_than_the_ceiling_travels_alone_rather_than_being_dropped() {
        let texts = vec!["a".repeat(2000), "b".into(), "c".into()];
        let batches = scan_batches(&texts, 1000);
        assert_eq!(batches[0].len(), 1);
        assert_eq!(batches.iter().map(|b| b.len()).sum::<usize>(), 3);
        assert!(scan_batches(&[], 1000).is_empty());
    }

    #[test]
    fn apply_tripwire_drops_a_flagged_span_and_counts_its_rule() {
        let spans = vec![
            span("s0", "the postgres password is hunter2"),
            span("s1", "an ordinary sentence with no secret in it"),
        ];
        let rules = vec![Some("credential".to_string()), None];
        let mut counters = PlanCounters::default();

        let kept = apply_tripwire(spans, rules, uuid::Uuid::nil(), &mut counters).unwrap();

        assert_eq!(kept.len(), 1, "the flagged span must not reach a chunk file");
        assert_eq!(kept[0].id, "s1");
        assert_eq!(counters.spans_dropped_tripwire.get("credential"), Some(&1));
    }

    #[test]
    fn apply_tripwire_refuses_a_mismatched_rule_count_rather_than_zip_silently() {
        let spans = vec![span("s0", "one span")];
        let rules = vec![None, None];
        let mut counters = PlanCounters::default();

        let result = apply_tripwire(spans, rules, uuid::Uuid::nil(), &mut counters);

        assert!(result.is_err(), "a length mismatch must refuse rather than guess an alignment");
    }

    #[test]
    fn the_table_names_a_tripwire_drop_by_its_rule() {
        let mut w = worklist();
        w.counters.tripwire("credential");
        w.counters.tripwire("credential");
        let text = render_table(&w);
        assert!(text.contains("2 spans refused before extraction"), "{text}");
        assert!(text.contains("credential 2"), "{text}");
    }

    #[test]
    fn a_clean_run_says_nothing_about_the_tripwire() {
        assert!(!render_table(&worklist()).contains("tripwire"));
    }
}
