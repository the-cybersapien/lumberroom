# Phase 4: Quality

> **Ends when:** a correction you make once does not resurface as a contradiction later.
> System PRD §7

Status: implemented 19 August 2026. Verification is pending. `scripts/correction-test.sh` is the
gate for the exit test below and has not yet run against a live server. No decision record
supersedes this spec; the design below stands as written.

Phase 4 is the phase that decides whether the store is worth reading in six months. Phases 2 and 3
add surfaces and safety. This one adds the property that the contents are true.

The failure it exists to fix is already visible. Running the Phase 1 done-when test four times left
four rows in the store, each claiming to be the project's official nickname, all four written by a
model acting correctly. A later session read the digest, noticed the contradiction, and refused to
answer. That is the good outcome. The bad one is the session that picks the oldest.

---

## 1. Supersession that retires

**Today.** `memory.supersedes` is a validated foreign key that nothing reads. A superseding write
lands beside the fact it replaces and both are returned by search.

**Design.** Two columns, set in the same transaction as the superseding write:

```sql
ALTER TABLE memory ADD COLUMN superseded_by uuid REFERENCES memory(id);
ALTER TABLE memory ADD COLUMN superseded_at timestamptz;
CREATE INDEX memory_live ON memory (tenant_id, namespace) WHERE superseded_by IS NULL;
```

The link is stored on both rows rather than derived, because every read filters on it and a
correlated subquery on the hot path is the wrong trade. The partial index keeps live-row scans
cheap regardless of how much history accumulates.

**Read semantics.** `memory_search` and `context_bootstrap` return live rows only. Nothing is
deleted: the history stays queryable, which is what makes the decision log in §4.7 of the system
PRD a side effect rather than a feature to build.

Add one optional parameter, additive, in keeping with the rule that signatures extend and never
rename:

```
memory_search(query, namespaces?, limit?, project?, include_superseded?)
```

**Chains.** A superseded by B superseded by C is normal and must resolve to C. Store the direct
link on each hop and filter on `superseded_by IS NULL`; reconstruct a chain with a recursive CTE
only when the decision log is being read. Reject a write whose `supersedes` target is already
superseded, and return the head of the chain in the error so the caller can retry against the
current row. Walk the chain on write to reject cycles; a two-row cycle makes both rows invisible.

**The part that is not schema.** Supersession only works if a model chooses to supersede rather
than write afresh, and models overwhelmingly write afresh. Two mechanisms, both required:

1. **Conflict candidates on write.** When `memory_write` is called without `supersedes`, and a
   live row in the same namespace scores above the conflict threshold, return it:

   ```json
   {
     "id": "...", "namespace": "user:me", "deduplicated": false,
     "possible_conflicts": [
       { "id": "...", "content": "The port is 8080", "similarity": 0.94 }
     ]
   }
   ```

   The tool description instructs the model to call `memory_write` again with `supersedes` set if
   the new fact replaces the old one. This is a similarity query, not an LLM in the write path, so
   it stays inside the constraint the Phase 1 PRD set.

2. **A review queue.** Conflicts the model ignores accumulate. `lumberroom review` lists them, and
   `lumberroom supersede <old> <new>` resolves one. The queue is the safety net for every case where the
   model did the easy thing.

---

## 2. Duplicate detection past exact match

**Today.** Byte-identical content in the same namespace returns the existing id. Anything else is
a new row.

**Design.** Three bands on cosine similarity against live rows in the same namespace:

| Band | Action |
|---|---|
| ≥ `DEDUPE_THRESHOLD` (start at 0.97) | Collapse. Return the existing id, `deduplicated: true` |
| `CONFLICT_THRESHOLD` to `DEDUPE_THRESHOLD` (start at 0.90) | Store, and return it as a conflict candidate |
| below | Store, say nothing |

**The numeric guard, which matters more than the thresholds.** "The port is 8787" and "The port is
8080" are near-identical to an embedding model and are the exact case supersession exists for.
Never collapse two texts whose digit sequences differ, or whose extracted identifiers differ, no
matter how high the similarity. Downgrade them to a conflict candidate instead. The same guard
applies to negation, which embeddings handle poorly: if one text contains a negation token the
other lacks, do not collapse.

Collapsing a correction into its own predecessor destroys data silently, and silent is the part
that makes it unacceptable. Every threshold in this section is biased toward storing too much.

**Calibration.** The starting numbers are guesses and must not stay guesses. Once the store holds
a few hundred real rows, dump every pair above 0.85 with its similarity, read them, and set the
two thresholds where the judgement actually falls. Record the calibration set in
`docs/research/dedupe-calibration.md` so a later change to the embedding model can be re-checked
against the same pairs.

---

## 3. Ageing

The PRD asks to age out stale facts. Nothing here deletes anything automatically: a personal
memory that silently forgets is worse than one that gets cluttered.

**Signals to record.** All cheap, all on the read path:

```sql
ALTER TABLE memory ADD COLUMN last_accessed_at timestamptz;
ALTER TABLE memory ADD COLUMN access_count int NOT NULL DEFAULT 0;
ALTER TABLE memory ADD COLUMN last_confirmed_at timestamptz;
```

