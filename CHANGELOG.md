# Changelog

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

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
  BEGIN;
  UPDATE memory           SET namespace = 'user:me' WHERE namespace = 'user:<your tenant>';
  UPDATE registry_history SET namespace = 'user:me' WHERE namespace = 'user:<your tenant>';
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

[0.1.0]: https://github.com/the-cybersapien/lumberroom/releases/tag/v0.1.0
