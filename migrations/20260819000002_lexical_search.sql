-- Lexical index so memory_search can blend cosine similarity with exact-term matching.
-- Vector search alone misses short factual queries ("ssh port", "db password location");
-- the blend is a ranking detail behind the same tool signature (PRD §5).

CREATE INDEX IF NOT EXISTS memory_content_fts
  ON memory USING gin (to_tsvector('english', content));
