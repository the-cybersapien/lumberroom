-- The registry stop-loss.
--
-- A superseded memory row is hidden by a filter and still on disk. A replaced registry value is
-- gone: the upsert sets `value = EXCLUDED.value` and bumps a counter, so a key rewritten five times
-- says "version 5" and cannot say what versions 1 through 4 held. "What was the Postgres port
-- before I changed it" is a registry question, and today the store cannot answer it.
--
-- This is the stop-loss. One append-only table and one trigger, no range type, no exclusion
-- constraint, no read path. The full versioned design with `tstzrange` and a GIST exclusion
-- constraint stays deferred and keeps its own decision record and its own migration.
--
-- Additive only, like every migration here. It creates a table and a trigger and alters nothing
-- that exists, so a binary built before it still reads and writes `registry` unchanged. Migrations
-- are forward-only: once this has applied, an older image cannot boot against the store, which is
-- why nothing below touches the live table's shape.

CREATE TABLE IF NOT EXISTS registry_history (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  -- The live row this value belonged to. No foreign key: a cascade would delete the history along
  -- with the row it exists to outlive, and a restricting key would make `delete()` start failing.
  -- Keeping the id as a plain column also separates "the key was rewritten" from "the key was
  -- deleted and written again", which have different ids and the same name.
  registry_id uuid NOT NULL,
  tenant_id   text NOT NULL DEFAULT 'me',
  namespace   text NOT NULL,
  kind        text NOT NULL,
  key         text NOT NULL,
  value       jsonb NOT NULL,
  provenance  jsonb NOT NULL,
  -- No CHECK on the vocabulary. The live table carries one, so every value arriving here already
  -- passed it. A stricter copy of that constraint would abort a legitimate registry write on the
  -- day a fourth level lands and only `registry` gets the ALTER, turning an archive into an outage.
  sensitivity text NOT NULL DEFAULT 'open',
  -- The version this value was, not the version that replaced it.
  version     int NOT NULL,
  replaced_at timestamptz NOT NULL DEFAULT now()
);

-- The one question this table answers: what has this key held, newest first.
CREATE INDEX IF NOT EXISTS registry_history_key
  ON registry_history (tenant_id, namespace, kind, key, replaced_at DESC);

-- A trigger rather than a CTE inside the upsert, and the reason is atomicity under a second writer.
--
-- A `WITH prior AS (SELECT ... FROM registry ...)` beside the upsert reads the statement snapshot,
-- while `ON CONFLICT DO UPDATE` follows the update chain to whatever the row became. Two writers on
-- one key then archive the same old value twice and lose the value in between. Adding `FOR UPDATE`
-- to that read closes the update race and leaves the insert race: rows another transaction inserted
-- after the snapshot stay invisible to a locking read, so the loser's `DO UPDATE` overwrites a value
-- it never saw. `OLD` has neither hole. It is the row the update applies to, in the same
-- transaction, by construction.
--
-- It also covers writers that are not the adapter. An owner fixing a value in psql leaves a history
-- row without knowing the table is there, and that is the point of a stop-loss.
CREATE OR REPLACE FUNCTION registry_archive_old_value() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO registry_history (
    registry_id, tenant_id, namespace, kind, key, value, provenance, sensitivity, version
  ) VALUES (
    OLD.id, OLD.tenant_id, OLD.namespace, OLD.kind, OLD.key,
    OLD.value, OLD.provenance, OLD.sensitivity, OLD.version
  );
  RETURN NULL;
END;
$$;

-- AFTER, so the archive can never alter or refuse the write it records.
--
-- No `WHEN` clause. Conditioning on `OLD.value IS DISTINCT FROM NEW.value` looks tidier and hands a
-- future UPDATE that skips the version bump a way to destroy a value in silence. An unconditional
-- trigger fails the other way, toward rows nobody needed.
--
-- UPDATE only. A DELETE is the owner asking for a value to be gone, and the registry holds
-- credential locations; keeping a copy of one that was deleted on purpose is the wrong default. The
-- full design decides whether delete archives, and under what retention.
DROP TRIGGER IF EXISTS registry_archive ON registry;
CREATE TRIGGER registry_archive
  AFTER UPDATE ON registry
  FOR EACH ROW EXECUTE FUNCTION registry_archive_old_value();
