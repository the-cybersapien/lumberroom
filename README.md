# Lumberroom

One memory that every AI tool you use can read from and write to, with you deciding what each one
is allowed to see.

You run seven AI surfaces. Each keeps its own memory. None of them share. You re-explain your setup
to ChatGPT, then again to Claude, then again to your coding agent, and when you correct one of them
the others never find out. Lumberroom holds the facts once, on infrastructure you own, and every tool
reads from the same place.

Three layers, doing three different jobs:

- **Memory remembers.** Fuzzy semantic recall. Commodity, kept deliberately thin and swappable.
- **Registry knows.** Exact structured facts with a canonical key and provenance. A memory holds
  "Dana mentioned her box runs Ubuntu"; the registry holds `machines.desktop.os = Ubuntu 26.04`,
  confirmed by you, on a date, superseding an older value.
- **Policy decides.** Which tool sees which facts. The layer that makes it safe to store anything
  worth storing.

The value is in the second and third. Full thesis: [`docs/prd/system-prd.md`](docs/prd/system-prd.md).

## Status

**Phases 1 to 4 are verified. Nothing is deployed.**

Phase 1 was exercised end to end in Rust and the release image was booted and driven through its
tools. Phases 2, 3 and 4 brought the built-in OAuth 2.1 authorization server, the sensitivity axis
with envelope encryption, supersession, the delete path, the review queue and the Obsidian export,
and three scripts drove all of it against a live server. [VERIFY.md](VERIFY.md) carries what they
printed:

| Script | What it drives | Result |
|---|---|---|
| [`scripts/oauth-flow-test.sh`](scripts/oauth-flow-test.sh) | 13 steps: metadata, the 401 challenge, registration, consent, code exchange, PKCE and replay refusals, a real tool call, refresh rotation | 43 PASS, 0 FAIL |
| [`scripts/policy-test.sh`](scripts/policy-test.sh) | one credential provably cannot see a fact another can | 20 PASS, 0 FAIL |
| [`scripts/correction-test.sh`](scripts/correction-test.sh) | a correction made once does not resurface as a contradiction | 13 PASS, 0 FAIL |

Every one of those runs was a local Docker stack on one machine. No VM runs any of this, and nothing
has left that machine.

The phases past the gates:

- **Phase 5, multi-user hardening.** Not scheduled. `tenant_id` sits on every table so it stays
  possible without a rewrite.
- **Phase 6, ingestion.** Ran end to end: a week of Claude Code and Codex transcripts through
  `plan`, `extract` and `submit`, queueing 222 proposals from 9,211 entries.
- **Phase 7, valid time.** Shipped. A memory carries `occurred_at` and `occurred_until` beside
  `created_at`, aliases collapse a renamed subject across namespaces, and the registry keeps what it
  replaces.
- **Phase 8, cleanup.** Both halves have run: the deterministic pass through
  `scripts/cleanup-test.sh`, the model pass against z.ai over the real store. The schedule now lives
  in the product, the deterministic pass on a timer in the server and the model pass as a compose
  service. The console at `/console` is where you decide what the queue proposes.

Read [ROADMAP.md](ROADMAP.md) for where each phase stands and [VERIFY.md](VERIFY.md) for what has
actually been run and what it printed.

One correction that reaches back into Phase 1. The recall monitor's "exact" ground-truth scan ran
`SET LOCAL enable_indexscan = off` outside a transaction, which Postgres answers with a warning and
no effect, so every comparison was the HNSW index against itself. The statement now runs inside a
transaction, and the monitor was re-run on 21 August 2026: mean recall@10 between 0.981 and 0.988
across five runs on 40,001 rows, with no true nearest neighbour missed in 1,900 probes.
[`docs/benchmarks.md`](docs/benchmarks.md) is the one page carrying every retrieval figure with the
run that produced it. The HNSW truncation finding is unaffected: that one was reproduced directly,
with a filtered query returning zero rows against a 40k-row corpus.

```
Claude Code, Hermes, OpenWebUI, lumberroom CLI ──── static bearer token ────┐
                                                                      │
Claude.ai, Cowork, mobile, ChatGPT ── OAuth 2.1 + PKCE ───────────────┤
                                                                      ▼
                             Caddy (auto-TLS) ──▶ lumberroom (Rust: axum + rmcp)
                                                        │
                                                        ▼
                                              Postgres 16 + pgvector (127.0.0.1)
```

