//! What a cleanup pass may propose, and the rules that keep it from acting.
//!
//! No I/O here, following the rest of `domain`. The three things this module owns are the kinds a
//! proposal can take, the cluster key that makes an hourly pass idempotent, and the sensitivity
//! rule that decides what a model may ever be shown.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::domain::types::Sensitivity;

/// What a proposal claims about its cluster.
///
/// Ordered by how much judgement each one takes, which is also the order of how much the pass is
/// trusted to have got it right. `Exact` needs none: two rows with the same normalised bytes are
/// the same row. `Contradiction` needs the most, and it is the one kind that never carries a
/// keep row, because deciding which of two conflicting facts holds is the owner's call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupKind {
    Exact,
    Paraphrase,
    Contradiction,
    Stale,
    /// One dated fact ended another. Stronger than `Contradiction` and resolved differently: a
    /// contradiction asks the owner to pick a survivor, a supersession writes an interval.
    Supersession,
}

impl CleanupKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Paraphrase => "paraphrase",
            Self::Contradiction => "contradiction",
            Self::Stale => "stale",
            Self::Supersession => "supersession",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "exact" => Some(Self::Exact),
            "paraphrase" => Some(Self::Paraphrase),
            "contradiction" => Some(Self::Contradiction),
            "stale" => Some(Self::Stale),
            "supersession" => Some(Self::Supersession),
            _ => None,
        }
    }

    /// Whether a proposal of this kind may name a survivor.
    ///
    /// A contradiction has no survivor by construction: the whole content of the finding is that
    /// two rows disagree, and a pass that also picked the winner would be writing the fact rather
    /// than reporting the conflict.
    pub fn has_keep(self) -> bool {
        !matches!(self, Self::Contradiction | Self::Stale)
    }

    /// Whether applying this retires rows or deletes them.
    ///
    /// Only `Stale` deletes, and only because there is nothing for its rows to supersede into.
    /// Everything else supersedes, which keeps the retired text readable through `memory history`.
    pub fn deletes(self) -> bool {
        matches!(self, Self::Stale)
    }
}

impl fmt::Display for CleanupKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What happens to one member of a cluster when the proposal is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Keep,
    Retire,
}

impl Disposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Retire => "retire",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "keep" => Some(Self::Keep),
            "retire" => Some(Self::Retire),
            _ => None,
        }
    }
}

/// The highest sensitivity a row may carry and still be shown to a model.
///
/// `Open` and nothing above it. Decision 0005 already draws this line for the lexical index, on the
/// same reasoning: a `tsvector` is the document, so indexing private content publishes it. Sending
/// a row to a provider publishes it further and to someone else. The deterministic pass runs over
/// everything, because nothing it reads leaves this machine.
pub const MODEL_VISIBLE_CEILING: Sensitivity = Sensitivity::Open;

/// Whether a row may be shown to a model.
///
/// Called at the boundary as a second check. The first one is the `sensitivity = 'open'` predicate
/// in the candidate query, which is where it has to live: a row a model may not see must never
/// enter the process that talks to the provider.
pub fn model_may_see(s: Sensitivity) -> bool {
    s <= MODEL_VISIBLE_CEILING
}

/// The idempotency key: this kind, over exactly these rows.
///
/// An hourly pass finds the same duplicate pair every hour until the owner acts on it. Without a
/// key the queue grows by one row per hour per cluster and stops being readable by lunchtime. Ids
/// are sorted so the same cluster hashes the same however the candidate query happened to order it.
pub fn cluster_key(kind: CleanupKind, member_ids: &[String]) -> String {
    let mut ids: Vec<&str> = member_ids.iter().map(String::as_str).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut h = Sha256::new();
    h.update(kind.as_str().as_bytes());
    for id in ids {
        h.update(b"\x1f");
        h.update(id.as_bytes());
    }
    hex::encode(h.finalize())
}

