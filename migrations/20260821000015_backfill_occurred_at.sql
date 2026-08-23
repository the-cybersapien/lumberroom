-- Fill valid time on the memories that were approved before there was a column to put it in.
--
-- Ingestion approved rows for weeks before phase 7, and every one of them took the day it was
-- approved. The transcript knew better the whole time: the span's timestamp sits on
-- `ingest_proposal_source.observed_at`, and until phase 7 it stopped there. This is the one-off that
-- carries it across. Approvals from here on fill it at write time and never reach this.
--
-- WHAT THE VALUE MEANS, and it is not what the column's name suggests. `min(observed_at)` is the
-- first moment somebody was recorded stating the fact, which bounds when the fact became true
-- without pinning it: a July transcript saying "we moved in June" backfills July. It is an upper
-- bound and the tightest one this store holds. Reading it as the instant the fact began misdates
-- every retrospective sentence, which is the conflation valid time was added to end, repeated one
-- level up inside the fix for it.
--
-- Three guards, and each one exists to keep the fill honest rather than merely complete:
--   only rows whose `occurred_at` is still NULL, so nothing already known is overwritten
--   only where the observation precedes what the store already recorded, since a fact observed
--     after it was stored means the two clocks disagree and a guess would be worse than a NULL
--   matched on tenant as well as id, because a memory id is unique per store and the join is not
--     the place to assume it.
UPDATE memory m
   SET occurred_at = src.first_observed
  FROM (
        SELECT p.tenant_id,
               p.memory_id,
               min(s.observed_at) AS first_observed
          FROM ingest_proposal p
          JOIN ingest_proposal_source s ON s.proposal_id = p.id
         WHERE p.state = 'written'
           AND p.memory_id IS NOT NULL
           AND s.observed_at IS NOT NULL
         GROUP BY p.tenant_id, p.memory_id
       ) src
 WHERE m.id = src.memory_id
   AND m.tenant_id = src.tenant_id
   AND m.occurred_at IS NULL
   AND src.first_observed <= m.created_at;
