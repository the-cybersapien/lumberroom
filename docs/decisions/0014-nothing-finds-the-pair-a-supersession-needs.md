# 0014. Nothing finds the pair a supersession needs

25 August 2026. Draft. Parts 1 and 2 are implemented and tested, the currency measure included.
Parts 3 and 4 are not built. The gate that decided whether part 4 was worth building has
been run and part 4 passed it, on the live store and on a replica. Every number here says whether a
run produced it.

## The decision

Four parts. The order changed under review and the reason is in each entry.

1. **Clear `occurred_until` when a revive undoes a supersession.** A bug in `main`, upstream of
   everything below, and the only item here that is finished rather than proposed.
2. **The as-of read reaches a model-facing surface, and the currency measure follows it.** Part 1 in
   the first draft, moved because a measure that asks the store what held at an instant cannot run
   through a door no client can open. The measure is the instrument's first user, not the other way
   round.
3. **Supersession gets proposed rather than guessed at.** It lives in the cleanup pass over stored
   rows, as a fifth proposal kind, not as a second queue in the import client.
4. **A graph over memories, in Postgres, held until one rerun says it is needed.** An edge table
   seeded from vector search, bounded in fan-out and depth, with the caller's grant applied at every
   hop. The rerun that decides it is named under *Reversal condition*.

## The context that forced it

An imported dump on 25 August 2026 carried two lines four days apart. The pair is real, the wording
below is invented, and no part of an imported dump belongs in this repository.

```
[2026-08-14] (stated) - I am more inclined to wait on <A> and watch how it behaves.
[2026-08-18] (stated) - Sold <A> for <B>.
```

Both were true. The second ended the first.

**The store can already record that, and the first draft of this record said it could not.** That
sentence was wrong and it was carrying four pieces of work. `memory_write` takes `supersedes`
(`src/mcp/mod.rs`), the console offers Replace this fact (`src/console/pages.rs`), and 0008 fixes
the rule: a successor with no `occurred_at` ends its predecessor at the successor's `created_at`.
One statement implements it, `RETIRE_PREDECESSOR_SQL` in the Postgres adapter.

What no code does is **produce the pair**. `crates/lumberroom/src/import/plan.rs` sets
`supersedes: None` on every proposal, with the comment "An import does not retire a live fact. The
owner supersedes by hand or not at all." The ingest submit path does the same. So a superseded fact
reaches the junk pass with nothing beside it, and that pass asks the only question it has: is this a
durable fact? It called the 14 August line a passing inclination and offered to drop it.

The verdict follows from the question, and the question is wrong. The line is not junk. It stopped
holding on the 18th. Deleting it destroys the reasoning behind a position still held, and the owner
had already said memory exists to hold why a position was taken and when to look again.

Two things this record now says plainly that the first draft did not. That pass only drops under
`--drop-junk`, so the default run reports and the owner decides: nothing was lost, and the trigger
was a near miss under an opt-in flag. And the one-line fix was available all along, since the same
prompt already carves out decisions as durable, so a clause saying a stated lean is durable and a
later reversal supersedes it would have changed that verdict.

Keeping both without ordering fails the other way. A task asking what the owner thinks about `<A>`
gets two contradictory facts and no way to rank them.

## The second failure, measured twice on 25 August 2026

Asked *which position has a regulatory decision date coming up that I said I would re-look at*,
`memory_search` returned six rows over `project:investing`, which holds **272 live rows** at the open
ceiling. None was the answer. The first draft called 272 the size of the store. It is the size of one
namespace; the store holds 467 rows at that ceiling across four.

The answer is a single row, naming a held position whose failure condition is a drug missing its
19 September PDUFA. Asked by name it comes back **first, at 0.834**. Asked as the question above it
does not appear at six, and it does not appear at **twenty**.

**The runs, so these figures clear the rule `docs/benchmarks.md` sets.** All on 25 August 2026,
scoped to `project:investing`, through `memory_search` with the shipped ranker and no reranker.

| run | store | result |
|---|---|---|
| `--limit 6` | live | six rows, 0.512 to 0.556, answer absent |
| `--limit 20` | live | twenty rows, 0.512 to 0.570, answer absent |
| `--limit 20` | local replica | twenty rows, top hit 0.5700, answer absent |
| by name | live | answer first, 0.834 |
| by name | local replica | answer first, 0.843 |

