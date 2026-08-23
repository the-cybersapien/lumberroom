-- Valid time beside transaction time.
--
-- `created_at` says when this store learned a fact. These two say when the fact held in the world,
-- and the two are never conflated again. The failure that forced this is in the store today:
-- ingestion queued 222 proposals from a week of transcripts with a July tail, and every one of them
-- carries the day it was written.
--
-- Nullable, no default, no backfill. Filling these from `created_at` would write the exact
-- conflation this migration exists to end into every row that predates it, and a NULL meaning
-- "unknown" is worth more than a date that is wrong.
--
-- Half-open, [occurred_at, occurred_until). The start instant is inside the period and the end
-- instant outside it, so a predecessor ending at T and a successor starting at T tile the timeline
-- exactly once. Under closed intervals T belongs to both rows, a point query at T returns two
-- contradictory answers, and nothing errors.

ALTER TABLE memory ADD COLUMN IF NOT EXISTS occurred_at    timestamptz;
ALTER TABLE memory ADD COLUMN IF NOT EXISTS occurred_until timestamptz;

-- The one invariant a scalar pair does not get for free. A range constructor rejects an inverted
-- period on construction; two columns accept one silently, and every as-of predicate downstream
-- would then read a period that ends before it starts.
ALTER TABLE memory DROP CONSTRAINT IF EXISTS memory_valid_period_check;
ALTER TABLE memory ADD CONSTRAINT memory_valid_period_check
  CHECK (occurred_at IS NULL OR occurred_until IS NULL OR occurred_at <= occurred_until);

-- Live rows in one namespace by when the fact became true. NULLS LAST so a dated fact outranks an
-- undated one under a newest-first ordering. Partial on the same predicate as `memory_live` from
-- migration 005, so the two stay consistent.
CREATE INDEX IF NOT EXISTS memory_occurred_at
  ON memory (tenant_id, namespace, occurred_at DESC NULLS LAST)
  WHERE superseded_by IS NULL;

-- Rows with no known start, which the ingest fill and the review queue both ask for. Partial, so it
-- stays the size of the gap rather than the size of the table.
CREATE INDEX IF NOT EXISTS memory_no_occurred_at
  ON memory (tenant_id, created_at)
  WHERE occurred_at IS NULL AND superseded_by IS NULL;
