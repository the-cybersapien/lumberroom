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
  intact and unreadable. Boot now warns once per affected namespace with the row count and the fix:

  ```sql
  UPDATE memory SET namespace = 'user:me' WHERE namespace = 'user:<your tenant>';
  ```

  Then update any `AUTH_TOKENS` grant naming `user:<your tenant>`.

  A related gap this does not fix: `sensitivity_default` is seeded for tenant `me` only, so a store
  on another tenant has an empty rule set, and an empty rule set classifies everything `open` and
  says nothing about having done so. Seed rows for your tenant, or run at the default.

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
