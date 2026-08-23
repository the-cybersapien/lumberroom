//! The valid-time argument on `memory_write`: its wording, and the parser that enforces it.
//!
//! The wording is the control. Nothing in the write path calls a model to extract a date, so the
//! only thing standing between a useful `occurred_at` column and a column full of today's date is
//! the sentence a model reads before it fills the argument in. Decision 0008 refuses date
//! extraction; what this argument carries is the user's own statement, relayed. The line runs
//! between transcribing a date the user said and inventing one from content.
//!
//! `OCCURRED_AT_DESCRIPTION` is the source of that sentence. The doc comment on
//! `WriteArgs::occurred_at` has to match it word for word, and a test compares the generated JSON
//! schema against this constant so an edit to either one fails rather than drifting.
//!
//! **`memory_search` gains no date argument, and this is where somebody will come looking for the
//! reason.** A range filter belongs to the as-of query, which decision 0008 defers to phase 2. The
//! shape it would take here is worse than deferring it: a model turning a question into a date
//! range is guessing, a hard filter drops the right row when the guess is wrong, and the store
//! then answers "nothing is known" about a fact it holds. That failure is the one this system
//! refuses everywhere else. Search hits already carry `occurred_at` when it is set, so a model
//! sees when a fact became true without any argument that lets it filter on a guess.

use chrono::{DateTime, NaiveDate, Utc};

use crate::domain::errors::{DomainError, Result};

/// The `occurred_at` argument description, read by every model that calls `memory_write`.
///
/// Section 7 of the phase 7 spec drafted this with "since March" as its example, and a blind
/// reviewer found the draft unfollowable: "since March" has no RFC 3339 form, so a model obeying
/// the example had to invent a day, an hour and an offset that the same sentence forbade. The date
/// form and the sentence about bare months close that gap. Keep both.
pub const OCCURRED_AT_DESCRIPTION: &str = "When this fact became true in the world. Two forms are \
accepted: a date, `2026-03-01`, read as midnight UTC, or a full RFC 3339 instant, \
`2026-03-01T09:30:00Z`. A bare month or year has no form here, so \"since March\" is omitted \
rather than turned into a day you chose. Set it only when the user stated the time, as in \"we \
moved to Postgres 16 on 4 June 2026\". Never infer a date from context, and never pass today's \
date because today is when you heard it: the store already records that separately.";

/// The date form, accepted beside RFC 3339 and read as midnight UTC.
const DATE_ONLY: &str = "%Y-%m-%d";

/// Turn the argument into an instant, or refuse.
///
/// Refusing is the point. A silent `None` on a malformed date leaves a model believing it recorded
/// something the store never saw, and the belief outlives the call: the model reports the date back
/// to the user, and nothing anywhere disagrees. An empty string is malformed too, for the same
/// reason. A model with no date to give omits the argument.
pub fn parse_occurred_at(raw: &str) -> Result<DateTime<Utc>> {
    let value = raw.trim();

    if let Ok(instant) = DateTime::parse_from_rfc3339(value) {
        return Ok(instant.with_timezone(&Utc));
    }

    // chrono refuses trailing input, so `2026-03-01T09:00` cannot reach this branch and land as
    // midnight with the time silently dropped.
    if let Ok(date) = NaiveDate::parse_from_str(value, DATE_ONLY) {
        return Ok(date
            .and_hms_opt(0, 0, 0)
            .expect("midnight exists on every calendar date")
            .and_utc());
    }

    Err(refusal(value))
}

/// One message for every rejected form, and it never suggests a repair.
///
/// Telling a model to pick the first of the month would put defect 1 back in the error path: it
/// would get a date, and the date would be a guess wearing the shape of a fact. Omission is the
/// only instruction here.
fn refusal(value: &str) -> DomainError {
    DomainError::validation(format!(
        "occurred_at `{}` is not one of the two accepted forms. Pass a date, `2026-03-01`, read as \
midnight UTC, or a full RFC 3339 instant, `2026-03-01T09:30:00Z`. A bare month or year cannot be \
represented, so omit occurred_at rather than choosing a day.",
        clip(value)
    ))
}