Auth is a chain rather than a mode. Static tokens from `AUTH_TOKENS` are honoured whatever
`AUTH_MODE` says, because the CLI and the hooks must keep working on the day OAuth is switched on.
`AUTH_MODE=oauth` adds the built-in authorization server on top ([decision
0002](docs/decisions/0002-built-in-oauth-server.md)). `AUTH_MODE=oidc` adds an external issuer such
as Logto instead, and lumberroom validates its JWTs without ever issuing one.

Rust, with `sqlx` running every statement through `query`/`query_as` and `.bind()` and `fastembed`
running bge-base on the box. The compile-time macros stay unused on purpose, so a query built with
`format!` fails to type-check rather than reaching the database. Why, and what it is explicitly not
for: [decision 0001](docs/decisions/0001-rust-rewrite.md).

---

## Deploy it

Two paths. Pick by whether a browser has to reach the server.

**Local, one command, no ceremony.** Token mode on loopback. This is what a Mac with Claude Code
needs and nothing else:

```bash
git clone https://github.com/the-cybersapien/lumberroom.git && cd lumberroom
sudo ./deploy/install.sh
```

No `--domain` means no TLS and no public listener: the server binds `127.0.0.1:8787` and you reach
it over an SSH tunnel or from the same box. The installer generates the secrets, builds the image
with the embedding weights baked in, starts Postgres and the server, polls `/readyz`, and prints the
client token with the exact command to run on your Mac.

The grant it writes for that client is the owner's own: every namespace up to `sealed`,
sealed-capable, registry write on, and `mayDelete` off, so `memory_forget` never reaches a model's
tool list. That flag gates `lumberroom forget` too, since the CLI and a tool call differ only by a header
the caller chooses, so a fresh install deletes nothing until the owner turns it on or issues a
second token for the CLI. `.env.example` spells out both edits. Among the secrets is
`secrets/lumberroom-kek`, the key every `private` row is wrapped under; back it up somewhere this box does
not hold, because losing it makes those rows unreadable in the database and in every backup.

**Production, for the surfaces that need OAuth.** Any Linux VM with Docker, arm64 or amd64:

```bash
sudo ./deploy/install.sh --domain lumberroom.example.com --email you@example.com --auth-mode oauth
```

`--auth-mode oauth` requires `--domain`, because the server refuses to boot without an argon2id
owner password hash in `OWNER_PASSWORD_HASH`, a cookie secret of at least 32 characters in
`OAUTH_COOKIE_SECRET`, and a `PUBLIC_URL` that is `https://` or a loopback address. The installer
prompts for the password, hashes it inside a throwaway container, and writes both secrets into
`.env`. Per-surface connection steps, the consent screen, the grant profiles and revocation:
[deploy/oauth.md](deploy/oauth.md).

The build needs outbound HTTPS to huggingface.co once, to bake the embedding weights into the image.
After that the server needs no outbound access.

Wiring a Mac is the same in both paths:

```bash
./client/wire-mac.sh --url https://lumberroom.example.com --token <token>
```

In the loopback path the URL is whatever your SSH tunnel exposes, `http://127.0.0.1:8787` by
default.

That registers the MCP server with Claude Code, installs the SessionStart hook that pulls the digest
at the start of every session, and appends the write rule to `~/.claude/CLAUDE.md`. It backs up
every file it touches and takes `--dry-run`. In OAuth mode it takes `--oauth-mode` and omits the
header, leaving Claude Code to run its own authorization flow on first use.

Then prove the loop:

```bash
LUMBERROOM_URL=https://lumberroom.example.com LUMBERROOM_TOKEN=<token> ./scripts/done-when-test.sh
```

Full runbook, including the Oracle Always Free specifics and the key-encryption key:
[DEPLOY.md](DEPLOY.md).

---

## The ten tools

Signatures are additive. Later phases extended them; nothing was renamed.

Every tool sits behind one grant, and `src/mcp/capability.rs` holds the single table that decides
which. `tools/list` filters on it, so a model never sees a tool its credential cannot call, and the
service checks the grant again on the call.

**Open. Every authenticated client, with namespace and sensitivity still applied inside the call.**

