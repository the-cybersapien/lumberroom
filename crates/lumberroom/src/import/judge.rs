//! Asking a model which dump lines are not durable facts.
//!
//! `junk::deterministic` decides structure and refuses to decide meaning, for a reason written out
//! there: every rule that catches a worthless aside also catches a terse true fact. This module is
//! where meaning gets decided, and it is opt-out rather than opt-in only because the owner sees the
//! verdicts before anything acts on them.
//!
//! # The model votes, the owner decides
//!
//! Nothing here drops a line on its own unless the owner passed `--drop-junk`. The default shows
//! what the model flagged and asks. That is not politeness: a memory store is the one place where a
//! confidently wrong deletion is invisible afterwards, because the missing fact leaves no gap that
//! anybody notices until they need it.
//!
//! # It asks for junk, not for keeps
//!
//! The model returns the indices it considers junk, and everything it does not name survives.
//! Inverting that, asking which lines to keep, makes a truncated or lazy answer delete the tail of
//! somebody's memory, and a model that returns an empty array would wipe the import. Under this
//! shape the same failures keep everything, which is the direction to fail in.

use serde_json::Value;

use crate::client::Result;
use crate::ingest::provider::{self, Provider};
use crate::ingest::Usage;

/// One line's verdict, in dump order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub index: usize,
    pub why: String,
}

/// How many lines go in one request. Small enough that a model answers about all of them rather
/// than summarising, large enough that a hundred-line dump is a handful of calls.
pub const BATCH: usize = 25;

const SYSTEM: &str = "\
You are reviewing lines exported from an assistant's memory about one person. Each line claims to \
be a durable fact. Your job is to name the ones that are not.

A line is junk when it cannot stand on its own in six months. That covers a turn of conversation \
captured as though it were a fact, an instruction that only meant something in the message it was \
typed into, an aside about a passing feeling or a price, a fragment with no subject, and a line \
that says nothing beyond what another line already says.

A line is NOT junk merely for being short, blunt, mundane, oddly worded, written in the first \
person, or phrased as a command. Terse facts are still facts.

Two kinds of line look disposable and are not. Keep both.

A decision the person made is durable however briefly it is written: choosing one thing over \
another, buying or selling, starting or stopping something, applying for something, switching \
provider or account. \"Sold X to buy Y\" is a decision with a reason behind it, not a fragment, and \
it is among the most valuable lines in the whole export.

A standing instruction is durable when it describes how the person wants work done from now on, \
including one that names a recurring time. Only an instruction that made sense solely inside the \
message it was typed into is junk.

When you are unsure, leave it out of your answer: keeping a weak fact costs a moment of the owner's \
attention, and dropping a real one loses it silently.