The replica is the 467 open rows exported from the live store and rewritten locally under the same
embedder, `Xenova/bge-base-en-v1.5@q8`. Its top hit on the failing query scored 0.5700 against the
live store's 0.570, so the two rank alike and nothing here is an artefact of one machine.

**This kills the cheaper explanation.** `SEARCH_DEFAULT_LIMIT` is 8, so six rows was a shallow read,
and `docs/benchmarks.md` says of this system that candidate generation has little headroom and the
rest is ranking. Had the answer been sitting at rank 12, the failure was ranking and no graph was
needed. It is not at rank 12, and it is not in twenty. A row that scores 0.834 against its own name
does not reach the top twenty of a question that describes it.

**The question joins a held position to a catalyst, and neither phrase appears in the row that
answers it.** That claim now has a run behind it.

**One thing the run took away.** Only **12 of those 272 rows carry `occurred_at`**, four percent. The
catalyst dates sit in prose. Calling this a join onto a *dated* catalyst overstates what the
valid-time columns hold, so what would answer it is a typed link rather than a date filter. That
narrows part 4 rather than supporting it.

**Content size, from the same export.** 467 rows average **247 bytes**, median 231, and the
`project:investing` rows average 290. The first draft said 262 with no run named.

## Part 1. The revive bug

`delete` revives a predecessor by clearing `superseded_by` and `superseded_at`. It never cleared
`occurred_until`. The row came back to live search, which filters on `superseded_by` alone, and
stayed invisible to every as-of read, which filters on `occurred_until`. `RETIRE_PREDECESSOR_SQL`
writes that column through `COALESCE`, so re-superseding the row kept the stale end rather than
correcting it. The owner's only repair was psql.

`INSERT INTO memory` never writes `occurred_until`, so the retire statement is its only author and
clearing it on revive destroys nothing an owner supplied. Fixed, with an integration test asserting
a revived row returns to both reads.

It stayed invisible because nothing on the model surface could pass `as_of`. Part 2 is what made it
reachable, which is why this went first: shipping the surface over a broken undo would have handed a
client a fact the store had quietly stopped being able to date.

## Part 2. The as-of read on a model surface, then the measure

**The argument shipped on 25 August 2026. The measure has not been built.**

Before it, the query, the port field, the service parameter and the capability gate all existed and
every caller passed `None`, in `mcp`, in the console and in `eval`. So a column of behaviour had no
reachable caller: the only code that could exercise the as-of read was a test, and a filter nothing
can invoke is a filter nobody can be wrong about. That is what made it worth doing first.

Confirmed on the dev store after the change, against a fact starting 25 August: a live search
returns it, `as_of` the 26th returns it, `as_of` the 20th does not and an older row answers instead,
and `as_of 2020-01-01` returns nothing at all. The last one is the `created_at` fallback below doing
its job; before it, every undated row matched every instant and 2020 would have answered with the
whole store.

**The policy sentence is the deliverable, and the field is the easy half.** The MCP layer withholds
`as_of` for two reasons and only the second is the capability. The first: a model turning a question
into a date is guessing, and this system refuses that pattern everywhere, which is the same stance
0008 takes on extractors supplying dates. Part 2 reverses that refusal, so it has to argue it.

The rule: `as_of` takes an explicit absolute instant. A model converting "in March" into a timestamp
is making the caller's assertion, not the store's, and the store records no difference between an
instant the owner typed and one a model computed. That is the cost, stated rather than hidden.

**No new capability.** `may_read_history` already opens `include_superseded`, `memory_history` and
`registry_history`. As-of is a fourth handle on a door three handles already open, and it exposes no
class of data the others do not. A split does not draw a boundary either: a client holding `as_of`
alone reconstructs every retired row by moving the instant, so granting it without
`include_superseded` narrows the interface and not the disclosure. The flag appears at roughly 87
sites across 35 files, so the split is a day of mechanical work buying nothing. Revisit it when a
client the owner does not run asks for that capability.

