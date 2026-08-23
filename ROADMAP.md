# Roadmap

The system PRD ([`docs/prd/system-prd.md`](docs/prd/system-prd.md)) describes five phases. This
document tracks where each one stands, what exists already, and what is missing.

Exit criteria are quoted verbatim from the system PRD. They are the only definition of done that
counts: each phase ends with a capability you can use, not a milestone you can report.

**Where this stands today (21 August 2026).** Phases 1 to 4 are verified, Phase 6 has run end to
end, and Phase 7 shipped. `VERIFY.md` carries the gate output for all of it.

Phase 6, ingestion. A week of Claude Code and Codex transcripts went through `plan`, `extract` and
`submit`, queueing 222 proposals from 9,211 entries with every exclusion counted by the rule that
made it. `scripts/ingest-test.sh` passes 19 assertions against its own server and database. Mode B
is the path that ran; Mode A has a skill and Mode C has a batch client, and neither has been used.

Phase 7, valid time. A memory carries `occurred_at` and `occurred_until` beside `created_at`, the
as-of query reads them, aliases collapse a renamed subject across namespaces, the registry keeps what
it replaces, and both clients can write and read a timeline. Decisions 0008 through 0010 record why.

Retrieval has a number that survives. LongMemEval-S recall@5 93.2% against a competitor's published
95.2%, on the same 500 questions and the same embedder, reproducible with
`scripts/eval-longmemeval.sh`. The whole gap is ordering: the right document already reaches the top
twenty for 98.4% of questions.

Phase 8, cleanup. A periodic pass reads the store as a whole and proposes duplicates,
contradictions and stale rows into a queue that the owner decides. It never retires a row on its
own. Both halves have run: the deterministic one through `scripts/cleanup-test.sh`, the model one
against z.ai with `glm-5.3` over the real store. Decision 0011 records it, including the measurement
that moved the lower similarity band from 0.85 to 0.65. Nothing schedules it yet.

Phase 8b, the surfaces around it. Ten MCP tools behind four capabilities, one table deciding which
grant opens which and `docs/permissions.md` written against it. A console page for the cleanup
queue. Both halves of the schedule inside the product: the deterministic pass on a timer in the
server, the model pass as a compose service. No cron anywhere.

**1,079 tests, 0 failures**: 774 in the server crate, 305 in the client. Nothing is deployed, and
every surface below runs on this machine only.

---

## Current state against the system PRD

Capability numbering follows system PRD §4.

The **Run** column exists because "the code is written" and "the behaviour has been observed" are
two claims worth keeping apart. A row that says `not yet` names the gate that would settle it; a row
that names a gate and a pass count has been watched.

| | Capability | State | Run | What exists | What is missing |
|---|---|---|---|---|---|
| 4.1 | One identity across every surface | **1 of 9 surfaces** | the wired one, Phase 1 | MCP over Streamable HTTP, three auth modes, per-client OAuth identity carrying its own grant row, one client wired | Eight surfaces. Each needs the owner's account, and all of them need a public endpoint |
| 4.2 | Automatic recall at start of work | **partial** | Phase 1, on an all-open store | `context_bootstrap`, p50 4ms, the SessionStart hook for Claude Code, an OpenWebUI Filter written but never installed | Browser surfaces have no hook and depend on the model choosing to call. The digest's cost over a store holding private rows is unmeasured |
| 4.3 | Write-back without being asked | **works, one surface** | Phase 1 | `memory_write`, the CLAUDE.md trigger rule, unprompted writes observed, per-client unprompted read and write rates recorded | Every other surface. This is the project's largest unknown |
| 4.4 | A registry of exact facts | **verified** | policy-test, 20 August | `registry` table, provenance, `registry_get`, the canonical key scheme enforced on write, `registry_alias` (migration 006) recording a rejected guess so the same wrong name resolves next time, per-kind review dates | policy-test wrote and read a registry entry against a live server. Nothing has resolved an alias outside the suite |
| 4.5 | Per-tool permissions | **verified, both axes** | namespace axis Phase 1; sensitivity axis policy-test, 20 PASS | Namespace grants per client, each now carrying a sensitivity ceiling, applied inside the query and checked again on every row on the way out | Nothing. `scripts/policy-test.sh` is the check the PRD asks for and it reached 20 PASS |
| 4.6 | Three sensitivity levels | **verified** | policy-test, 20 PASS | `sensitivity` and namespace defaults (004), envelope encryption and `kek_state` (008), `KEK_PROVIDER` with `none` refusing rather than storing plaintext, the credential tripwire, `sealed_item` for content the server cannot read | KEK escrow, still an open question in [0004](docs/decisions/0004-kek-provider.md). The KEK round trip and the sealed path both ran |
| 4.7 | Corrections that stick | **verified** | correction-test, 13 PASS | `superseded_by` and `superseded_at` (005), both halves of a correction written in one transaction, a partial index over live rows, conflict candidates returned on write, ageing and a review queue | Calibration. 0.97 is still a guess; only the lower similarity band has a measurement behind it |
| 4.8 | Full inspection and export | **partly verified** | search and bootstrap Phase 1; delete and export not yet | `memory_forget` and `lumberroom forget` with a dry run, `lumberroom export --obsidian` writing tombstones and never unlinking, `lumberroom review`, `lumberroom stats` with per-client rates and staleness, `pg_dump` backups encrypted to an `age` recipient | A run of the delete and export paths against real data |

