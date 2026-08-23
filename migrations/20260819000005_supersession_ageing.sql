-- Phase 4, steps 1 and 5. Supersession that retires, and the signals ageing needs.
--
-- Phase 1 shipped `supersedes` as a validated foreign key that nothing read, so a correction
-- landed beside the fact it replaced and search returned both. Running the done-when test four
-- times left four contradictory answers in the store, each written by a model acting correctly.

-- The link is stored on both rows rather than derived: every read filters on it, and a correlated
-- subquery on the hot path is the wrong trade.
ALTER TABLE memory ADD COLUMN IF NOT EXISTS superseded_by uuid REFERENCES memory(id);
ALTER TABLE memory ADD COLUMN IF NOT EXISTS superseded_at timestamptz;

-- Live-row scans stay cheap regardless of how much history accumulates.
CREATE INDEX IF NOT EXISTS memory_live
  ON memory (tenant_id, namespace)
  WHERE superseded_by IS NULL;

-- Reading the chain backwards, for the decision log and for cycle detection on write.
CREATE INDEX IF NOT EXISTS memory_supersedes ON memory (supersedes) WHERE supersedes IS NOT NULL;

-- Ageing signals. Nothing here deletes anything: a personal memory that silently forgets is worse
-- than one that gets cluttered. These feed ranking, the review queue and the staleness numbers.
ALTER TABLE memory ADD COLUMN IF NOT EXISTS last_accessed_at  timestamptz;
ALTER TABLE memory ADD COLUMN IF NOT EXISTS access_count      int NOT NULL DEFAULT 0;
-- Set when a write restates an existing fact rather than contradicting it. Repetition is
-- confirmation, and it is the only positive signal available without asking.
ALTER TABLE memory ADD COLUMN IF NOT EXISTS last_confirmed_at timestamptz;

-- "Live rows never retrieved" is one of the three staleness numbers.
CREATE INDEX IF NOT EXISTS memory_never_accessed
  ON memory (tenant_id, created_at)
  WHERE last_accessed_at IS NULL AND superseded_by IS NULL;

-- Registry entries age at different rates by kind: a host entry ages slowly, a model-route ages
-- fast because routing preferences change monthly. Expiry marks a row for review, never removes it.
ALTER TABLE registry ADD COLUMN IF NOT EXISTS review_after timestamptz;