**A precondition, before this reaches a model.** A timeless row satisfies the as-of predicate at
every instant, so a superseded timeless fact and its timeless replacement both come back for any
instant before the retirement. Most of the store is timeless. Either give a timeless row a start
before as-of ships, or widen the predicate to fall back on `created_at`, and say in the response
which one answered. Returning both facts is the failure this record exists to name, and it would be
this record's own feature producing it.

**Then the measure, which shipped on 25 August 2026.** Given a store holding both facts and a
question asked at an instant, does the store report the one that held then? Returning both is a
failure, and `CaseOutcome::passed` cannot score it as anything else: a case passes only when the
expected row is present *and* the row it replaced is absent.

It reports two numbers, and only the second needs a fixture. **Coverage** counts supersession pairs
and how many carry a closed interval, which is the number this record said nobody knew. It needs
nothing but the store. **Accuracy** takes labelled pairs, asks at the stated instant through the
same `memory_search` a model calls, and scores the answer. A case naming no expectation is refused
rather than scored, because it would pass and measure nothing.

Coverage alone is not enough and the module says so: a store could close every interval and still
answer the wrong version if the boundaries are wrong.

**First run, on the dev replica: 5 pairs, all closed, 1 with both halves dated.** That number is not
worth much yet. The replica was rebuilt through `memory_write` without replaying supersession links,
so those five are the dev store's own history rather than the imported corpus.

Two honest limits on it. The measure scores parts 2 and 3 and **cannot adjudicate part 4**: the
graph is argued from a compositional question that carries no dates and names no entity, which a
currency measure would not score at all. The first draft bound them into one gate and that was
wrong. And the first draft said the store scores at the floor by construction, which is true of the
imported dump and false of the store: rows written through `memory_write` with `supersedes` already
close their interval. The number the measure returns is the fraction of pairs carrying a closed
interval, and nobody here knows it.

**Where the pairs come from is a problem this record does not solve.** 0007 rejected hand-picked
fixtures in terms: choosing cases the author already suspects matter measures whether the author's
model of the system is self-consistent. Pairs drawn from the owner's own dumps are that fixture.
Take them, and write down the expected answers before running anything.

## Part 3. Supersession proposed, in the cleanup pass

**Venue first, because the first draft put it in the wrong place.** `cleanup_proposal` already
carries a kind, a `keep_id`, staleness checks and a proposed/applied/rejected state machine, and
applying routes through the same `supersede` the console does. Part 3 is a fifth kind, a prompt
clause and a candidate source. Building it in the import client would give two prompts, two queues
and two apply paths for one concept, and 0011 already flagged a queue that grows faster than anyone
reads it.

**The first cut links an incoming fact to a stored one.** The schema supports that today. It does
not support a link between two lines of a single dump: `ingest_proposal.supersedes` references
`memory(id)`, and two lines arriving together have no ids until approval. Closing that needs a
nullable proposal-to-proposal column and one branch in `approve` that swaps it for the predecessor's
id. This record does not add it, and the record's own motivating pair is a case it does not cover.

The loss is an hour, not a fact. `approve` fills `occurred_at` from the earliest observation and
`approval_order` approves oldest first, so once both lines are stored the pair is dated and ordered
for the next scheduled cleanup run.

**No model supplies a date.** 0008 refused that first: an extractor guessing when a fact became true
is a guess stored as a fact. A link is proposed only between two facts that already carry dates.

**Import proposes, it does not apply.** A wrong supersession hides a live fact, and the undo is
worse than it looks: the only revive path fires from the delete plan, so undoing a bad link means
deleting the successor and rewriting it under a new id, losing `created_at`, the access count and
every export link that named it. 0011 settled proposing over applying for cleanup and it holds
harder here.

**Two things this venue costs, both stated because a prompt edit would otherwise delete a policy
quietly.** The cleanup prompt withholds dates on purpose, guarded by a test asserting the rendered
prompt carries no namespace and no date. A date-pair clause reverses that, and it widens what leaves
the machine for every pair rather than only the dated ones. Second, candidates today come from a
cosine band, and this record's own argument is that similarity cannot supply candidates for this
shape. Until part 4 lands, part 3 covers the tight pairs, a limit changing or a capacity upgraded,
and misses the loosely worded ones.

