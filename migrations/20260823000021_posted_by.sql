-- Who posted a proposal, from the credential and never from the body.
--
-- produced_by on cleanup_proposal and extractor on ingest_proposal are strings the poster chose,
-- and a posted row looked the same in the queue as one this server's own pass wrote. The
-- service sets this from Principal.client on the HTTP paths and leaves it NULL for the
-- in-process cleanup pass, so a reader deciding whether to trust the claim has the poster beside
-- it. Nullable on both tables: rows written before this column existed have no poster to name.
ALTER TABLE cleanup_proposal ADD COLUMN IF NOT EXISTS posted_by text;
ALTER TABLE ingest_proposal  ADD COLUMN IF NOT EXISTS posted_by text;
