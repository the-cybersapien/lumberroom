//! Recall against a fixture the owner wrote, scored server-side.
//!
//! # What this measures
//!
//! Whether the store answers questions the owner actually asks, phrased the way he asks them. Each
//! case names a question and the row that should come back for it, so a number here says "the
//! things I put in are the things I get out". That is the opposite bias to a public benchmark: it
//! can only test what he thought to ask, it goes stale as the store grows, and a case whose target
//! row was superseded scores as a miss for a reason that has nothing to do with ranking. A rising
//! number means the fixture's questions retrieve better. It says nothing about questions nobody
//! wrote down.
//!
//! # What this does not measure
//!
//! Answer quality, since nothing here reads the content of a hit. Ranking against somebody else's
//! haystack, which is `lumberroom eval-longmemeval` and a different question. HNSW's approximation
//! error, which is `services::recall`, measured against an exact scan rather than against an
//! expectation. Coverage of the store, since a fixture of twenty cases touches twenty rows.
//!
//! # Anti-cases
//!
//! A case marked `Expect::Nothing` names a question the store must answer with silence. A hit on
//! one is worth more than a recall point and is reported apart from the aggregate: recall says the
//! store was quiet when it should have spoken, and a violation says it spoke confidently about
//! something it does not hold. Averaging the second into the first hides it.
//!
//! # Cases arrive as data
//!
//! Nothing here reads a file. The fixture is a client's to parse and hand over, which is what lets
//! a route, a test and a scheduled job all call this with the same arithmetic. The arithmetic is in
//! pure functions below for the same reason.
//!
//! `bin/lumberroom.mjs eval` and `lumberroom eval` both compute these numbers client-side over
//! `memory_search`. The pure functions here are pinned by the same test vectors those clients use,
//! because three implementations of one metric is three chances to publish a different number.

use serde::{Deserialize, Serialize};

use super::Ctx;
use crate::domain::errors::{DomainError, Result};

/// The rank cut every metric here reads. `bin/lumberroom.mjs` asks `memory_search` for five hits and
/// scores recall@1 and recall@5 out of that one call, so a deeper fetch would change the numbers
/// without changing the question.
pub const EVAL_LIMIT: i64 = 5;

/// A fixture is small by construction. This bound exists so a route cannot be handed a file that
/// turns one request into ten thousand searches.
pub const MAX_CASES: usize = 500;

/// What a case expects back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// The row that should come back, by id. A miss is a recall point.
    Id(String),
    /// Silence. Any hit at all is a violation.
    Nothing,
}

/// A fixture line as `bin/lumberroom.mjs` and `client/eval-fixture.example.jsonl` write it.
///
/// `EvalCase` converts through this rather than deriving its own shape, so a caller can hand over
/// the owner's existing JSONL line by line and nobody has to keep two fixture formats alive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalCaseWire {
    pub question: String,
    /// The row this question should return.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_id: Option<String>,
    /// The literal `"none"`, which marks an anti-case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// One question and what it should return.
///
/// A case naming neither `expect_id` nor `expect: "none"` is refused rather than scored. The node
/// client scores it as a guaranteed miss, which turns a typo in a fixture key into a recall
/// regression the owner would go hunting for in the ranking.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "EvalCaseWire", into = "EvalCaseWire")]
pub struct EvalCase {
    pub question: String,
    pub expect: Expect,
    /// Why the owner wrote this case down. Carried through to the report so a failure line says
    /// what was being protected rather than only which question broke.
    pub origin: Option<String>,
}

impl TryFrom<EvalCaseWire> for EvalCase {
    type Error = String;

