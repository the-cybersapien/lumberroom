-- Phase 6, step 1. The proposal store. Ingestion proposes and never writes.
--
-- Nothing in this migration touches the memory table's write path. A proposal becomes a memory in
-- exactly one way: the owner approves it and the handler calls services::write::run, which is what
-- keeps the credentials refusal, the ceiling check, the grant check, the tripwire, duplicate
-- collapse, the dedupe bands and supersession validation in one place. A second insert path would
-- put seven checks in one branch and none in the other.

-- The queue. One row per distinct fact, whatever number of transcripts stated it.
--
-- The unique constraint on (tenant_id, fingerprint) is the whole idempotency story. Re-proposing
-- the same content inserts a source row and touches nothing else, which turns "this preference
-- appeared 808 times" into one row with 808 sources rather than a similarity guess.
--
-- speaker, quote and auto are frozen at first insert and never upgraded. A fact first proposed as
-- main_model and later arriving from an owner_typed span gains a source row and stays queued: a row
-- the owner is reading in the queue must not write itself while he reads it. The per-source value
-- lives on ingest_proposal_source and `ingest show` reports the strongest speaker across sources.
CREATE TABLE IF NOT EXISTS ingest_proposal (
  id            uuid PRIMARY KEY,
  tenant_id     text NOT NULL,
  -- sha256 of the normalised content. The same function that produces
  -- recall_emission.content_sha256, because two normalisers give the echo check a hash that can
  -- never meet a proposal's.
  fingerprint   text NOT NULL,
  content       text NOT NULL,
  namespace     text NOT NULL,
  tags          text[] NOT NULL DEFAULT '{}',
  supersedes    uuid REFERENCES memory(id),
  speaker       text NOT NULL,
  -- The verbatim owner span, set only when speaker is owner_typed. A quote on any other speaker is
  -- a model asserting the owner said something, which is not evidence.
  quote         text,
  -- Passed the substring check against the frozen span. Computed by the server on the way in and
  -- never taken from the request: a client that could set this could approve its own writes.
  auto          boolean NOT NULL DEFAULT false,
  extractor     text NOT NULL,
  state         text NOT NULL DEFAULT 'proposed'
                CHECK (state IN ('proposed', 'rejected', 'written')),
  memory_id     uuid REFERENCES memory(id),
  -- The write refusal, rule name only. The tripwire's matched text never lands here: an error that
  -- echoes a secret puts it in whatever log, report or transcript reads the queue next.
  last_error    text,
  last_error_at timestamptz,
  decided_at    timestamptz,
  created_at    timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, fingerprint)
);

-- The queue read, which is the only read a person makes here: one state, newest first.
CREATE INDEX IF NOT EXISTS ingest_proposal_queue
  ON ingest_proposal (tenant_id, state, created_at DESC);

-- Where a proposal came from, one row per transcript entry that stated it.
--
-- source_key is file_path '#' entry_uuid and is the primary key half that makes a re-post
-- idempotent: the same entry re-read by a later run updates nothing and inserts nothing.
CREATE TABLE IF NOT EXISTS ingest_proposal_source (
  proposal_id  uuid NOT NULL REFERENCES ingest_proposal(id) ON DELETE CASCADE,
  source_key   text NOT NULL,
  file_path    text NOT NULL,
  session_id   text,
  is_sidechain boolean NOT NULL DEFAULT false,
  entry_uuid   text,
  speaker      text NOT NULL,
  observed_at  timestamptz,
  run_id       uuid NOT NULL,
  PRIMARY KEY (proposal_id, source_key)
);

-- "What did this run produce", which is the question a report answers.
CREATE INDEX IF NOT EXISTS ingest_proposal_source_run
  ON ingest_proposal_source (run_id);

-- Incrementality. A live transcript grows all day and reading it whole on every run reprocesses
-- everything already seen.
--
-- The identity is the file path. A session id spans several files, 562 of the 685 files measured
-- carried their parent's session id, and keying on it means the last file walked wins.
--
-- byte_offset always sits on a line boundary and only ever moves forward. The advance is
-- GREATEST(byte_offset, :new) in SQL, because a nightly run and an interactive one overlap on an
-- ordinary Tuesday and a plain assignment lets the older run finish last and drag the mark
-- backwards, which re-reads and re-proposes everything between the two ceilings.
--
-- prefix_sha256 catches what the offset cannot: a file rewritten or truncated in place. It belongs
-- to whichever offset won, so it moves under the same guard or the two disagree.
CREATE TABLE IF NOT EXISTS ingest_watermark (
  tenant_id     text NOT NULL,
  file_path     text NOT NULL,
  session_id    text,
  is_sidechain  boolean NOT NULL DEFAULT false,
  byte_offset   bigint NOT NULL DEFAULT 0,
  prefix_sha256 text NOT NULL DEFAULT '',
  entries_seen  bigint NOT NULL DEFAULT 0,
  -- Set once when a run recognises a file it created itself, cleared only by hand through unskip.
  skip_reason   text,
  skip_run_id   uuid,
  -- The byte range one ingest conversation occupied in this file, written by the plan that closed
  -- the fence. Entries inside it are dropped so a run cannot eat its own output.
  fence_from    bigint,
  fence_until   bigint,
  fence_run_id  uuid,
  last_run_id   uuid,
  updated_at    timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, file_path)
);

