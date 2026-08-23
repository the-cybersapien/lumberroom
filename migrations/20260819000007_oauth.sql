-- Decision 0002. The built-in OAuth 2.1 authorization server.
--
-- Access tokens are opaque and stored hashed. Two reasons, both practical: revocation is a single
-- UPDATE that takes effect on the next request, which a self-signed JWT cannot offer without a
-- revocation list that is the same table anyway; and there is no signing key to manage, rotate or
-- leak. One indexed lookup per request is well inside the bootstrap latency budget.
--
-- Nothing here stores a token, a code or a secret in a form that reading this table recovers.

CREATE TABLE IF NOT EXISTS oauth_client (
  client_id      text PRIMARY KEY,
  -- Public clients authenticate with PKCE alone and have no secret. Confidential clients store a
  -- hash. Either way the column never holds the credential itself.
  secret_hash    text,
  client_name    text NOT NULL DEFAULT 'unnamed client',
  -- Compared exactly at both /authorize and /token. Never prefix-matched: a prefix match on a
  -- redirect URI is an open redirect with extra steps.
  redirect_uris  text[] NOT NULL,
  grant_types    text[] NOT NULL DEFAULT '{authorization_code,refresh_token}',
  software_id    text,
  software_version text,
  -- 'dcr' for RFC 7591 self-registration, 'manual' for a credential the owner issued. Registration
  -- is not authorization: a DCR client exists but holds nothing until the owner consents, which
  -- requires the owner's password.
  registered_via text NOT NULL DEFAULT 'dcr' CHECK (registered_via IN ('dcr', 'manual')),

  -- The grant, assigned by the owner at the consent screen and editable afterwards without a
  -- restart. This is the fix for Phase 1's "grants live in AUTH_TOKENS and change on restart".
  -- Each entry is {"namespace": "<glob>", "max": "open|private|sealed"}.
  grant_read     jsonb NOT NULL DEFAULT '[]'::jsonb,
  grant_write    jsonb NOT NULL DEFAULT '[]'::jsonb,
  -- The registry holds credential locations, so writing to it stays an operator action.
  registry_write boolean NOT NULL DEFAULT false,
  -- A property of the client, not of the grant: it asserts the client can decrypt locally. A
  -- client without it may hold a sealed ceiling and still only ever receive ciphertext.
  sealed_capable boolean NOT NULL DEFAULT false,
  -- A model that can silently delete memories is a worse failure than one that hoards them.
  may_delete     boolean NOT NULL DEFAULT false,
  -- Null until the owner has approved this client at least once. No consent, no token.
  consented_at   timestamptz,
  profile        text,

  created_at     timestamptz NOT NULL DEFAULT now(),
  last_used_at   timestamptz,
  revoked_at     timestamptz
);

CREATE INDEX IF NOT EXISTS oauth_client_live ON oauth_client (created_at DESC) WHERE revoked_at IS NULL;

-- Authorization codes: single use, short lived, bound to one redirect URI and one PKCE challenge.
CREATE TABLE IF NOT EXISTS oauth_code (
  code_hash      text PRIMARY KEY,
  client_id      text NOT NULL REFERENCES oauth_client(client_id) ON DELETE CASCADE,
  redirect_uri   text NOT NULL,
  -- S256 only. plain is not accepted, and the metadata advertises only S256 because newer clients
  -- refuse to proceed without code_challenge_methods_supported.
  code_challenge text NOT NULL,
  scope          text NOT NULL DEFAULT '',
  -- RFC 8707. The audience the resulting token is bound to.
  resource       text,
  expires_at     timestamptz NOT NULL,
  -- Set on first exchange. A second exchange of the same code is a replay: it fails and revokes
  -- everything already issued from it.
  consumed_at    timestamptz,
  created_at     timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS oauth_code_expiry ON oauth_code (expires_at);

CREATE TABLE IF NOT EXISTS oauth_token (
  token_hash   text PRIMARY KEY,
  client_id    text NOT NULL REFERENCES oauth_client(client_id) ON DELETE CASCADE,
  scope        text NOT NULL DEFAULT '',
  resource     text,
  -- Refresh rotation issues a new access token in the same family; revoking the family revokes
  -- every token descended from one authorization.
  family_id    uuid NOT NULL,
  expires_at   timestamptz NOT NULL,
  revoked_at   timestamptz,
  created_at   timestamptz NOT NULL DEFAULT now(),
  last_used_at timestamptz
);

CREATE INDEX IF NOT EXISTS oauth_token_client ON oauth_token (client_id, created_at DESC);
CREATE INDEX IF NOT EXISTS oauth_token_family ON oauth_token (family_id);
CREATE INDEX IF NOT EXISTS oauth_token_expiry ON oauth_token (expires_at) WHERE revoked_at IS NULL;

CREATE TABLE IF NOT EXISTS oauth_refresh (
  token_hash  text PRIMARY KEY,
  client_id   text NOT NULL REFERENCES oauth_client(client_id) ON DELETE CASCADE,
  family_id   uuid NOT NULL,
  expires_at  timestamptz NOT NULL,
  -- Rotation: exchanging a refresh token consumes it and issues a successor. Presenting a consumed
  -- one means it leaked, so the whole family dies rather than the single token.
  consumed_at timestamptz,
  revoked_at  timestamptz,
  created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS oauth_refresh_family ON oauth_refresh (family_id);

-- Per-client session correlation, so "did this surface read before it answered" is answerable.
-- Phase 2 §5 needs it and nothing in Phase 1 recorded it.
ALTER TABLE tool_calls ADD COLUMN IF NOT EXISTS session_id text;
ALTER TABLE tool_calls ADD COLUMN IF NOT EXISTS namespace  text;
CREATE INDEX IF NOT EXISTS tool_calls_client_session
  ON tool_calls (client, session_id, created_at DESC);
