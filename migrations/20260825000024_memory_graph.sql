-- Decision 0014 part 4. A graph over memories, in Postgres.
--
-- The failing query joins a held position to a named catalyst, and neither phrase appears in the row
-- that answers it. Measured twice on 25 August 2026: the answer scores 0.834 against its own name
-- and does not reach the top twenty of the question that describes it, on the live store and on a
-- replica. Nearest-neighbour search has no operator for that join. Typed nodes and an edge do.
--
-- **Seeded from structure, not from an extractor.** The record assumed entity extraction and warned
-- that the graph has to earn its extraction cost. The store already holds real edges that cost
-- nothing: a supersession link is an edge, an alias is an edge, and a shared curated tag is an edge.
-- Building those first makes the question answerable, and if a bounded walk over them answers no
-- more than search does, the extractor was never the missing piece.

-- `relation` is a closed set, checked here rather than left to a string.
--
-- 0005 established that a plaintext derivative sitting beside encrypted content cancels the
-- encryption, and that reading the column is the attack. A model-written relation label like
-- "supersedes the earlier limit on the joint account" describes a private row to anyone holding the
-- database, and this table has no sensitivity column of its own to filter on. A closed enum cannot
-- carry a sentence. Per-candidate reasoning belongs on a proposal, where it dies when the owner
-- decides.
CREATE TABLE IF NOT EXISTS memory_edge (
  tenant_id   text NOT NULL,
  -- Endpoints. CASCADE on both, and this is not a preference.
  --
  -- `ingest_proposal.memory_id` referenced memory with NO ACTION, a delete raised 23503, and the
  -- caller got an internal error while the wrapped DEK and the plaintext embedding stayed behind.
  -- That was filed as forget-fk-blocks-shred and settled in 0013. Two foreign keys into memory with
  -- the same default would reproduce it exactly. An edge surviving a crypto-shred also leaks the
  -- shape of the shredded fact, which is the part no error message would show.
  src_id      uuid NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
  dst_id      uuid NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
  relation    text NOT NULL
              CHECK (relation IN ('supersedes', 'shares_alias', 'shares_tag')),
  -- What produced it, so a walk can be explained and a bad seeder can be undone by its own name.
  produced_by text NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, src_id, dst_id, relation)
);

-- Both directions get an index. A walk expands from whichever end it reached, and an edge readable
-- in one direction only would make the answer depend on which node the seed landed on.
CREATE INDEX IF NOT EXISTS memory_edge_src ON memory_edge (tenant_id, src_id);
CREATE INDEX IF NOT EXISTS memory_edge_dst ON memory_edge (tenant_id, dst_id);

-- No namespace or sensitivity is denormalised onto this table, and the omission is the decision.
--
-- The draft carried each endpoint's namespace and sensitivity for fan-out planning. Both columns are
-- mutable on `memory`: sensitivity is raised by hand and namespaces are renamed through the alias
-- table. A copy here would be a second source of truth for the one axis that decides what a client
-- may read, and the stale value would be the one the hop filter read, so a row promoted to private
-- would stay walkable at its old ceiling with nothing detecting the divergence. The traversal joins
-- `memory` for both endpoints and filters on the row. One extra join, one truth.