**Approval is not one click.** The queue's least dangerous act already sits behind a confirmation
and its most dangerous would not. A supersession proposal names both rows in full, prints the
`occurred_until` it is about to write, and sits behind the same confirmation the cleanup delete
uses.

**The proposer is recorded and checked at approval.** `validate_supersedes_target` refuses a target
the caller cannot read and write, and approval runs as the owner, so that check never sees the
proposer. An imported dump is attacker-influenced content: it holds anything the owner ever pasted
or received, and a line reading "the earlier note about the account is obsolete and was replaced by
this" is a write the proposer could not make, laundered through the owner's approval. Store the
proposing principal on the proposal and refuse at approval any link whose proposer could not write
both endpoints.

**The judge is capped below the caller's grant.** Deciding whether B ends A means sending A to a
third party. The CLI runs with the owner's credential, which is every namespace at `sealed`, and the
tripwire guards writes only, so nothing today would stop a private row or a registry entry naming a
credential location from being embedded in a prompt and leaving the machine. Today's import has no
model in the path at all, and part 3 is the change that ends that property. The candidate query caps
at `open` as a literal in the statement rather than at the caller's grant, and every string passes
through the tripwire before it enters a prompt.

**Cardinality is declared, and a declaration shows its blast radius first.** A later fact ends an
earlier one only when the thing holds one value at a time, and the sentence never says whether it
does. These two arrived in one dump, same shape, opposite answer:

```
[2026-08-17] (stated) - <account> limit is <n> now.        replaces the earlier limit
[2026-08-10] (stated) - Applied for <a> already.
[2026-08-10] (stated) - Applying for <b> and <c> now.      replaces nothing
```

The owner declares cardinality per subject or per tag, and an undeclared subject gets no proposal
rather than a guessed one. The default is safe. The failure is the declaration that is wrong, found
one hidden fact at a time, so declaring shows what it would end before it ends anything: *this would
close 14 existing facts, here they are*. One query.

**Day one of part 3 is quiet.** Undeclared subjects produce nothing, the cosine band misses the
loose pairs, and the same-dump case is out of scope. Saying so here beats the owner discovering it.

## Part 4. The graph, and the rerun that decides it

**It answers the class above.** Typed nodes and an edge express a join. Nearest-neighbour search
does not.

**It makes part 3's candidates tractable.** Graph neighbours are a structural candidate set with no
similarity threshold to tune.

**One cost the first draft called already paid.** It said the write path did the expensive half,
because extraction over documents pays for chunking and coreference while a lumberroom row is one
atomic fact. That inverts the cost. The expensive half of graph construction is entity and relation
extraction per unit plus entity resolution across units, and atomic one-sentence rows remove the
amortisation a document gets. Entity resolution is itself a matching threshold, which is the thing
graph neighbours were meant to avoid. The atomicity is also a prompt rule and not an invariant:
nothing enforces it, the direct `memory_write` surface never stated it, and rows written before that
prompt existed carry no guarantee.

**Two assets shorten the build.** `entity_alias` is already a typed-name table with valid time and a
one-hop invariant, which is half a node table. `SUBJECT_HISTORY_SQL` is a working depth-capped
recursive walk that matches grants in SQL.

### Traversal is strict, and it reverses a rule this codebase already made

A client walks the subgraph it may read. The caller's grant arrives as three parallel arrays,
prefix, exact and ceiling, the form `split_grants` already builds for the timeline walk, and the
`EXISTS` over them applies **inside the recursive term**, so a node the caller may not read is never
expanded. `sensitivity_rank` is a database function, so nothing calls into Rust.

The first draft said traversal uses "the same predicate the row filter uses". It cannot. Search
resolves globs against a requested namespace list before the query runs, and a walk has no requested
list, because which namespaces it visits is the answer rather than the question.

**`SUBJECT_HISTORY_SQL` does the opposite on purpose, and this record reverses it for the graph
alone.** That statement applies the grant at the final join and never inside the recursion, because
filtering inside would sever a chain at the first row the caller may not read and report a partial
timeline as a complete one. It drops unreadable rows and counts what it dropped, so the caller is
told.

The discriminator: **a chain has a readable anchor and one subject**, so the gap can be described
without mapping the grant. **A graph's shape is the answer**, so any description of the gap is the
answer leaking. Edge count, degree and path length are facts about the store that no content filter
hides.

