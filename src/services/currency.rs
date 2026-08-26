//! Does the store report the fact that held, rather than every fact it ever held?
//!
//! Decision 0014 part 2. Distinct from `services::eval`, which asks whether the right row is
//! retrievable at all, and from `services::recall`, which asks whether the index agrees with an
//! exact scan. This asks a question neither of those can: given a store holding a fact and the fact
//! that replaced it, and a question asked at an instant, does the answer name the one that held
//! then. **Returning both is a failure**, and it is the failure the record was written about.
//!
//! # Two numbers, and only one of them needs a fixture
//!
//! **Coverage** counts supersession pairs and asks how many carry a closed interval. A pair whose
//! retired half has no `occurred_until` has a link and no period, so no instant separates the two
//! versions and an as-of read returns them together. Nothing but the store is needed to count that,
//! which is why it runs first: 0014 says the number is unknown, and an unknown number is cheaper to
//! measure than to argue about.
//!
//! **Accuracy** takes labelled pairs and asks the store directly, at two instants each. It needs a
//! fixture and the fixture is the hard part, for the reason 0007 gives: cases chosen by the person
//! who built the thing measure whether their model of it is self-consistent. The expected answers
//! are written down before a run, and this module refuses a case that names no expectation rather
//! than scoring it as a pass.
//!
//! # Why coverage is not enough on its own
//!
//! A store could close every interval and still answer the wrong version, if the boundaries are
//! wrong. Coverage says the bookkeeping happened; accuracy says the bookkeeping is right. Reporting
//! only the first would be the same mistake as reporting recall and calling it retrieval quality.

use serde::{Deserialize, Serialize};

use super::Ctx;
use crate::domain::errors::{DomainError, Result};
use crate::ports::memory::PairCounts;

/// One labelled pair, asked at one instant.
///
/// The question is the owner's own wording, because a measure run on phrasing nobody uses reports
/// on a store nobody queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrencyCase {
    /// What to ask, in the owner's words.
    pub question: String,
    /// The instant to ask it at.
    pub as_of: chrono::DateTime<chrono::Utc>,
    /// The id that held at `as_of`.
    pub expect_id: String,
    /// The id that must not come back, which is the other half of the pair. Naming it is what
    /// separates "answered correctly" from "answered with everything and happened to include the
    /// right row".
    pub refuse_id: String,
}

/// What one case did.
#[derive(Debug, Clone, Serialize)]
pub struct CaseOutcome {
    pub question: String,
    pub as_of: String,
    /// The expected row came back.
    pub found: bool,
    /// The row that stopped holding came back too. True here is a failure whatever `found` says.
    pub also_returned_the_other: bool,
    /// Rank of the expected row, absent when it did not come back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<usize>,
}

impl CaseOutcome {
    /// A pass is the expected row present and the retired one absent. Both present is the failure
    /// this measure exists to catch, so it can never score as a pass on the strength of `found`.
    pub fn passed(&self) -> bool {
        self.found && !self.also_returned_the_other
    }
}

/// The whole run.
#[derive(Debug, Clone, Serialize)]
pub struct CurrencyReport {
    pub coverage: PairCounts,
    /// `None` when the store holds no pairs, rather than a 1.0 that reads as perfect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_fraction: Option<f64>,
    pub cases: Vec<CaseOutcome>,
    /// Cases that passed, over cases run. `None` when no fixture was supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accuracy: Option<f64>,
    /// Cases where both versions came back. Reported apart from `accuracy` because averaging the
    /// store's central failure into a single number is how it stays invisible.
    pub returned_both: usize,
}

/// Pass rate over cases, or `None` for an empty set.
///
/// Pure, so the arithmetic has one home and a test can pin it without a database.
pub fn accuracy(outcomes: &[CaseOutcome]) -> Option<f64> {
    if outcomes.is_empty() {
        return None;
    }
    let passed = outcomes.iter().filter(|o| o.passed()).count();
    Some(passed as f64 / outcomes.len() as f64)
}