| Tool | What it does |
|---|---|
| `context_bootstrap(project?)` | One call, one round trip. User profile, active project context, recent writes, registry summary, sealed inventory, rendered as markdown. Served from a 30s cache. |
| `memory_search(query, namespaces?, limit?, project?, include_superseded?)` | Cosine search over pgvector blended with a lexical match. Defaults to `user:me` + `global` + the active project, then other projects at a score penalty. Retired facts are excluded unless `include_superseded` asks for them. |
| `memory_write(content, namespace, tags?, supersedes?, sensitivity?)` | Embeds on write. No LLM in the write path. Restatements collapse; corrections do not. `sensitivity` raises a write above the namespace default and can never lower it. Returns `possible_conflicts`. |
| `registry_get(kind, key, namespace?, project?)` | Exact lookup of a host, service, credential location, model route, or dataset. Project overrides beat global defaults, and an alias resolves a wrong guess to the canonical key. |
| `alias_list(namespace?)` | The pairs of names recorded as meaning the same subject. Namespaces the credential cannot read are absent, because a list of names is a disclosure no content filter sees. |

**`mayDelete`.**

| Tool | What it does |
|---|---|
| `memory_forget(id, reason?, dry_run?)` | Deletes one memory permanently, and takes its wrapped key with it. |

**`mayReadHistory`. A retired fact can be more revealing than the one that replaced it, which is why this is its own grant rather than a rider on read access.**

| Tool | What it does |
|---|---|
| `memory_history(id, namespace?)` | Every version of one fact, oldest first, retired versions included. Versions the credential may not read are counted in `withheld` rather than shown. |
| `registry_history(kind, key, namespace?, limit?)` | What a registry key used to hold, newest first, without the value it holds now. A key reached through a redirect answers here and names what it resolved from. |

**`registryWrite`. The registry holds credential locations, and a name that steers every later search is the same class of fact, so both writes are operator actions.**

| Tool | What it does |
|---|---|
| `registry_set(namespace, kind, key, value)` | Records an exact operational value under a canonical dotted key. A rejected key is remembered as a redirect so the next caller reaching for the same wrong name lands on the right row. |
| `alias_set(namespace, alias, canonical, since?, until?, origin?)` | Records that two names mean one subject, so a search for either finds the facts written under the other. Renames are the case it exists for. |

`possible_conflicts` is the mechanism that makes a correction stick. When a write lands close to an
existing fact but not close enough to collapse into it, the server returns the neighbours it
refused to merge, and the tool description tells the model to call `memory_write` again with
`supersedes` pointing at the one it just replaced. The store cannot tell a correction from a
restatement; the model in the conversation is the only party that can.

### Namespaces and sensitivity

- `user:me` facts about you and how you work
- `project:<slug>` facts scoped to one codebase
- `global` facts true everywhere: infrastructure, conventions, credential locations
- `personal:*` and `credentials:*` classify above `open` by default

A grant is now a namespace glob paired with a sensitivity ceiling, on each of the read and write
axes. `{"namespace": "project:*", "max": "private"}` grants every project namespace up to
`private`. A bare string is still a valid grant and deserialises to a ceiling of `open`, which is
what kept every Phase 1 grant valid when the axis landed: a grant written before sensitivity
existed gains no reach over content that did not exist when it was written. Two
patterns matching one namespace resolve to the more generous ceiling, because being granted both is
being granted both.

The three levels are three different mechanisms:

- **`open`** is stored in the clear, indexed lexically and semantically, and searchable by anyone
  whose grant reaches the namespace.
- **`private`** is encrypted at rest with a per-row key wrapped under a key-encryption key. It
  drops out of the lexical index, so exact-phrase search does not reach it ([decision
  0005](docs/decisions/0005-private-drops-lexical-search.md)).
- **`sealed`** is encrypted by the client before it arrives. The server stores bytes it holds no key
  for and cannot read under any circumstance, and it is not searchable by any means.

