-- Phase 8, step 1. The cleanup queue.
--
-- The store accumulates. A week of ingestion put 222 proposals in it; the correction gate left four
-- test rows in user:me that reached the next session's digest as facts about the owner; two rows in
-- project:sutr carry different values for the same nickname. None of that is a bug in any one write
-- path. It is what a store that only ever grows looks like after a month, and nothing in the system
-- reads the store as a whole and says so.
--
-- This table is what a periodic pass writes into. It proposes and never acts, which is the same
-- rule ingestion follows and for the same reason: a personal memory that silently forgets is worse
-- than one that gets cluttered. Applying a proposal calls services::review::supersede or
-- services::forget::run, so the grant check, the ceiling check and the history rules stay in the
-- one place that already holds them.
CREATE TABLE IF NOT EXISTS cleanup_proposal (
  id            uuid PRIMARY KEY,
  tenant_id     text NOT NULL,
  -- exact:        byte-identical content after normalisation. No judgement involved.
  -- paraphrase:   the same fact in different words. One survives, the rest retire into it.
  -- contradiction: two rows that cannot both hold. The owner picks; the pass only points.
  -- stale:        nothing has read it and nothing refers to it.
  kind          text NOT NULL
                CHECK (kind IN ('exact', 'paraphrase', 'contradiction', 'stale')),
  namespace     text NOT NULL,
  -- The row that survives, and the one every retiring member supersedes into. Null for `stale`,
  -- where there is nothing to keep and the proposal is a delete.
  keep_id       uuid REFERENCES memory(id) ON DELETE CASCADE,
  -- Why, in the words of whatever produced it. A proposal a person cannot evaluate in one read is
  -- a proposal that sits in the queue forever.
  rationale     text NOT NULL,
  -- 'exact', 'cosine', or the model id that produced it. A number in a report means nothing without
  -- the thing that produced it, and the cheap tier and the expensive tier disagree often enough
  -- that the queue has to say which one spoke.
  produced_by   text NOT NULL,
  -- The cosine that grouped the cluster, when a cosine did. Null when a model grouped it.
  --
  -- Published per proposal on purpose. The dedupe bands are uncalibrated guesses (0.97 and 0.90,
  -- Phase 4 spec), and calibrating them needs a person reading real pairs with their scores. This
  -- queue is that instrument, so the score rides along rather than being a second exercise.
  similarity    double precision,
  -- obsolete is the exit a contradiction needs. That kind names no survivor, so it is resolved by
  -- hand with `sutr supersede` and then has nothing left to say. Without a terminal state it sits
  -- in `proposed` forever and the queue fills with findings the store has already answered.
  state         text NOT NULL DEFAULT 'proposed'
                CHECK (state IN ('proposed', 'applied', 'rejected', 'obsolete')),
  -- Rejection is a signal, not a delete: the same cluster will be found again next hour, and a pass
  -- that re-proposes what the owner already refused is a pass he stops reading.
  reason        text,
  decided_at    timestamptz,
  created_at    timestamptz NOT NULL DEFAULT now(),
  -- sha256 over the kind and the sorted member ids. The idempotency story, and it is the whole
  -- reason an hourly pass is safe to run hourly: finding the same cluster again updates nothing.
  cluster_key   text NOT NULL,
  UNIQUE (tenant_id, cluster_key)
);

-- The queue read: one state, newest first, matching ingest_proposal_queue.
CREATE INDEX IF NOT EXISTS cleanup_proposal_queue
  ON cleanup_proposal (tenant_id, state, created_at DESC);

-- One row per memory in the cluster.
--
-- ON DELETE CASCADE on memory_id rather than a nullable reference: a proposal that has lost a
-- member is not a proposal about a smaller cluster, it is a proposal about a store that changed
-- underneath it, and apply refuses it anyway.
CREATE TABLE IF NOT EXISTS cleanup_proposal_member (
  proposal_id  uuid NOT NULL REFERENCES cleanup_proposal(id) ON DELETE CASCADE,
  memory_id    uuid NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
  disposition  text NOT NULL CHECK (disposition IN ('keep', 'retire')),
  -- The content as it stood when the pass looked at it. Apply compares this against the row and
  -- refuses when they differ: a proposal written an hour ago must not retire a row the owner
  -- edited since.
  seen_content text NOT NULL,
  PRIMARY KEY (proposal_id, memory_id)
);

CREATE INDEX IF NOT EXISTS cleanup_proposal_member_by_memory
  ON cleanup_proposal_member (memory_id);

-- When each scope last ran, so an hourly pass reads what changed rather than the store.
--
-- Keyed by scope rather than by namespace: a run covers a namespace glob, and two runs over
-- overlapping globs each need their own mark.
CREATE TABLE IF NOT EXISTS cleanup_watermark (
  tenant_id    text NOT NULL,
  scope        text NOT NULL,
  cadence      text NOT NULL CHECK (cadence IN ('hourly', 'daily')),
  last_run_at  timestamptz NOT NULL,
  -- The newest created_at the last run considered. A row written during a run is picked up by the
  -- next one rather than skipped, because this advances to what was read and not to now().
  through      timestamptz NOT NULL,
  PRIMARY KEY (tenant_id, scope, cadence)
);