/// Keep a stray paragraph out of the refusal and out of the log line carrying it.
fn clip(value: &str) -> String {
    const LIMIT: usize = 64;
    match value.char_indices().nth(LIMIT) {
        Some((cut, _)) => format!("{}...", &value[..cut]),
        None => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Phrases a model has to see. Each one is a prohibition that a rewrite for brevity would drop
    /// first, so the test names them rather than checking the length of the string.
    const REQUIRED: [&str; 4] = [
        "never pass today's date",
        "Never infer a date from context",
        "the store already records that separately",
        "A bare month or year has no form here",
    ];

    fn squash(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn rfc3339_instant_converts_to_utc() {
        let parsed = parse_occurred_at("2026-03-01T12:30:00+05:30").expect("accepted");
        assert_eq!(parsed.to_rfc3339(), "2026-03-01T07:00:00+00:00");
    }

    #[test]
    fn rfc3339_instant_in_utc_survives_unchanged() {
        let parsed = parse_occurred_at("2026-03-01T09:30:00Z").expect("accepted");
        assert_eq!(parsed.to_rfc3339(), "2026-03-01T09:30:00+00:00");
    }

    #[test]
    fn date_only_reads_as_midnight_utc() {
        let parsed = parse_occurred_at("2026-03-01").expect("accepted");
        assert_eq!(parsed.to_rfc3339(), "2026-03-01T00:00:00+00:00");
    }

    #[test]
    fn surrounding_whitespace_does_not_refuse_a_good_date() {
        let parsed = parse_occurred_at("  2026-03-01  ").expect("accepted");
        assert_eq!(parsed.to_rfc3339(), "2026-03-01T00:00:00+00:00");
    }

    #[test]
    fn bare_month_is_refused() {
        let err = parse_occurred_at("2026-03").expect_err("a month is not a date");
        assert!(err.client_message().contains("bare month or year"));
    }

    #[test]
    fn bare_year_is_refused() {
        parse_occurred_at("2026").expect_err("a year is not a date");
    }

    #[test]
    fn month_name_is_refused() {
        parse_occurred_at("March").expect_err("a month name is not a date");
    }

    #[test]
    fn empty_is_refused_rather_than_read_as_omitted() {
        parse_occurred_at("   ").expect_err("an empty argument is a date the model thinks it sent");
    }

    #[test]
    fn refusal_names_both_accepted_forms() {
        let err = parse_occurred_at("last Tuesday").expect_err("prose is not a date");
        let message = err.client_message();
        assert!(message.contains("2026-03-01"), "date form missing from: {message}");
        assert!(message.contains("RFC 3339"), "instant form missing from: {message}");
        assert!(message.contains("omit occurred_at"), "no omission instruction in: {message}");
    }

    /// The examples in the refusal are format, and a model must not read them as a repair. Nothing
    /// in the message may offer a day to fill in, which is how defect 1 would come back through the
    /// error path.
    #[test]
    fn refusal_offers_omission_and_no_repair() {
        let message = parse_occurred_at("2026-03").unwrap_err().client_message().to_string();
        assert!(message.contains("omit occurred_at"), "no omission instruction in: {message}");
        for repair in ["first of", "first day", "default to", "assume"] {
            assert!(!message.contains(repair), "refusal offers `{repair}`: {message}");
        }
    }

    #[test]
    fn a_long_argument_does_not_land_whole_in_the_refusal() {
        let message = parse_occurred_at(&"x".repeat(400)).unwrap_err().client_message().to_string();
        assert!(message.len() < 400, "refusal echoes the argument: {} chars", message.len());
    }

    /// The fence itself. An edit that softens the wording fails here rather than passing review.
    #[test]
    fn description_keeps_every_prohibition() {
        for phrase in REQUIRED {
            assert!(
                OCCURRED_AT_DESCRIPTION.contains(phrase),
                "the occurred_at description lost `{phrase}`"
            );
        }
    }

    /// What a model reads is the generated schema, so assert against the schema rather than the
    /// constant alone. Equality in both directions: a doc comment edited in `mod.rs` and a constant
    /// edited here both fail.
    #[test]
    fn write_schema_carries_the_fence_verbatim() {
        let schema = serde_json::to_value(schemars::schema_for!(crate::mcp::WriteArgs))
            .expect("the derived schema serialises");
        let property = schema
            .get("properties")
            .and_then(|p| p.get("occurred_at"))
            .unwrap_or_else(|| panic!("memory_write has no occurred_at argument yet: {schema}"));
        let described = property
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or_else(|| panic!("occurred_at carries no description: {property}"));

        assert_eq!(
            squash(described),
            squash(OCCURRED_AT_DESCRIPTION),
            "the doc comment on WriteArgs::occurred_at has drifted from OCCURRED_AT_DESCRIPTION"
        );
        for phrase in REQUIRED {
            assert!(squash(described).contains(phrase), "the schema lost `{phrase}`");
        }
    }
}
