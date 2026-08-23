# Switching auth to Logto

Phase 1 ships with bearer tokens because they get the loop running the same evening. The PRD's
target is Logto validating every request. Both modes are implemented; this is the switch.

The server is an OAuth resource server in both directions. It never issues, refreshes, or stores
tokens, so nothing here is hand-rolled OAuth.

## 1. DNS and certificates

Point two more names at the box, for example `auth.example.com` and `auth-admin.example.com`.
Uncomment the two blocks at the bottom of `deploy/Caddyfile` and set `LOGTO_DOMAIN` and
`LOGTO_ADMIN_DOMAIN` in `.env`.

## 2. Bring Logto up

Logto keeps its own database on the same Postgres instance:

```bash
docker compose exec -T db psql -U lumberroom -d lumberroom -c "CREATE DATABASE logto"
docker compose --profile logto --profile tls up -d
docker compose logs -f logto      # first boot seeds the schema
```

Open `https://auth-admin.example.com` and create the admin account.

## 3. Register lumberroom as an API resource

In the Logto console:

1. **API resources**, create one. Identifier: `https://memory.example.com/mcp`. That string is
   the audience, and it must match `MCP_RESOURCE_URL` exactly.
2. Add a scope, for example `memory.rw`. Put it on a role you assign to your own account, and take
   it off the role Logto hands new sign-ups. A scope carried by the default role admits anyone who
   registers.
3. **Applications**, create a native or SPA application for Claude Code and note the client id.
4. **Sign-in experience**, turn sign-up off. The application id is public: it sits in
   `~/.claude.json` and in the address bar of every sign-in, and a loopback redirect needs no
   server, so an account a stranger can create is an account that can run the flow against it.
5. **Users**, open your own account and copy the user id. That string is the `sub` claim, and step
   4 below is where the server learns to accept it and nothing else.

## 4. Point the server at it

In `.env`:

```ini
AUTH_MODE=oidc
MCP_RESOURCE_URL=https://memory.example.com/mcp
OIDC_ISSUER=https://auth.example.com/oidc
OIDC_AUDIENCE=https://memory.example.com/mcp
OIDC_JWKS_URI=https://auth.example.com/oidc/jwks
OIDC_REQUIRED_SCOPES=memory.rw
OIDC_ALLOWED_SUBJECTS=usr_1a2b3c4d5e
```

`OIDC_ALLOWED_SUBJECTS` is the line that authorizes a person. Everything above it establishes which
application the token was minted for, and grants hang off that application id. Leave the list empty
and the server refuses to boot, because an empty list would mean every account the issuer holds.
List one `sub` per account you want in, comma separated.

Keep `AUTH_TOKENS` in place, with the `client` names matching the Logto application ids you want
to grant namespaces to. In `oidc` mode the token field is ignored and the entry becomes the
namespace grant for that client. An empty list is refused at boot: a client with no entry is
refused rather than defaulted, so an empty list would leave the server answering nobody.

```bash
docker compose up -d server
curl -s https://memory.example.com/.well-known/oauth-protected-resource | jq
```

That metadata document is how an MCP client discovers where to authenticate. A rejected token now
comes back as 401 with `WWW-Authenticate: Bearer error="invalid_token", resource_metadata="..."`.

## 5. Re-wire the client

```bash
claude mcp remove memory --scope user
claude mcp add --transport http memory https://memory.example.com/mcp \
  --scope user --client-id <logto application id>
```

Claude Code runs the OAuth flow in a browser and stores the token. Check with `/mcp` in a
session, then `node bin/lumberroom.mjs doctor` still works if you keep one token client configured
for operations.

## Rolling back

Set `AUTH_MODE=token`, restart, re-run `wire-mac.sh` with the bearer token. The two modes share
the same principal and namespace grant path, so nothing about stored memory changes.
