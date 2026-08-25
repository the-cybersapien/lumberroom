# 0008. A memory carries two clocks, and only one of them exists today

20 August 2026. Accepted, implemented, including the phase 2 this record deferred. Amended twice, on 20 and 25 August 2026; the second edits rule D1 and makes `occurred_at` fillable. Both amendments are below.

The header first read "phase 1 implemented". That was wrong on the day it was written: `occurred_at`
appeared in this record and nowhere else in the tree, and two researchers found the discrepancy
independently. `docs/specs/phase-7-valid-time.md` is the plan that implements it.

## The decision

A memory row gains valid time: `occurred_at`, when the fact became true in the world, and
`occurred_until`, when it stopped. Both nullable. `created_at` keeps its meaning, which is when the
store learned the fact, and the two are never conflated again.

Phase 1 is the columns, the write path, the ingest fill, the supersession backfill and range
filters over live rows. Phase 2 is the as-of query that reads retired rows, and it is deferred with
its reasoning below so nobody has to rediscover it.

## The context that forced it

lumberroom has one clock. `created_at` says when a row was written, and every read, every filter and every
tiebreak uses it. That is transaction time, and a memory system needs valid time too.

The failure is live in this store today, not hypothetical. Ingestion queued 222 proposals on
20 August from Claude Code and Codex transcripts spanning a week, and the Codex sessions reach back
to July. Every one of those rows carries 20 August. A fact the owner stated in July and a fact he
stated this morning are, to this store, the same age. The transcript knew better: the span's
timestamp sits on `ingest_proposal_source.observed_at` and stops there.

Three questions the store cannot answer, and all three are ordinary:

- What did I decide in March?
- What was the Postgres port before I changed it?
- Which of these two facts is older, given both arrived in the same backfill?

The retrieval benchmark agrees, though it is the weaker argument. `temporal-reasoning` is the
second-worst category at 91.7% against a published 95.5%, and the harness writes every session with
the same timestamp because the write path has nowhere to put the session's real date.

## What lost, and why

**One timestamp with a smarter ranker.** Weighting `created_at` harder cannot separate "written
recently" from "true recently", and a week of backfilled history makes those opposites. The
distinction is in the data or it is nowhere.

**A `valid_from` string, matching the registry.** `Provenance.valid_from` is a `String` today, which
cannot be compared, indexed or ranged over without parsing it at every read. Memory takes
`timestamptz`. The registry now carries a different shape for the same concept, and aligning it is
owed rather than done. Recorded here so the divergence is deliberate.

**Deriving the date from the content with a model.** An extractor guessing when a fact became true
is a guess stored as a fact, and the tripwire's whole design rests on refusing that pattern.

## Costs accepted

A nullable column is a third state to reason about, and most rows will hold it. A preference has no
date and forcing one would be a lie, so a NULL means timeless and every query has to say what it
does with those.

**The correction-versus-change trap, which one rule cannot serve.** When B replaces A there are two
different worlds. The port moved, so A was true until B, which is a change. Or A was always wrong
and B is the truth, which is a correction, and A was never valid at all. Change is the default here
because it fits the ingest case and the benchmark case, and because the alternative silently
rewrites history the owner may want to see. Correction is the known limitation: `occurred_until` is
settable at supersede time for the caller who knows the difference.

A successor with no `occurred_at` sets its predecessor's `occurred_until` to the successor's
`created_at`, so the timeline stays contiguous rather than leaving a gap nobody can query across.

The recency term ships at weight zero. Fusion is under experiment at the same time, and adding a
second uncontrolled variable to the ranking would make both results unreadable.

## What this is not for

**It is not a fix for the benchmark, and it should not be judged as one.** LongMemEval's temporal
questions mostly need the right session found, which is lexical and semantic work, and answering
"how many weeks ago" belongs to a reader model that retrieval metrics never score. Expect
`occurred_at` to move that category a little. The product case stands on its own: a store that
misdates a week of its own memory is wrong whatever a benchmark says.

## Amendment, 20 August 2026: phase 2 was deferred on a premise that was false

