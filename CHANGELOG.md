# Changelog

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.3.0] - 2026-08-31

One change since 0.2.0. A whole store leaves as one file and goes into another install.

### Added

- **`lumberroom archive export <path>` and `lumberroom archive import <path>`**, every memory,
  registry entry, alias and sealed item in one file. A `.lumber` file is `.jsonl.gz.age` under an
  alias, so anyone holding the passphrase reads one without this binary:

  ```bash
  age -d store.lumber | gunzip | jq .
  ```

  The extension is an alias rather than a container of its own, because an export of a person's
  memory should never need our binary to open it.

- **Two import modes over one service.** Merge is the default and runs every row through
  `services::write::run`, so the credential tripwire, the classification floor, the sensitivity
  ceilings, the grant check and the dedupe bands all still decide. `--restore` reproduces a store
  exactly, keeps ids and timestamps, and refuses a target holding any row. `--dry-run` prints the
  counts and writes nothing, which is the safety net an archive import gets in place of the review
  queue the other import commands hold facts in.
- **A replay lands nothing twice.** Idempotence comes from the job's archive-id map rather than the
  dedupe bands: both bands sit inside `if supersedes_id.is_none()`, the exact-match path also
  requires `open`, and `collapse_target` declines a row that is not live. A second run leaning on
  them would duplicate every superseded row, every private row, and every row the first pass
  retired.
- **`ARCHIVE_MAX_DECOMPRESSED_BYTES`**, 2 GiB by default. An uploaded file inflates in this process
  before anything parses it, so the reader caps what it will inflate rather than measuring the result
  afterwards. It also refuses an inflated scrypt work factor before the key stretching runs.
- **Two admin routes**, `/admin/archive/export` and `/admin/archive/import`, behind the grant that
  already means the whole store: `*` at `sealed`. No new grant flag, so no deployment edits
  `AUTH_TOKENS` for this.

### Changed

- **No archive carries envelope ciphertext.** `seal` authenticates the row id as associated data, so
  a copied `content_ct` fails its tag check at every destination and under every key. Private
  content travels as plaintext inside the age layer and is resealed on arrival, which makes the
  passphrase the only thing between that file and whoever holds it. `sealed_item.ciphertext` is the
  one blob a copy preserves, because the client holds that key and nothing binds associated data
  to it.
- **Embeddings do not travel.** A vector from one model against documents from another returns
  confident nonsense rather than an error, so the destination embeds what it accepts and the file
  records which model wrote the original.
- **An archive import refuses to start while `SENSITIVITY_TRIPWIRE` is off.** The tripwire runs at
  `Sensitivity::Open` and nowhere else, so `private` and `sealed` rows pass through unscanned in
  both directions and the one check that covers the rest has to be on.
- **`SealedRepository` gains `list_for_archive`**, which narrows the non-enumerability the port and
  the encryption migration both state. One caller reaches it, and only after the whole-store grant
  check has passed. Decision 0015 records the cost and the condition for reversing it.

### Known limitations

- An import does not replay registry history. Registry history has no write path, so the values an
  archived key passed through stay out of reach after a restore, even though the file lists them.
- A restored alias carries a fresh `created_at`. It replays through the same call the console uses
  to create one, and that call has no field for the original. Restore is exact for memories and
  sealed items, near-exact for aliases.
- The graph's free edges are too coarse to carry the compositional query they were built for. The
  measurement is in decision 0014 and the work is unfinished.
- `conflicts` still pulls pairs before checking each one's visibility, so its limit counts rows the
  caller may not see. The per-pair check is correct; the counting is not.
- Sealed content is neither embedded nor searchable, by design.

## [0.2.0] - 2026-08-26

Thirteen changes since 0.1.0. Valid time works, the store can be asked what it believed on a date,
and four failures that produced silence rather than an error are fixed.

### Added

- **`lumberroom import`**, three subcommands. `import prompt` prints a portable prompt asking an
  assistant for its memory of you. `import claude` surveys an export directory against its manifest
  and says what is missing, and it deliberately cannot fetch: a downloads directory holds many
  archives and importing the wrong one is undone by hand. `import memory-dump` parses a saved dump
  and fills the proposal queue with `--submit`, with no model in the path and no key configured.