    fn try_from(w: EvalCaseWire) -> std::result::Result<Self, Self::Error> {
        // `expect === 'none'` and nothing looser, which is what `bin/lumberroom.mjs` compares. A fixture
        // saying `"None"` falls to the refusal below rather than being read as an anti-case.
        let anti = w.expect.as_deref() == Some("none");
        // The id is carried as written, because `rank_of` compares byte for byte. Only the
        // emptiness test trims, which asks whether the key carries anything at all.
        let named = w.expect_id.as_deref().filter(|s| !s.trim().is_empty());
        let expect = match (anti, named, w.expect.as_deref()) {
            (true, Some(_), _) => {
                return Err(format!(
                    "case {:?} names both expect_id and expect: \"none\". One says the store holds \
                     this row and the other says it holds nothing for the question.",
                    clip(&w.question)
                ))
            }
            (true, None, _) => Expect::Nothing,
            (false, Some(id), _) => Expect::Id(id.to_string()),
            (false, None, Some(other)) => {
                return Err(format!(
                    "case {:?} sets expect to {other:?}. The only value that means anything is \
                     \"none\", which marks a question the store must answer with nothing.",
                    clip(&w.question)
                ))
            }
            (false, None, None) => {
                return Err(format!(
                    "case {:?} names neither expect_id nor expect: \"none\", so there is nothing \
                     to score it against.",
                    clip(&w.question)
                ))
            }
        };
        Ok(Self { question: w.question, expect, origin: w.origin })
    }
}

impl From<EvalCase> for EvalCaseWire {
    fn from(c: EvalCase) -> Self {
        match c.expect {
            Expect::Id(id) => {
                Self { question: c.question, expect_id: Some(id), expect: None, origin: c.origin }
            }
            Expect::Nothing => Self {
                question: c.question,
                expect_id: None,
                expect: Some("none".into()),
                origin: c.origin,
            },
        }
    }
}

/// Enough of a question to recognise it in an error, short enough not to paste a fixture into a log.
fn clip(question: &str) -> String {
    question.chars().take(60).collect()
}

impl EvalCase {
    pub fn hit(question: impl Into<String>, expect_id: impl Into<String>) -> Self {
        Self { question: question.into(), expect: Expect::Id(expect_id.into()), origin: None }
    }

    pub fn anti(question: impl Into<String>) -> Self {
        Self { question: question.into(), expect: Expect::Nothing, origin: None }
    }

    pub fn with_origin(mut self, origin: impl Into<String>) -> Self {
        self.origin = Some(origin.into());
        self
    }
}

/// A question the store answered when it should have stayed quiet.
#[derive(Debug, Clone, Serialize)]
pub struct Violation {
    pub question: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// The top hit's id, which is the row to go look at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub got: Option<String>,
}

/// One normal case, kept so a report names the cases that missed rather than only counting them.
#[derive(Debug, Clone, Serialize)]
pub struct CaseOutcome {
    pub question: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    pub expect_id: String,
    /// Zero-based position of the expected row, absent when it never appeared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalReport {
    pub cases: usize,
    pub normal_cases: usize,
    pub anti_cases: usize,
    /// `None` when the fixture holds no normal case. Zero would read as "every question missed",
    /// which is a different claim from "nothing was asked".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recall_at_1: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recall_at_5: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mrr: Option<f64>,
    pub violations: Vec<Violation>,
    pub outcomes: Vec<CaseOutcome>,
    pub limit: i64,
}

impl EvalReport {
    /// True when the store answered a question it holds nothing for. Read this before the recall
    /// numbers, which cannot express it.
    pub fn has_violations(&self) -> bool {
        !self.violations.is_empty()
    }
}

/// Where the expected id landed in a rank-ordered hit list, zero-based.
///
/// Byte for byte, which is what `bin/lumberroom.mjs` compares. Folding case here would score a hit on a
/// fixture that client scores as a miss, and one fixture producing two recall numbers is the thing
/// this file exists to prevent. The store writes lowercase UUIDs, so a case needing the fold is a
/// case whose id was mistyped.
pub fn rank_of(hit_ids: &[String], expect_id: &str) -> Option<usize> {
    hit_ids.iter().position(|id| id == expect_id)
}

/// Fraction of ranks that landed inside the top `k`, counting a miss as zero.
///
/// `None` for an empty set, because a mean over nothing is not zero. `ranks` carries one entry per
/// normal case, `None` where the expected row never appeared.
pub fn recall_at(ranks: &[Option<usize>], k: usize) -> Option<f64> {
    if ranks.is_empty() {
        return None;
    }
    let hits = ranks.iter().filter(|r| r.is_some_and(|rank| rank < k)).count();
    Some(hits as f64 / ranks.len() as f64)
}

