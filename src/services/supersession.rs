//! Finding the pair a supersession needs, and refusing to guess when the store cannot know.
//!
//! Decision 0014 part 3. The store can already record that one fact ended another: `memory_write`
//! takes `supersedes`, the console offers Replace this fact, and 0008 fixes what the interval
//! becomes. What no code does is **produce the pair**, so a superseded fact reaches the junk pass
//! with nothing beside it and that pass asks the only question it has.
//!
//! # Cardinality is declared, never inferred
//!
//! A later fact ends an earlier one only when the subject holds one value at a time, and the
//! sentence never says whether it does. "The limit is 40k now" replaces its predecessor. "Applying
//! for b and c" replaces nothing. Same shape, opposite answers, and the difference is not in the
//! text, so no model reading the text can recover it.
//!
//! The owner declares it per tag. An undeclared tag produces nothing, which makes silence the
//! default and a wrong proposal something the owner opted into. A tag declared `many` is a
//! declaration too: it stops the pass re-offering a subject already settled.
//!
//! # A declaration shows its blast radius first
//!
//! The dangerous case is not the undeclared tag, it is the one declared wrong. That is discovered
//! one hidden fact at a time, because a supersession removes a row from every future answer without
//! deleting anything. So [`preview`] answers "this would end N existing facts, here they are"
//! before anything is written, and it costs one query.
//!
//! # What this proposes, and what it refuses
//!
//! Only consecutive dated rows sharing a `single` tag, oldest first by **valid time**. Never by
//! `created_at`: a July fact approved in August is older than an August fact approved in July, and
//! the transaction clock says the opposite.
//!
//! Two rows dated the same day are refused rather than proposed. Closing that period would write an
//! empty interval, which reads as "never true", and a caller who asked to replace a fact did not ask
//! to erase it. That case is common in imported dumps, where every line in a day carries that day.

use serde::Serialize;

use super::Ctx;
use crate::domain::cleanup::{CleanupKind, Disposition};
use crate::domain::errors::{DomainError, Result};
use crate::domain::policy::NamespaceGrant;
use crate::ports::cleanup::{
    Arity, Candidate, CandidateQuery, Cardinality, CleanupRepository, NewMember, NewProposal,
};

/// How many rows one query pulls for a tag. A subject with more dated facts than this is a subject
/// the owner should look at before declaring anything about it.
const TAG_SCAN_LIMIT: i64 = 500;

/// One pair the pass would propose.
#[derive(Debug, Clone, Serialize)]
pub struct Pair {
    pub earlier_id: String,
    pub earlier_content: String,
    pub earlier_occurred_at: String,
    pub later_id: String,
    pub later_content: String,
    pub later_occurred_at: String,
}

/// What declaring a tag `single` would do to the store as it stands.
#[derive(Debug, Clone, Serialize)]
pub struct Preview {
    pub tag: String,
    /// Live dated rows carrying the tag.
    pub dated_rows: usize,
    /// Pairs that would be proposed. Each one ends a fact that is live today.
    pub would_end: Vec<Pair>,
    /// Rows sharing a day with their neighbour, which cannot be ordered and are skipped. Reported
    /// because a subject that is mostly same-day rows will produce far fewer proposals than the
    /// row count suggests, and the owner should know that before declaring.
    pub same_day_skipped: usize,
    /// The scan window filled, so this shows the newest [`TAG_SCAN_LIMIT`] facts and not the whole
    /// subject. Said out loud: a flat count with no flag reads as "this is everything", and a
    /// preview that quietly means "some of it" is worse than no preview.
    pub truncated: bool,
}

/// The pairs a `single` tag implies, oldest first.
///
/// Pure over the rows, so the rule has one home and a test can pin it without a database. The input
/// is already ordered by valid time; this walks consecutive rows and drops any pair that shares a
/// day.
pub fn pairs_from(rows: &[Candidate]) -> (Vec<Pair>, usize) {
    let mut pairs = Vec::new();
    let mut same_day = 0usize;
    for window in rows.windows(2) {
        let (a, b) = (&window[0], &window[1]);
        let (Some(at), Some(bt)) = (a.occurred_at, b.occurred_at) else { continue };
        if bt <= at {
            // Equal dates, or an ordering the query should have prevented. Either way there is no
            // interval to write, so nothing is proposed.
            same_day += 1;
            continue;
        }
        pairs.push(Pair {
            earlier_id: a.id.clone(),
            earlier_content: a.content.clone(),
            earlier_occurred_at: at.to_rfc3339(),
            later_id: b.id.clone(),
            later_content: b.content.clone(),
            later_occurred_at: bt.to_rfc3339(),
        });
    }
    (pairs, same_day)
}