- **A bounded graph walk**, and a router deciding when it is worth running. It answers a join
  nearest-neighbour search cannot: a query tying a held position to a named catalyst, where neither
  phrase appears in the row that answers it. Seeded from structure rather than an extractor, so the
  free edges went first. On a replica that produced 4,737 structural edges, and decision 0014 says
  plainly that it answered and not well enough.
- **Supersession proposals**, from cardinality that is declared and never inferred. A later fact
  ends an earlier one only when the subject holds one value at a time, and the sentence never says
  whether it does. "The limit is 40k now" replaces its predecessor, "applying for b and c" replaces
  nothing, and the two are the same shape, so no model reading the text can separate them.
- **An as-of read**, with a caller and a coverage number. That number counts supersession pairs
  carrying a closed interval, because a pair whose retired half has no `occurred_until` has a link
  and no period, so no instant separates the two versions and an as-of read returns both.
- **`occurred_at` on a directly written memory**, and a backfill for the rows that lost it. Measured
  on a live store: 0 of 175 rows written by one client into one namespace carried a date, against
  100% in the namespaces ingestion fills. Ingestion calls `write::run_observed` and bypasses the
  near-now fence; `memory_write` did not, so an agent recording an event on the day it happened
  could never date it.

### Fixed

- **The personal namespace is always `user:me`, whatever `TENANT_ID` is set to.** It used to be
  `user:<TENANT_ID>`, while `lumberroom write` printed
  `--namespace is required (user:me | project:<slug> | global)` to everyone. On a store configured
  with any other tenant, a person followed that instruction, the write succeeded, and the memory
  landed in a namespace `default_read_namespaces` never asks for. It was gone with no error.

  At the default `TENANT_ID=me` nothing changes.

  **If you set `TENANT_ID` to anything else, your existing personal memories are stranded.** They are
  intact, and the bootstrap profile, registry precedence and `memory_forget`'s default set no longer
  ask for them. Search still reaches them as a penalised secondary namespace while
  `SEARCH_INCLUDE_ALL_PROJECTS` is on, which is why they can show up in a result and still be
  missing from everything else.

  Boot warns once per affected namespace, listing the row count for each table that holds rows under
  it. Eight tables key on namespace and all of them have to move, or the registry, the aliases and
  the ingest queue stay behind:

  ```sql
  -- Stop the server before running this. It moves rows a running server writes to, and nothing
  -- here holds a concurrent write off; a write that lands mid-block can put a fresh row under the
  -- old namespace behind an UPDATE that has already passed.
  --
  -- One run per stranded namespace. A store that used more than one tenant over its life has more
  -- than one, and boot names each.
  BEGIN;
  -- Drop this line and its partner below if you do not own `registry`. The archive trigger is
  -- unconditional by design, so moving a registry row files a revision that records a value nothing
  -- changed. Turning it off for the transaction keeps the archive honest. Without it the block is
  -- still correct, because `registry_history` moves last and carries those rows along.
  ALTER TABLE registry DISABLE TRIGGER registry_archive;
  UPDATE memory           SET namespace = 'user:me' WHERE namespace = 'user:<your tenant>';
  UPDATE ingest_proposal  SET namespace = 'user:me' WHERE namespace = 'user:<your tenant>';
  UPDATE cleanup_proposal SET namespace = 'user:me' WHERE namespace = 'user:<your tenant>';
  -- These four are keyed on (tenant_id, namespace, ...), so a row that already exists under
  -- `user:me` collides. Decide per key which value wins; the UPDATE below keeps the one already
  -- under `user:me` and leaves the old row in place for you to read and delete.
  UPDATE registry       SET namespace = 'user:me' WHERE namespace = 'user:<your tenant>'
    AND NOT EXISTS (SELECT 1 FROM registry r
                     WHERE r.tenant_id = registry.tenant_id AND r.namespace = 'user:me'
                       AND r.kind = registry.kind AND r.key = registry.key);
  UPDATE registry_alias SET namespace = 'user:me' WHERE namespace = 'user:<your tenant>'
    AND NOT EXISTS (SELECT 1 FROM registry_alias a
                     WHERE a.tenant_id = registry_alias.tenant_id AND a.namespace = 'user:me'
                       AND a.kind = registry_alias.kind AND a.alias_key = registry_alias.alias_key);
  UPDATE entity_alias   SET namespace = 'user:me' WHERE namespace = 'user:<your tenant>'
    AND NOT EXISTS (SELECT 1 FROM entity_alias e
                     WHERE e.tenant_id = entity_alias.tenant_id AND e.namespace = 'user:me'
                       AND e.alias = entity_alias.alias);
  UPDATE sealed_item    SET namespace = 'user:me' WHERE namespace = 'user:<your tenant>'
    AND NOT EXISTS (SELECT 1 FROM sealed_item s
                     WHERE s.tenant_id = sealed_item.tenant_id AND s.namespace = 'user:me'
                       AND s.key_hmac = sealed_item.key_hmac);
  -- Last, and this order is the point when the trigger is left on. `registry` carries an
  -- unconditional AFTER UPDATE trigger that archives the pre-update row, so moving a registry row
  -- lays down a fresh `registry_history` row under the old namespace. Moving the history first
  -- leaves that one behind and boot warns again about a migration that looked like it worked.
  UPDATE registry_history SET namespace = 'user:me' WHERE namespace = 'user:<your tenant>';
  ALTER TABLE registry ENABLE TRIGGER registry_archive;
  COMMIT;
  ```

  Re-run the server afterwards: anything the warning still names is a row a collision left behind.
  `tool_calls` also carries a namespace and is deliberately not moved, because it records what a
  client asked for at the time it asked.

  Then update any `AUTH_TOKENS` grant naming `user:<your tenant>`. Boot warns about those separately,
  including on a store with no rows yet.

  **A second person gets their own `TENANT_ID`, not their own `user:<id>`.** If `user:alice` and
  `user:bob` hold two people's facts, leave them where they are: merging them into `user:me` cannot
  be undone without a backup.