**What `private` does not protect, stated here rather than buried in a research file.** The
embedding stays plaintext, because there is no ANN index over ciphertext and an unsearchable level
would push everything worth protecting back into `open`. Published inversion work recovers most of
a short text from its embedding: Morris et al. report 92% exact recovery of 32-token inputs with
black-box query access, a 2025 reproduction confirms it, and later work drops the need for the
victim's model or a paired corpus. Those are figures from the literature, not measurements on this
system. The claim lumberroom makes is narrow and it is the one to hold it to: `private` protects the
verbatim text of a row from whoever holds the database, and leaks its gist. It protects nothing at
all against the live server, which decrypts to answer a search. Evidence and citations:
[`docs/research/encryption-and-sensitivity.md`](docs/research/encryption-and-sensitivity.md) §1.

Seeded namespace defaults live in `sensitivity_default` (migration
`20260819000004_sensitivity.sql`): everything is `open` except `personal:finance` and
`personal:health`, which are `private`, and `credentials:*`, which is `sealed`. Editing that table
is a twice-a-year job. Writing there needs a key: `.env.example` and `install.sh` ship
`KEK_PROVIDER=file` and provision `secrets/lumberroom-kek`, since a namespace that classifies `private`
with no key configured is a namespace nothing can be written to. `KEK_PROVIDER=none` stays available
and stays a refusal rather than a downgrade, which is the part that does not move ([decision
0004](docs/decisions/0004-kek-provider.md)).

Where a grant lives depends on the credential. A static bearer client's grant stays in
`AUTH_TOKENS`; an OAuth client's grant is a row in `oauth_client` that the consent screen writes and
that changes without a restart. Neither authority copies into the other, which is the whole point:
[decision 0003](docs/decisions/0003-grants-in-the-database.md).

---

## Operating it

`bin/lumberroom.mjs` is dependency-free Node and its usage block is the authoritative list. What it
carries today:

```bash
lumberroom doctor                       # connectivity, auth, readiness, tool list
lumberroom login                        # OAuth 2.1 + PKCE through a loopback listener
lumberroom clients                      # registered OAuth clients, how each one registered, consent state
lumberroom bootstrap                    # the digest as markdown
lumberroom search "how do we deploy"
lumberroom write "..." --namespace user:me --tags preference
lumberroom forget <id> [--dry-run]      # or --query "..." for the near-duplicates of a phrase
lumberroom review [--stale] [--conflicts] [--registry]
lumberroom supersede <old-id> <new-id>
lumberroom registry get|set|alias ...
lumberroom stats [--hours 168] [--by-client]
lumberroom export --obsidian ~/vault
lumberroom eval [--fixture client/eval-fixture.example.jsonl]
lumberroom seal <key> --namespace credentials:aws   # client-side AES-256-GCM, then lumberroom unseal
lumberroom recall                       # the HNSW recall monitor
lumberroom tools
```

Three subcommands live in the Rust binary instead, because argon2 and CSPRNG bytes are not things a
shell script should improvise:

```bash
docker compose run --rm -T server lumberroom-server hash-password   # stdin in, one argon2id PHC string out
docker compose run --rm -T server lumberroom-server generate-kek    # a fresh key-encryption key, hex, on stdout
docker compose exec -T server lumberroom-server verify-kek          # does the configured key match this store
```

`lumberroom hash-password` in the Node CLI prints that docker invocation rather than hashing anything
itself. Revoking an OAuth client has no subcommand on either side yet: `lumberroom clients` lists them,
and revocation is one `UPDATE` in psql. [DEPLOY.md](DEPLOY.md) carries the statement.

`lumberroom stats` answers the question that matters: how often does a model call these tools on its own?
Every tool call writes a `tool_calls` row, including a refused one, and `unprompted` separates the
model deciding from the hook or you forcing it. Calls arriving without an `X-Memory-Invocation`
header count as model-initiated; `lumberroom` and the SessionStart hook always send one. `--by-client`
splits the same window per credential and adds the write-to-read ratio. Example output:

```
window: last 1h
totals: 34 calls, 0 failed, unprompted 9 (0.265)
  memory_search        14 calls     4 unprompted  p50 44ms  p95 238ms  [claude-code-mac]
  context_bootstrap    12 calls     1 unprompted  p50  4ms  p95  30ms  [claude-code-mac]
  memory_write          7 calls     4 unprompted  p50 184ms p95 197ms  [claude-code-mac]
```

Those latencies are Phase 1 observations on an all-open store. A store holding private rows pays a
ciphertext round trip on any read that returns one and a sealed-count query for a client whose
ceiling reaches `sealed`, and neither has been measured.