`last_accessed_at` and `access_count` update when a row is actually returned in a result, batched
so a search does not turn into a write storm. `last_confirmed_at` is set when a write restates an
existing fact rather than contradicting it, which is a real signal: repetition is confirmation.

**What uses them.**

- **Ranking.** A small recency-and-use boost, capped low. A fact retrieved often is more likely to
  be the one wanted; a fact never retrieved in a year probably is not. Keep the weight small
  enough that it cannot outrank semantic relevance.
- **The review queue.** Rows older than a threshold and never retrieved are listed by
  `lumberroom review --stale`, with three actions: confirm, supersede, delete.
- **Registry TTLs by kind.** A `host` entry ages slowly; a `model-route` ages fast because routing
  preferences change monthly. Per-kind expectations live in config, and expiry marks a row for
  review, never removes it.

**Staleness as a number**, since §8 of the system PRD says a store that is read often and written
rarely is decaying: percentage of live rows never retrieved, median age of rows that were
retrieved, and write-to-read ratio per client. All three are already derivable once the columns
above exist; add them to `lumberroom stats`.

---

## 4. Recall testing against real memories

The PRD is specific that this is tested against your own memories, not a benchmark. Benchmarks
measure whether the embedding model is good. This measures whether the system remembers *your*
things.

**The fixture.** A file of cases, each a question, the row that should come back, and where the
case came from:

```yaml
- question: what cloud does the memory system run on
  expect_id: 2ae3164f-8dc3-42cb-a5a8-db4719e0a1cd
  origin: real miss, 2026-08-19
- question: what is my bank account number
  expect: none          # anti-case: must return nothing
  origin: privacy check
```

**How it grows.** Every time recall misses in real use, that question becomes a case. The suite is
a regression record of actual failures rather than a set invented up front, which is the only
version of this that stays honest.

**What it reports.** `lumberroom eval` runs each case through `memory_search` and prints recall@1,
recall@5, MRR, and the anti-case violations separately. Anti-cases are pass/fail: a question that
must return nothing and returns something is a failure regardless of the aggregate score.

**When it runs.** Before and after any change to the embedding model, the ranking weights, or the
dedupe thresholds. A change that improves the aggregate while breaking three previously-passing
cases is a regression, so report per-case deltas, not just totals.

---

## 5. The Obsidian mirror

One markdown note per fact, in a vault folder the tool owns, so the whole store is browsable
without an AI tool in the loop.

**One way only.** The database is the record of truth and the vault is a window onto it, exactly
as the system PRD states. Nothing reads edits back out of the vault. If two-way sync is ever
wanted it is a separate decision with a conflict model attached, not an extension of this.

**Layout.**

```
<vault>/lumberroom/
  registry/host/desktop.md
  registry/model-route/coding.md
  memory/user-me/2026-08-19-typescript-preference.md
  index.md
```

**A note.** Frontmatter carries provenance because that is the half fuzzy memory cannot answer:

```markdown
---
lumberroom_id: 0f4a...
kind: host
key: machines.desktop.os
namespace: global
source_client: claude-code-mac
confirmed: true
valid_from: 2026-08-19
supersedes: [[machines.desktop.os-2026-03-02]]
---

Ubuntu 26.04
```

Wikilinks on `supersedes` mean the decision log is navigable in the graph view, which is the one
thing Obsidian does better than a database.

**The export.** `lumberroom export --obsidian <path>` is idempotent, writes deterministic filenames, and
only ever touches files inside its own folder. Rows deleted from the database leave a tombstone
note rather than a deleted file: a tool that deletes files in a personal vault gets one chance to
be wrong. Run it from cron or by hand.

**Sensitivity interacts here.** Once Phase 3 lands, `sealed` content cannot be exported at all,
and `private` content in a vault synced to a third party defeats the encryption it was given. The
export takes a maximum sensitivity, defaulting to `open`.

---

## Exit test

The exit criterion is testable, and should be tested the way Phase 1 was: a script that runs the
real client, not a reasoned argument.

`scripts/correction-test.sh`:

1. Session A states a fact. Assert it is stored and retrievable.
2. Session B, fresh, states the corrected version of the same fact. Assert the model either passed
   `supersedes` or was handed the conflict, and that the correction resolved.
3. Session C, fresh, asks the question. Assert the answer contains the new value, does not contain
   the old value, and that the session did not have to ask which was current.
4. Assert the old row is still in the database with `superseded_by` set, so history survived.

Step 3 is the criterion verbatim. Steps 1, 2 and 4 are what make a pass mean something.

---

## Order of work

1. Supersession columns, read filtering, chain and cycle rules. Nothing else works without it.
2. Conflict candidates on write, plus the tool description changes. This is what makes
   supersession actually happen.
3. The review queue, which catches everything the model does not.
4. Dedupe bands with the numeric and negation guards, then calibration against real data.
5. Ageing columns and the staleness numbers.
6. `lumberroom eval`, seeded from misses observed during 1 to 5.
7. The Obsidian mirror, last, because it mirrors whatever the earlier steps settle on.
