# 0011. The cleanup pass proposes, and the model only ever sees open rows

21 August 2026. Accepted, both halves verified. The deterministic half by
`scripts/cleanup-test.sh` and `tests/cleanup.rs`; the model half by a run against z.ai with
`glm-5.3` over the owner's own store, recorded below.

## The decision

A periodic pass reads the store as a whole and writes findings into `cleanup_proposal`. It never
retires a row. Applying a proposal is a separate act, and it goes through `services::review::supersede`
and `services::review::delete` rather than touching `memory` itself.

The pass has two halves. The deterministic half finds exact duplicates and pairs above a cosine of
0.97, runs on the server, reads every sensitivity, and sends nothing anywhere. The judgment half
takes pairs between 0.85 and 0.97, runs in the `lumberroom` client, and sees rows at `open` and nothing above.

## The context that forced it

The owner's store held 20 memories on the day this was written. Of the 13 live ones: two rows a test
harness wrote and never cleaned up, an injection probe from a security run, two statements of the
same image-generation preference in different words, and two different values for the same project
nickname.

Both nickname rows answered searches. Both test rows reached the next session's digest as facts
about the owner, which is how they were found.

None of that is a bug in a write path. `memory_write` checks the row in front of it and flags a
conflict; the review queue lists pairs above a threshold. Nothing steps back and asks what the store
now contains, and after a month that is where the damage is.

## What lost, and why

**Acting rather than proposing.** A pass that retires rows on its own is a memory that forgets
without being asked, and the failure is silent: the fact is gone and nothing says so. Ingestion made
this call first and this follows it. The cost is a queue the owner has to read, which is real, and
it is the cost the whole approval gate exists to pay.

**Running the model call on the server.** The provider path, the key storage, the retry and every
JSON tolerance a model has forced already live in the `lumberroom` client. A server that called a provider would
need all of them again, and it would need the key. It would also put an outbound connection to a
third party inside the process holding the KEK.

**Showing the model everything.** Decision 0005 already draws this line for the lexical index, on
the argument that a `tsvector` is the document rather than an index over it, so indexing private
content publishes it. Sending a row to a provider publishes it further and to someone else. The
filter is a `sensitivity = 'open'` predicate inside the candidate query, never a pass over results,
so a row the model may not see never enters the process that talks to the provider.

**A separate `mayCleanup` capability.** `may_ingest`'s own comment applies verbatim: a client that
can post proposals can fill a queue, and a queue the owner stops reading is an approval gate in name
only. A cleanup proposal names existing rows and asks to retire them, which is the same trust and
the same blast radius. A second flag would be one more thing to forget to set.

**The HNSW index for the pair query.** Every query here filters by namespace, by sensitivity and to
live rows, and a filtered HNSW query returned zero rows against 40,000 in this repository once,
having pulled forty candidates and discarded all forty with no error. A pass whose job is to notice
what the store contains must not be able to answer "nothing" because an index truncated. The
distance is computed exactly, the work is new rows times live rows, and a test asserts the `ORDER BY`
that would bring the planner back.

## Costs accepted

**The queue is another thing to read.** Rejecting is a terminal state, so a cluster the owner has
refused is never proposed again, and `obsolete` closes a finding the store has answered on its own.
Both exist because the failure mode here is a queue that grows faster than anyone reads it.

**The thresholds were guesses, and one of them has now been measured.** 0.97 and 0.85 came from the
Phase 4 spec, which says they are guesses. The lower one was wrong: the owner's store held exactly
one duplicate a person had found by reading it, and it scores 0.694, so 0.85 would never have shown
it to a model. The floor is 0.65 and `--min-similarity` overrides it. The upper one is still
untested. Every proposal publishes the similarity that produced it, so the queue stays the
instrument.

**The model tier is named rather than described.** The default is `qwen/qwen3.7-flash`, chosen
because a probe on 21 August 2026 over five clusters from the owner's own store scored it 4 of 5
exactly, ahead of every other tier including Opus, at $0.00019 and 6.9 seconds. Haiku found zero
contradictions across two runs, so "something cheap" is not a specification. Both numbers are one
day's measurement of one provider and will need retaking.

**Every duplicate must have arrived some other way.** `services::write::run` collapses a
near-identical write, so a duplicate cannot be created through it at all. This pass earns its place
on rows that predate those checks, on cross-namespace copies the write path deliberately never
collapses, and on contradictions no threshold finds. That is a narrower claim than "the store
accumulates duplicates", and it is the true one.

## What this is explicitly not for

**Deciding which of two conflicting facts holds.** A `contradiction` proposal names no survivor and
cannot be applied. The pass points; the owner decides. A pass that also picked the winner would be
writing facts rather than reporting conflicts, and it would be doing it with a model that has seen
two sentences and nothing else.

**Compaction or summarisation.** Nothing here rewrites the text of a memory. Every kind either
retires a row into another row that already exists or deletes an unread one.

**Anything at `private` or `sealed` reaching a model.** The deterministic half covers those, and it
is the only half that ever will.

## The reversal condition

If the queue produces findings the owner rejects more often than he applies, the thresholds are
wrong or the model tier is. Both are recorded per proposal, so the queue itself answers which.

If a `paraphrase` proposal ever retires a row that carried a correction, the numeric guard in the
prompt has failed and the judgment half comes out. That failure destroys data and does it quietly,
which is why the guard is stated twice: in the prompt, and in `collapse_block` on the write path
that this pass deliberately does not go around.

## The run that settled the model half

21 August 2026, `glm-5.3` through z.ai, over the owner's live store at a floor of 0.65. Nineteen
pairs, three calls, 4,967 prompt and 1,109 completion tokens. **Nothing discarded**: every verdict
parsed and every pair reference resolved to a pair in its own batch.

| verdict | count | what it found |
|---|---|---|
| `contradiction` | 1 | two different values for the same project nickname, at 0.954 |
| `same` | 1 | the same image-generation preference stated twice, at 0.694 |
| `unrelated` | 17 | correct on all 17 |

Both findings were the two a person had already identified by reading the store, which is the only
check available: there is no labelled set here and inventing one would measure the fixture.

The contradiction is the case that matters. Two rows differing only in an identifier are exactly
what the prompt's hardest rule guards, because collapsing them destroys the correction. `glm-5.3`
called it a contradiction, named no survivor, and gave a reason that stands without re-reading the
pair. A `same` verdict there would have taken the model half back out.

Run three times. With the watermark left alone the second run queued nothing. With the watermark
cleared, all nineteen pairs were judged again and produced the same nineteen verdicts, two already
known and nothing queued, so the cluster keys held across a full re-judgement.

One call it declined that a person might not have: two rows about the same project and the same
path, one naming the subdirectories and the other the GitHub repositories. Neither contains the
other, so merging them would lose a fact, and `unrelated` is what the prompt asks for when two
statements differ in a detail that could matter.

**Not measured:** whether 0.97 is the right upper bound, `qwen/qwen3.7-flash` on this same store, and
the `stale` kind, which needs a row nothing has read for ninety days and the store is younger than
that.