Health endpoints: `/healthz` needs no credentials, `/readyz` reports the embedder and checks that
the schema dimension matches the configured one, `/statsz` needs a token. `/admin/whoami` answers
what the credential you present resolves to, from the code path that enforces it, which is the fast
way to settle an argument about a grant.

---

## Development

```bash
cp .env.example .env                     # then set POSTGRES_PASSWORD
docker compose up -d db                  # Postgres 16 + pgvector on 127.0.0.1:5432

# The build needs g++ (ONNX Runtime links libstdc++) and OpenSSL headers. This image has both.
docker build -t lumberroom-builder -f Dockerfile.builder .

# -j 1 is not optional: linking the lib-test and integration binaries at the same time gets the
# linker OOM-killed in the container, and it reads as a compile error rather than a memory limit.
./scripts/cargo.sh test -j 1
./scripts/cargo.sh test -j 1 -p lumberroom
```

The test suite needs no `AUTH_TOKENS` in `.env`: each test sets the credentials it needs in
process. Fill that variable in when you want to run the server rather than the suite. The
first image build downloads the 209MB of bge-base-en-v1.5 weights from huggingface.co, into a
BuildKit cache mount that later builds copy from, and the release image carries them at `/models`,
which is where `MODEL_CACHE_DIR` points.

Starting the server itself needs the key-encryption key in place first, since `.env.example` ships
`KEK_PROVIDER=file`: the three commands are in that file under `KEK_PROVIDER`, or run
`./deploy/install.sh` and let it do them. The test suite needs none of it: it sets its own fixed
key in the environment and truncates `kek_state` with the rows it describes.

`scripts/cargo.sh` runs cargo inside that builder image, joined to the compose network so the
integration suite can reach the database. It uses its own `lumberroom_rust_test` database and the hash
embedder, so it downloads nothing. It skips rather than fails when no database is
reachable, and the tests serialise themselves on a Postgres advisory lock because each one
truncates that database. The lock crosses processes, which an in-process mutex does not, and six
test binaries are six processes. The last observed count was 774 in the server crate and 305 in the
client, 0 failures, on 22 August 2026. It moves with every phase, so run the suite and read the last
line rather than trusting that figure.

---

## What is not built yet

Six of the seven surfaces are unconnected, which is the largest gap and the one everything else
waits on. Beyond that:

- **0.97 is still a guess.** Only the lower similarity band has a measurement behind it, the one
  that moved it from 0.85 to 0.65 ([decision 0011](docs/decisions/0011-cleanup-proposes.md)).
  `DEDUPE_THRESHOLD` was picked rather than calibrated, and the procedure in
  [`docs/specs/phase-4-quality.md`](docs/specs/phase-4-quality.md) §2 is what would settle it. A
  numeric guard is what makes being wrong about 0.97 survivable.
- **Sealed items have no bulk listing.** `lumberroom seal` and `lumberroom unseal` work one key at a time.
  Nothing enumerates what is stored, so there is no way to answer "what have I sealed" short of SQL.
- **The cleanup queue has no `unreject`.** A cluster rejected because the code that proposed it was
  wrong blocks its own replacement, and the row has to be deleted by hand.
- **RFC 8707 audience binding is not enforced on the opaque token path.** The token endpoint records
  the resource a client asked for, and validation reads it without comparing it. A token minted for
  this server is accepted by this server, which is the only deployment there is, but the check the
  RFC asks for is absent.
- **Client registration has no rate limit.** Dynamic registration is open by design and a
  registered client holds nothing until the owner consents, so the cost of abuse is rows in
  `oauth_client` that nothing purges.
- **A grant change leaves no audit row.** Consent overwrites the profile in place, so there is no
  record that a client used to be `narrow` and is now `full`.
- **KEK rotation and escrow.** `KEK_ID` is written on every row so a rotation is distinguishable
  from data loss, and nothing rewraps. Escrow is an open question the owner has not answered, and
  losing the key makes every private row unreadable, backups included.

Phases, exit criteria and the current gap analysis: [ROADMAP.md](ROADMAP.md).

---

## Layout

