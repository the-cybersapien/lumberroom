# Research: pgvector in production

Commissioned because the earlier benchmark, 100k rows of random vectors on a laptop, measures the
index rather than production. This one looks for real-world evidence: failure reports, GitHub
issues, and migration stories. Confidence is labelled where the evidence is thinner than it looks.

**Verdict: Postgres with pgvector is the right foundation for this workload, without
qualification.** Every documented failure threshold sits 100 to 1,000 times above a store of tens
of thousands of rows. What is worth acting on is not the ceiling, it is the configuration.

---

## The finding that mattered, reproduced here

Every search this system runs filters by namespace before ranking, which is HNSW's hard case.
With `hnsw.iterative_scan` off, which is the default, the scan pulls a fixed candidate batch, then
applies the filter, and returns whatever survives.

Reproduced on this schema shape, 40,000 rows, a namespace holding 0.5% of them:

```
asked for 10 rows, iterative_scan = off
  Index Scan using t_embedding_idx (actual rows=0)
  Rows Removed by Filter: 40

asked for 10 rows, iterative_scan = strict_order
  Index Scan using t_embedding_idx (actual rows=10)
  Rows Removed by Filter: 3385
```

**Zero rows, no error.** The caller is told nothing is known, about facts that are present. For a
memory system this is the worst failure available, and it becomes more likely with every phase:
Phase 2 adds `work:` and `personal:` namespaces, Phase 3 narrows reads further by sensitivity, and
per-client grants make selective filters the norm rather than the exception.

Fixed in `db/migrations/003_hnsw_recall.sql`, which sets the setting on the database so it travels
with the schema rather than living in a config file that a different deployment might not have.

