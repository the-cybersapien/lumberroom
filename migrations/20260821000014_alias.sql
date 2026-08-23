-- Names that denote the same thing.
--
-- Additive and forward only, like every migration here: it creates one table and one index and
-- touches nothing that exists. An older binary keeps booting against a store that has run this.
--
-- Supersession is the wrong tool for a rename. A project called Butler, then Clara, then Sivella
-- leaves every Butler fact true and about the same subject, so retiring those rows would destroy
-- history and hide facts that still hold. This table says two names are one subject, which is a
-- different claim from "this fact was replaced" and needs its own rows.
--
-- `registry_alias` from migration 006 is this idea one layer down, keyed by (kind, key) inside the
-- registry. This generalises it to any name in a namespace and adds valid time, so the store knows
-- Butler was the current name until a date rather than only that it was ever a name.
CREATE TABLE IF NOT EXISTS entity_alias (
  tenant_id  text NOT NULL DEFAULT 'me',
  namespace  text NOT NULL,
  -- Lowercased. A person types "Butler" and the fact that mentions it says "butler", and matching
  -- on exact bytes would fail on that pair and defeat the whole feature. The adapter lowercases on
  -- every write and every lookup; this comment is the reason a reader must not store mixed case
  -- here by hand.
  alias      text NOT NULL,
  -- The name every alias in a group resolves to. A canonical name is never itself an alias: the
  -- adapter repoints the group when a canonical name is renamed, so a group is one hop deep and a
  -- lookup terminates without a recursion limit.
  canonical  text NOT NULL,
  -- Half-open, [since, until), the same convention migration 011 gave a memory's valid time. The
  -- name was current from `since` and stopped being current at `until`. NULL on the left means no
  -- known start; NULL on the right means the name is still current.
  since      timestamptz,
  until      timestamptz,
  -- `manual` when the owner stated the alias, `derived` when something read it out of a fact.
  origin     text NOT NULL DEFAULT 'manual' CHECK (origin IN ('manual', 'derived')),
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, namespace, alias),
  -- The floor of the one-hop invariant, enforced here so a writer in psql cannot lay down the
  -- trivial cycle the adapter refuses.
  CONSTRAINT entity_alias_not_self CHECK (alias <> canonical),
  -- Two scalars accept an inverted period in silence where a range constructor would reject it,
  -- and every reader downstream would then see a name that stopped being current before it started.
  CONSTRAINT entity_alias_period CHECK (since IS NULL OR until IS NULL OR since <= until)
);

-- The group lookup: every alias that resolves to one canonical name. The hot read expands a query
-- over a whole group, so this index is what keeps that to one indexed scan.
CREATE INDEX IF NOT EXISTS entity_alias_canonical
  ON entity_alias (tenant_id, namespace, canonical);