The two structural gaps this table flagged as belonging to no phase are both addressed in code.

**The delete path exists.** `memory_forget` deletes one memory by id, refuses without `mayDelete` on
the grant, and stays out of `tools/list` for a client that does not hold it, so a model cannot see
the tool it may not call. `lumberroom forget` covers the same ground from the CLI, by id or by query, with
a dry run that changes nothing. Deleting a private row takes its wrapped key with it. None of this
has been run against a store holding anything real.

**Registry keys are canonical.** The scheme is enforced on write, non-canonical keys are refused,
and a refused guess is recorded in `registry_alias` (migration 006) so the same wrong name resolves
to the right row the next time a tool asks. Aliases resolve one hop, an exact key beats a redirect,
and a model's rejected guess cannot overwrite a mapping the owner wrote by hand.

---

## Phase 1. One tool, working end to end

> **Ends when:** you tell Claude Code a fact on Monday and a fresh session on Wednesday recalls it
> without prompting.

**Status: met, verified in Rust, not deployed.**

The done-when test passes with the real client: one session states a fact and the model writes it
unprompted, a separate fresh session recovers it through the SessionStart hook without the fact
being mentioned. Evidence and the exact transcript are in [VERIFY.md](VERIFY.md). Decisions and
the departures from the Phase 1 PRD are in [DECISIONS.md](DECISIONS.md).

Shipped: Postgres 16 with pgvector, MCP over Streamable HTTP, four tools, bearer-token auth with
namespace grants, per-call instrumentation, a recall monitor, a deploy kit, a client wiring script,
and 70 tests.

**Rewritten in Rust.** Decided 19 August 2026, before Phase 2 rather than after, because the
codebase is 3,092 lines today and Phases 2 to 4 multiply it. The reasons are compile-time SQL
verification with `sqlx`, the ability to zeroize and lock key material that Phase 3 will hold, and a
supply chain of six direct dependencies rather than 185 npm packages. Explicitly **not** for
weight: the Node service used 4.6% of the box, and the one real size number turned out to be
accidental, 276MB of unused platform binaries removed without changing language. Full reasoning,
costs accepted and the reversal condition: [decision 0001](docs/decisions/0001-rust-rewrite.md).

**The Rust build has reproduced [VERIFY.md](VERIFY.md)**: 70 tests, the done-when test with the
real Claude Code client, the recall monitor, the grant assertions and the injection payloads
treated as data. The release image has since been booted and driven through all four tools. Phase 1
is done again, in Rust.

**One number in [VERIFY.md](VERIFY.md) does not mean what it reads as.** The recall monitor compares
an approximate HNSW scan against an exact one, and its exact arm never worked: it set
`enable_indexscan = off` on a pooled connection with no transaction open, which Postgres answers
with a warning and no effect, so the exact scan went through the index as well. Every recall figure
the monitor produced is HNSW compared against itself and could not have caught a truncation failure.
The statement is now wrapped in a transaction and the monitor needs re-running before any of those
numbers can be quoted again. This does not touch the HNSW truncation finding itself, which was
reproduced directly with a filtered query returning zero rows against a 40,000-row corpus rather
than through the monitor.

**Carried forward as debt, and what has since happened to it:**