/// Mean reciprocal rank over the same set. A miss contributes zero rather than being dropped, so
/// MRR falls when retrieval fails instead of being computed over the cases that happened to work.
pub fn mean_reciprocal_rank(ranks: &[Option<usize>]) -> Option<f64> {
    if ranks.is_empty() {
        return None;
    }
    let sum: f64 = ranks.iter().map(|r| r.map_or(0.0, |rank| 1.0 / (rank + 1) as f64)).sum();
    Some(sum / ranks.len() as f64)
}

/// Score a set of cases whose searches have already run.
///
/// Split out from `run` so the arithmetic can be exercised without a database, an embedder or a
/// principal. `results` pairs each case with the ids that came back for it, in rank order.
pub fn score(results: &[(EvalCase, Vec<String>)], limit: i64) -> EvalReport {
    let mut ranks: Vec<Option<usize>> = Vec::new();
    let mut outcomes: Vec<CaseOutcome> = Vec::new();
    let mut violations: Vec<Violation> = Vec::new();

    for (case, hit_ids) in results {
        match &case.expect {
            Expect::Nothing => {
                // Any hit, at any score. A threshold here would let a confident wrong answer pass
                // by being a little less confident, and the fixture's whole claim about these
                // questions is that the store holds nothing for them.
                if !hit_ids.is_empty() {
                    violations.push(Violation {
                        question: case.question.clone(),
                        origin: case.origin.clone(),
                        got: hit_ids.first().cloned(),
                    });
                }
            }
            Expect::Id(expect_id) => {
                let rank = rank_of(hit_ids, expect_id);
                ranks.push(rank);
                outcomes.push(CaseOutcome {
                    question: case.question.clone(),
                    origin: case.origin.clone(),
                    expect_id: expect_id.clone(),
                    rank,
                });
            }
        }
    }

    EvalReport {
        cases: results.len(),
        normal_cases: ranks.len(),
        anti_cases: results.len() - ranks.len(),
        recall_at_1: recall_at(&ranks, 1),
        recall_at_5: recall_at(&ranks, 5),
        mrr: mean_reciprocal_rank(&ranks),
        violations,
        outcomes,
        limit,
    }
}