-- The skipped list is a report the owner reads before wondering why a project produced nothing.
CREATE INDEX IF NOT EXISTS ingest_watermark_skipped
  ON ingest_watermark (tenant_id) WHERE skip_reason IS NOT NULL;

-- The run record. Every exclusion is counted by the rule that made it, because an exclusion with no
-- counter is an exclusion nobody finds.
--
-- traversal_capped earns its column on its own: silent partial coverage of a corpus reads exactly
-- like complete coverage, so nobody may read "41 proposals from one week" as "everything one week
-- held". files_held_back names the files whose watermark refused to advance, which is the only
-- place the owner learns that bytes are still pending.
CREATE TABLE IF NOT EXISTS ingest_run (
  id                   uuid PRIMARY KEY,
  tenant_id            text NOT NULL,
  started_at           timestamptz NOT NULL DEFAULT now(),
  finished_at          timestamptz,
  scope                jsonb NOT NULL DEFAULT '{}',
  extractor            text NOT NULL,
  files_seen           int NOT NULL DEFAULT 0,
  files_skipped        jsonb NOT NULL DEFAULT '{}',
  entries_seen         bigint NOT NULL DEFAULT 0,
  entries_excluded     jsonb NOT NULL DEFAULT '{}',
  unknown_types        jsonb NOT NULL DEFAULT '{}',
  spans_cut            int NOT NULL DEFAULT 0,
  chunks               int NOT NULL DEFAULT 0,
  chunks_missing       int NOT NULL DEFAULT 0,
  chunks_failed        int NOT NULL DEFAULT 0,
  files_held_back      jsonb NOT NULL DEFAULT '[]',
  fenced_entries       int NOT NULL DEFAULT 0,
  fences_unclosed      int NOT NULL DEFAULT 0,
  proposals_new        int NOT NULL DEFAULT 0,
  proposals_reinforced int NOT NULL DEFAULT 0,
  confirmations        int NOT NULL DEFAULT 0,
  traversal_capped     boolean NOT NULL DEFAULT false,
  artifact_sessions    jsonb NOT NULL DEFAULT '[]'
);

CREATE INDEX IF NOT EXISTS ingest_run_recent ON ingest_run (tenant_id, started_at DESC);

-- What the store handed out. Content the store emitted cannot come back to it as a new fact.
--
-- The key is deliberately not a session id, and the first version of this table had it wrong. Ctx
-- carries a session id only when a client sends x-session-id, and nothing sends it: Claude Code's
-- MCP client attaches no per-session header and the bootstrap hook discards the stdin JSON that
-- carries Claude Code's own. Every row would have held a null against a key that required one and
-- the layer would have fired never. The two id spaces also differ, so the join could not have
-- matched even with the header in place.
--
-- So the check is tenant-wide on content hash inside a time window, and it never touches a session
-- id. The loop being guarded does not care which session the echo happened in. session_id stays as
-- a nullable diagnostic, ready for the day a client sends the header; no query filters on it and
-- nothing joins on it.
--
-- content_sha256 is the same function of the same input as ingest_proposal.fingerprint, which is
-- the only reason a proposal can ever match an emission.
CREATE TABLE IF NOT EXISTS recall_emission (
  tenant_id        text NOT NULL,
  content_sha256   text NOT NULL,
  memory_id        uuid NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
  tool             text NOT NULL,
  session_id       text,
  first_emitted_at timestamptz NOT NULL DEFAULT now(),
  last_emitted_at  timestamptz NOT NULL DEFAULT now(),
  emit_count       bigint NOT NULL DEFAULT 1,
  PRIMARY KEY (tenant_id, content_sha256, memory_id, tool)
);

-- The check runs per candidate fact against a hash and a time bound, so the index carries both. A
-- fact read a thousand times stays one row, because a repeat emission bumps the count through
-- ON CONFLICT rather than inserting.
CREATE INDEX IF NOT EXISTS recall_emission_lookup
  ON recall_emission (tenant_id, content_sha256, first_emitted_at);
