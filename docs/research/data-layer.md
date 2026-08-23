# Research — ORM, query builder, or neither

Commissioned to replace inline SQL in request handlers. Evaluated against the five query shapes this
service actually runs, not against popularity.

**Recommendation: Kysely, a typed query builder, not an ORM. And leave the migration runner alone.**

The brief asked for an ORM. The honest finding is that an entity ORM is the wrong tool for this
particular service, and the reasoning is below rather than buried. The goal behind the instruction —
get SQL out of handlers and into named, testable, typed functions — is served either way, and the
first stage of the migration is identical under every outcome.

---

## The five query shapes, by tool

| Shape | Drizzle | Prisma | Kysely | TypeORM | MikroORM | plain `pg` |
|---|---|---|---|---|---|---|
| Named CTEs | native | none, raw only | native, typed | native | native | raw |
| **Recursive CTE** | **unsupported, [#209 open since 2023](https://github.com/drizzle-team/drizzle-orm/issues/209)** | raw only, TypedSQL has an open Postgres bug | native, typed | native | native, CTE-join bug fixed days ago | raw |
| **FULL OUTER JOIN** | raw fragment | raw only | native | **absent from the source entirely** | undocumented | raw |
| `percentile_disc WITHIN GROUP`, `json_build_object` | raw fragment | raw only | raw fragment, documented idiom | raw string, untyped | raw, untyped | raw |
| `ON CONFLICT DO UPDATE SET version = version + 1` | via `sql` fragment | `upsert()` has open atomicity issues | **native typed expression builder** | **structurally cannot express it** | undocumented | raw |
| `vector(768)` and `<=>` | native type, but [HNSW index DDL bug #5792](https://github.com/drizzle-team/drizzle-orm/issues/5792) | `Unsupported()` plus raw, and a live drift regression | official thin `pgvector/kysely` wrappers | best native column type, but no HNSW index type at all | no official integration | raw plus the official serialization helper |

Two entries decided it.

**Drizzle cannot do recursive CTEs.** Open since March 2023, verified still open. Phase 4 walks
supersession chains, which is precisely a recursive CTE, so choosing Drizzle means hand-writing a raw
fragment for the exact query that motivated bringing in a builder.

**TypeORM cannot express `version = version + 1` in an upsert.** `.orUpdate()` renders only
`col = EXCLUDED.col`, and a breaking change in March 2026 removed the override. It also has no
`FULL OUTER JOIN` anywhere in its source, and a data-loss bug in migration generation that has been
open since 2019.

---

## Why not an ORM at all

An entity ORM maps rows to objects and generates CRUD from that mapping. This service's complexity
is five hand-tuned SQL shapes: a hybrid-search blend, a single-round-trip JSON-nested digest, a
versioned upsert, a percentile aggregate, and soon a recursive traversal. **None of those are
object-graph operations,** and across all three entity ORMs every one of the five drops to a raw or
minimally-typed escape hatch.

So an ORM's fixed costs — Prisma's 78MB client and mandatory codegen step, TypeORM's reflection at
initialisation, a migration system that wants to own the migration history — buy typed CRUD on the
roughly 30% of the surface that was never the maintainability problem.

The stated goals are maintainability and a future storage swap. A query builder serves the first as
well as an ORM and the second **better**, because it does not tempt you to lean on a mapping layer
that would itself need replacing.

---

## Why Kysely specifically

- **Recursive CTEs and FULL OUTER JOIN are native and typed**, the two places every other candidate
  falls back to strings.
- **`onConflict().doUpdateSet()` takes an expression builder**, so `eb('version', '+', 1)` type
  checks. A direct match for the registry upsert.
- **Zero runtime dependencies, about 1.7MB**, sitting directly on the `pg.Pool` already constructed.
  No codegen, no binary, nothing added to boot time on a small ARM box.
- CTE results type-thread into later `selectFrom` calls, so the hybrid search keeps its types across
  all three CTEs.

Its pgvector support is thinner than Drizzle's on paper: the official `pgvector/kysely` package is
six `sql`-wrapped distance helpers with no typed column. In practice this service touches vectors in
exactly two places, the column definition and the `<=>` ordering, and the existing `toVector()` and
`::vector` cast pattern ports unchanged. Grade it "official but thin," not missing — and note that
Drizzle's richer story is currently undermined by a bug generating HNSW index DDL without the
operator class, which is exactly the index we use.

---

## The fair argument against

Kysely is pre-1.0 and raised its minimum Node version to 22 in May 2026, leaving no downgrade
headroom. More substantially: **the two hardest queries stay mostly raw `sql` fragments under Kysely
too.** The delta over a disciplined plain-`pg` repository layer is real but modest — roughly 40%
typed builder and 60% typed raw fragments, against 100% raw with hand-written interfaces.

If appetite for another dependency is low, plain `pg` behind a repository layer is a legitimate
choice rather than a fallback. What Kysely buys is typed CTEs, a typed full outer join and a typed
upsert-with-increment. Three real wins, one more dependency floor to track.

---

## Migration path

1. **Extract repositories, with no new dependency.** Move the SQL out of the five handler files into
   named functions behind the ports. **Valuable under every outcome**, including never adopting a
   builder, and it is the actual fix for "SQL inside request handlers." Do this first and separately.
2. **Introduce Kysely on the same pool.** Convert the straightforward queries first: the registry
   lookup, the write path's insert and dedupe check, and the upsert.
3. **Convert the hybrid search.** `.with()` for the three CTEs, `.fullJoin()` for the merge, a typed
   `sql` fragment for the weighted score. The most work and the most payoff.
4. **Leave the digest as a raw `sql<Digest>` block, probably permanently.** Four nested
   `json_build_object` subqueries in one round trip do not map onto any builder's DSL, Kysely
   included. A well-organised raw template with a hand-written result type is the right long-term
   answer here, not a stopgap.
5. **Convert the percentile aggregate.** Low risk.
6. **Build the recursive CTE in Kysely when Phase 4 needs it.** The query that would have hurt most
   under Drizzle.
7. **Leave the migration runner alone.** None of the six candidates adopts a directory of
   hand-written `.sql` files; every one wants to own the directory and the tracking table. The
   advisory-lock runner already works. The one improvement worth making is independent of this
   decision: **checksum verification of already-applied migrations**, which a from-scratch runner
   lacks and Flyway-style tools provide.

---

## The two arguments for an ORM, tested against this codebase

The case for an ORM is usually made on two grounds. Both are real in general and both were checked
here rather than argued.

### "You do not have to maintain SQL injection protection"

Injection protection does not come from an ORM. It comes from **parameterised queries**, where the
driver sends values separately from the statement, and this codebase already does that everywhere.
Verified: no template interpolation appears in any SQL string, and every statement carrying a
placeholder passes a values array.

Tested end to end against the running server. `'; DROP TABLE memory; --` was submitted as fact
content, as a tag, as a search query, and as a registry key. Every table survived and the payload
was stored verbatim as data, which is the correct outcome.

**The real force in the argument is discipline, not mechanism**: nothing stops a future edit from
concatenating a string. That is worth fixing, and an ORM is not the only way to fix it — nor a
complete one, since every ORM has a raw escape hatch with exactly this risk.

`test/sql-safety.test.ts` makes it a build failure instead. It scans the whole source tree for
template literals that both look like SQL and contain an interpolation, and it asserts that every
parameterised query in the adapter passes its values. It includes a test that the rule catches the
pattern it exists to catch, so it cannot rot into a no-op. That is a **stricter** guarantee than an
ORM provides, because it also covers the escape hatch an ORM would leave open.

### "An ORM makes switching the underlying engine easier"

True for an application whose queries are CRUD over generic tables. Not true for this one, and the
reason is visible in what the SQL is made of:

| Construct | Occurrences | Portable? |
|---|---|---|
| `= ANY($n::text[])` | 11 | Postgres arrays |
| `::vector` and `<=>` | 5 | pgvector only |
| `to_tsvector`, `websearch_to_tsquery`, `ts_rank` | 5 | Postgres full-text |
| `percentile_disc ... WITHIN GROUP` | 4 | varies by engine |
| `json_build_object`, `json_object_agg` | 3 | Postgres spelling |
| `FULL OUTER JOIN` | 1 | not in MySQL, not in SQLite |
| `ON CONFLICT` | 1 | Postgres and SQLite spelling |

No ORM abstracts any of these. Prisma requires `Unsupported("vector(768)")` and hand-written
migration SQL for both the column and the index; TypeORM has a vector column type and no HNSW index
type at all. A different engine means rewriting the search, the digest and the index strategy no
matter what sits on top.

And the coverage cuts the wrong way. Of the eleven statements in the adapter, an entity ORM would
express **five natively** — the plain inserts and single-row selects — and push the other six to raw
SQL. **The six it cannot express are the six that carry the complexity**, so an ORM's typing and
query-construction benefits apply precisely where they were never needed, and vanish precisely where
they would have helped.

Portability here comes from the repository port, which already exists: `search()` is a named
function with a typed signature that callers depend on, and the SQL is an implementation detail
behind it. A second engine means a second implementation satisfying the same port tests. That seam
is identical whether the inside is raw `pg`, Kysely, or an ORM's escape hatch.

## Effect on portability, honestly

**Neither better nor worse, and claiming otherwise would be dishonest.** The SQL is intrinsically
Postgres-shaped: `<=>`, HNSW, `tsvector`, `websearch_to_tsquery`, `json_build_object`,
`percentile_disc`, array types with GIN indexes. No tool here abstracts that; the entity ORMs fall
back to the same raw Postgres SQL for the same queries.

Portability comes from the repository seam in stage 1 — the fact that `search()` is a named function
with a typed signature that callers depend on, not the SQL string inside it. That seam is the same
shape whether the inside is raw `pg` or a Kysely chain. **If a future storage swap is a real goal
rather than a hedge, stage 1 is the lever, and the builder choice is not.**
