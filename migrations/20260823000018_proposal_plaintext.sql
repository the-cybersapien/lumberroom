-- A proposal's plaintext does not outlive the encryption of the row it became.
--
-- ingest_proposal.content and .quote hold an extracted fact in the clear from the moment it is
-- queued. Approval writes the memory row through the same path as memory_write, so a fact bound
-- for a private namespace is sealed under the KEK there, and then the proposal row kept the same
-- sentence in plaintext for good. A dump read the private fact out of the queue table with no key
-- and no inversion. Nothing reads the text after a decision: idempotency runs on the fingerprint
-- unique index, and the written and rejected branches of the post path use state and memory_id
-- alone.
--
-- A trigger rather than a change to mark_written, for the reason the registry archive is one: it
-- covers every writer of the column, psql included, and it cannot be skipped by a second code
-- path that sets memory_id. BEFORE UPDATE, so it edits the row being written instead of writing
-- it twice.
--
-- Two cases clear the text.
--
-- The proposal is linked to a memory whose content is encrypted. The row exists by then, which is
-- the moment the plaintext has somewhere safer to live.
--
-- The proposal is unlinked from its memory, which is what the ON DELETE SET NULL in migration
-- 000019 does when the memory is forgotten. A crypto-shred that left the sentence in the queue
-- table would not be a shred. The memory row is gone by the time this fires, so its level cannot
-- be checked and the text is cleared whatever the level was; for an open row that costs a
-- sentence already deleted on purpose.
--
-- content stays NOT NULL and becomes the empty string, so a reader built before this migration
-- still reads the column as text. quote was nullable already.
CREATE OR REPLACE FUNCTION ingest_proposal_clear_plaintext() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.memory_id IS NOT NULL
     AND NEW.memory_id IS DISTINCT FROM OLD.memory_id
     AND EXISTS (SELECT 1 FROM memory m WHERE m.id = NEW.memory_id AND m.content_ct IS NOT NULL)
  THEN
    NEW.content := '';
    NEW.quote := NULL;
  ELSIF OLD.memory_id IS NOT NULL AND NEW.memory_id IS NULL THEN
    NEW.content := '';
    NEW.quote := NULL;
  END IF;
  RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS ingest_proposal_clear_plaintext ON ingest_proposal;
CREATE TRIGGER ingest_proposal_clear_plaintext
  BEFORE UPDATE OF memory_id ON ingest_proposal
  FOR EACH ROW EXECUTE FUNCTION ingest_proposal_clear_plaintext();

-- The rows already in that state. The trigger fires on a change to memory_id and these rows have
-- had theirs since approval.
UPDATE ingest_proposal p
   SET content = '', quote = NULL
  FROM memory m
 WHERE m.id = p.memory_id
   AND m.content_ct IS NOT NULL
   AND (p.content <> '' OR p.quote IS NOT NULL);
