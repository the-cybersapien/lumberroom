# 1. Rewrite the service in Rust

**Date:** 19 August 2026 · **Status:** accepted · **Decided by:** the owner

## Decision

Rewrite the lumberroom server in Rust, before Phase 2. Keep the schema, the migrations, the PRDs, the
phase specs, the design sprint and the research unchanged: none of them are language-bound.

## What this is not for

**It is not for weight,** and the measurements say so plainly. The Node service uses 270MB resident
on a box with 24GB, which is 4.6%. Search takes 33ms of which the embedding is 11-16ms, so the
runtime is not in the critical path. Rust's `ort` and Go's `onnxruntime_go` bind the same C++ ONNX
Runtime that Node does, so the ~280MB of runtime and weights does not move; a rewrite saves the
Node baseline of roughly 50MB, about 12%, of a process using 5% of the machine.

The image was the one real number, and it turned out not to be a language problem either.
`onnxruntime-node` ships prebuilt binaries for six platforms when we run one, and bundles a browser
build unreachable from a Node process. Removing them took the image from 763MB to 487MB with no
code change. **The weight we were carrying was accidental, not intrinsic.**

Stating this explicitly because a decision recorded for the wrong reason gets revisited for the
wrong reason.

## Why then

Three arguments, none of them about size.

**Compile-time SQL verification.** `sqlx` checks hand-written SQL against a live database schema at
compile time: column names, types, nullability. This service's value is in six hand-tuned
statements: a hybrid-search blend over pgvector, a single-round-trip JSON digest, a versioned
upsert, a percentile aggregate, and soon a recursive supersession walk. An ORM cannot express any of
them and drops each to an unchecked raw escape hatch; a query builder types the surrounding code but
not the SQL. `sqlx` is the only option that keeps the SQL and checks it. It is a better answer to
the question that started this than either candidate in `docs/research/data-layer.md`.

**Key material in Phase 3.** In a garbage-collected runtime a decrypted data key or plaintext sits
on the heap until collection, is *copied* by the collector, and can be paged to swap. It cannot be
reliably zeroized. Rust allows zeroize-on-drop and `mlock`. Phase 3 stores credential locations
under `sealed` and holds a key-encryption key in memory for the life of the process, so this stops
being a preference and becomes a property of the thing being built.

**Supply chain.** Six direct dependencies pull 185 npm packages into a box that holds personal
facts and pointers to credentials. A compiled Cargo tree with vendored, auditable dependencies and a
single static binary is a materially smaller surface, and it removes npm from the runtime entirely.

## Why now rather than later

The codebase is 3,092 lines of source and 1,240 of tests. Phase 2 adds six client integrations,
Phase 3 adds encryption and key management, Phase 4 adds supersession and an eval harness, and the
console follows. **This is the cheapest this decision will ever be**, and the cost curve is steep.

## What it costs, accepted

- **The TypeScript SDK is the reference implementation.** Protocol changes land there first; the
  Rust SDK follows. Phase 2 is precisely the phase that depends on protocol edges, CIMD, dynamic
  client registration, RFC 9728 discovery, so this is a real risk taken with open eyes. Mitigation
  is that the wire behaviour is already specified and tested, so a gap in the SDK is something to
  implement rather than to discover.
- **153 passing tests are rewritten**, not ported. Their assertions carry over; their code does not.
- **Phase 1 is re-verified rather than assumed.** The done-when test, the recall monitor, the
  policy assertions and the injection guard all have to pass again against the Rust build. A rewrite
  that cannot reproduce `VERIFY.md` has not landed.

## Stack

| | | |
|---|---|---|
| MCP | `rmcp` 3.x | Official SDK. Streamable HTTP server transport, mounted in axum |
| HTTP | `axum` 0.8 | The transport mounts as a service; the admin surface sits beside it |
| Database | `sqlx` 0.9 | Compile-time checked SQL. The reason for the rewrite |
| Vectors | `pgvector` 0.4 | sqlx integration for the `vector` type |
| Embeddings | `fastembed` | bge-base-en-v1.5 q8, the model already chosen and measured on arm64 |
| JWT | `jsonwebtoken` 11 | For `AUTH_MODE=oidc` |

## What carries over untouched

The Postgres schema and all three migrations, including the HNSW recall settings that were the most
valuable finding so far. The PRDs, the roadmap, every phase spec, both design sprint documents, and
all five research documents. The deploy kit's shape: compose, Caddy, the installer, backups. The
`lumberroom` CLI is dependency-free JavaScript that talks HTTP, so it keeps working against a Rust server
unchanged, which also makes it a useful cross-check during the port.

## Reversal condition

If `rmcp` cannot serve a protocol edge that a Phase 2 surface requires, and implementing it in the
SDK is not tractable, the fallback is to keep the Rust service and put the TypeScript MCP layer in
front of it, and not to unwind the rewrite. The database, the schema and the tools would be unaffected.
