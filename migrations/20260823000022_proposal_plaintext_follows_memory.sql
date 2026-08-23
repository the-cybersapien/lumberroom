-- A proposal's plaintext goes when the memory it became is sealed later, not only at approval.
--
-- Migration 000018 clears ingest_proposal.content and .quote when a proposal is linked to an
-- encrypted memory and when it loses its link. Both fire on a change to memory_id. A memory that
-- was open at approval and is encrypted afterwards, by a reclassification done in psql or by a
-- later code path, changes nothing on the proposal row, so the queue table kept the sentence in
-- the clear beside a memory row that no longer does.
--
-- This trigger watches the other side of the link. When memory.content_ct goes from NULL to a
-- value, every proposal pointing at that row loses its text. AFTER UPDATE rather than BEFORE:
-- the proposal rows are a different table, and the write to them has to see the memory row in
-- its new state. The function reads no level, only whether ciphertext exists, which is the same
-- test 000018 makes and the one a trigger can make without the classification table.
--
-- The proposal at state 'proposed' for a private namespace is still in the clear, and on purpose:
-- the owner reads it from the queue and approval writes it through write::run, so nothing sealed
-- can stand in for it until he decides. services::ingest::reject clears the text on refusal.
CREATE OR REPLACE FUNCTION memory_sealed_clears_proposal_plaintext() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.content_ct IS NOT NULL AND OLD.content_ct IS NULL THEN
    UPDATE ingest_proposal
       SET content = '', quote = NULL
     WHERE tenant_id = NEW.tenant_id
       AND memory_id = NEW.id
       AND (content <> '' OR quote IS NOT NULL);
  END IF;
  RETURN NULL;
END;
$$;

DROP TRIGGER IF EXISTS memory_sealed_clears_proposal_plaintext ON memory;
CREATE TRIGGER memory_sealed_clears_proposal_plaintext
  AFTER UPDATE OF content_ct ON memory
  FOR EACH ROW EXECUTE FUNCTION memory_sealed_clears_proposal_plaintext();