- Auth was bearer tokens, not Logto. OIDC mode is built and has never run against a live Logto
  tenant, and that is still true. It stopped being optional at the first browser surface, which is
  what [decision 0002](docs/decisions/0002-built-in-oauth-server.md) answers: lumberroom now issues its
  own tokens under `AUTH_MODE=oauth`, and Logto is no longer on the path.
- No sensitivity column. **Closed.** Migration `20260819000004_sensitivity.sql` adds it to `memory`
  and `registry` with a default of `open`, on a store where every row was open, which is the cheap
  moment the note was written to protect.
- "Whether any repeat got prevented" (Phase 1 PRD §7) is not measurable from `tool_calls` alone.
  Nothing records the question a tool did not have to ask. Still open. See Measurement below.

---

## Phase 2. Every surface connected

> **Ends when:** a fact you tell ChatGPT shows up in Claude Code the next day, and you notice you
> did not repeat yourself.

**Status: the server side is implemented and unverified. No surface is connected.**

Spec: [`docs/specs/phase-2-surfaces.md`](docs/specs/phase-2-surfaces.md).

Surfaces: Claude.ai web and mobile, ChatGPT web and mobile, OpenWebUI, the second Claude Code
install, Hermes, Cowork. Each gets its own grant and its own client identity, because a grant that
cannot tell two clients apart is not a grant.

The work divides into four pieces:

1. **Deploy.** Public HTTPS, real certificate. `deploy/install.sh` provisions the box, writes the
   compose environment, fronts the server with Caddy and installs an encrypted backup cron. It has
   never been run end to end on the target, and the one step the author could not observe from a Mac
   is the one that decides whether `KEK_PROVIDER=file` boots at all: Docker Desktop remaps a bind
   mount's ownership, so a KEK file chowned to the container's uid on a Linux host has been reasoned
   about rather than watched. Nothing else in this phase is testable until this lands.
2. **Auth escalation.** Logto is off the path.
   [Decision 0002](docs/decisions/0002-built-in-oauth-server.md) builds the authorization server
   into lumberroom: `AUTH_MODE=oauth` serves RFC 8414 discovery and RFC 7591 registration, takes the
   owner's password at a consent screen, and issues opaque access tokens stored as hashes.
   Registration is deliberately not authorization, so a self-registered client holds an empty grant
   until the owner consents. The wire protocol is proved by
   [`scripts/oauth-flow-test.sh`](scripts/oauth-flow-test.sh), which has not been run against a live
   server, and that script does not prove what Claude.ai's or ChatGPT's own client code does. Claude
   Code's fallback probing masks the whole class of bug that only the browser surfaces expose.
3. **Per-client identity and grants.** An OAuth client's grant is a row in `oauth_client` that takes
   effect on the next request; a static bearer client's grant stays in `AUTH_TOKENS`, and neither
   authority writes into the other. The reasoning and the cost of two places to look:
   [decision 0003](docs/decisions/0003-grants-in-the-database.md). `/admin/whoami` answers "what may
   this credential see" from the code that enforces it, which is the mitigation.
4. **Canonical registry keys.** Landed. The scheme is enforced on write and `registry_alias`
   (migration 006) catches a rejected guess so it resolves next time.

**What is left in this phase is the part code cannot do.** Connecting a surface needs the owner's
accounts and a box on the public internet: registering the connector, walking the consent screen in
a real browser, and reading back what each client reports itself as. One check from the spec is the
owner's, ten minutes, and undone. A second one that used to sit next to it is retired:

- Log into a personal ChatGPT Plus or Pro account, Developer Mode, and add a custom connector with a
  plain `Authorization: Bearer` header against a throwaway endpoint. With OAuth built and available
  regardless, this no longer settles whether ChatGPT can be connected at all; it settles whether
  ChatGPT can be connected with the simpler credential instead of going through the authorization
  server. The tier question and the write-capability question still come free with it. Nobody has
  tested ChatGPT's Developer Mode connector at all, and that stays unknown regardless of which
  credential type ends up in use.
- **Retired: email `mcp-review@anthropic.com` for the `static_headers` beta.** It was worth an email
  when a grant would have let four surfaces connect with no authorization server in the path.
  [Decision 0002](docs/decisions/0002-built-in-oauth-server.md) built that authorization server into
  lumberroom instead: Claude.ai, Cowork, mobile and ChatGPT connect over OAuth with nothing to request from
  Anthropic and no external tenant to configure. The beta was an escape hatch from a cost the project
  no longer pays.