fn scan_query(ctx: &Ctx) -> CandidateQuery {
    CandidateQuery {
        namespace: None,
        // The judge and the queue only ever see rows at open. 0011 set that rule for the cleanup
        // pass and 0014 restates it for this one: deciding whether B ends A means sending A
        // somewhere, and the caller's grant is not the right ceiling for that.
        max_sensitivity: crate::domain::types::Sensitivity::Open,
        grant: ctx.principal.read.clone(),
        // No window. A supersession pair can be years apart, and `since` exists to bound work for
        // passes that compare a new row against its neighbours. The bound here is the tag.
        since: None,
        limit: TAG_SCAN_LIMIT,
    }
}

/// The rows a tag's window holds, oldest first, and whether the window filled.
///
/// The query returns newest first so the limit bites at the old end, because the newest fact on a
/// single-valued subject is the one that holds today and dropping it would leave the subject's real
/// current value invisible to the pass. Pairing needs oldest first, so this reverses.
async fn scan(
    ctx: &Ctx,
    repo: &dyn CleanupRepository,
    tag: &str,
) -> Result<(Vec<Candidate>, bool)> {
    let mut rows = repo.tagged_dated(ctx.tenant(), &scan_query(ctx), tag, TAG_SCAN_LIMIT).await?;
    let truncated = rows.len() as i64 >= TAG_SCAN_LIMIT;
    rows.reverse();
    Ok((rows, truncated))
}

/// What declaring `tag` as `single` would end, without writing anything.
pub async fn preview(ctx: &Ctx, repo: &dyn CleanupRepository, tag: &str) -> Result<Preview> {
    let tag = tag.trim();
    if tag.is_empty() {
        return Err(DomainError::validation("a cardinality declaration needs a tag"));
    }
    let (rows, truncated) = scan(ctx, repo, tag).await?;
    let (would_end, same_day_skipped) = pairs_from(&rows);
    Ok(Preview {
        tag: tag.to_string(),
        dated_rows: rows.len(),
        would_end,
        same_day_skipped,
        truncated,
    })
}

/// Record a declaration.
///
/// The preview is not enforced here, because refusing to declare until the owner has looked would
/// make the console the only way in and leave the CLI unable to do the thing the console does. The
/// surfaces show the preview; this records the decision.
pub async fn declare(
    ctx: &Ctx,
    repo: &dyn CleanupRepository,
    tag: &str,
    arity: Arity,
    note: Option<&str>,
) -> Result<()> {
    let tag = tag.trim();
    if tag.is_empty() {
        return Err(DomainError::validation("a cardinality declaration needs a tag"));
    }
    repo.declare_arity(ctx.tenant(), tag, arity, note).await
}

/// Every declaration.
pub async fn declarations(ctx: &Ctx, repo: &dyn CleanupRepository) -> Result<Vec<Cardinality>> {
    repo.arities(ctx.tenant()).await
}

/// Drop a declaration.
pub async fn forget(ctx: &Ctx, repo: &dyn CleanupRepository, tag: &str) -> Result<bool> {
    repo.forget_arity(ctx.tenant(), tag.trim()).await
}

/// What one pass did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PassReport {
    /// Tags declared `single`. Only these are looked at.
    pub tags_scanned: usize,
    pub pairs_found: usize,
    pub queued: usize,
    pub already_known: usize,
    pub same_day_skipped: usize,
    /// Subjects whose scan window filled, so the pass saw the newest facts and not all of them.
    pub truncated_tags: usize,
}