So the drop count does not survive into the graph, in either form. Not a count, and not a boolean
either: a "your grant truncated this walk" flag is a probing oracle, since a client varies the seed,
watches it flip and maps the boundary. `SUBJECT_HISTORY_SQL` is not touched, and back-propagating
strictness to it would reintroduce the partial-timeline lie it was written against.

Three more rules on the same surface:

- **The grant predicate reads `memory`, never a copy on the edge.** The edge carries each endpoint's
  namespace and sensitivity for fan-out planning, and those are a second source of truth for a
  mutable column. A row promoted to `private` would stay walkable at its old ceiling and nothing
  would detect the divergence. Join `memory` for both endpoints and filter on the row.
- **Degree is computed inside the caller's subgraph.** A global degree threshold is a function of
  sealed and private writes, so a low-privilege client that watches an entity's neighbours vanish
  learns the volume and timing of sealed activity without a row crossing the boundary. This project
  has already shipped four disclosures of that class, all values computed before the grant ran or
  after only one of its axes, and none caught by a test.
- **Traversal is subject to `may_read_history`.** A walk along a supersession edge reaches retired
  rows, which is what `memory_history` refuses the same client. That gate was already breached once
  by a second spelling of the same door. One `assert_may_read` at the service call site, not a new
  flag.

**The relation on an edge is a closed enum.** 0005 established that a plaintext derivative beside
encrypted content cancels the encryption, and a model-written relation label describes a private row
to anyone holding the database. The edge table has no sensitivity column of its own to filter on.
Per-candidate judge reasoning lives on the proposal and dies when the owner decides.

**The edge table cascades on delete.** Two foreign keys into `memory` with `NO ACTION` is the shape
that made a delete raise a foreign-key violation while the wrapped DEK and the plaintext embedding
stayed, filed as `forget-fk-blocks-shred` and settled in 0013. An edge surviving a crypto-shred also
leaks the shape of the shredded fact.

**Seeding inherits a trap this project has paid for.** `hnsw.iterative_scan` defaults to off, and a
filtered search at 40k rows once returned zero for a query asking for ten, because the scan pulled
40 candidates and the namespace filter removed all 40. Migration `003` holds it off with
`strict_order` and `ef_search=100`. A traversal seeded from a narrow namespace inherits the whole
failure and answers "no path" for an index reason, which would fire the reversal condition below on
the wrong cause.

### Scale

Postgres holds this. A 768-dimension vector is 3,080 bytes, past the out-of-line threshold, so the
bulk sits in TOAST rather than the heap and every hop that reads a row pays a detoast.
`docs/research/pgvector-at-scale.md` carries a measured 391MB HNSW index at 100k rows, which is
worth more than deriving it here.

An edge carries two ids, a relation, and each endpoint's namespace and sensitivity, near 100 bytes.
Three edges per memory at 100k memories is 300k rows and roughly 30MB before indexes, closer to 60MB
with two endpoint indexes and realistic row overhead. The first draft said 100MB and did not
reproduce from its own inputs.

Fan-out breaks before size. A hub entity links to a large share of the store, and an unbounded second
hop from one is unbounded work. Ten seeds, twenty-five per hop, depth two touches 6,500 edges worst
case, counting both hops. Those three numbers are a design target and belong in a test rather than
in a query somebody edits.

## What lost

**Another extraction pass.** Dedupe and the tripwire are structural and stay. The junk pass exists
because the store cannot say "true then, not now". The first draft argued that a fifth pass for
supersession and a sixth for the next shape is a treadmill that does not end, and stated that as
fact. It is a prediction. The competing one, that supersession is the last big shape because dedupe
and the tripwire cover the rest, is cheaper to test and is what part 3 tests.

**Cheaper answers to the second failure, none of them yet tried.** A `catalyst` tag against the
existing GIN index and the two partial valid-time indexes. Query decomposition into two searches
with an id intersection in SQL. A reranker over the top 20, which is where `docs/benchmarks.md` says
the remaining headroom lives. Query expansion. The first draft named none of these and ruled them
out by assertion. The rerun below decides whether any of them is enough.

