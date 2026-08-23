-- The capability that gates reading facts which no longer hold.
--
-- A grant over live rows is not a grant over the history behind them. A retired fact can be more
-- revealing than the one that replaced it: an old credential location is exactly the shape that
-- gets superseded rather than deleted, and the as-of query reads precisely those rows.
--
-- Default false, so every client that consented before this keeps exactly the reach it had. It
-- follows `may_ingest` and `may_delete`, and for the same reason: the boundary has to be the grant,
-- because any header that would distinguish an operator from a model is one a model can set.
ALTER TABLE oauth_client
  ADD COLUMN IF NOT EXISTS may_read_history boolean NOT NULL DEFAULT false;
