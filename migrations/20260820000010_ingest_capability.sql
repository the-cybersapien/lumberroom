-- Phase 6, step 2. The capability that gates every ingest route.
--
-- A model that can post proposals can fill the queue, and a queue the owner stops reading is an
-- approval gate in name only. So `mayIngest` is off unless the grant says otherwise, the same
-- shape `mayDelete` takes and for the same reason: the header that tells a CLI apart from a model
-- is one a model can set for free, so the boundary has to be the grant.
--
-- Default false, so every client that consented before this migration keeps exactly the reach it
-- had. The owner grants it again at the consent screen when he wants it.
ALTER TABLE oauth_client
  ADD COLUMN IF NOT EXISTS may_ingest boolean NOT NULL DEFAULT false;