Background: pgvector 0.8.0 introduced iterative index scans for exactly this, and the behaviour has
not changed architecturally through 0.8.6. Crunchy Data's
[hybrid search post](https://www.crunchydata.com/blog/hybrid-vector-search) documents the canonical
case: default `ef_search=40` with a 10% selective filter returns around 4 rows when 10 were asked
for.

---

## Where pgvector actually breaks, with citations

No full postmortem exists. What exists is issues with hard numbers, all far above this scale:

- [#822](https://github.com/pgvector/pgvector/issues/822): 40M rows, 768 dimensions, 40GB
  `maintenance_work_mem`, 11 workers. Build stalled at 29.2% after 19 hours, with "hnsw graph no
  longer fits into maintenance_work_mem after 570,905 tuples."
- [#559](https://github.com/pgvector/pgvector/issues/559): 1M rows, 512 dimensions. Inserts went
  from milliseconds to 5-8 seconds per 100-row batch once the HNSW index existed.
- [#692](https://github.com/pgvector/pgvector/issues/692): a real memory leak in
  `HnswUpdateNeighborsOnDisk` during vacuum, 128KB per call, a plausible OOM source under heavy
  deletion.
- [#700](https://github.com/pgvector/pgvector/issues/700): the operational cliff worth knowing.
  A 32GB instance with 16GB `shared_buffers` held about 2,100 QPS until the index outgrew
  `shared_buffers`. At 2.5M rows and a 19GB index, 1,225 QPS. At 5M rows and 38GB, **12.9 QPS**,
  with the buffer hit ratio still reading 98-99%. Hit ratio is a misleading health signal once the
  graph exceeds RAM.
- [#721](https://github.com/pgvector/pgvector/issues/721): the planner abandoning the index for a
  sequential scan depending on `LIMIT` and selectivity. One reporter saw `LIMIT 50` use the index
  at 64ms and `LIMIT 100` fall back to a scan at 3,420ms. Unresolved upstream, so it is on us to
  notice it in `EXPLAIN` output.

Applying #700 to our own measured numbers: 391MB of index at 100k rows is roughly 4KB per row, so
on a 24GB box with 8-12GB of shared buffers the cliff would sit somewhere near 2 to 3 million rows.
Two orders of magnitude beyond realistic growth.

**No failure-driven migration story was found.** The migration content that exists is cost or
feature driven.

---

## Recall, which matters more than latency here

No production recall study exists at this scale; that data has not been published. The closest is a
synthesis piece reporting recall segmented by query frequency: head queries 0.94, torso 0.87, and
**tail queries 0.41 to 0.43**, all hidden behind a 0.91 average. Treat the exact figures as
secondary, but the shape is credible and it is the wrong shape for a memory system, where the rare
specific fact is exactly the one worth storing.

The answer to "nobody has published this" is to measure it rather than to guess. At this scale an
exact scan is cheap, so ground truth is always available. `lumberroom recall` samples stored memories,
uses each as its own query, and compares the indexed result against a forced sequential scan. A
memory should always retrieve itself, so a miss is unambiguous.

```
$ lumberroom recall --sample 25
sampled 25 stored memories, comparing indexed search against an exact scan
recall@10: 100.0%
nearest-neighbour misses: 0 of 25
```

Below 90% it prints the weakest queries and what to change. There is also a test asserting it.

---

## Tuning applied

| Setting | Was | Now | Why |
|---|---|---|---|
| `hnsw.iterative_scan` | off | `strict_order` | Every query is filtered. Off risks silent truncation |
| `hnsw.ef_search` | 40 | 100 | The default is widely held too low; at this query rate the cost is free |
| `ef_construction` | 64 | 128 | Community convention for 768 dimensions. Builds are rare, recall is not |
| `m` | 16 | 16 | Already the default and already right |
| `shm_size` | 64MB | 1GB | Parallel HNSW builds fail below roughly 512MB |

`strict_order` over `relaxed_order` because this workload is single-user and recall-critical, so it
can afford the extra graph traversal to keep exact distance ordering.

---

## Triggers to reconsider

- **Row count.** Nothing urgent below 1M. Start paying attention near **2 to 3M**, where the
  measured 4KB-per-row index size approaches available shared buffers on a 24GB box.
- **Version.** pgvector **below 0.8.4** is a trigger regardless of scale: 0.8.3 and 0.8.4 fixed
  possible index corruption and a graph-repair error during vacuum. We run **0.8.6**, verified.
- **Recall.** Self-defined, since no industry number exists at this scale. Suggest recall@10 below
  0.9 on real queries.
- **Operational.** The `LIMIT`-sensitive planner flip in #721, if it ever bites.

---

## Alternatives

**No neutral benchmark exists.** Every comparison found has a stake: the pgvector-versus-Qdrant
piece showing Qdrant at roughly 15 times the QPS has a co-author at Qdrant; Timescale's 50M-vector
comparison is published by the company selling pgvectorscale. Say that plainly rather than citing
either as fact.

The informal crossover where dedicated vector databases start to matter is around **10M vectors**,
or high sustained QPS, or index types pgvector lacks. Every threshold found sits 100 to 1,000 times
above this workload.

**pgvectorscale** is real and current, and an independent benchmark found StreamingDiskANN 9 times
faster at p95 with a 14 times smaller index at 1M vectors. Its entire value is keeping an index
larger than RAM performant. That is a problem this system does not have and will not have for
years, so it would add an extension dependency for no measurable gain.

One lead worth noting rather than acting on: an independent piece argues that hybrid search, the
`ts_rank` blending specifically, is the weak point of Postgres-native setups, and suggests ParadeDB
as an alternative worth benchmarking. Unverified for our queries, and worth testing before believing.

---

## Known gaps

- **ARM is under-researched.** No Ampere or Graviton pgvector benchmark exists anywhere. pgvector's
  CPU-dispatched distance functions ([PR #311](https://github.com/pgvector/pgvector/pull/311)) are
  x86-64 only, so an ARM box computes distances without an optimization x86 gets. Our own benchmark
  ran under linux/arm64, so it shares that handicap and is representative in this one respect.
- **HNSW across a Postgres major upgrade** is not authoritatively documented. Historical rebuild
  requirements in the changelog applied to IVFFlat. Test a restore and reindex before depending on
  it.
- **Filtered-versus-unfiltered latency and recall deltas** are not quantified anywhere for pgvector.
  Which is what `lumberroom recall` is for.
