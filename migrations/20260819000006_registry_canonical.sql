-- Phase 2 §4. Canonical registry keys, and the alias table that makes rejection survivable.
--
-- The system PRD is explicit that this matters from day one: six writers without a scheme produce
-- desktop.gpu, machines.desktop.gpu and hardware.desktop.gpu for one fact, and preventing that
-- beats cleaning it up. This must be in place before the second writer connects.
--
-- Rejection alone is not enough. A model that gets rejected invents a second variant rather than
-- the canonical one, so every rejected guess is recorded as a redirect instead of becoming a
-- duplicate fact.
CREATE TABLE IF NOT EXISTS registry_alias (
  tenant_id  text NOT NULL DEFAULT 'me',
  namespace  text NOT NULL,
  kind       text NOT NULL,
  alias_key  text NOT NULL,
  canonical  text NOT NULL,
  -- Where the redirect came from: a hand-written mapping, or a write the server rejected.
  origin     text NOT NULL DEFAULT 'manual' CHECK (origin IN ('manual', 'rejected-write', 'migration')),
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, namespace, kind, alias_key)
);

CREATE INDEX IF NOT EXISTS registry_alias_canonical
  ON registry_alias (tenant_id, namespace, kind, canonical);

-- Phase 1 keys are free-form and there are few of them. They are migrated by hand rather than by
-- a guess in SQL: read them, map them, record the old names here with origin 'migration'.
-- The CHECK is deliberately not applied to the existing column, because a constraint that
-- retroactively invalidates stored rows turns a naming cleanup into an outage.
