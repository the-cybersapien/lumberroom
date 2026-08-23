# What the spike established

Run before writing the service, against the live Postgres, on `linux/arm64` — the deploy
architecture. Every item below is a fact that would otherwise have been discovered mid-rewrite.

## Verified working

```
SPIKE embed: 768 dims                        fastembed 6, bge-base-en-v1.5 q8
SPIKE sqlx+pgvector: 1 rows stored           sqlx 0.9 + pgvector 0.4, real round trip
SPIKE mcp: streamable http service mounted   rmcp 3.1, mounted in axum
SPIKE OK
```

## Four things it caught

**1. sqlx version alignment is not optional.** The first attempt pinned `sqlx = "0.8"` while
`pgvector` resolved 0.9.0, so the tree carried both and `pgvector::Vector` implemented
`sqlx::Encode` for the *other* crate. The error reads as a missing trait impl and has nothing to do
with features:

```
error[E0277]: the trait bound `pgvector::Vector: sqlx::Encode<'_, _>` is not satisfied
```

`pgvector` accepts `>=0.8, <0.10`, so pin the workspace to whatever it resolves and check with
`cargo tree | grep sqlx` — two versions means the impls will not line up.

**2. `pgvector` has no `sqlx` feature in its manifest.** Its features are `postgres` and `halfvec`.
The sqlx integration rides on the implicit feature Cargo generates for an optional dependency, so
`features = ["sqlx", "postgres"]` is correct even though `sqlx` appears nowhere in `[features]`.

**3. The build needs `g++`, and `rust:1-slim` does not have it.** ONNX Runtime links libstdc++:

```
/usr/bin/ld: cannot find -lstdc++
```

The build stage installs `pkg-config libssl-dev ca-certificates g++`. `ort` also downloads a
prebuilt ONNX Runtime into `~/.cache/ort.pyke.io` during the build, so the build stage needs
network access — the same constraint the model prefetch already had.

**4. `TextEmbedding::embed` takes `&mut self`,** which shapes the adapter. A shared embedder needs
interior mutability, and embedding is CPU-bound at 11-16ms, so the work also has to leave the async
runtime. The adapter therefore holds `Arc<Mutex<TextEmbedding>>` and runs inside `spawn_blocking`.
Taking either half of that naively would serialise every request behind the model.

## Noted, not a problem

```
onnxruntime cpuid_info warning: Unknown CPU vendor. cpuinfo_vendor value: 0
```

ONNX Runtime does not recognise the Ampere CPU for its dispatch tables. Harmless, and it matches
the research finding that pgvector's and ONNX's CPU-specific optimisations are x86-first — see
`docs/research/pgvector-at-scale.md`.

## API shape, for reference

`rmcp` 3.1 differs from the 0.9 line the first attempt resolved:

- content is `ContentBlock::text(..)`, not `Content::text(..)`
- `ServerInfo` is `#[non_exhaustive]`, so it is built with
  `ServerInfo::new(caps).with_instructions(..)` rather than a struct literal
- the axum router needs its state type pinned: `let router: axum::Router = Router::new().nest_service("/mcp", service)`
- features: `server`, `macros`, `transport-streamable-http-server`
