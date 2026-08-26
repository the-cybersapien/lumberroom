-- Decision 0014 part 3. Supersession becomes a cleanup kind, and cardinality becomes declarable.
--
-- The store can already record that one fact ended another. What nothing does is find the pair:
-- every import proposal is written with `supersedes: None`, so a superseded fact reaches the junk
-- pass with nothing beside it and that pass asks the only question it has, which is whether the
-- line is durable. It is durable. It stopped holding.
--
-- This lands in the cleanup queue rather than in the import client, because pair-finding, model
-- judgement, an owner-gated queue and an apply path calling `services::review::supersede` all
-- already exist here. A second queue would be two prompts, two review surfaces and two apply paths
-- for one concept, and 0011 already names a queue nobody reads as the failure mode.

-- The fifth kind.
--
-- `contradiction` is the near neighbour and the wrong home: it says two rows cannot both hold and
-- leaves the owner to pick, which is right when neither row is older in any meaningful sense. A
-- supersession says something stronger and dated, that this one held until that one started, and it
-- resolves by writing an interval rather than by choosing a survivor.
ALTER TABLE cleanup_proposal DROP CONSTRAINT IF EXISTS cleanup_proposal_kind_check;
ALTER TABLE cleanup_proposal ADD CONSTRAINT cleanup_proposal_kind_check
  CHECK (kind IN ('exact', 'paraphrase', 'contradiction', 'stale', 'supersession'));

-- How many values a subject holds at once.
--
-- A later fact ends an earlier one only when the thing holds one value at a time, and no sentence
-- says whether it does. These two arrived in one dump, the same shape with opposite answers:
--
--   [2026-08-17] <account> limit is <n> now.      replaces the earlier limit
--   [2026-08-10] Applied for <a> already.
--   [2026-08-10] Applying for <b> and <c> now.    replaces nothing
--
-- No model can read cardinality off the text, because it is not in the text. The owner declares it,
-- and an undeclared subject produces no proposal rather than a guessed one. The default is silence.
CREATE TABLE IF NOT EXISTS subject_cardinality (
  tenant_id  text NOT NULL,
  -- A tag, matched against `memory.tags`. A tag rather than a free-text subject because tags are
  -- already the thing the owner curates and the thing the store indexes; inventing a second
  -- subject key would need its own resolution rules and its own drift.
  tag        text NOT NULL,
  -- 'single' means one value at a time, so a later dated fact carrying this tag may end an earlier
  -- one. 'many' is a declaration too, and a useful one: it stops the pass from re-offering a
  -- subject the owner has already said is a list.
  arity      text NOT NULL CHECK (arity IN ('single', 'many')),
  -- Free text, because a declaration the owner cannot explain to themselves in six months is one
  -- they will not trust when it hides a fact.
  note       text,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, tag)
);
