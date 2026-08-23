//! The prompt the cleanup pass sends, and the shape it asks back.
//!
//! One job per call and a small one: given pairs the deterministic pass could not decide, say for
//! each whether the two rows are the same fact, whether they contradict, or whether a cosine simply
//! put two unrelated sentences near each other.
//!
//! Measured on 21 August 2026 across four tiers on this exact task: `qwen3.7-flash` scored 4 of 5
//! clusters exactly, which beat every other tier including Opus, at $0.00019 and 6.9 seconds.
//! Haiku found zero contradictions in two runs, which is why the cheap tier here is a named model
//! rather than "whatever is cheapest".
//!
//! # What the model is never told
//!
//! Which rows are the owner's own words, when anything was written, and what namespace anything
//! sits in beyond the one it is asked about. None of that helps decide whether two sentences say
//! the same thing, and all of it is more of the store leaving the machine than the task needs.

/// Frozen with the prompt. A rationale that reads as a category rather than a sentence is a
/// rationale nobody can act on, so the prompt asks for one line in plain words.
pub const SYSTEM: &str = "\
You compare pairs of remembered facts and say how they relate. You answer with JSON and nothing \
else. You are conservative: when two statements could both be true at once, they are not a \
contradiction, and when they differ in any detail that could matter, they are not the same fact.";

pub const BODY: &str = r#"
For each pair below, decide which one of these it is.

  same          The two say the same thing. Different words, one fact. Either could be deleted
                without losing anything.
  contradiction The two cannot both be true. A port that is 8080 and 8787, a name that is Warden
                and Lumen, a number that changed.
  unrelated     A similarity score put them together and they are about different things, or they
                are about the same subject and both hold at once.

Rules that decide the hard cases:

- Two statements about different subjects are unrelated, however alike the words are.
- A general statement and a specific one are unrelated. "The tests run in Docker" and "The
  integration tests need Postgres" are both true.
- Numbers, versions, ports, dates and names that differ are a contradiction, never `same`. This
  matters more than any other rule here: collapsing a correction into the thing it corrects
  destroys the correction.
- A statement and its negation are a contradiction.
- When you are not sure, answer `unrelated`. A missed duplicate costs a person one line of
  clutter; a wrong merge costs them a fact.

For `same`, name which id survives in `keep`. Prefer the one that is more specific, then the one
that reads better. For `contradiction` and `unrelated`, leave `keep` out: which of two conflicting
facts holds is not yours to decide.

`why` is one plain sentence a person can act on without re-reading the pair. Not a category, not a
restatement of the rule.

Answer with this object and nothing else. No prose before it, no prose after it.

{"clusters": [
  {"pair": "<the pair id given below>", "verdict": "same|contradiction|unrelated",
   "keep": "<id, only when verdict is same>", "why": "<one sentence>"}
]}

Every pair below gets exactly one entry. If you have nothing to say about a pair, it is
`unrelated`.
"#;

/// The pairs, rendered.
///
/// A short opaque pair id rather than the memory uuids in the verdict, so a model that hallucinates
/// an id produces a pair the caller cannot find rather than a verdict about the wrong rows. The
/// uuids still appear as `a` and `b` because `keep` has to name one.
pub fn render(pairs: &[super::Pair]) -> String {
    let mut out = String::with_capacity(pairs.len() * 320);
    out.push_str(BODY);
    out.push_str("\nThe pairs:\n\n");
    for (i, p) in pairs.iter().enumerate() {
        out.push_str(&format!(
            "pair {}\n  a  {}\n     {}\n  b  {}\n     {}\n  cosine {:.3}\n\n",
            i + 1,
            p.a_id,
            p.a_content,
            p.b_id,
            p.b_content,
            p.similarity
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(a: &str, b: &str) -> crate::cleanup::Pair {
        crate::cleanup::Pair {
            similarity: 0.91,
            namespace: "user:me".into(),
            a_id: "11111111-1111-4111-8111-111111111111".into(),
            a_content: a.into(),
            b_id: "22222222-2222-4222-8222-222222222222".into(),
            b_content: b.into(),
        }
    }

    #[test]
    fn every_pair_is_numbered_from_one() {
        let text = render(&[pair("x", "y"), pair("p", "q")]);
        assert!(text.contains("pair 1"));
        assert!(text.contains("pair 2"));
        assert!(!text.contains("pair 0"), "a zero-based list and a one-based prompt disagree");
    }

    #[test]
    fn both_ids_reach_the_model_because_keep_has_to_name_one() {
        let text = render(&[pair("x", "y")]);
        assert!(text.contains("11111111-1111-4111-8111-111111111111"));
        assert!(text.contains("22222222-2222-4222-8222-222222222222"));
    }

    #[test]
    fn the_numeric_rule_is_in_the_prompt() {
        // The rule that stops a correction being collapsed into the thing it corrects. It has been
        // the expensive mistake in every version of this task.
        assert!(BODY.contains("never `same`"));
        assert!(BODY.contains("destroys the correction"));
    }

    #[test]
    fn the_prompt_tells_the_model_what_to_do_when_it_is_unsure() {
        assert!(BODY.contains("answer `unrelated`"));
    }

    #[test]
    fn the_rendered_prompt_carries_no_namespace_or_date() {
        // Nothing beyond the two sentences and their score leaves the machine. A namespace is a
        // fact about the owner's projects and it does not help decide whether two sentences agree.
        let text = render(&[pair("x", "y")]);
        assert!(!text.contains("user:me"));
    }
}