/// Propose a supersession for every consecutive dated pair on a `single` tag.
///
/// Proposes and never applies, which is 0011's rule and holds harder here: a wrong supersession
/// hides a live fact, and the only undo runs off the delete plan, so reversing one costs the
/// successor row and every link that named it.
///
/// **Chained pairs are order-dependent on apply, and the rationale says so.** Three dated facts on
/// one subject queue two proposals, `(a,b)` and `(b,c)`. `cleanup::apply` refuses any proposal whose
/// member is already retired, so approving `(b,c)` first retires `b` and leaves `(a,b)` permanently
/// unappliable with `a` live and unended. Only oldest-first works. Enforcing that here would mean
/// this pass reaching into the queue's ordering, which is the console's job; saying it in the text
/// the owner reads at the moment of deciding is the honest version.
///
/// Nothing is sent to a model. The pass has dates and a declaration, which is everything the
/// decision needs, and 0008 refuses a model supplying the date. A judgement pass over these pairs
/// would be asking a model to confirm arithmetic the owner already declared.
pub async fn run(ctx: &Ctx, repo: &dyn CleanupRepository) -> Result<PassReport> {
    let mut report = PassReport::default();
    let declared = repo.arities(ctx.tenant()).await?;

    for c in declared.iter().filter(|c| c.arity == Arity::Single) {
        report.tags_scanned += 1;
        let (rows, truncated) = scan(ctx, repo, &c.tag).await?;
        if truncated {
            report.truncated_tags += 1;
        }
        let (pairs, same_day) = pairs_from(&rows);
        report.same_day_skipped += same_day;
        report.pairs_found += pairs.len();

        for pair in pairs {
            let earlier = rows.iter().find(|r| r.id == pair.earlier_id);
            let later = rows.iter().find(|r| r.id == pair.later_id);
            let (Some(earlier), Some(later)) = (earlier, later) else { continue };

            let proposal = NewProposal {
                kind: CleanupKind::Supersession,
                namespace: later.namespace.clone(),
                keep_id: Some(later.id.clone()),
                rationale: format!(
                    "`{}` is declared to hold one value at a time. This fact held from {}, and the \
                     one that replaced it starts {}. Approving writes that end date; it does not \
                     delete anything. Where a subject has several of these, approve them oldest \
                     first: applying a later pair retires a row an earlier pair still names, and \
                     the earlier one can then never be applied.",
                    c.tag, pair.earlier_occurred_at, pair.later_occurred_at
                ),
                produced_by: "cardinality".to_string(),
                // No cosine grouped this. The declaration and the two dates did, and publishing a
                // similarity here would invite reading it as the confidence.
                similarity: None,
                posted_by: None,
                members: vec![
                    NewMember {
                        memory_id: earlier.id.clone(),
                        disposition: Disposition::Retire,
                        seen_content: earlier.content.clone(),
                    },
                    NewMember {
                        memory_id: later.id.clone(),
                        disposition: Disposition::Keep,
                        seen_content: later.content.clone(),
                    },
                ],
            };
            match repo.queue(ctx.tenant(), proposal).await? {
                (crate::ports::cleanup::QueueOutcome::Queued, _) => report.queued += 1,
                (crate::ports::cleanup::QueueOutcome::AlreadyKnown, _) => report.already_known += 1,
            }
        }
    }
    Ok(report)
}

/// The grant the scheduled pass runs under, named rather than spelled inline at the call site.
pub fn whole_tenant() -> Vec<NamespaceGrant> {
    NamespaceGrant::everything()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::types::Sensitivity;
    use chrono::{DateTime, TimeZone, Utc};

    fn row(id: &str, day: Option<(i32, u32, u32)>) -> Candidate {
        let occurred_at: Option<DateTime<Utc>> =
            day.map(|(y, m, d)| Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap());
        Candidate {
            id: id.to_string(),
            namespace: "user:me".to_string(),
            sensitivity: Sensitivity::Open,
            content: format!("fact {id}"),
            created_at: Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap(),
            access_count: 0,
            occurred_at,
        }
    }

    #[test]
    fn consecutive_dated_rows_pair_up_oldest_first() {
        let rows = [
            row("a", Some((2026, 1, 5))),
            row("b", Some((2026, 3, 9))),
            row("c", Some((2026, 7, 2))),
        ];
        let (pairs, skipped) = pairs_from(&rows);
        assert_eq!(skipped, 0);
        assert_eq!(pairs.len(), 2, "three facts in a row make two endings");
        assert_eq!((pairs[0].earlier_id.as_str(), pairs[0].later_id.as_str()), ("a", "b"));
        assert_eq!((pairs[1].earlier_id.as_str(), pairs[1].later_id.as_str()), ("b", "c"));
    }

    /// The dump case. Every line in a day carries that day, and closing `[T, T)` would read as
    /// "never true" for a fact the owner asked to replace rather than erase.
    #[test]
    fn two_facts_on_one_day_are_skipped_rather_than_proposed() {
        let rows = [row("a", Some((2026, 8, 17))), row("b", Some((2026, 8, 17)))];
        let (pairs, skipped) = pairs_from(&rows);
        assert!(pairs.is_empty(), "an empty period is not an ending");
        assert_eq!(skipped, 1);
    }

    #[test]
    fn an_undated_row_ends_nothing_and_is_not_an_error() {
        let rows = [row("a", Some((2026, 1, 5))), row("b", None), row("c", Some((2026, 7, 2)))];
        let (pairs, _) = pairs_from(&rows);
        assert!(pairs.is_empty(), "a row with no start cannot be ordered against another");
    }

    #[test]
    fn one_fact_alone_ends_nothing() {
        assert!(pairs_from(&[row("a", Some((2026, 1, 5)))]).0.is_empty());
        assert!(pairs_from(&[]).0.is_empty());
    }
}