This phase is also where the PRD says to measure the thing it calls the largest unknown: whether
browser tools write back at all without a lifecycle hook. Per-client read and write rates, including
which calls were unprompted, are recorded and surfaced through `lumberroom stats --by-client`. The phase
still needs a threshold at which the fallback ladder triggers, and no client has produced a number
to set it against.

---

## Phase 3. Permissions and encryption

> **Ends when:** ChatGPT provably cannot see a fact that Claude Code can, and you have checked.

**Status: implemented and unverified. The exit check is written and has not been run.**

Spec: [`docs/specs/phase-3-policy-encryption.md`](docs/specs/phase-3-policy-encryption.md).

The sensitivity axis (`open`, `private`, `sealed`) joins the namespace axis. Every grant now carries
a ceiling per namespace, because a single ceiling cannot express "work agent sees work notes, never
personal finance" when both are `private`. The ceiling goes into the query and every returned row is
checked against it a second time on the way out.

`private` content is encrypted with a per-row key wrapped under a KEK, and the KEK comes from a
provider: `none`, `file` or `env`. The default is `none`, which refuses a private write rather than
storing private content in plaintext under a label that says otherwise.
[Decision 0004](docs/decisions/0004-kek-provider.md) states what each provider actually defends,
which is a stolen dump and a leaked backup, and what none of them defend, which is the running box.
It also leaves KEK escrow open, and that question has to be answered before encryption is turned on
for writes the owner cares about: a row encrypted under a key nobody can fetch again is unreadable
in the database and in every backup taken since.

[Decision 0005](docs/decisions/0005-private-drops-lexical-search.md) takes private rows out of the
lexical index, because a `tsvector` beside the ciphertext is the document with the stopwords removed
and defends nothing. Exact-phrase search over private notes is the price.

The exit criterion is something the owner runs: an assertion that a specific fact is invisible to
one client and visible to another, made against the live system rather than reasoned about. The
Phase 1 grant tests were the template. [`scripts/policy-test.sh`](scripts/policy-test.sh) is that
check, six steps against two real credentials, and it has not been run against a live server. Until
it has, no private row has been encrypted and read back anywhere.

Both of this phase's side requirements landed:

- A delete path, from `memory_forget` or `lumberroom forget`, with the same grant checks as a write plus
  an explicit `mayDelete` the owner has to grant.
- Encrypted backups. `deploy/backup.sh` is written to encrypt every dump to an `age` recipient
  whose private key lives off the box, and that is a different key from the KEK on purpose: one
  compromise should not open every historical archive. The round trip through a real `age` binary
  and back into a throwaway database has not been run.

---

## Phase 4. Quality

> **Ends when:** a correction you make once does not resurface as a contradiction later.

**Status: implemented and unverified, with one piece of it knowingly uncalibrated.**

Spec: [`docs/specs/phase-4-quality.md`](docs/specs/phase-4-quality.md).

Supersession retires the old value now. A write carrying `supersedes` lands both halves in one
transaction, so a correction can never come to rest beside the fact it corrects; retired rows leave
search by default and stay in the database, reachable with `include_superseded`. Ageing, a review
queue for stale and conflicting rows, and the Obsidian mirror are all in.

**The dedupe thresholds are guesses and should be read as guesses.** `DEDUPE_THRESHOLD` is 0.97 and
`CONFLICT_THRESHOLD` is 0.90. Both are design targets picked before any real data existed, and the
Phase 4 spec's calibration procedure needs a few hundred real rows and a person reading the pairs
above 0.85. That work cannot start until the store holds real content, which makes it the first
thing to do after the deploy rather than something to leave until the numbers misbehave. What makes
0.97 survivable in the meantime is the guard: a collapse is refused when the two texts disagree
about a digit, an identifier or a negation, and refused across two sensitivity levels. The guard's
own boundaries are guesses too, and it is English-only.

`lumberroom eval`, the recall check against real questions rather than a benchmark, is not built. The
recall monitor is a different thing: it measures the index against an exact scan, not the system
against the owner's questions.

The exit criterion is [`scripts/correction-test.sh`](scripts/correction-test.sh), which drives the
spec's three moments directly through `memory_write` and `memory_search`. It has not been run
against a live server.

