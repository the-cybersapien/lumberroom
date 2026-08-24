//! The extraction prompt, baked into the binary.
//!
//! One text, two modes. Mode A substitutes the file-writing instructions and Mode B replaces them
//! with "return the JSON object and nothing else", and nothing else differs: a fact extracted by a
//! subagent and a fact extracted by a provider have to be the same fact or the two modes are two
//! products.
//!
//! **Corrections come first in the extract list on purpose.** A person states a preference once and
//! repeats it never; they correct a wrong assumption at the moment it costs them something, and that
//! span is the only place the real fact appears. The first live run bore this out from the other
//! side: the extractors had almost no owner-typed spans to work with and produced 17 confident
//! wrong facts out of 99, every one of them inferred from an assistant talking to itself.
//!
//! **This prompt is not a line of defence and two of three models proved it.** On 20 August 2026,
//! five spans went to three GLM models under these rules plus a line telling the extractor not to
//! extract a memory system's own digest. One span was lumberroom's own digest. Two models pulled lumberroom's
//! facts out of it and proposed them back as new memories. The exclusions in spec §4 are the only
//! defence there is.

/// The shared body. Everything except how the answer comes back.
pub const BODY: &str = r#"A durable fact is one that will still be true and still be worth knowing in six months. It is
about the person, their machines, their projects, their preferences or their decisions.

**Look hardest at the spans where the person corrected something.** Those carry the facts nobody
wrote down, because the person typed them only when the work in front of them had already assumed
otherwise. A correction sounds like "no, it lives in", "that's not the", "use X instead", "actually
it is", or a flat statement of a name, a path or a host dropped into the middle of something else.
Extract what the person said is true. Never extract the mistake they were correcting.

Extract:
  - a correction, and whatever the person had to supply because the work in front of them had it
    wrong: where a repository lives, which host runs a service, what an address or a name actually
    is, which of two things a term refers to
  - a stated preference: how they want work done, which tool they use, what they refuse
  - a fact about their setup: a machine, an OS, a port, a path, a service, a model route
  - a decision with its reason: what was chosen and what it lost to

Do not extract:
  - anything true only inside one session: a file being edited, a test currently failing
  - a summary of what happened, or a narration of the conversation
  - a fact about a codebase that the codebase already states
  - a restatement of something another span in this chunk already says
  - anything from a memory system's own digest or recall output, whatever else the span says
  - anything containing a password, API key, token, private key or connection string with
    credentials in it, whatever else the span says

The JSON object has this shape:

  {"facts": [
    {
      "content": "one sentence, standalone, no pronouns referring outside itself",
      "namespace": "user:me" | "project:<slug>" | "global",
      "tags": ["short", "lowercase"],
      "source_span_id": "the id of the span this came from",
      "speaker": "the speaker of that span, copied",
      "quote": "the exact substring of that span, only when speaker is owner_typed",
      "confidence": "stated" | "inferred"
    }
  ]}

Rules that decide whether a fact is usable:
  - "stated" means the person said it in their own words in an owner_typed span, and quote is
    a verbatim substring of that span. Anything else is "inferred". A wrong quote is worse
    than no quote: it will be checked against the transcript and the fact discarded.
  - content is what a person would want read back to them, not a report. Write "the Postgres
    port on the dev box is 5433", not "the user discussed Postgres configuration".
  - one fact per entry. Two facts joined by "and" are two entries.
  - Prose rules, and they are enforced: no em dashes anywhere. Active voice. No adverb where a
    plain verb works. No "Note that". No "not X, it's Y" contrasts, state Y. Never mention an
    AI, an assistant or a model as the author of anything."#;

/// What the speaker values mean, for a model that has never seen this taxonomy.
pub const SPEAKERS: &str = r#"The spans are a JSON array. Each span has: id, speaker, text, session_id, timestamp, cwd, and
for tool spans, tool_name. The speaker values mean:
  owner_typed    the person typed this
  main_model     the assistant said this
  subagent       a subagent said this
  tool_returned  a tool produced this"#;

/// Mode B. The provider returns the object; there is no file to write.
pub fn provider_system(chunk_num: usize, total: usize) -> String {
    format!(
        "You are extracting durable facts from chunk {chunk_num} of {total} of one person's agent \
         transcripts.\n\n{SPEAKERS}\n\n{BODY}\n\nReturn the JSON object and nothing else. If the \
         chunk holds no durable fact, return exactly {{\"facts\": [], \"refusal\": \
         \"<no-facts/>\"}}. That is a correct and expected answer. Most chunks are ordinary work \
         with nothing durable in them. Returning nothing costs nothing. Inventing a fact to look \
         productive costs the person their store."
    )
}

/// Mode A. The subagent writes a file, and a missing file reads as a crashed agent.
pub fn agent_prompt(
    run_id: &str,
    chunk_path: &str,
    out_path: &str,
    chunk_num: usize,
    total: usize,
) -> String {
    format!(
        "{marker}{run_id}\n\nYou are extracting durable facts from chunk {chunk_num} of {total} of \
         one person's agent transcripts. Read {chunk_path}. Write your result to {out_path}. Touch \
         no other file.\n\nDo not call any memory tool. Do not call any tool whose name starts with \
         mcp__lumberroom__ or mcp__agentmemory__. Do not write to any store. Your only output is the JSON \
         file.\n\n{SPEAKERS}\n\n{BODY}\n\nIf the chunk holds no durable fact, write exactly \
         {{\"facts\": [], \"refusal\": \"<no-facts/>\"}} to {out_path} and stop. That is a correct \
         and expected answer. Write the file even if facts is empty. A missing file reads as a \
         crashed agent.",
        marker = crate::ingest::FENCE_RUN,
    )
}