The section below defers the as-of query, and the first reason it gives is wrong. It says the query
"collides with the `SEARCH_LIVE` predicate compiled into the search SQL". That predicate has never
been compiled in. It has always been a parameter of the `search_sql!` macro, which is why
`SEARCH_LIVE` and `SEARCH_ALL` already existed side by side before this phase started. As-of is a
third variant built from the same macro.

Reading the file would have settled it. It was not read, and a query worth having sat behind an
imagined obstacle for as long as this record stood.

Two of the four collisions were real and both were smaller than stated. The digest's join count and
the bootstrap cache are untouched, because an as-of read is a different statement rather than an
edit to the live one. The one genuine cost is the bind parameter, since tests pin the statement's
parameter count, and only the as-of variants took a new one so the live texts stayed byte for byte
identical.

The policy half of the deferral stands and was answered rather than dodged. A grant over live rows
is not a grant over the history behind them, so `may_read_history` follows `may_delete` and
`may_ingest`: off by default, grantable on both auth paths, checked in `services::search` because a
repository holds no principal and the statement would otherwise serve retired rows to anyone.

`docs/specs/phase-7-valid-time.md` and decision 0009 carry what shipped.

## Amendment, 25 August 2026: the fence made the column ceremony, and nearly fired the reversal

The reversal condition at the bottom says the column is ceremony if a year of use shows nothing
setting it outside ingestion. Eight days in, measured on the live store: **0 of 175** rows written
directly by one client into `project:investing` carried a date, while the same client ran at **100%**
in `project:lumberroom`, `global` and `user:me`. The split is not habit. Those namespaces are filled
by the ingest path, which calls `write::run_observed` and bypasses the near-now fence.

The fence is what did it. `memory_write` refuses any `occurred_at` inside 86,400 seconds of now, so
an agent recording an event on the day it happens cannot date it, ever, and no path existed to supply
the date later: nothing in the store wrote `occurred_at` after the insert. The reasoning behind the
refusal was sound and its blast radius was not examined. Rule D1 stopped a model stamping today onto
something it merely heard today, and it also stopped every real date an event carried.

Three changes, and the first one is the rule this amendment edits.

**The fence now admits a same-day date the content itself names.** Rule D1 stands in every other
case. The objection it was built on is that an invented date is indistinguishable afterwards from a
stated one; a date written verbatim in the row's own text is checkable against that row forever, by
anyone, which is the one thing an invented timestamp can never be. Four renderings are matched, day
precision only. The future stays shut regardless of what the sentence claims, because a future start
reads live and never reads as-of.

**`occurred_at` becomes fillable, and stays immovable.** A new repository method writes it only
where it is NULL, refused in the statement rather than by the caller, so a gap can be filled and a
start can never be moved. The service refuses any date the content does not name, refuses the future,
and requires the caller's write grant. This exists because roughly 90 rows already carry an explicit
date in their prose with `occurred_at` NULL, and before this they were unrecoverable.

**An undated row no longer holds at every instant.** The as-of predicate read
`occurred_at IS NULL OR occurred_at <= t`, so a row with no date matched every instant including
instants before the store existed, and a retired fact came back beside its undated replacement. It
now falls back to `created_at`: the store does not claim a fact held before it learned it. No field
says which clock answered because the row already does, since a hit carries `occurred_at` only when
it is set.

What this does not fix: two facts dated the same day still write no end when one supersedes the
other, because closing the period would write an empty interval. That is recorded in 0014.

## Phase 2, deferred and specified

The as-of query is the payoff. `memory_search(as_of: t)` should answer what held at `t`, which means
reading rows that `superseded_by IS NULL` currently hides. It is deferred because it collides with
four things at once: the `SEARCH_LIVE` predicate compiled into the search SQL, the digest's seven
grant-filtered subqueries whose join count is pinned by a test, the bootstrap cache, and dedupe's
assumption that it compares against live rows only.

It is also a policy question rather than only a query. A client granted read over live rows has not
obviously been granted the history behind them, and a retired fact can be more revealing than the
one that replaced it. That decision belongs with the two-axis grant model and it has not been made.

## Reversal condition

If a year of use shows every row carrying a NULL `occurred_at` because nothing ever sets it outside
ingestion, the column is ceremony and the ranker should go back to one clock.