**Judging this by LongMemEval.** `knowledge-update` scores 98.7% recall@5 in `docs/benchmarks.md`,
which reads like the problem is solved. The first draft dismissed it by attacking an answer-accuracy
column graded by a reader model. **That column does not exist on that page**, which states at the
top that it runs retrieval recall alone with no answer generation and no judge model, and 0007
already refused the QA-accuracy metric. The correct answer is one line and does not need the
invented column: recall asks whether the right evidence surfaced, and the failure here is that both
versions surface with no ordering between them. That page also says what it does not cover, in its
own words: it measures ranking on synthetic chat, and supersession, valid time and policy appear
nowhere in it.

## Costs accepted

**Two facts can contradict on their face and both hold**, in different scopes. A judge comparing two
strings without their scope answers with confidence and gets it wrong. The evidence it needs is not
in the text it sees, which is the cardinality problem wearing different clothes.

**A verdict without its reasoning is worth less than it looks.** Any supersession pass returns
per-candidate reasoning before the verdict, as a schema requirement.

**Strict traversal cannot distinguish absence from denial, and must not.** A client sees no path
whether none exists or none is theirs, and now with no count and no flag either. This widens a gap
already in the store, where an ungranted namespace answers search with an empty list. A client
reasoning off a severed subgraph will be confidently incomplete with nothing marking it, and
debugging that lands on the owner. The console reads as the owner, so it shows the whole graph and
marks which edges each client would refuse.

**Two similar walks now filter in opposite places**, held apart by the paragraph above and by
citation, with no compiler or test enforcing the distinction.

**Three fields where the shape wants four.** A row carries `created_at` for when the store learned a
fact and `occurred_at` with `occurred_until` for when it held. Nothing records when the store
stopped believing something, which differs from the fact ending. 0008 named the consequence: when B
replaces A, either A held until B, a change, or A was never true, a correction, and one rule cannot
serve both. That is an amendment to 0008 rather than a part of this record.

**Same-day pairs still write no end.** Two facts dated one day produce identical timestamps, and
closing the period would write an empty interval meaning "never true", so the store leaves the end
open and the fact reads as holding at every instant. A dump gives every line in a day the same date,
so this is common rather than exotic. Both supersede paths now return the condition and the history
line says "end date unknown" rather than looking finished. The hole itself is not closed.

**Deleting the middle of a chain leaves an interval nothing answers.** Splicing moves the link and
leaves the predecessor's end on the deleted row's date, so an as-of read between the two returns
nothing for a period the store holds an answer for.

**Timeless facts stay timeless.** A preference has no date and forcing one lies. Most rows carry
NULL on both columns, and every query says what it does with those.

## What this is not for

**Not a retrieval improvement until a number says so.** The claim is that the store can answer a
question it cannot answer today. Whether that improves what a task retrieves is a separate claim
wanting a separate number.

**Not a fix for the junk pass.** That pass keeps lines that were never facts. It should shrink as
superseded facts stop arriving at it. Shrink is not disappear.

**Not a second datastore.** Traversal speed nobody needs, bought with two copies of the truth that
can disagree.

## Reversal condition

**The gate that stood here has been run, and part 4 passed it.** The condition was: rerun the failing
query at limit 20 on the store that produced the original observation, and if the answer appears, the
failure is ranking and part 4 loses its only measured case. It ran on 25 August 2026, on the live
store and on a replica, and the answer appeared in neither. The table under *The second failure*
carries both. The cheaper answers, a reranker over the top 20 and query decomposition, are not ruled
out as *fixes*; what is ruled out is the claim that deeper retrieval alone reaches this row.

**Ten questions, written down first, and this is now the live gate.** If a bounded traversal at ten
seeds, twenty-five
fan-out and depth two answers no more of them than search does, the graph is not earning its
extraction cost and this record is superseded. The questions get written and their expected answers
fixed before the traversal exists, because 0007's objection applies to this record as much as to a
benchmark: ten cases chosen after the design, by the person who made it, cannot disconfirm it.

**For parts 2 and 3.** If the currency measure lands and the store already scores near its ceiling,
the premise is wrong and neither is worth finishing. That gate does not extend to part 4, which the
measure cannot score.
