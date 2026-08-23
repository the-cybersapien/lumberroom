# Mode A: the session as extractor

Extraction (`docs/specs/phase-6-ingestion.md` §8) runs one of two ways. This document is about
choosing between them, and about what has actually run as of this writing.

## What has run

Mode B ran once, end to end, on 20 August 2026: `--source claude --project memoryEngine --since 7d
--max-files 40`, extracted through z.ai's `glm-5.3`, submitted with `--no-auto`. It read 9,211
entries from 40 files, excluded 6,668, cut 2,543 survivors into 284 spans and 40 chunks, spent
267,607 prompt tokens and 22,630 completion tokens across 40 requests, and queued 177 proposals with
zero refused and zero files held back. A second `plan` straight afterward cut zero new spans, which
is the criterion the pipeline is built against.

Mode A has not run. The skill (`.claude/skills/lumberroom-ingest/SKILL.md`) is written against the locked
`plan`/`submit` contract and the prompt baked into `crates/lumberroom/src/ingest/prompt.rs`, but no one
has typed `/lumberroom-ingest` and watched a chunk of subagents come back. The first real invocation is
the gate that settles whether the dispatch loop, the fence, and the missing-chunk handling behave as
specified.

## Mode A versus Mode B

Mode A dispatches subagents through the Agent tool, inside the live session running the skill. Mode
B calls a provider's chat completion endpoint directly from `lumberroom ingest extract`. Both read the
same `spans/chunk-NN.json` files, both write the same `out/chunk-NN.json` shape, and `submit` cannot
tell which mode produced a given output file. Every fact from either mode lands in the same proposal
queue behind the same review gate; neither mode changes what reaches the store.

**Reach for Mode A when:**

- No provider key is configured, or the owner does not want to set one up for this run.
- The run is small enough to dispatch as one message of Agent calls. A chunk holds up to 40 spans or
  24,000 characters (spec §8.1 step 7), so in practice this means a few dozen chunks, not hundreds.
- The owner is present and interactive. Mode A needs a live session to dispatch subagents into; it
  cannot run unattended or from a schedule.
- The concern is which infrastructure a span touches. Mode A's spans go to subagents inside this same
  session and nowhere else; no extraction request crosses the network to a third-party provider.
  `plan`'s tripwire scan still posts span plaintext to the owner's own lumberroom server regardless of
  mode (spec §8, §9.1). That traffic is unrelated to which mode runs, and it stays inside the
  owner's own infrastructure either way.

**Reach for Mode B when:**

- The corpus is large. 40 chunks already spent 267,607 prompt tokens through `glm-5.3` (measured 20
  August 2026); a backfill over months of transcripts runs hundreds of chunks, and dispatching
  hundreds of Agent calls in one message is not what the tool is built for.
- The run should happen unattended: on a schedule, overnight, or with nobody watching a session.
- The resource being conserved is the session's own context rather than provider spend. Mode A spends
  this session's tokens and turns on every chunk it dispatches; Mode B spends a provider bill instead
  and leaves the session free.

## Cost, stated as what is measured and what is not

The z.ai figures above are a measurement of Mode B at that scale, not a claim about Mode A. Mode A's
cost lands in the dispatching session's own token budget, and nothing has measured that yet because
nothing has run it. Expect it to cost proportionally in session tokens rather than provider spend;
say that, not a number, until a first run produces one.

## See also

- `docs/ingestion.md`: the three-stage pipeline (`plan`, extraction, `submit`), the exclusion table,
  the run directory layout, provider keys for Mode B.
- `docs/specs/phase-6-ingestion.md` §7, §8, §11: the fence mechanism, both extraction modes in full,
  and the skill's contract.

Mode B's side of the choice is in `docs/ingestion-providers.md`, including what each provider
costs in tokens and which models honour the shape they are asked for.
