-- Recall settings for filtered vector search.
--
-- Every search this system runs filters by namespace before ranking, which is HNSW's hard case.
-- With hnsw.iterative_scan off (the default), the scan pulls a fixed candidate batch, applies the
-- filter afterwards, and returns whatever survives. Measured on this schema shape: a query asking
-- for 10 rows against a namespace holding 0.5% of the table returned ZERO, having pulled 40
-- candidates and filtered all 40 away. No error, no warning. For a memory system, silently
-- answering "nothing is known" about a fact that is present is the worst failure available.
--
-- strict_order keeps pulling candidates until the limit is satisfied, preserving exact distance
-- ordering. relaxed_order is cheaper and allows minor ordering violations; this workload is
-- single-user and recall-critical, so it buys the ordering.
--
-- ef_search defaults to 40, widely held to be too low for production recall. At this query rate
-- the extra work is free.
--
-- These are set on the database rather than in postgresql.conf so they travel with the schema and
-- apply however Postgres is run: compose, a managed service, or bare metal.

DO $$
BEGIN
  EXECUTE format('ALTER DATABASE %I SET hnsw.iterative_scan = %L', current_database(), 'strict_order');
  EXECUTE format('ALTER DATABASE %I SET hnsw.ef_search = %L', current_database(), '100');
END
$$;

-- ef_construction defaults to 64. For 768-dimension embeddings the community convention is
-- 128 to 200: more build time for a better-connected graph, which is the trade this workload
-- wants since builds are rare and recall matters. m stays at 16, which is already the default.
DROP INDEX IF EXISTS memory_embedding_hnsw;
CREATE INDEX memory_embedding_hnsw
  ON memory USING hnsw (embedding vector_cosine_ops)
  WITH (m = 16, ef_construction = 128);
