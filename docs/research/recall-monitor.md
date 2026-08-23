# Research: what the recall monitor measures

Written after re-running the monitor for the first time since its exact arm was fixed. Every figure
here comes from a run on a scratch database seeded for the purpose, on 21 August 2026. The commands
and settings are recorded below so a later run can disagree with this one on the same ground.

**Two findings, and the second is the one that matters.**

First: on 40,001 rows the HNSW index loses nothing worth measuring. Mean recall@10 sat between
0.981 and 0.988 across five runs, no run missed a single true nearest neighbour in 1,900 probes,
and the small deficit is tie-breaking at the k boundary rather than a neighbour the index failed to
find. The 10th-nearest distance the index returns matches the exact 10th to within 1.1e-07.

Second: the monitor can still compare an exact scan against an exact scan and report perfect
recall, for a reason that has nothing to do with the `SET LOCAL` bug that was fixed. At `k=1` the
planner declines the HNSW index and both arms run sequentially. The run reports `recall_at_k: 1.0`,
which is true and worthless. The tell is in the report already: `index_ms` and `exact_ms` come out
within 0.2% of each other. Any recall figure whose two timings are comparable is a self-comparison.

---

## What it measures

`recall::measure` samples stored rows, embeds each row's own content as a query, and runs that
query twice through `nearest_ids`: once with the planner free to use the HNSW index, once inside a
transaction with `enable_indexscan` and `enable_indexonlyscan` off. It reports the mean fraction of
the exact top-k that the indexed arm also returned, and counts the probes where the indexed arm
missed the true nearest row.

## What it does not measure

**It never filters.** `nearest_ids` restricts to `tenant_id` and `embedding IS NOT NULL` and
nothing else: no namespace, no sensitivity ceiling, no `superseded_by IS NULL`. Every search a
client actually runs carries all three. Filtered vector search is HNSW's hard case and the place
the truncation failure lives, so the monitor is measuring the easy shape of the query and staying
silent about the dangerous one. The section below reproduces the dangerous one by hand on the same
corpus.

**It measures self-retrieval, not retrieval.** The query is the stored text, so the target sits at
distance zero and every probe asks whether the index can find an exact copy of a row it holds. A
person asks a question that shares few words with the answer. Nothing here says what recall that
question gets, and no figure in this document should be quoted as if it did.

**It says nothing about the production embedder.** This run used `EMBED_PROVIDER=hash`, which is
what makes it hermetic and reproducible with no model download. Hash vectors are sparse, and a
21-token sentence lands on 29 of 768 dimensions on average, with values drawn from a small set, so
exact distance ties are common in a way they are not for `bge-base`. Ties inflate the apparent miss
rate and they also make the index's job easier. Both directions of that bias are real, and neither
is quantified.

**Mean recall hides the tail.** `recall_at_k` is a mean over probes and `worst` carries five
entries. A store where 1% of rows are unreachable would show a mean near 0.99.

---

## Establishing that the exact arm is exact

Two independent checks, both on the running server rather than on the source.

`EXPLAIN ANALYZE` on the statement `nearest_ids` issues, with and without the transaction settings:

```
planner free
  Limit -> Index Scan using memory_embedding_hnsw on memory (actual rows=10)   37.5 ms

BEGIN; SET LOCAL enable_indexscan=off; SET LOCAL enable_indexonlyscan=off;
  Limit -> Sort (top-N heapsort) -> Seq Scan on memory (actual rows=40001)    311.1 ms
```

Then the counters, which cover the binary rather than a reconstruction of its SQL. Over one
100-probe run at `k=10`, `pg_stat_user_indexes.idx_scan` for `memory_embedding_hnsw` rose by
exactly 100 and `pg_stat_user_tables.seq_scan` on `memory` rose by about the same. One arm through
the index, one arm through the heap, 100 probes each. Read the counters after the run settles:
Postgres flushes them asynchronously and a read taken immediately after the HTTP response was short
by four.

**The statistics matter and nobody would notice if they were wrong.** Before `ANALYZE` ran on the
freshly loaded table, the planner estimated 98 rows where there were 40,001, costed a bitmap scan
and a sort below the index scan, and used no HNSW index in either arm. The monitor would have
reported a clean number from two exact scans. A recall figure taken on a store that has never been
analyzed is not a recall figure.