/// Count the cases that returned the fact and its replacement together.
pub fn returned_both(outcomes: &[CaseOutcome]) -> usize {
    outcomes.iter().filter(|o| o.also_returned_the_other).count()
}

/// Measure the store, and the fixture if one was given.
///
/// Coverage runs on every call. Accuracy runs only over the cases supplied, and an empty slice is a
/// legitimate run reporting coverage alone rather than an error: the number 0014 wants first does
/// not need a fixture and should not be gated behind writing one.
pub async fn run(ctx: &Ctx, cases: &[CurrencyCase]) -> Result<CurrencyReport> {
    // The grant itself, not a resolved namespace list. A list comes from the namespaces holding
    // live rows, and a namespace whose memories were all superseded into successors elsewhere would
    // be missing from it, so its pairs would count toward neither number. The measure exists to
    // count exactly those rows.
    let coverage = ctx.repos.memories.pair_counts(ctx.tenant(), &ctx.principal.read).await?;

    let mut outcomes = Vec::with_capacity(cases.len());
    for case in cases {
        if case.expect_id.trim().is_empty() || case.refuse_id.trim().is_empty() {
            return Err(DomainError::validation(format!(
                "case {:?} names no expectation. A case with nothing to compare against scores as a \
                 pass and measures nothing",
                case.question
            )));
        }
        // The same call a model makes, so the number describes the surface rather than a shortcut
        // around it. `include_superseded` stays off: as-of already reads retired rows and the two
        // together are refused.
        let result =
            super::search::run(ctx, &case.question, None, Some(20), None, None, Some(case.as_of))
                .await?;
        let ids: Vec<&str> = result.hits.iter().map(|h| h.id.as_str()).collect();
        outcomes.push(CaseOutcome {
            question: case.question.clone(),
            as_of: case.as_of.to_rfc3339(),
            found: ids.contains(&case.expect_id.as_str()),
            also_returned_the_other: ids.contains(&case.refuse_id.as_str()),
            rank: ids.iter().position(|id| *id == case.expect_id).map(|i| i + 1),
        });
    }

    Ok(CurrencyReport {
        closed_fraction: coverage.closed_fraction(),
        coverage,
        accuracy: accuracy(&outcomes),
        returned_both: returned_both(&outcomes),
        cases: outcomes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(found: bool, both: bool) -> CaseOutcome {
        CaseOutcome {
            question: "q".into(),
            as_of: "2026-03-01T00:00:00Z".into(),
            found,
            also_returned_the_other: both,
            rank: found.then_some(1),
        }
    }

    #[test]
    fn returning_both_versions_is_a_failure_even_when_the_right_one_is_first() {
        let both = outcome(true, true);
        assert!(!both.passed(), "the store answered with the fact and its replacement");
        assert_eq!(accuracy(&[both]), Some(0.0));
    }

    #[test]
    fn a_pass_needs_the_right_row_and_the_absence_of_the_other() {
        assert!(outcome(true, false).passed());
        assert!(!outcome(false, false).passed(), "silence is not a correct answer");
        assert_eq!(accuracy(&[outcome(true, false), outcome(false, false)]), Some(0.5));
    }

    #[test]
    fn an_empty_fixture_scores_nothing_rather_than_scoring_perfect() {
        assert_eq!(accuracy(&[]), None);
        assert_eq!(returned_both(&[]), 0);
    }

    #[test]
    fn double_answers_are_counted_apart_from_the_pass_rate() {
        let set = [outcome(true, true), outcome(true, false), outcome(true, true)];
        assert_eq!(returned_both(&set), 2);
        // One pass in three, and the two failures are both the double answer rather than a miss.
        assert!((accuracy(&set).unwrap() - 1.0 / 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn an_empty_store_reports_no_fraction_rather_than_a_perfect_one() {
        assert_eq!(PairCounts::default().closed_fraction(), None);
        let some = PairCounts { pairs: 4, closed: 1, dated_but_open: 2, both_dated: 3 };
        assert_eq!(some.closed_fraction(), Some(0.25));
    }
}