```
docs/               PRDs, phase specs, decisions, research, design
src/domain/         types, errors, namespace rules, policy, canonical keys, tripwire. No I/O
src/ports/          one file per port: memory, registry, tool_calls, sealed, oauth, embedder
src/services/       bootstrap, search, write, forget, review, export, registry, recall
src/crypto/         the KEK provider trait and the per-row envelope
src/authserver/     the built-in OAuth 2.1 server: routes, consent pages, sessions, login limiter
src/adapters/       postgres (the only module with SQL), embedding, auth
src/mcp/            tool registration and descriptions; the tool list is per credential
src/http/           axum routes, the admin surface, the well-known documents; MCP mounts at /mcp
src/console/        the console at /console: OAuth clients, aliases, the cleanup queue
migrations/         SQL, applied at boot by sqlx
tests/              integration suite, against a real database
crates/lumberroom/    the Rust client: transcript ingestion and the cleanup daemon
bin/lumberroom.mjs        dependency-free CLI and hook client
client/             wire-mac.sh, the SessionStart hook, the OpenWebUI filter, the eval fixture
deploy/             install.sh, Caddyfile, backup.sh, oauth.md, Oracle and Logto notes
scripts/            cargo.sh, lumberroom.sh, the acceptance gates and the eval harness
```

Two clients, and they reach you differently. `bin/lumberroom.mjs` runs anywhere node does and needs
nothing installed: `client/wire-mac.sh` installs it to `~/.local/bin/lumberroom`, and that is the `lumberroom`
on your PATH. The Rust client in
`crates/lumberroom` is built for Linux inside the builder image and never lands on the host PATH at
all: `./scripts/lumberroom.sh` mounts the release binary into a container, mounts your transcript
directories at their real host paths, and reads the ingest credential out of `AUTH_TOKENS` so it
stays out of your shell history. A cross-platform release of that binary is still owed.

One rule carries the architecture: **domain and services never import from adapters.** A service
asks a `MemoryRepository` for rows and does not know Postgres exists, which is what makes a second
storage implementation possible. See [docs/architecture.md](docs/architecture.md).

## Documentation

| | |
|---|---|
| [ROADMAP.md](ROADMAP.md) | Start here: the phases, what stands where, current gaps against the system PRD |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Building, testing, the acceptance gates, and what a pull request needs |
| [docs/traps.md](docs/traps.md) | Findings that cost real time, and what to do instead |
| [SECURITY.md](SECURITY.md) | Supported versions and how to report a vulnerability |
| [DEPLOY.md](DEPLOY.md) | Runbook: the two deploy paths, the KEK, backups, troubleshooting |
| [deploy/oauth.md](deploy/oauth.md) | The OAuth production path, per surface, with the consent screen |
| [DECISIONS.md](DECISIONS.md) | Phase 1 decisions, the measurements behind them, departures from the PRD |
| [VERIFY.md](VERIFY.md) | What was actually run, and what it printed |
| [docs/prd/](docs/prd/) | The system PRD and the Phase 1 PRD |
| [docs/specs/](docs/specs/) | Phase specifications: surfaces, policy, quality, ingestion, valid time |
| [docs/decisions/](docs/decisions/) | Numbered records of choices that shape the build |
| [docs/research/](docs/research/) | Findings the specs are built on |
| [docs/design/](docs/design/) | The console: information architecture, spec, and the design system it ships with |
| [docs/benchmarks.md](docs/benchmarks.md) | Every retrieval figure, with the run that produced it |
| [docs/permissions.md](docs/permissions.md) | Every grant field, and which tools each capability opens |

## Granting a client

`AUTH_TOKENS` decides what each client may read, write and do.
[`docs/permissions.md`](docs/permissions.md) covers every field, the two axes a namespace grant
carries, which tools each capability opens, and the one asymmetry that catches people: an
unrestricted `read` implies `sealedCapable` and never implies `mayDelete`, `mayIngest` or
`mayReadHistory`.

## Keeping the store clean

A periodic pass proposes duplicates, contradictions and stale rows into a queue you decide. It never
retires a row on its own. [`docs/cleanup-schedule.md`](docs/cleanup-schedule.md) covers the two
cadences and how to install them; [decision 0011](docs/decisions/0011-cleanup-proposes.md) covers
why it proposes rather than acts.

## Licence

Apache-2.0. The full text is in [LICENSE](LICENSE), and
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) lists the third-party components this project
depends on and the terms they come under.
