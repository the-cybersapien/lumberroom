# Architecture

What the service should look like to be maintainable and to survive a change of storage engine,
and where it currently falls short of that.

The target is ports and adapters, applied plainly: interfaces and constructor arguments, no
container, no framework, no ceremony. This is one person's service and the architecture should be
legible in an afternoon.

---

## Why change anything

Two goals, both stated rather than inferred.

**Storage must be swappable.** Not because Postgres is wrong (measurements and research say it is
right for this workload) but because being locked to it is a bad position to hold by accident. The
lock-in today is not the database, it is that SQL is written inside the request handlers.

**The service must be maintainable.** Eleven SQL statements are spread across six files that also
do authorization, validation, formatting and transport. Every one of those files has more than one
reason to change.

Where the code stands:

| File | Lines | SQL statements | Also does |
|---|---|---|---|
| `mcp/http.ts` | 217 | 2 | routing, auth, registry writes, stats |
| `tools/context_bootstrap.ts` | 249 | 2 | grants, caching, markdown rendering |
| `tools/memory_search.ts` | 168 | 2 | grants, ranking weights, result shaping |
| `tools/memory_write.ts` | 123 | 3 | grants, validation, dedupe, cache invalidation |
| `tools/registry_get.ts` | 81 | 1 | grants, namespace precedence |
| `instrument.ts` | 76 | 1 | invocation parsing, aggregation |

The parts that are already right are worth naming, because they are the pattern to copy: `config.ts`
validates everything at boot and hands back a typed object, `namespaces.ts` is pure logic with no
I/O, and `embed/` is an interface with three implementations chosen by configuration. The embedder
is the proof that this shape works here; storage should look the same.

---

## Target shape

```
src/
  domain/          types and rules. No I/O, no imports from anywhere below.
                   Namespace, Sensitivity, Grant, Principal, Memory, RegistryEntry, errors.

  ports/           interfaces the domain needs the outside world to satisfy.
                   MemoryRepository, RegistryRepository, ToolCallRepository, Embedder, Clock.

  services/        use cases. One per thing the system does, depending only on ports.
                   BootstrapService, SearchService, WriteService, RegistryService, StatsService.

  adapters/        implementations of ports.
    postgres/      repositories, schema, migrations.
    embedding/     local, openai, hash.  (already this shape)
    auth/          token, oidc.          (already this shape)

  interfaces/      how the outside world reaches the services.
    mcp/           tool registration. Translates arguments and results, holds no logic.
    http/          routes. Translates requests and errors, holds no logic.

  platform/        logging, request context, metrics, shutdown.
  app.ts           the composition root. The only file that knows every concrete type.
```

The dependency rule is the only one that matters: **domain and services never import from adapters
or interfaces.** A service asks a `MemoryRepository` for rows; it does not know Postgres exists.
That single rule is what makes a second storage implementation possible, and it is testable with a
lint rule rather than discipline.

### What a port looks like

```ts
export interface MemoryRepository {
  search(q: SearchQuery): Promise<SearchHit[]>;
  insert(m: NewMemory): Promise<Memory>;
  findExact(tenant: string, namespace: string, content: string): Promise<Memory | null>;
  findById(tenant: string, id: string): Promise<Memory | null>;
  digest(q: DigestQuery): Promise<DigestData>;
  namespaceCounts(tenant: string): Promise<Map<string, number>>;
}
```

`SearchQuery` carries namespaces, an embedding, a limit and the ranking weights. It does not carry
SQL, a table name, or anything else that assumes an engine. The hybrid ranking is the one place
this is genuinely hard: blending vector distance with lexical rank is expressed differently on every
engine, so it belongs behind the port rather than in a service. The port promises ranked results;
how they are ranked is the adapter's business.

### What a service looks like

```ts
export class WriteService {
  constructor(
    private readonly memories: MemoryRepository,
    private readonly embedder: Embedder,
    private readonly policy: PolicyService,
  ) {}

  async write(principal: Principal, input: WriteInput): Promise<WriteResult> { ... }
}
```

Constructor arguments, no container. Tests hand it fakes; `app.ts` hands it Postgres.

