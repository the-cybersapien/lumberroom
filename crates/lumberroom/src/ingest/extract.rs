//! `lumberroom ingest extract`, the Mode B driver.
//!
//! Consumes the same `spans/` and writes the same `out/` that Mode A does, so `submit` cannot tell
//! which produced a chunk.

use std::io::IsTerminal;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;

use crate::client::{err, err_code, Result};
use crate::ingest::provider::{extract_chunk, resolve, Provider};
use crate::ingest::{write_json, ChunkOutput, FailedChunk, RunPaths, RunState, Usage};

#[derive(Debug, Clone)]
pub struct ExtractArgs {
    pub run_id: uuid::Uuid,
    pub provider: String,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub concurrency: usize,
    pub rpm: Option<u32>,
    pub timeout_secs: u64,
    /// Print the estimate and the first prompt, call nothing.
    pub dry_run: bool,
    /// Only the chunks `state.json` marked failed, and only those.
    pub retry_failed: bool,
    pub yes: bool,
}

/// One request gets three retries on a 429 or a 5xx, exponential from two seconds. Any other 4xx
/// is a bad key or a bad model id and does not improve on a second try.
const MAX_ATTEMPTS: u32 = 4;
const INITIAL_BACKOFF: Duration = Duration::from_secs(2);

pub async fn run(args: &ExtractArgs) -> Result<RunState> {
    let paths = RunPaths::new(args.run_id)?;
    paths.create()?;
    let worklist = paths.read_worklist()?;
    let mut state = paths.read_state();

    let env = crate::config::ProcessEnv;
    let cfg_path = crate::config::config_path(&env);
    let file = crate::config::FileConfig::load(cfg_path);
    let provider = resolve(&args.provider, &file, args.model.as_deref(), args.base_url.as_deref())?;
    state.provider = Some(args.provider.clone());
    state.model = Some(provider.model.clone());

    let total = worklist.chunks.len();
    let todo: Vec<usize> = if args.retry_failed {
        state.failed.iter().map(|f| f.index).collect()
    } else {
        worklist.chunks.iter().map(|c| c.index).filter(|i| !paths.chunk_out(*i).exists()).collect()
    };

    if todo.is_empty() {
        crate::out("nothing to send: every chunk already has an out/ file");
        return Ok(state);
    }

    let mut users: Vec<(usize, String)> = Vec::with_capacity(todo.len());
    let mut total_chars = 0usize;
    for idx in todo {
        let text = std::fs::read_to_string(paths.chunk_in(idx))
            .map_err(|e| err(format!("could not read spans/chunk-{idx:02}.json: {e}")))?;
        total_chars += text.chars().count();
        users.push((idx, text));
    }
    let token_estimate = total_chars / 4;

    if args.dry_run {
        print_estimate(&provider, users.len(), total_chars, token_estimate);
        let (first_idx, first_user) = &users[0];
        let system = crate::ingest::prompt::provider_system(first_idx + 1, total);
        crate::out("--- first prompt: system ---");
        crate::out(&system);
        crate::out("--- first prompt: user ---");
        crate::out(first_user);
        return Ok(state);
    }

    // Printed unconditionally, `--yes` included: a cron log with no record of what was sent
    // where is not a log.
    print_estimate(&provider, users.len(), total_chars, token_estimate);

    if !args.yes {
        if !std::io::stdin().is_terminal() {
            return Err(err("extract needs --yes to send chunks with no terminal to confirm on"));
        }
        crate::prompt("continue? [y/N] ");
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .map_err(|e| err(format!("could not read the answer: {e}")))?;
        if !answer.trim().eq_ignore_ascii_case("y") && !answer.trim().eq_ignore_ascii_case("yes") {
            return Err(err("not confirmed, sending nothing"));
        }
    }

    let http = reqwest::Client::new();
    let semaphore = Arc::new(Semaphore::new(args.concurrency.max(1)));
    let spacing = args.rpm.filter(|r| *r > 0).map(|r| Duration::from_millis(60_000 / u64::from(r)));

    let mut handles = Vec::with_capacity(users.len());
    for (idx, user) in users {
        if let Some(gap) = spacing {
            tokio::time::sleep(gap).await;
        }
        let permit = semaphore.clone();
        let http = http.clone();
        let provider = provider.clone();
        let paths = paths.clone();
        let system = crate::ingest::prompt::provider_system(idx + 1, total);
        let timeout_secs = args.timeout_secs;
        handles.push(tokio::spawn(async move {
            let _permit = permit.acquire_owned().await.expect("the semaphore is never closed");
            let started = Instant::now();
            let outcome =
                run_chunk_with_retry(&http, &provider, &system, &user, timeout_secs).await;
            // Written inside the task, not after every handle is awaited in order: a stuck chunk
            // 0 must not hold up disk writes for chunks 1..N that finished over the network while
            // it hangs, or a kill mid-run throws away work that already completed.
            let outcome = outcome.and_then(|(output, usage)| {
                match write_json(&paths.chunk_out(idx), &output) {
                    Ok(()) => Ok((output, usage)),
                    Err(e) => Err(("write_failed".to_string(), Some(e.message))),
                }
            });
            (idx, outcome, started.elapsed())
        }));
    }

    let mut running_total = state.done.len();
    for handle in handles {
        let (idx, outcome, elapsed) =
            handle.await.map_err(|e| err(format!("a chunk task did not finish: {e}")))?;
        match outcome {
            Ok((output, usage)) => {
                state.done.retain(|d| *d != idx);
                state.done.push(idx);
                state.failed.retain(|f| f.index != idx);
                state.usage.requests += usage.requests;
                state.usage.prompt_tokens += usage.prompt_tokens;
                state.usage.completion_tokens += usage.completion_tokens;
                running_total += 1;
                let facts = fact_summary(&output);
                crate::out(&format!(
                    "chunk {idx:02}  {facts:<16} {:>6}ms  ({running_total}/{total})",
                    elapsed.as_millis()
                ));
            }
            Err((reason, detail)) => {
                state.failed.retain(|f| f.index != idx);
                state.failed.push(FailedChunk { index: idx, reason: reason.clone(), detail });
                crate::out(&format!(
                    "chunk {idx:02}  failed: {reason:<16} {:>6}ms",
                    elapsed.as_millis()
                ));
            }
        }
        state.updated_at = chrono::Utc::now();
        paths.write_state(&state)?;
    }

    print_usage(&state.usage);
    Ok(state)
}

