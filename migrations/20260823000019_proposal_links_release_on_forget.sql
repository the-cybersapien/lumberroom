-- Forgetting a memory that came from the ingest queue no longer fails on a foreign key.
--
-- ingest_proposal.memory_id and .supersedes reference memory(id) with the default NO ACTION, so
-- DELETE FROM memory raised 23503 for any row that had been approved from the queue or named as a
-- proposal's supersession target. The transaction rolled back, the caller got an internal error,
-- and the row stayed, wrapped DEK, plaintext embedding and all. A dry run had said the row was
-- doomed. The same key meant a credential holding only the ingest capability could pin any memory
-- against the owner's forget by proposing a fact that supersedes it.
--
-- ON DELETE SET NULL on both. A proposal whose memory is forgotten keeps its fingerprint, so the
-- content stays blocked from being proposed again, and loses the link; migration 000018's trigger
-- clears its plaintext in the same statement. A proposal whose supersession target is forgotten
-- becomes a plain proposal, and approving it writes a fresh row rather than failing on a target
-- that is gone.
--
-- memory's own supersedes and superseded_by keys are left at NO ACTION on purpose. SET NULL on
-- superseded_by would revive every row the deleted one had retired, in namespaces the caller may
-- never have been granted, with no service in the loop to say no. The delete path edits those
-- links itself under the caller's grant and lets the constraint fail on any it was not told
-- about.
--
-- Postgres cannot alter the referential action in place, so each key is dropped and re-added
-- under its original name. The names are the ones Postgres generated for the inline REFERENCES
-- in migration 000009.
ALTER TABLE ingest_proposal
  DROP CONSTRAINT IF EXISTS ingest_proposal_memory_id_fkey,
  ADD CONSTRAINT ingest_proposal_memory_id_fkey
    FOREIGN KEY (memory_id) REFERENCES memory(id) ON DELETE SET NULL;

ALTER TABLE ingest_proposal
  DROP CONSTRAINT IF EXISTS ingest_proposal_supersedes_fkey,
  ADD CONSTRAINT ingest_proposal_supersedes_fkey
    FOREIGN KEY (supersedes) REFERENCES memory(id) ON DELETE SET NULL;