This phase was already visible in Phase 1's behaviour. Running the done-when test four times left
four conflicting "official nicknames" in the store, and a session reading the digest correctly
refused to pick one. That is the case the exit check reproduces on purpose.

---

## Phase 5. Cut

> Multi-user hardening, only if this ever becomes a product.

Not scheduled. `tenant_id` exists on every table so this stays possible without a rewrite, which
is the entire investment Phase 1 made in it.

---

## Measurement

System PRD §8 defines success. Three of the four measures are instrumented; one is not.

| Measure | How it is read today |
|---|---|
| You stop repeating yourself | **Not instrumented, and still undecided.** See below |
| Writes keep pace with reads | `lumberroom stats --by-client`: reads, writes, the write-to-read ratio and the share of calls that were unprompted, per client per window. Implemented, never run against traffic from two credentials |
| No tool sees outside its grant | Grant assertions in the suite, plus [`scripts/policy-test.sh`](scripts/policy-test.sh) as the live check, which reached 20 PASS on 20 August 2026: one credential provably cannot see what another can, across search, the digest, the namespace inventory, the registry and a write |
| You still use it after three months | Call volume over time in `tool_calls` |

Rows two and three moved with Phases 3 and 4. Per-client rates exist in code, down to whether a
call was the model's own idea, and the live grant assertion is written rather than argued about,
which is what the PRD asked for. Running it is the step that remains.

The first measure is the one that matters and the only one without a number behind it. Nothing
records the question a tool did not need to ask. It was meant to be settled in Phase 2 and it was
not: **none of the three options below was picked, and no work went into any of them.** That goes
here so the list reads as an open question rather than as a plan.

1. **Attribution on read.** When a bootstrap or search result is actually used in an answer, count
   it. Requires the client to report back, which no client does.
2. **A manual tally.** A `lumberroom note-prevented` command run when you notice it. Honest, low effort,
   and biased by whether you remember to run it.
3. **A/B by absence.** Periodically run a session with memory disabled and count the questions it
   asks that a memory-enabled session does not. Expensive, but it is the only one that produces a
   number you can trust.

The nearest thing that exists is `lumberroom eval`, the Phase 4 recall check against the owner's own
questions. It is built now, in `services::eval` with a client command over it, and it has no fixture
to run against: nobody has written the questions. It would answer whether the store returns the
right fact, which is a different question from whether it saved the owner from asking.

**This measure is the one thing in the PRD with no path to a number.** Three of the four measures are
instrumented and two of those have been read. This one has not been designed, let alone built, and it
is the measure the whole product is for. Picking one of the three options above is a decision rather
than a task, and it is the owner's.

The PRD's failure conditions are worth restating as tripwires: everything ending up in `open`
because classifying is a chore, and recall being bad enough that the honest answer was an
extraction engine rather than a control plane.

---

## Decisions

Numbered records of choices that shape the build live in
[`docs/decisions/`](docs/decisions/), indexed at
[`docs/decisions/README.md`](docs/decisions/README.md). They exist for decisions whose reasoning
would otherwise be lost, or which would be revisited for the wrong reason. Phase 1's own log is
[DECISIONS.md](DECISIONS.md), and where a record below reverses part of it, that file says so
rather than being rewritten to agree.

| | Decision | Date |
|---|---|---|
| [0001](docs/decisions/0001-rust-rewrite.md) | Rewrite the service in Rust, before Phase 2 | 19 Aug 2026 |
| [0002](docs/decisions/0002-built-in-oauth-server.md) | Build the OAuth 2.1 authorization server into lumberroom rather than stand up Logto | 19 Aug 2026 |
| [0003](docs/decisions/0003-grants-in-the-database.md) | An OAuth client's grant is a database row; a bearer client's grant stays in `AUTH_TOKENS` | 19 Aug 2026 |
| [0004](docs/decisions/0004-kek-provider.md) | The KEK sits behind a provider, and the local providers defend less than a KMS | 19 Aug 2026 |
| [0005](docs/decisions/0005-private-drops-lexical-search.md) | Private content drops lexical search | 19 Aug 2026 |

## Research

Findings that inform these specs live in [`docs/research/`](docs/research/). The system PRD names
prior art as an unresearched risk and asks for it before Phase 2.
