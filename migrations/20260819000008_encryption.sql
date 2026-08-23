-- Phase 3, step 3. Encryption columns, nullable. Nothing populates them yet.
--
-- private is per-row envelope encryption: a fresh 256-bit DEK per row, content sealed with
-- AES-256-GCM, the DEK wrapped by a KEK that never touches this disk. Per-row grain makes a delete
-- a crypto-shred and makes KEK rotation cheap.
--
-- The embedding stays plaintext, because search has to work. That leaks the gist of a private row
-- to anyone holding the database, which is a claim we can defend and do defend in writing
-- (docs/research/encryption-and-sensitivity.md). It is not a claim to make silently.

-- A private row has no plaintext content, so the column cannot stay NOT NULL. The CHECK replaces
-- the guarantee it was providing: exactly one representation is present.
ALTER TABLE memory ALTER COLUMN content DROP NOT NULL;

ALTER TABLE memory ADD COLUMN IF NOT EXISTS content_ct   bytea;
ALTER TABLE memory ADD COLUMN IF NOT EXISTS content_nonce bytea;
ALTER TABLE memory ADD COLUMN IF NOT EXISTS dek_wrapped  bytea;
ALTER TABLE memory ADD COLUMN IF NOT EXISTS dek_nonce    bytea;
ALTER TABLE memory ADD COLUMN IF NOT EXISTS enc_alg      text;
-- Which KEK wrapped this row's DEK. Rotation rewraps and updates this; without it a rotation is
-- indistinguishable from data loss.
ALTER TABLE memory ADD COLUMN IF NOT EXISTS kek_id       text;

ALTER TABLE memory DROP CONSTRAINT IF EXISTS memory_content_representation;
ALTER TABLE memory ADD CONSTRAINT memory_content_representation CHECK (
  (content IS NOT NULL AND content_ct IS NULL)
  OR
  (content IS NULL AND content_ct IS NOT NULL AND dek_wrapped IS NOT NULL
     AND content_nonce IS NOT NULL AND dek_nonce IS NOT NULL AND enc_alg IS NOT NULL)
);

-- Step 4 of the Phase 3 migration order, as a constraint rather than a note: do not write an
-- encrypted row until a restart has proved the key can be recovered. The server verifies the
-- fingerprint at boot and refuses the first encrypted write if it cannot.
--
-- The fingerprint is an HMAC of a fixed label under the KEK, so it identifies the key without
-- being derived from it in a way that helps an attacker.
CREATE TABLE IF NOT EXISTS kek_state (
  tenant_id    text PRIMARY KEY DEFAULT 'me',
  kek_id       text NOT NULL,
  fingerprint  text NOT NULL,
  provider     text NOT NULL,
  verified_at  timestamptz NOT NULL DEFAULT now()
);

-- sealed. The server holds no key and can never read these, including under full compromise.
--
-- Keyed by an HMAC of the canonical name computed client-side, so the server cannot enumerate what
-- is stored either. Not searchable, retrievable only by exact key: that is the whole point of the
-- level, and pretending otherwise would be the failure mode.
CREATE TABLE IF NOT EXISTS sealed_item (
  tenant_id     text NOT NULL DEFAULT 'me',
  namespace     text NOT NULL,
  key_hmac      text NOT NULL,
  -- Opaque to this server. Format and cipher are the client's business; alg is recorded so a
  -- client can tell whether it can read a blob before trying.
  ciphertext    bytea NOT NULL,
  alg           text NOT NULL,
  source_client text NOT NULL,
  created_at    timestamptz NOT NULL DEFAULT now(),
  updated_at    timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, namespace, key_hmac)
);