---

## The corpus

| | |
|---|---|
| Rows | 40,001, all with a 768-dimension embedding |
| Namespaces | `project:alpha` 12,000, `project:beta` 10,000, `global` 8,000, `user:me` 6,000, `project:gamma` 3,800, `project:rare` 200 |
| Sensitivity | `open` throughout, so `sample_content` returns plaintext and no KEK is involved |
| Text | one generated sentence per row, ten fragments drawn from six lists of 90 phrases plus a serial number, 18 to 26 tokens, no two identical |
| Embedder | `hash-v1-768`, computed outside the database and loaded with `COPY` |
| Index | `memory_embedding_hnsw`, m=16, ef_construction=128, built incrementally by the load, 156 MB |
| Database | Postgres 16.15, pgvector 0.8.6, the compose `db` container, scratch database `lumberroom_recall` |
| Server | `lumberroom-server:0.1.0` on port 8799, `AUTH_MODE=token`, `EMBED_PROVIDER=hash`, `KEK_PROVIDER=none` |
| Host | 8 cores, everything in Docker on one machine |

The load path was `COPY`, not the write service, so nothing here exercises dedupe, conflict
detection, or the embedding pipeline. Vectors computed outside Rust match `HashEmbedder` to 1.1e-08
per component, checked against one row written through `memory_write` on the same server. The table
was analyzed after the load.

---

## The numbers

`hnsw.iterative_scan = strict_order` and `hnsw.ef_search = 100`, both from migration
`20260819000003`, confirmed with `SHOW` on a fresh connection after restarting the server. The
restart matters: `ALTER DATABASE ... SET` reaches new connections only, and a pool opened during
first boot predates the migration it just applied.

| Run | sample | k | recall@k | top-1 misses | index ms/query | exact ms/query |
|---|---|---|---|---|---|---|
| 1 | 100 | 10 | 0.9830 | 0 | 7.8 | 89.1 |
| 2 | 500 | 10 | 0.9828 | 0 | 5.3 | 85.5 |
| 3 | 500 | 10 | 0.9834 | 0 | 12.5 | 117.8 |
| 4 | 500 | 10 | 0.9808 | 0 | 12.2 | 112.2 |
| 5 | 300 | 10 | 0.9880 | 0 | 8.5 | 89.8 |
| 6 | 200 | 50 | 0.9857 | 0 | 6.2 | 81.4 |

Spread across the four 500-and-300-probe runs at `k=10` is 0.7 points. The per-query timings move
by a factor of two between runs on an otherwise idle machine, which is what running four containers
and a build on one laptop looks like; treat them as an order of magnitude and no more.

**The deficit is tie-breaking, not misses.** On 200 probes, the distance of the indexed arm's 10th
row exceeded the exact 10th on 10 probes, and the largest excess was 1.0e-07, which is float32
rounding. On 100 probes, 10 had an exact tie between the 10th and 11th nearest rows, with a mean
gap of 0.0023 between those two ranks. When rank 10 and rank 11 sit at the identical distance, the
two arms pick different rows and the monitor scores it a miss although the index returned a
neighbour just as near. Set-overlap recall counts that as a failure. It is not one.

Widening the search does not move it, which is what you would expect if there is nothing left to
find:

| `hnsw.ef_search` | recall@10, 300 probes | index ms/query |
|---|---|---|
| 40 (pgvector default) | 0.9823 | 4.5 |
| 100 (migration 003) | 0.9880 | 8.5 |
| 400 | 0.9807 | 15.8 |

Those three differ by less than the run-to-run spread. **Migration 003's `ef_search=100` earns
nothing on this measurement**, and the reason to keep it is the filtered case below, which this
measurement does not cover.

---

## The run that proves the monitor can still fool itself

`sample=200, k=1` reported `recall_at_k: 1.0`, with `index_ms` 16,449 and `exact_ms` 16,419. Two
arms, the same time, to within 0.2%.

`EXPLAIN` on the `LIMIT 1` form shows a sequential scan and a top-N heapsort with the planner
entirely free. It priced both plans and preferred the heap: 2,209.02 for the sequential scan against
2,507.79 for the HNSW scan, the second read off the same query with `enable_seqscan` off. Repeating
the run while watching the counters, `idx_scan` on `memory_embedding_hnsw` rose by **zero** over 100
queries.

