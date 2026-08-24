# PRD: Agent Memory Control Plane, Phase 1

**Owner:** maintainer · **Date:** 2026-08-18 · **Status:** ready to build · **Scope:** Phase 1 only (walking skeleton, single-tenant, tier 0)

Related project docs: `north-star.md` (product thesis), `agentic-memory-design.md` (architecture, partly superseded), `build-decision.md` (hosting + scope decision). This PRD implements the Phase 1 slice of those decisions and is the document you build from.

---

## 1. Goal

One durable fact written by one client is retrieved by another, on infrastructure you keep, without you restating it.

**Done when:** you tell Claude Code on the Mac a fact on Monday, start a fresh session Wednesday, and `context_bootstrap` surfaces that fact without you mentioning it.

Out of scope for Phase 1: encryption, tiering, per-client ceilings, multi-tenancy, extraction LLM, `supersedes` consolidation, the browser clients. Those are Phases 2–4.

In scope: the Oracle host, OAuth, Postgres + pgvector, four tools, one wired client (Claude Code on the Mac), instrumentation.

---

## 2. Non-goals

- No multi-tenant machinery. Keep `tenant_id` as a column, hardcode one value, skip RLS.
- No encryption. Everything plaintext at rest for now. The box is the trust boundary.
- No extraction. Store raw content plus an embedding. No LLM in the write path.
- No commercialization surface. This is a personal utility.

---

## 3. Architecture

```
Claude Code (Mac) ──HTTPS──▶ Caddy (TLS) ──▶ OAuth provider (Logto)
                                              │  validates token
                                              ▼
                                     MCP server (Streamable HTTP)
                                              │
                                              ▼
                                     Postgres 16 + pgvector (localhost)
```

**Host:** Oracle Cloud Always Free ARM VM (Ampere A1). Always up, $0.
**TLS:** Caddy in front, auto-cert. Security list open on 443 only. SSH on a non-default port or key-only.
**OAuth:** Logto, self-hosted on the box. Issues tokens for the MCP server as a protected resource. Do not hand-roll.
**MCP server:** your code. Streamable HTTP transport, MCP spec 2026-07-28 or later. Validates the bearer token against Logto on every request.
**DB:** Postgres 16, pgvector extension, bound to `127.0.0.1`. No public exposure.

---

## 4. Data model

```sql
CREATE EXTENSION IF NOT EXISTS vector;

CREATE TABLE memory (
  id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id     text NOT NULL DEFAULT 'me',      -- hardcoded Phase 1, kept for schema stability
  namespace     text NOT NULL,                   -- 'user:me' | 'project:<slug>' | 'global'
  content       text NOT NULL,
  embedding     vector(768),                      -- bge-base dims; change if model changes
  tags          text[] DEFAULT '{}',
  supersedes    uuid REFERENCES memory(id),       -- stored, not yet consolidated
  source_client text NOT NULL,
  created_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ON memory USING hnsw (embedding vector_cosine_ops);
CREATE INDEX ON memory (tenant_id, namespace);

CREATE TABLE registry (
  id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id     text NOT NULL DEFAULT 'me',
  namespace     text NOT NULL,
  kind          text NOT NULL,                    -- 'host'|'service'|'credential-ref'|'model-route'|'dataset'
  key           text NOT NULL,
  value         jsonb NOT NULL,
  provenance    jsonb NOT NULL,                   -- {source_client, conv_id, confidence, user_confirmed, valid_from}
  version       int NOT NULL DEFAULT 1,
  created_at    timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, namespace, kind, key)
);

CREATE TABLE tool_calls (                          -- instrumentation
  id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  client        text NOT NULL,
  tool          text NOT NULL,
  succeeded     boolean NOT NULL,
  unprompted    boolean,                           -- true = model called it on its own
  latency_ms    int,
  created_at    timestamptz NOT NULL DEFAULT now()
);
```

---

## 5. Tool surface (four tools)

Keep signatures stable; Phases 2–4 extend, don't rename.

**`context_bootstrap(project?) → digest`**
The "check memory first" primitive. Returns a compact digest: user profile facts, active project context, recent memory writes, registry summary. One call, cacheable. Must return under ~200ms: a slow bootstrap trains models to skip it. Tool description must state it runs before any substantive work.

**`memory_search(query, namespaces?, limit?) → rows[]`**
Embed the query, cosine search filtered by namespace. Default namespaces `['user:me', 'global']` plus the active project. Return content, tags, source_client, created_at.

**`memory_write(content, namespace, tags?, supersedes?) → id`**
Embed on write, store raw. No extraction. `supersedes` recorded but not acted on yet.

**`registry_get(kind, key, namespace?) → value`**
Exact lookup. No fuzziness.

---

## 6. Client wiring: Claude Code on the Mac

1. Register the MCP server as a remote connector with OAuth against Logto.
2. `SessionStart` hook calls `context_bootstrap` automatically. This is the one guaranteed-automatic read path.
3. `CLAUDE.md` rule for writes, phrased as a trigger not a suggestion: *"After any exchange that establishes a decision, preference, or durable fact, call `memory_write`. Without asking, without announcing."*
4. Aggressive tool descriptions reinforce both.

---

## 7. Instrumentation

Every tool invocation writes one `tool_calls` row. The `unprompted` flag is the signal that matters: it separates "the model chose to call this" from "the hook or the user forced it." Without the disposable Phase 0 rig, this is how you read whether the loop works. Also log success/failure so a transport error can't masquerade as the model declining to write.

Weekly, eyeball: unprompted read rate, unprompted write rate, and whether any repeat got prevented. Numbers per client once more than one is wired.

---

## 8. Security checklist (Phase 1 floor)

- Postgres bound to localhost, never public.
- Security list: 443 in, nothing else. SSH key-only.
- Caddy auto-TLS, HSTS on.
- Logto validates every MCP request; no unauthenticated path to the tools.
- The real leak surface is grant logic. Phase 1 has one client so it's trivial, but write the token→client mapping so Phase 2 can add per-client namespace denials without a rewrite.
- Backups daily incremental, local. Note for Phase 3: once encryption lands, backups must not carry the KEK alongside ciphertext.

---

## 9. Build order

1. Provision the Oracle VM, lock the security list, install Postgres + pgvector.
2. Stand up Logto, register the MCP server as a protected resource.
3. Caddy in front, TLS verified end to end.
4. MCP server skeleton: token validation, Streamable HTTP, health check.
5. Schema migration (section 4).
6. Implement the four tools (section 5). Embeddings via bge-base on the box, or OpenAI `text-embedding-3-small` if you skip hosting the model. Pick before writing the embedding column.
7. Wire Claude Code on the Mac (section 6).
8. Instrumentation table + logging (section 7).
9. Run the done-when test (section 1).

Estimate: a weekend for a working loop if Logto and Caddy go smoothly. The OAuth integration is the schedule risk; budget for it.

---

## 10. Decisions still open before you code

- **Embedding model:** bge-base self-hosted (no external dependency, uses box CPU) vs OpenAI `text-embedding-3-small` (near-zero cost, one less thing to run). Pick one; it fixes the `vector()` dimension.
- **Logto vs Ory Hydra:** Logto is faster to stand up with a UI; Hydra is stricter on spec. Logto recommended for Phase 1.
- **bge-base on ARM:** confirm it runs acceptably on the Ampere A1 before committing, or fall back to the OpenAI embeddings path.