Answer with a JSON object and nothing else:

  {\"junk\": [{\"index\": 0, \"why\": \"a few words, no more\"}]}

`index` is the number shown against the line. Name only lines you are confident about. An empty \
array is a correct and common answer.";

fn user_block(lines: &[(usize, String)]) -> String {
    let mut s = String::from("Lines:\n\n");
    for (i, text) in lines {
        s.push_str(&format!("{i}. {text}\n"));
    }
    s
}

fn shaped(v: &Value) -> bool {
    v.get("junk").map(|j| j.is_array()).unwrap_or(false)
}

/// Ask about one batch. Indices are the caller's, passed through untouched, so a partial answer
/// cannot shift verdicts onto the wrong lines.
pub async fn assess_batch(
    http: &reqwest::Client,
    p: &Provider,
    lines: &[(usize, String)],
    timeout_secs: u64,
) -> Result<(Vec<Verdict>, Usage)> {
    if lines.is_empty() {
        return Ok((Vec::new(), Usage::default()));
    }
    let (value, usage) =
        provider::call_json(http, p, SYSTEM, &user_block(lines), timeout_secs, shaped).await?;
    Ok((verdicts_from(&value, lines), usage))
}

/// An index the batch never contained is dropped rather than trusted. A model that answers about
/// line 900 of a 25 line batch has lost track, and honouring it would delete something nobody
/// looked at.
pub fn verdicts_from(value: &Value, lines: &[(usize, String)]) -> Vec<Verdict> {
    let known: std::collections::HashSet<usize> = lines.iter().map(|(i, _)| *i).collect();
    value
        .get("junk")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    let index = v.get("index").and_then(Value::as_u64)? as usize;
                    if !known.contains(&index) {
                        return None;
                    }
                    let why = v
                        .get("why")
                        .and_then(Value::as_str)
                        .unwrap_or("the model did not say")
                        .chars()
                        .take(120)
                        .collect();
                    Some(Verdict { index, why })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A provider that cannot be reached is not a verdict of "keep everything" and not a reason to
/// abandon the import. The caller reports the failure and leaves every line in.
pub fn describe_failure(e: &crate::client::CliError) -> String {
    format!(
        "the judgement pass could not run: {}. Every line was kept. \
         Pass --keep-all to skip it deliberately.",
        e.message
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lines() -> Vec<(usize, String)> {
        vec![
            (0, "Prefers a dark terminal.".to_string()),
            (1, "These are too expensive.".to_string()),
            (2, "The office laptop runs Debian 13.".to_string()),
        ]
    }

    #[test]
    fn the_indices_the_model_names_are_the_ones_returned() {
        let v = json!({"junk": [{"index": 1, "why": "an aside about a price"}]});
        let got = verdicts_from(&v, &lines());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].index, 1);
        assert_eq!(got[0].why, "an aside about a price");
    }

    /// A model that answers about a line the batch never held has lost the thread, and honouring it
    /// deletes something nobody reviewed.
    #[test]
    fn an_index_outside_the_batch_is_ignored() {
        let v = json!({"junk": [{"index": 900, "why": "invented"}, {"index": 2, "why": "real"}]});
        let got = verdicts_from(&v, &lines());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].index, 2);
    }

    /// Every degenerate answer has to mean "keep everything", because the alternative direction
    /// loses facts silently.
    #[test]
    fn a_missing_empty_or_malformed_answer_keeps_every_line() {
        for v in [
            json!({"junk": []}),
            json!({}),
            json!({"junk": "none"}),
            json!({"junk": [{"why": "no index at all"}]}),
            json!({"junk": [{"index": "one"}]}),
        ] {
            assert!(verdicts_from(&v, &lines()).is_empty(), "{v} should keep everything");
        }
    }

    #[test]
    fn a_verdict_with_no_reason_still_says_something() {
        let v = json!({"junk": [{"index": 0}]});
        let got = verdicts_from(&v, &lines());
        assert_eq!(got[0].why, "the model did not say");
    }

    #[test]
    fn the_shape_check_accepts_only_an_object_carrying_a_junk_array() {
        assert!(shaped(&json!({"junk": []})));
        assert!(!shaped(&json!({"keep": []})));
        assert!(!shaped(&json!({"junk": 3})));
    }

    /// The prompt has to keep telling the model that terse is not junk. Losing this line is how a
    /// judgement pass starts eating real facts.
    /// The prompt has to keep telling the model that terse is not junk. Losing these lines is how a
    /// judgement pass starts eating real facts, and one model already proved it: judging a real
    /// export, glm-5.3 called a portfolio decision and a standing weekly instruction fragments with
    /// no durable meaning, on no evidence but their brevity and imperative mood.
    #[test]
    fn the_prompt_defends_short_facts_decisions_and_standing_instructions() {
        assert!(SYSTEM.contains("Terse facts are still facts"));
        assert!(SYSTEM.contains("When you are unsure, leave it out"));
        assert!(SYSTEM.contains("A decision the person made is durable"));
        assert!(SYSTEM.contains("A standing instruction is durable"));
    }

    #[test]
    fn an_empty_batch_asks_nothing() {
        let got = verdicts_from(&json!({"junk": [{"index": 0, "why": "x"}]}), &[]);
        assert!(got.is_empty());
    }
}
