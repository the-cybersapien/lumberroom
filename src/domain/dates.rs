//! Dates a fact states about itself, read out of its own text.
//!
//! One rule, two callers. The near-now fence asks whether a row's content names the day a caller
//! wants to stamp on it; the date review asks which days a row names at all. Both have to agree
//! about what counts as "the text says so", because the fence is what admits a write and the review
//! is what proposes one. Two implementations would drift and the drift would show up as a proposal
//! the fence then refuses.
//!
//! **Day precision only, and no inference.** A sentence writes out a day or it does not. "Last
//! Tuesday", "since March" and "Q3" name no day, and turning any of them into one is the guess that
//! 0008 refuses and that this module exists to avoid. Four renderings are recognised because those
//! are the ones people write.
//!
//! No regex crate, so this scans tokens by hand. The grammar is small enough that a dependency
//! would cost more than it saves.

use chrono::NaiveDate;

/// The four renderings, in the order a reader would guess them.
///
/// `2026-03-04`, `4 March 2026`, `March 4, 2026`, `4 Mar 2026`.
const MONTHS: [(&str, u32); 12] = [
    ("january", 1),
    ("february", 2),
    ("march", 3),
    ("april", 4),
    ("may", 5),
    ("june", 6),
    ("july", 7),
    ("august", 8),
    ("september", 9),
    ("october", 10),
    ("november", 11),
    ("december", 12),
];

fn month_from(word: &str) -> Option<u32> {
    let w = word.trim_matches(|c: char| !c.is_ascii_alphabetic()).to_ascii_lowercase();
    if w.len() < 3 {
        return None;
    }
    MONTHS.iter().find_map(|(name, n)| {
        // Full name, or the three-letter form. `may` is both, which costs nothing.
        (w == *name || (w.len() == 3 && name.starts_with(&w))).then_some(*n)
    })
}

fn digits(word: &str) -> Option<u32> {
    let w = word.trim_matches(|c: char| !c.is_ascii_digit());
    if w.is_empty() || !w.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    w.parse().ok()
}

/// Every day this text writes out, in the order they appear, deduplicated.
///
/// Returns days, not instants. A caller that needs an instant reads it as midnight UTC, the same
/// reading `memory_write` gives a bare date, so the two paths cannot disagree about what a date
/// means.
pub fn extract(content: &str) -> Vec<NaiveDate> {
    let tokens: Vec<&str> = content.split_whitespace().collect();
    let mut found: Vec<NaiveDate> = Vec::new();
    let mut push = |d: NaiveDate| {
        if !found.contains(&d) {
            found.push(d);
        }
    };

    for (i, raw) in tokens.iter().enumerate() {
        // `2026-03-04`, possibly wearing a comma or a full stop.
        let bare = raw.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
        if bare.len() == 10 && bare.as_bytes().get(4) == Some(&b'-') {
            if let Ok(d) = NaiveDate::parse_from_str(bare, "%Y-%m-%d") {
                push(d);
                continue;
            }
        }

        // `4 March 2026` and `4 Mar 2026`.
        if let (Some(day), Some(m), Some(year)) = (
            digits(raw),
            tokens.get(i + 1).and_then(|t| month_from(t)),
            tokens.get(i + 2).and_then(|t| digits(t)),
        ) {
            if (1..=31).contains(&day) && (1000..=9999).contains(&year) {
                if let Some(d) = NaiveDate::from_ymd_opt(year as i32, m, day) {
                    push(d);
                    continue;
                }
            }
        }

        // `March 4, 2026`.
        if let (Some(m), Some(day), Some(year)) = (
            month_from(raw),
            tokens.get(i + 1).and_then(|t| digits(t)),
            tokens.get(i + 2).and_then(|t| digits(t)),
        ) {
            if (1..=31).contains(&day) && (1000..=9999).contains(&year) {
                if let Some(d) = NaiveDate::from_ymd_opt(year as i32, m, day) {
                    push(d);
                }
            }
        }
    }
    found
}

/// Does this text write out this exact day?
///
/// The fence's question. Answered through [`extract`] so the two can never disagree about a
/// rendering: anything the review can propose, the fence can admit, and nothing else.
pub fn states(content: &str, day: NaiveDate) -> bool {
    extract(content).contains(&day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn all_four_renderings_read_as_the_same_day() {
        for text in [
            "cleared on 2026-03-04 by the regulator",
            "cleared on 4 March 2026 by the regulator",
            "cleared on March 4, 2026 by the regulator",
            "cleared on 4 Mar 2026 by the regulator",
        ] {
            assert_eq!(extract(text), vec![d(2026, 3, 4)], "{text}");
            assert!(states(text, d(2026, 3, 4)), "{text}");
        }
    }

    #[test]
    fn a_month_or_a_year_alone_names_no_day() {
        for text in ["since March 2026", "in 2026", "last Tuesday", "by Q3", "on the 4th"] {
            assert!(extract(text).is_empty(), "{text} should name no day");
        }
    }

    #[test]
    fn two_dates_in_one_sentence_both_come_back_and_repeats_do_not() {
        let text = "approved on 4 March 2026 after the panel met on 2026-01-09, again 4 March 2026";
        assert_eq!(extract(text), vec![d(2026, 3, 4), d(2026, 1, 9)]);
    }

    /// A day the calendar does not have is not a day. Left to `from_ymd_opt` rather than to a range
    /// check, so February keeps its own length.
    #[test]
    fn an_impossible_day_is_not_a_date() {
        assert!(extract("filed on 30 February 2026").is_empty());
        assert!(extract("filed on 2026-02-30").is_empty());
    }

    /// Punctuation rides along in prose and must not hide the day.
    #[test]
    fn trailing_punctuation_does_not_hide_a_date() {
        assert_eq!(extract("it landed (4 March 2026)."), vec![d(2026, 3, 4)]);
        assert_eq!(extract("it landed 2026-03-04, late."), vec![d(2026, 3, 4)]);
    }
}
