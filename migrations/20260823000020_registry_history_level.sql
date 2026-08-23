-- A registry key's past is classified at least as high as its present.
--
-- The archive from migration 000012 records a replaced value at the level it carried while it was
-- current. That is right when a key is lowered: a value stored at private stays private in the
-- archive after the key is rewritten at open. It is wrong in the other direction. A write that
-- raises a key from open to private, with the value unchanged, archives an identical copy of the
-- now-private value at open, and registry_history hands it to an open-ceiling client that
-- registry_get has just refused. The thing the owner classified is the thing the archive serves.
--
-- So the archive records the higher of the two levels, and a raise lifts the rows already
-- archived below the new level. Neither direction ever lowers a row: history can become harder to
-- read and never easier.
--
-- The function body replaces the one from 000012 under the same name, so the existing trigger
-- picks it up and nothing about when it fires changes. AFTER UPDATE, every row, no WHEN clause,
-- for the reasons that migration gives.
CREATE OR REPLACE FUNCTION registry_archive_old_value() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  archived_level text;
BEGIN
  archived_level := CASE
    WHEN sensitivity_rank(OLD.sensitivity) >= sensitivity_rank(NEW.sensitivity)
      THEN OLD.sensitivity
    ELSE NEW.sensitivity
  END;

  INSERT INTO registry_history (
    registry_id, tenant_id, namespace, kind, key, value, provenance, sensitivity, version
  ) VALUES (
    OLD.id, OLD.tenant_id, OLD.namespace, OLD.kind, OLD.key,
    OLD.value, OLD.provenance, archived_level, OLD.version
  );

  IF sensitivity_rank(NEW.sensitivity) > sensitivity_rank(OLD.sensitivity) THEN
    UPDATE registry_history
       SET sensitivity = NEW.sensitivity
     WHERE tenant_id = OLD.tenant_id
       AND namespace = OLD.namespace
       AND kind = OLD.kind
       AND key = OLD.key
       AND sensitivity_rank(sensitivity) < sensitivity_rank(NEW.sensitivity);
  END IF;
  RETURN NULL;
END;
$$;