/// Run every case through the caller's own search and score the results.
///
/// Searches run with this principal's grant, so a narrow token scores its own view of the store and
/// a case pointing at a row it may not read is a miss. That is the honest reading: the number
/// describes retrieval as this client experiences it.
///
/// Cases run in order rather than concurrently. Each one embeds a query, and a fixture racing
/// itself through one embedder measures queueing.
pub async fn run(ctx: &Ctx, cases: &[EvalCase]) -> Result<EvalReport> {
    if cases.is_empty() {
        return Err(DomainError::validation(
            "the fixture holds no cases. Each case is a question and the row it should return, or \
             a question the store must answer with nothing.",
        ));
    }
    if cases.len() > MAX_CASES {
        return Err(DomainError::validation(format!(
            "the fixture holds {} cases, limit is {MAX_CASES}. This is one search per case against \
             a curated fixture, not a benchmark harness.",
            cases.len()
        )));
    }
    for (i, case) in cases.iter().enumerate() {
        if case.question.trim().is_empty() {
            return Err(DomainError::validation(format!("case {} has an empty question", i + 1)));
        }
        if let Expect::Id(id) = &case.expect {
            if id.trim().is_empty() {
                return Err(DomainError::validation(format!(
                    "case {} expects a row and names no id. Use the anti-case form to say the \
                     store should return nothing.",
                    i + 1
                )));
            }
        }
    }

    let mut results: Vec<(EvalCase, Vec<String>)> = Vec::with_capacity(cases.len());
    for case in cases {
        // Namespaces, project and history are all left to the default, which is what the clients
        // send. A fixture case is a question the owner would type, and typing it does not carry a
        // namespace either.
        let found =
            super::search::run(ctx, &case.question, None, Some(EVAL_LIMIT), None, None, None)
                .await?;
        results.push((case.clone(), found.hits.into_iter().map(|h| h.id).collect()));
    }

    Ok(score(&results, EVAL_LIMIT))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // These vectors are the ones `crates/lumberroom/src/commands.rs` pins for its own copy of this
    // arithmetic. Change one and change both, or the two clients publish different numbers for the
    // same fixture.

    #[test]
    fn rank_reads_the_first_position_and_nothing_after_it() {
        assert_eq!(rank_of(&ids(&["a", "b", "c"]), "a"), Some(0));
        assert_eq!(rank_of(&ids(&["a", "b", "c"]), "c"), Some(2));
        assert_eq!(rank_of(&ids(&["a", "b"]), "z"), None);
        assert_eq!(rank_of(&[], "a"), None);
    }

    #[test]
    fn a_mistyped_id_misses_here_exactly_as_it_misses_in_the_node_client() {
        let stored = ids(&["9f1c2b4e-0000-4a1b-8c3d-1122334455aa"]);
        assert_eq!(rank_of(&stored, "9f1c2b4e-0000-4a1b-8c3d-1122334455aa"), Some(0));
        assert_eq!(rank_of(&stored, "9F1C2B4E-0000-4A1B-8C3D-1122334455AA"), None);
        assert_eq!(rank_of(&stored, " 9f1c2b4e-0000-4a1b-8c3d-1122334455aa"), None);
    }

    #[test]
    fn recall_counts_a_miss_as_zero_rather_than_dropping_it() {
        let ranks = vec![Some(0), None, Some(3)];
        assert_eq!(recall_at(&ranks, 1), Some(1.0 / 3.0));
        assert_eq!(recall_at(&ranks, 5), Some(2.0 / 3.0));
    }

    #[test]
    fn recall_over_no_cases_is_absent_rather_than_zero() {
        assert_eq!(recall_at(&[], 1), None);
        assert_eq!(mean_reciprocal_rank(&[]), None);
    }

    #[test]
    fn mrr_pins_to_a_hand_computed_value() {
        // 1/1 + 0 + 1/4, over three cases.
        let ranks = vec![Some(0), None, Some(3)];
        let got = mean_reciprocal_rank(&ranks).expect("three cases");
        assert!((got - (1.25 / 3.0)).abs() < 1e-12, "{got}");
    }

    #[test]
    fn a_case_that_never_appears_scores_zero_everywhere() {
        let results = vec![(EvalCase::hit("where do I live", "row-1"), ids(&["x", "y"]))];
        let r = score(&results, EVAL_LIMIT);
        assert_eq!(r.normal_cases, 1);
        assert_eq!(r.recall_at_1, Some(0.0));
        assert_eq!(r.recall_at_5, Some(0.0));
        assert_eq!(r.mrr, Some(0.0));
        assert_eq!(r.outcomes[0].rank, None);
        assert!(!r.has_violations());
    }

    #[test]
    fn an_anti_case_that_returns_anything_is_a_violation_and_stays_out_of_recall() {
        let results = vec![
            (EvalCase::hit("what os do I run", "row-1"), ids(&["row-1"])),
            (
                EvalCase::anti("my bank account number").with_origin("never stored"),
                ids(&["row-9", "row-8"]),
            ),
        ];
        let r = score(&results, EVAL_LIMIT);
        assert_eq!(r.cases, 2);
        assert_eq!(r.normal_cases, 1);
        assert_eq!(r.anti_cases, 1);
        // The perfect normal case stays perfect. A violation must not be averaged away, and it must
        // not drag the recall number either.
        assert_eq!(r.recall_at_1, Some(1.0));
        assert_eq!(r.mrr, Some(1.0));
        assert!(r.has_violations());
        assert_eq!(r.violations.len(), 1);
        assert_eq!(r.violations[0].got.as_deref(), Some("row-9"));
        assert_eq!(r.violations[0].origin.as_deref(), Some("never stored"));
    }

    #[test]
    fn a_quiet_anti_case_is_no_violation() {
        let results = vec![(EvalCase::anti("a wifi password I never mentioned"), vec![])];
        let r = score(&results, EVAL_LIMIT);
        assert!(!r.has_violations());
        assert_eq!(r.anti_cases, 1);
        assert_eq!(r.normal_cases, 0);
    }

    #[test]
    fn a_fixture_of_only_anti_cases_reports_no_recall_at_all() {
        let results = vec![(EvalCase::anti("q1"), vec![]), (EvalCase::anti("q2"), ids(&["row-2"]))];
        let r = score(&results, EVAL_LIMIT);
        assert_eq!(r.recall_at_1, None);
        assert_eq!(r.recall_at_5, None);
        assert_eq!(r.mrr, None);
        assert_eq!(r.violations.len(), 1);
    }

    #[test]
    fn an_empty_fixture_scores_to_an_empty_report() {
        let r = score(&[], EVAL_LIMIT);
        assert_eq!(r.cases, 0);
        assert_eq!(r.normal_cases, 0);
        assert_eq!(r.anti_cases, 0);
        assert_eq!(r.recall_at_1, None);
        assert!(!r.has_violations());
    }

    #[test]
    fn rank_five_is_outside_the_cut_this_fixture_scores() {
        let hits = ids(&["a", "b", "c", "d", "e", "row-1"]);
        // Six ids is more than `run` ever produces at `EVAL_LIMIT`, and the cut still has to hold
        // if a caller scores a deeper list.
        let r = score(&[(EvalCase::hit("q", "row-1"), hits)], EVAL_LIMIT);
        assert_eq!(r.recall_at_5, Some(0.0));
        assert_eq!(r.outcomes[0].rank, Some(5));
        assert!((r.mrr.expect("one case") - 1.0 / 6.0).abs() < 1e-12);
    }

    #[test]
    fn the_report_serialises_under_snake_case_names_the_clients_read() {
        let r = score(&[(EvalCase::hit("q", "row-1"), ids(&["row-1"]))], EVAL_LIMIT);
        let v = serde_json::to_value(&r).expect("serialises");
        assert!(v.get("recall_at_1").is_some());
        assert!(v.get("recall_at_5").is_some());
        assert!(v.get("mrr").is_some());
        assert!(v.get("normal_cases").is_some());
        assert!(v.get("anti_cases").is_some());
        assert!(v.get("violations").is_some());
    }

    #[test]
    fn a_case_deserialises_from_the_fixture_line_the_owner_already_has() {
        // Both lines are copied out of `client/eval-fixture.example.jsonl`, shortened.
        let case: EvalCase = serde_json::from_str(
            r#"{"question":"what os does my desktop run","expect_id":"row-1","origin":"user:me"}"#,
        )
        .expect("id form");
        assert_eq!(case.expect, Expect::Id("row-1".into()));
        assert_eq!(case.origin.as_deref(), Some("user:me"));

        let anti: EvalCase =
            serde_json::from_str(r#"{"question":"my bank account number","expect":"none"}"#)
                .expect("anti form");
        assert_eq!(anti.expect, Expect::Nothing);
        assert_eq!(anti.origin, None);
    }

    #[test]
    fn a_case_that_scores_against_nothing_is_refused_rather_than_counted_as_a_miss() {
        // A typo'd `expect_id` key is the case this catches. The node client scores it zero and
        // says nothing, which reads as a ranking regression.
        let e = serde_json::from_str::<EvalCase>(r#"{"question":"what os"}"#).unwrap_err();
        assert!(e.to_string().contains("neither expect_id nor"), "{e}");

        let e =
            serde_json::from_str::<EvalCase>(r#"{"question":"q","expect":"maybe"}"#).unwrap_err();
        assert!(e.to_string().contains("only value that means anything"), "{e}");

        // `"None"` is not `"none"`. Reading it as an anti-case would score a case the node client
        // scores as a normal miss.
        let e =
            serde_json::from_str::<EvalCase>(r#"{"question":"q","expect":"None"}"#).unwrap_err();
        assert!(e.to_string().contains("only value that means anything"), "{e}");

        let both = r#"{"question":"q","expect":"none","expect_id":"row-1"}"#;
        let e = serde_json::from_str::<EvalCase>(both).unwrap_err();
        assert!(e.to_string().contains("names both"), "{e}");
    }

    #[test]
    fn a_case_round_trips_back_into_a_fixture_line() {
        let line = r#"{"question":"what os","expect_id":"row-1","origin":"user:me"}"#;
        let case: EvalCase = serde_json::from_str(line).expect("parses");
        let back = serde_json::to_value(&case).expect("serialises");
        assert_eq!(back["expect_id"], "row-1");
        assert!(back.get("expect").is_none());

        let anti = EvalCase::anti("q");
        let back = serde_json::to_value(&anti).expect("serialises");
        assert_eq!(back["expect"], "none");
        assert!(back.get("expect_id").is_none());
    }
}