- **`user:me` is the only user namespace a write may name.** `memory_write` and the ingest queue now
  refuse `user:<anything else>` with a message that says so, instead of storing a fact nothing reads
  again. Reads stay permissive, so the console, the export and an explicit-namespace search still
  reach stranded rows while you move them. The validation error and the console's namespace hint no
  longer offer `user:<id>` as a shape, which is what led people into this in the first place.

- **The recall settings are re-applied at boot.** They are set with `ALTER DATABASE`, which stores
  them in `pg_db_role_setting`, a cluster catalog that a single-database `pg_dump` does not carry. A
  restored database came back with the migration recorded as applied and the settings gone, and
  without `hnsw.iterative_scan` a filtered search can return nothing at all rather than fewer rows.
- **A failed migration no longer wedges the next one.** sqlx holds a session-level advisory lock
  across `Migrator::run` and seven paths return before releasing it. Run on a pooled connection, a
  failure returned that connection to the pool still holding the lock, and the next attempt blocked
  forever instead of reporting the error that caused the first failure.
- **The review queue applies the reader's grant inside the query.** It filtered afterwards, which
  `src/ports/memory.rs` and `docs/permissions.md` both forbid, and the limit counted rows before
  filtering: with six unreadable stale rows older than three readable ones, a restricted caller
  asking for three got zero while nine stale rows existed.
- **A revived row is no longer stranded.** Deleting a correction cleared `superseded_by` and
  `superseded_at` and never cleared `occurred_until`, so the row returned to live search and stayed
  invisible to every as-of read, with psql the only repair.

### Changed

- **The extraction prompt puts corrections first.** A person states a preference once and corrects a
  wrong assumption at the moment it costs them something, and that span is the only place the real
  fact appears. The first live run returned 17 confident wrong facts out of 99, every one inferred
  from an assistant talking to itself.
- The README gives the client its own install section, and the Homebrew tap it describes now exists.

### Known limitations

- The graph's free edges are too coarse to carry the compositional query they were built for. The
  measurement is in decision 0014 and the work is unfinished.
- `conflicts` still pulls pairs before checking each one's visibility, so its limit counts rows the
  caller may not see. The per-pair check is correct; the counting is not.
- Sealed content is neither embedded nor searchable, by design.