---

## Prod readiness

The gaps, in the order they would hurt.

**Request correlation.** Log lines carry no request id, so two concurrent calls interleave with no
way to separate them. Generate or accept an id per request, carry it in `AsyncLocalStorage`, and
include it in every line and every error returned to a client. This is the difference between a log
you can debug from and a log you can only read.

**An error taxonomy.** Errors are ad hoc: some throw `Error`, some throw `AuthError`, and the
mapping to a status code is written at each call site. Define the small set the domain actually has
— `NotFound`, `Forbidden`, `Validation`, `Conflict`, `Unavailable` — and map them to HTTP and to MCP
results in exactly one place each. Then a new endpoint cannot invent a new error shape by accident.

**Query timeouts.** No statement timeout is set. One pathological query can hold a pool connection
until the client gives up. Set `statement_timeout` on the pool, and give the bootstrap path a
tighter one than the rest, since it has a latency budget it is supposed to honour.

**Backpressure.** The pool has a size and no queue limit, so load turns into unbounded waiting
rather than a fast failure. Cap the wait and return `Unavailable`.

**An audit trail that can answer questions.** `tool_calls` records that a call happened and whether
it succeeded. It cannot say which row was written, which was deleted, or why a request was refused,
which means the Phase 3 exit test cannot actually assert what it claims and a delete leaves no
record of what went. Writes and deletes need their own audit rows carrying the target id, the actor
and the reason.

**Metrics.** `/statsz` answers product questions about model behaviour. It does not answer
operational ones: error rates, latency distributions, pool saturation, embedder health. A
`/metrics` endpoint in Prometheus text format costs little and is the difference between noticing
degradation and being told about it.

**Configuration surface.** Already good. It validates at boot and fails loudly, which is why several
classes of deployment error cannot happen. Keep that standard: every new setting gets validated in
`config.ts` rather than read from `process.env` at the point of use. Two settings currently break
that rule by reading env at module scope in the search and write paths.

---

## Testing, per layer

The current suite is good and tests the wrong shapes in one respect: it constructs tool contexts by
hand, so it tests handlers rather than use cases.

- **domain**: pure functions, no fixtures. Namespace grammar, grant matching, ranking arithmetic.
- **services**: fake repositories. This is where behaviour belongs — dedupe, supersession, grant
  narrowing, digest assembly. Fast, and independent of whether Postgres exists.
- **adapters/postgres**: a real database, one transaction per test, rolled back. Tests SQL, not
  behaviour. This is the layer a second storage implementation would have to satisfy, so the suite
  doubles as the specification for one.
- **interfaces**: the wire. Auth rejection, status codes, MCP result shapes, truncation limits.

The existing integration and wire suites already cover the bottom and top. What is missing is the
middle, and it is missing because there is no middle.

---

## Order of work

1. Extract `domain/` and `ports/`. Types only, no behaviour moves yet. Nothing breaks.
2. Write `adapters/postgres/` implementing the ports, moving SQL out of the tool files verbatim.
   No behaviour change, so the existing tests are the safety net.
3. Introduce services, moving logic out of the tool handlers. The handlers become translation.
4. `app.ts` composition root; `interfaces/mcp` and `interfaces/http` stop constructing their own
   dependencies.
5. Platform work: request context, error taxonomy, timeouts, metrics, audit rows.
6. Add the service-level test layer, and a lint rule enforcing the dependency direction.

Steps 1 to 4 are mechanical and behaviour-preserving; the 113 existing tests are what makes that
claim checkable. Step 5 is new capability. Step 6 is what stops the shape eroding.

---

## Storage decision

Postgres 16 with pgvector, and the reasoning is in
[`docs/research/`](research/). The architecture above is what makes that decision reversible rather
than permanent: a different engine means a second `adapters/` implementation satisfying the same
port tests, not a rewrite of the tools.

The query builder or ORM used inside the Postgres adapter is an adapter-local choice. It should not
appear in a port signature, a service, or a domain type. If it does, the abstraction has leaked and
the portability it was meant to buy is gone.