/// Why a proposal cannot be applied.
///
/// Separate from the general error type because every one of these means the store moved under a
/// proposal rather than that something went wrong, and the caller shows them to a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyRefusal {
    /// Already applied or already rejected.
    NotProposed(String),
    /// A member is gone.
    MemberMissing(String),
    /// A member's text is not what the pass read.
    MemberChanged(String),
    /// A member was retired by something else in the meantime.
    MemberRetired(String),
    /// The kind carries no survivor, so there is nothing to apply.
    NothingToApply(CleanupKind),
}

impl fmt::Display for ApplyRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotProposed(state) => {
                write!(f, "this proposal is {state}, and only a proposed one can be applied")
            }
            Self::MemberMissing(id) => write!(f, "memory {id} is no longer in the store"),
            Self::MemberChanged(id) => write!(
                f,
                "memory {id} has changed since the pass read it, so this proposal describes a \
                 cluster that no longer exists"
            ),
            Self::MemberRetired(id) => {
                write!(f, "memory {id} was already superseded by something else")
            }
            Self::NothingToApply(kind) => write!(
                f,
                "a {kind} proposal names no survivor. It is a finding to act on by hand, not one \
                 to apply"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cluster_key_does_not_depend_on_the_order_the_query_returned() {
        let a = cluster_key(CleanupKind::Paraphrase, &["b".into(), "a".into(), "c".into()]);
        let b = cluster_key(CleanupKind::Paraphrase, &["c".into(), "b".into(), "a".into()]);
        assert_eq!(a, b);
    }

    #[test]
    fn a_repeated_member_does_not_change_the_key() {
        let a = cluster_key(CleanupKind::Exact, &["a".into(), "b".into()]);
        let b = cluster_key(CleanupKind::Exact, &["a".into(), "b".into(), "a".into()]);
        assert_eq!(a, b);
    }

    #[test]
    fn the_kind_is_part_of_the_key() {
        // The same two rows can be a paraphrase pair and a contradiction pair, and those are two
        // findings the owner answers differently. One key for both would hide the second.
        let ids = vec!["a".to_string(), "b".to_string()];
        assert_ne!(
            cluster_key(CleanupKind::Paraphrase, &ids),
            cluster_key(CleanupKind::Contradiction, &ids)
        );
    }

    #[test]
    fn a_different_cluster_gets_a_different_key() {
        let a = cluster_key(CleanupKind::Exact, &["a".into(), "b".into()]);
        let b = cluster_key(CleanupKind::Exact, &["a".into(), "c".into()]);
        assert_ne!(a, b);
    }

    #[test]
    fn separators_stop_two_ids_running_together_into_one_key() {
        // Without the 0x1f, ["ab","c"] and ["a","bc"] hash the same, and two unrelated clusters
        // collide on the unique constraint so the second is never queued.
        assert_ne!(
            cluster_key(CleanupKind::Exact, &["ab".into(), "c".into()]),
            cluster_key(CleanupKind::Exact, &["a".into(), "bc".into()])
        );
    }

    #[test]
    fn only_a_model_visible_row_is_open() {
        assert!(model_may_see(Sensitivity::Open));
        assert!(!model_may_see(Sensitivity::Private));
        assert!(!model_may_see(Sensitivity::Sealed));
    }

    #[test]
    fn a_contradiction_names_no_survivor_and_never_deletes() {
        assert!(!CleanupKind::Contradiction.has_keep());
        assert!(!CleanupKind::Contradiction.deletes());
    }

    #[test]
    fn only_stale_deletes() {
        assert!(CleanupKind::Stale.deletes());
        assert!(!CleanupKind::Exact.deletes());
        assert!(!CleanupKind::Paraphrase.deletes());
    }

    #[test]
    fn every_kind_round_trips_through_its_wire_string() {
        for k in [
            CleanupKind::Exact,
            CleanupKind::Paraphrase,
            CleanupKind::Contradiction,
            CleanupKind::Stale,
        ] {
            assert_eq!(CleanupKind::parse(k.as_str()), Some(k));
        }
        for d in [Disposition::Keep, Disposition::Retire] {
            assert_eq!(Disposition::parse(d.as_str()), Some(d));
        }
    }
}