## [0.1.0] - 2026-08-24

First tagged release. One Rust binary, a Postgres database with pgvector, and two clients that talk
to them.

### Added

- **An MCP server at `/mcp`**, ten tools behind four capabilities. Five sit open to every
  authenticated client: `context_bootstrap`, `memory_search`, `memory_write`, `registry_get`,
  `alias_list`. `memory_forget` needs `mayDelete`; `memory_history` and `registry_history` need
  `mayReadHistory`; `registry_set` and `alias_set` need `registryWrite`. `tools/list` filters per
  credential, so a client never sees a tool its grant does not cover, and the service checks the
  grant again on the call.
- **A registry of exact facts**, keyed by a canonical dotted string with provenance and a date, kept
  apart from fuzzy semantic memory. A memory can hold that someone mentioned their box runs Ubuntu;
  the registry holds `machines.desktop.os = Ubuntu 26.04`, confirmed and superseding whatever value
  came before it.
- **A two-axis policy model.** A grant pairs a namespace glob with a sensitivity ceiling, on each of
  the read and write axes, and two matching patterns resolve to the more generous ceiling. The
  sensitivity filter runs inside the query itself, never as a pass over results already fetched, so a
  row a client may not see never enters that client's process.
- **A built-in OAuth 2.1 authorization server** (docs/decisions/0002): RFC 8414 discovery, RFC 7591
  dynamic client registration, PKCE with S256 as the only accepted method, an owner login behind a
  consent screen, opaque access tokens stored as hashes, and refresh token rotation. Static bearer
  tokens are honoured alongside it whatever `AUTH_MODE` is set to.
- **Envelope encryption for `private` rows and client-side encryption for `sealed` rows.** The server
  holds no key for `sealed` content and cannot read it under any circumstance, at rest or in a search.
- **A console at `/console`**, gated on the owner password, covering reading, write, registry,
  aliases, the ingest queue, the cleanup queue, and what each client may reach.
- **Transcript ingestion with a review queue**, so a week of transcripts becomes a proposal list the
  owner reads rather than facts written straight into the store. Run once end to end before this
  release: 99 candidates queued from 15 chunks, 60 approved and 39 rejected, nothing written without
  a keystroke. `docs/ingestion.md` carries the counters and what the run failed to settle.
- **A cleanup pass that proposes and never retires on its own** (docs/decisions/0011). Applying a
  proposal goes through the same supersede-or-delete path the console and CLI already use, so a
  retired row leaves the history a correction leaves.
- **Two clients**: `bin/lumberroom.mjs`, a dependency-free Node CLI and hook client, and
  `crates/lumberroom`, the Rust client that runs transcript ingestion and the cleanup daemon's model
  half.

### Known limitations

Carried over from README.md's "What is not built yet", stated at the same weight rather than
softened for a release note.

- The OAuth wire protocol has a gate. The browser and mobile clients that depend on it have none,
  and this is the largest gap in the release.
- `DEDUPE_THRESHOLD` at 0.97 is a guess picked before any real data existed, not a calibrated number.
- Sealed items have no bulk listing. `lumberroom seal` and `unseal` work one key at a time, and
  nothing enumerates what is stored.
- RFC 8707 audience binding is not enforced on the opaque token path. The token endpoint records the
  resource a client asked for and validation reads it without comparing it.
- Client registration has no rate limit.
- A grant change leaves no audit row, so nothing records that a client used to hold less.
- Nothing rewraps on KEK rotation. `KEK_ID` is written on every row so a rotation is distinguishable
  from data loss, and the rewrap itself does not exist yet.
- Ingestion ranks nothing. The queue arrives flat and in arrival order, and 17 of the 99 candidates
  in the first real run were wrong when checked against the repository. Reading the queue is the
  whole defence.
- `submit` collapses exact duplicate proposals on a content hash and misses near-duplicates, so
  overlapping chunks queue the same fact more than once.

[0.3.0]: https://github.com/the-cybersapien/lumberroom/releases/tag/v0.3.0
[0.2.0]: https://github.com/the-cybersapien/lumberroom/releases/tag/v0.2.0
[0.1.0]: https://github.com/the-cybersapien/lumberroom/releases/tag/v0.1.0
