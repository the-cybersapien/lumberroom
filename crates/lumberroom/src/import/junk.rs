//! Deciding which lines of a dump are not worth proposing.
//!
//! # Two passes, and only one of them is safe to run unattended
//!
//! **Structure** is decidable here. A line with no letters in it, a line too short to be a fact, a
//! line that echoes the prompt back: those are wrong whatever they say, and the rules below drop
//! them with no model and no network.
//!
//! **Sense** is not. A real dump filed conversational turns as memories: an aside about a price, a
//! half-sentence about a purchase, an instruction that only meant anything in the message it was
//! typed into. Every one of them is a grammatical sentence of the right length, and every rule that
//! would catch them also catches real facts. Terse facts are still facts, and this owner's dump is
//! full of short true ones sitting beside short worthless ones.
//!
//! So the deterministic pass stays narrow on purpose, and judgement is a separate opt-in pass that
//! asks a model and then asks the owner. A filter that quietly drops a third of somebody's memory
//! because it guessed at meaning is worse than a queue with some noise in it, and the queue was
//! already the place noise gets removed.

/// Why a line was dropped. Every variant is a rule the owner can read in the report and argue with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Nothing but punctuation, digits or whitespace.
    NoWords,
    /// Too short to be a fact that stands on its own in six months.
    TooShort,
    /// The model repeated the prompt's own example line back as a memory.
    PromptEcho,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoWords => "no words",
            Self::TooShort => "too short",
            Self::PromptEcho => "echoes the prompt",
        }
    }
}

/// Below this a line cannot carry a subject and a claim. Deliberately low: the shortest genuine
/// facts observed in a real dump run to about twenty characters, and this sits well under them so
/// the rule never has to make a judgement call.
const MIN_CHARS: usize = 10;

/// Fragments of `dump_prompt`'s own text. A model that restates the instruction as a memory is the
/// one semantic failure worth catching without a model, because the string is known exactly.
const PROMPT_FRAGMENTS: &[&str] = &[
    "the fact, as one sentence that stands on its own",
    "one entry per line, oldest first",
    "use [unknown] when you hold no date",
    "put the whole export inside one code block",
];

pub fn deterministic(content: &str) -> Option<Reason> {
    let t = content.trim();
    if !t.chars().any(char::is_alphabetic) {
        return Some(Reason::NoWords);
    }
    if t.chars().count() < MIN_CHARS {
        return Some(Reason::TooShort);
    }
    let lower = t.to_lowercase();
    if PROMPT_FRAGMENTS.iter().any(|f| lower.contains(f)) {
        return Some(Reason::PromptEcho);
    }
    None
}

/// What to do about lines the structural pass cannot judge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Policy {
    /// Ask a model which lines are not durable facts, show the owner, and let them decide. The
    /// default, because the alternatives are trusting a model with somebody's memory or importing
    /// everything and reviewing it one row at a time.
    #[default]
    Assess,
    /// Assess and drop without asking. For a run nobody is watching.
    DropWithoutAsking,
    /// Skip the judgement pass. Everything structural still applies.
    KeepAll,
}

impl Policy {
    /// `--drop-junk` and `--keep-all` are mutually exclusive, and saying so beats silently letting
    /// one win.
    pub fn from_flags(drop_junk: bool, keep_all: bool) -> Result<Self, &'static str> {
        match (drop_junk, keep_all) {
            (true, true) => Err("--drop-junk and --keep-all contradict each other. Pass one."),
            (true, false) => Ok(Self::DropWithoutAsking),
            (false, true) => Ok(Self::KeepAll),
            (false, false) => Ok(Self::Assess),
        }
    }

    pub fn wants_a_model(self) -> bool {
        !matches!(self, Self::KeepAll)
    }
}

/// Normalised for comparison: case folded, whitespace collapsed, trailing sentence punctuation
/// dropped. A dump repeats itself across sections, and a real one stated the same departure from a
/// company in both CAREER and DECISIONS with only the final full stop differing.
pub fn dedupe_key(content: &str) -> String {
    let folded: String = content.trim().to_lowercase();
    let collapsed: String = folded.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.trim_end_matches(['.', '!', ';', ',']).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structural_junk_is_named_by_its_rule() {
        assert_eq!(deterministic("...").unwrap(), Reason::NoWords);
        assert_eq!(deterministic("123 456").unwrap(), Reason::NoWords);
        assert_eq!(deterministic("Yes ok").unwrap(), Reason::TooShort);
        assert_eq!(
            deterministic("The fact, as one sentence that stands on its own.").unwrap(),
            Reason::PromptEcho
        );
    }

    /// The rule that matters most is the one that does nothing. A terse fact is still a fact, and a
    /// filter that reaches for meaning here would take real memories with it.
    #[test]
    fn a_short_true_fact_survives_the_structural_pass() {
        for keep in [
            "The office laptop runs Debian 13.",
            "Prefers a dark terminal.",
            "Chose SQLite over Postgres.",
            "The card limit is now higher.",
        ] {
            assert_eq!(deterministic(keep), None, "{keep} should survive");
        }
    }

    #[test]
    fn contradictory_flags_are_refused_rather_than_resolved() {
        assert!(Policy::from_flags(true, true).is_err());
        assert_eq!(Policy::from_flags(true, false).unwrap(), Policy::DropWithoutAsking);
        assert_eq!(Policy::from_flags(false, true).unwrap(), Policy::KeepAll);
        assert_eq!(Policy::from_flags(false, false).unwrap(), Policy::Assess);
        assert!(!Policy::KeepAll.wants_a_model());
        assert!(Policy::Assess.wants_a_model());
    }

    #[test]
    fn the_dedupe_key_ignores_case_spacing_and_a_trailing_stop() {
        assert_eq!(
            dedupe_key("Sam left the firm for health reasons."),
            dedupe_key("sam  left the firm   for health reasons")
        );
        assert_ne!(dedupe_key("Sam left the firm."), dedupe_key("Sam joined the firm."));
    }
}