So `recall@1 = 1.0` on this store is an exact scan compared against an exact scan. Not one number
in the `k=1` row of any report is evidence about the index. The `k=10` runs are safe by the same
test, at 9 to 16 times the exact arm's speed, and that ratio is the only thing separating them.

This is the same class of failure as the `SET LOCAL` bug, arriving through the planner rather than
through the session. The fix for the first was to open a transaction. There is no fix for the
second at this layer, because the planner is allowed to choose, so `RecallReport` now carries
`exact_speedup` and the reader has to look at it. Below roughly 2, the report is not a recall
measurement.

That field is implemented and has not run: the workspace did not compile while it was written,
for reasons in another module. Every ratio quoted here was computed by hand from the `index_ms` and
`exact_ms` the runs already returned.

---

## What the monitor cannot see, reproduced by hand

The same corpus, a namespace filter, one probe vector, asking for 10 rows:

| `iterative_scan` | `ef_search` | namespace | share of table | rows returned |
|---|---|---|---|---|
| off | 40 | `project:gamma` | 9.5% | **2** |
| off | 100 | `project:gamma` | 9.5% | 10 |
| strict_order | 100 | `project:gamma` | 9.5% | 10 |
| off | 40 | `project:rare` | 0.5% | 10 |
| strict_order | 100 | `project:rare` | 0.5% | 10 |

Two rows where ten were asked for, no error, under pgvector's own defaults. The plan says it
plainly: `Index Scan using memory_embedding_hnsw ... Rows Removed by Filter: 38`. It pulled 40
candidates and 38 of them were in other namespaces.

`project:rare` survives the default settings for a reason worth knowing: at 200 rows out of 40,001
the planner picks the btree on `(tenant_id, namespace, sensitivity_rank(sensitivity))` and sorts
200 rows exactly, so HNSW never runs. The danger zone is the middle. A namespace large enough that
the planner reaches for the vector index and small enough that a fixed candidate batch mostly falls
outside it.

Migration 003 holds. The monitor, as written, would report 0.98 on a database where every filtered
search was returning two rows out of ten.

---

## What would change these numbers

- **A real embedder.** `bge-base` vectors are dense and tie far less, which should raise set-overlap
  recall and makes the index work harder at the same time. Direction unknown, and this run says
  nothing about it. Believed, not measured.
- **More rows.** 40,001 is the scale the truncation finding used and roughly 2,000 times the live
  store. HNSW recall degrades with graph size, so a store an order of magnitude larger deserves its
  own run.
- **Filtering.** Covered above, and the gap between 10 rows and 2 is not a rounding difference.
- **`ef_construction`.** The index here was built incrementally by 40,000 inserts. A bulk
  `CREATE INDEX` builds a different graph, and neither was compared against the other.
- **Deleted rows.** This corpus has no supersessions and no deletions. HNSW keeps deleted entries in
  the graph until a vacuum, and a store with heavy churn is a different index.

## Re-running it

```bash
docker compose exec -T db psql -U lumberroom -d postgres -c 'CREATE DATABASE lumberroom_recall;'
docker run -d --name lumberroom-recall --network lumberroom_default -p 127.0.0.1:8799:8787 \
  -e DATABASE_URL=postgres://lumberroom:$PASSWORD@db:5432/lumberroom_recall \
  -e AUTH_MODE=token -e AUTH_TOKENS='[{"client":"recall","token":"...","read":["*"],"write":["*"]}]' \
  -e EMBED_PROVIDER=hash -e KEK_PROVIDER=none lumberroom-server:0.1.0
# seed, then:
docker compose exec -T db psql -U lumberroom -d lumberroom_recall -c 'ANALYZE memory;'
docker restart lumberroom-recall
curl -s -H "Authorization: Bearer $TOKEN" \
  'http://127.0.0.1:8799/admin/recall?sample=500&k=10'
```

Seed with `COPY` and vectors computed outside the database, or the load costs an hour. Analyze
after the load, restart the server after the migration, and check `exact_speedup` before reading
`recall_at_k`. The scratch database was dropped after this run.

Never point this at the live `lumberroom` database. The binary that boots against it applies every
migration it carries, and migrations are forward-only.
