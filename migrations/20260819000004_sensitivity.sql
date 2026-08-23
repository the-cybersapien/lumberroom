-- Phase 3, step 1. The sensitivity axis, additive and behaviour-neutral.
--
-- Every existing row becomes 'open', which is what it already effectively was. Nothing reads the
-- column until the two-axis grant parser lands, so this migration is safe to ship on its own.

-- Ordering as SQL, because the filter runs in the query rather than in the application: a row a
-- client may not see should never enter that client's process memory. IMMUTABLE so it can be
-- indexed and folded into a plan.
CREATE OR REPLACE FUNCTION sensitivity_rank(level text) RETURNS int
  LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS $$
  SELECT CASE level WHEN 'open' THEN 0 WHEN 'private' THEN 1 WHEN 'sealed' THEN 2 ELSE 99 END
$$;

ALTER TABLE memory
  ADD COLUMN IF NOT EXISTS sensitivity text NOT NULL DEFAULT 'open';
ALTER TABLE memory
  DROP CONSTRAINT IF EXISTS memory_sensitivity_check;
ALTER TABLE memory
  ADD CONSTRAINT memory_sensitivity_check CHECK (sensitivity IN ('open', 'private', 'sealed'));

-- The registry holds credential locations, so it needs the axis as much as memory does.
ALTER TABLE registry
  ADD COLUMN IF NOT EXISTS sensitivity text NOT NULL DEFAULT 'open';
ALTER TABLE registry
  DROP CONSTRAINT IF EXISTS registry_sensitivity_check;
ALTER TABLE registry
  ADD CONSTRAINT registry_sensitivity_check CHECK (sensitivity IN ('open', 'private', 'sealed'));

-- Every read filters on (tenant, namespace, sensitivity). Rank rather than the text so the
-- index answers a ceiling comparison directly.
CREATE INDEX IF NOT EXISTS memory_tenant_namespace_sensitivity
  ON memory (tenant_id, namespace, sensitivity_rank(sensitivity));

-- The lexical index becomes conditional.
--
-- A Postgres tsvector is not an index over the document, it is the document, stemmed. Recovering
-- private content from it needs no attack and no model, so private content cannot be in it.
-- The consequence is deliberate and documented: private rows are semantic-only, and exact-phrase
-- search does not reach them (docs/research/encryption-and-sensitivity.md).
--
-- On an all-open store this is a no-op, which makes now the cheapest possible moment to do it.
DROP INDEX IF EXISTS memory_content_fts;
CREATE INDEX memory_content_fts
  ON memory USING gin (to_tsvector('english', content))
  WHERE sensitivity = 'open';

-- Classification is inferred, not asked for. A write with no explicit level takes its namespace
-- default, so nobody classifies anything in the normal case (system PRD §9: if using the system
-- means classifying every sentence, the system has failed at the product level).
--
-- Patterns use the same trailing-wildcard form as namespace grants. Longest matching pattern wins,
-- so 'credentials:*' beats '*'.
CREATE TABLE IF NOT EXISTS sensitivity_default (
  tenant_id   text NOT NULL DEFAULT 'me',
  pattern     text NOT NULL,
  sensitivity text NOT NULL CHECK (sensitivity IN ('open', 'private', 'sealed')),
  created_at  timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, pattern)
);

-- Seeded from the Phase 3 spec §2. Editing this table is expected about twice a year.
INSERT INTO sensitivity_default (tenant_id, pattern, sensitivity) VALUES
  ('me', '*',                'open'),
  ('me', 'global',           'open'),
  ('me', 'project:*',        'open'),
  ('me', 'user:me',          'open'),
  ('me', 'personal:finance', 'private'),
  ('me', 'personal:health',  'private'),
  ('me', 'credentials:*',    'sealed')
ON CONFLICT (tenant_id, pattern) DO NOTHING;
