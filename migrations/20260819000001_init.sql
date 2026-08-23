-- Phase 1 schema. Mirrors PRD §4.
-- tenant_id stays a plain column with one hardcoded value: no RLS, no multi-tenant machinery,
-- but Phase 2 can add both without a table rewrite.

CREATE EXTENSION IF NOT EXISTS vector;

CREATE TABLE IF NOT EXISTS memory (
  id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id       text NOT NULL DEFAULT 'me',
  namespace       text NOT NULL,
  content         text NOT NULL,
  embedding       vector(768),
  tags            text[] NOT NULL DEFAULT '{}',
  supersedes      uuid REFERENCES memory(id),
  source_client   text NOT NULL,
  embedding_model text,
  created_at      timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS memory_embedding_hnsw
  ON memory USING hnsw (embedding vector_cosine_ops);
CREATE INDEX IF NOT EXISTS memory_tenant_namespace
  ON memory (tenant_id, namespace);
CREATE INDEX IF NOT EXISTS memory_created_at
  ON memory (created_at DESC);
CREATE INDEX IF NOT EXISTS memory_tags
  ON memory USING gin (tags);

CREATE TABLE IF NOT EXISTS registry (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id   text NOT NULL DEFAULT 'me',
  namespace   text NOT NULL,
  kind        text NOT NULL,
  key         text NOT NULL,
  value       jsonb NOT NULL,
  provenance  jsonb NOT NULL,
  version     int NOT NULL DEFAULT 1,
  created_at  timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, namespace, kind, key)
);

CREATE TABLE IF NOT EXISTS tool_calls (
  id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  client     text NOT NULL,
  tool       text NOT NULL,
  succeeded  boolean NOT NULL,
  unprompted boolean,
  latency_ms int,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS tool_calls_created_at ON tool_calls (created_at DESC);
CREATE INDEX IF NOT EXISTS tool_calls_tool ON tool_calls (tool, unprompted);