fn fact_summary(output: &ChunkOutput) -> String {
    if output.facts.is_empty() {
        "<no-facts/>".to_string()
    } else if output.facts.len() == 1 {
        "1 fact".to_string()
    } else {
        format!("{} facts", output.facts.len())
    }
}

fn print_estimate(provider: &Provider, chunks: usize, chars: usize, tokens: usize) {
    crate::out(&format!("chunks {chunks} · {chars} chars · ~{tokens} tokens (4 chars/token)"));
    crate::out(&format!("provider {} at {}", provider.name, provider.base_url));
    crate::out(&format!("model {}", provider.model));
}

fn print_usage(usage: &Usage) {
    crate::out(&format!(
        "usage: {} requests, {} prompt tokens, {} completion tokens",
        usage.requests, usage.prompt_tokens, usage.completion_tokens
    ));
}

/// One chunk end to end: the timeout wrap that turns a hang into a failed chunk, then up to
/// `MAX_ATTEMPTS` tries on a 429 or a 5xx. Everything else is fatal on the first response, because
/// a bad key or a bad model id does not improve by being sent again (§10.5).
async fn run_chunk_with_retry(
    http: &reqwest::Client,
    provider: &Provider,
    system: &str,
    user: &str,
    timeout_secs: u64,
) -> std::result::Result<(ChunkOutput, Usage), (String, Option<String>)> {
    let mut attempt = 0u32;
    let mut backoff = INITIAL_BACKOFF;

    loop {
        attempt += 1;
        let attempted = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            extract_chunk(http, provider, system, user, timeout_secs),
        )
        .await;

        let result = match attempted {
            Ok(inner) => inner,
            Err(_) => {
                Err(err_code(format!("{} timed out after {timeout_secs}s", provider.name), 3))
            }
        };

        match result {
            Ok(pair) => return Ok(pair),
            Err(e) => {
                let retryable = e.code == 429 || (500..600).contains(&e.code);
                if retryable && attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(backoff).await;
                    backoff *= 2;
                    continue;
                }
                return Err((classify(e.code), Some(crate::client::truncate(&e.message, 200))));
            }
        }
    }
}

fn classify(code: i32) -> String {
    match code {
        429 => "http_429".to_string(),
        3 => "timeout".to_string(),
        c if (400..600).contains(&c) => format!("http_{c}"),
        _ => "unparseable".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_names_the_reason_state_json_records() {
        assert_eq!(classify(429), "http_429");
        assert_eq!(classify(500), "http_500");
        assert_eq!(classify(400), "http_400");
        assert_eq!(classify(3), "timeout");
        assert_eq!(classify(1), "unparseable");
    }

    #[test]
    fn fact_summary_names_the_common_and_the_empty_case() {
        assert_eq!(fact_summary(&ChunkOutput::default()), "<no-facts/>");
        let one = ChunkOutput {
            facts: vec![crate::ingest::ExtractedFact {
                content: "c".into(),
                namespace: "user:me".into(),
                tags: vec![],
                source_span_id: "s1".into(),
                speaker: None,
                quote: None,
                confidence: None,
            }],
            refusal: None,
        };
        assert_eq!(fact_summary(&one), "1 fact");
    }
}
