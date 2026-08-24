# OAuth deployment runbook

`AUTH_MODE=oauth` turns on the built-in authorization server (decision 0002): the box you deploy
becomes its own OAuth 2.1 issuer, with no external identity provider. It never issues a token to
itself and never talks to a third party. It validates its own tokens against `oauth_client`,
`oauth_code`, `oauth_token` and `oauth_refresh` in the same Postgres database everything else
lives in.

Use this instead of `deploy/logto.md` unless you specifically want an external issuer. Static
bearer tokens (`AUTH_TOKENS`) are honoured in every mode, so the CLI, hooks, Hermes and OpenWebUI
keep working exactly as before when you turn this on for the browser surfaces that need it.

---

## 1. Generating the two secrets

`./deploy/install.sh --domain <fqdn> --auth-mode oauth` does both of these for you: it prompts
for the owner password on the terminal, builds the image, hashes the password inside a throwaway
container, and writes the result into `.env` alongside a freshly generated cookie secret. This
section is what it runs, for when you need to do it by hand: rotating the password, restoring
`.env` on a second box, or running non-interactively.

**The owner password hash.** The password itself is never stored, only its Argon2id hash:

```bash
read -rs OWNER_PW; echo
printf '%s' "$OWNER_PW" | docker compose run --rm -T server lumberroom-server hash-password
unset OWNER_PW
# reads the password from stdin, prints a $argon2id$... hash to stdout
```

`read -rs` masks the input so it never lands in shell history or a terminal scrollback, and `-T`
on `run` disables pseudo-tty allocation so the piped stdin reaches the container cleanly.

Put the result in `.env` as `OWNER_PASSWORD_HASH`, **single-quoted**. An Argon2 PHC string is
full of `$` characters, and an unquoted value gets mangled the instant anything sources `.env`
with `sh` instead of Docker Compose's own parser (`deploy/backup.sh` used to do exactly this; see
the comment on `AUTH_TOKENS` in `.env.example` for the general shape of the trap).

**The cookie secret**, at least 32 characters, signs the consent-screen session cookie:

```bash
openssl rand -hex 32
```

Put it in `.env` as `OAUTH_COOKIE_SECRET`.

The server validates both at boot, along with `PUBLIC_URL` being `https://` (or a loopback
address for local testing). Missing either is a startup failure with a message naming the exact
setting, not a runtime surprise on the first login attempt.

## 2. What the consent screen asks

1. **The owner logs in** with the password behind `OWNER_PASSWORD_HASH`. Failed attempts are
   rate-limited per `OAUTH_LOGIN_ATTEMPTS_PER_MINUTE` (default 5/minute).
2. **The screen shows the requesting client**: its name, its redirect URI, and whether it was
   manually issued or arrived through self-registration (RFC 7591 DCR, on by default via
   `OAUTH_DCR_ENABLED`). Registration alone holds nothing: a self-registered client exists in
   `oauth_client` with `consented_at` null and no grant, and every token request against it fails
   until this step happens.
3. **The owner picks a grant profile** (below), preselected to `OAUTH_DEFAULT_PROFILE`
   (default `standard`). Approving writes the profile's namespace grant into that client's row and
   stamps `consented_at`, which is what makes the client live.
4. The client is redirected back with an authorization code (PKCE S256 only; a client that omits
   `code_challenge_method=S256` or sends `plain` is refused before it reaches this screen at all).

## 3. The three grant profiles

Defined once, in `src/domain/oauth.rs`, and applied identically to read and write. Phase 2 spec
§3 starts grants symmetric, and the asymmetry that matters is the sensitivity ceiling both axes
already carry, not a read/write split.

| Profile | Namespaces | Sensitivity ceiling | Delete | Registry write | Sealed-capable |
|---|---|---|---|---|---|
| **full** | every namespace (`*`) | sealed | yes | yes | yes |
| **standard** | `user:me`, `global`, `project:*` | open | no | no | no |
| **narrow** | `user:me`, `global` | open | no | no | no |

In plain language: **full** is the owner's own client, everything including sealed content, and
the only profile that can delete a memory or touch the registry. **standard** adds project notes
to the owner's own notes and shared facts, but stops at `open`: no private or sealed content, no
matter what the client asks for. **narrow** drops project notes too, leaving the owner's own notes
and shared facts. Phase 2 spec §3's starting-grant table maps ChatGPT to `narrow` and the
Claude.ai family to `standard` as sensible defaults; the owner still chooses per client at consent
time, and nothing stops picking `full` for a second Claude Code install the way Phase 1 did with
`AUTH_TOKENS`.

The static-token grant `.env.example` and `install.sh` write for the owner's own client is `full`
with one difference: `mayDelete` is false, so `memory_forget` stays out of that client's tool list.
`full` here answers a consent screen the owner is looking at, while the static grant is what a model
gets handed at every startup. The flag gates `lumberroom forget` as well, so a client with it off deletes
by no route at all; `.env.example` carries the two ways to change that.

A grant made here takes effect immediately and survives a restart without editing `.env`. This
is the fix for the Phase 1 problem where every grant change needed a redeploy.

## 4. Revoking or changing a client's access

Open `/console/clients` in a browser, signed in as the owner. Every live client's card carries a
Revoke button, one click and no confirmation, and a "Change access" editor beside it that writes a
narrower or wider grant without revoking the client at all. A tightened grant is often what you
actually want: it keeps the client's existing token working while cutting what it can reach, rather
than forcing it through registration and consent again. Both apply on the client's next call:
nothing reconnects and no token is reissued. `docs/permissions.md` covers the editor itself, the
scope picker and the advanced view.

```bash
docker compose exec -T server lumberroom clients
```

`lumberroom clients` lists every OAuth row: how it registered (`dcr` or `manual`), whether the owner
has consented, and its current profile. It does not revoke or change anything; it is how you find
the `client_id` for a card you cannot reach in the console, or when scripting from a terminal with
no browser. The consent screen itself points here ("You can change or revoke this later with
`lumberroom clients`"), which is accurate for finding the client and stops short of the action
itself.

Without browser access to `/console`, revoking by hand is a last resort:

```bash
docker compose exec -T db psql -U lumberroom -d lumberroom -c \
  "SELECT client_id, client_name, profile, registered_via, consented_at, last_used_at FROM oauth_client WHERE revoked_at IS NULL ORDER BY created_at DESC;"
docker compose exec -T db psql -U lumberroom -d lumberroom -c \
  "UPDATE oauth_client SET revoked_at = now() WHERE client_id = '<client_id>' AND revoked_at IS NULL;"
```

Revoking kills the client and every access and refresh token it holds in one step:
`oauth_token` and `oauth_refresh` both cascade off `oauth_client(client_id)`, so there is no second
table to remember to clear. An in-flight access token is checked against `oauth_token.revoked_at` on
every request, so revocation is effective on the next request, not on next token expiry.

`/admin/whoami`, called with any credential (a static token or an OAuth access token, same as
`/mcp`), answers what that credential currently resolves to, client and read/write lists and the
registry-write flag, straight from the code path that enforces it. Calling it with a just-revoked
client's access token is the fastest way to confirm a revoke actually took.

## 5. Per-surface connection steps

Background and the full surface table: `docs/specs/phase-2-surfaces.md` §1–2. This section is the
short version for connecting each one once the server is up.

**Claude Code (both installs), Hermes, OpenWebUI** stay on static bearer tokens; they do not
need `AUTH_MODE=oauth` at all. Give each a distinct entry in `AUTH_TOKENS` and skip to
`client/wire-mac.sh` (Claude Code) or that surface's own connector settings (Hermes, OpenWebUI),
pointing at `https://<domain>/mcp` with `Authorization: Bearer <token>`.

Write each entry's ceilings out in the object form, `{"namespace":"*","max":"sealed"}`. There is no
profile name on this path, and the three ways to write a grant list do not mean the same thing: a
bare `"*"` stops at `open`, an omitted list means every level, and the object form means what you
typed. The comment on `AUTH_TOKENS` in `.env.example` has the whole rule.

**Claude.ai web and Cowork.** These two are **indistinguishable from this server**: same
connector infrastructure, same OAuth callback, no signal in the request tells them apart. Decide
**before** connecting either one whether they need different grants:

- If one grant is fine for both, register one connector and consent once.
- If Cowork's autonomous sessions need a narrower grant than interactive Claude.ai chat, you must
  register **two separate connectors** (two client registrations, two consents, two grants) and
  treat them as two clients from the start. Retrofitting this later means re-adding the connector
  by hand on every device that uses it. Decide now, before issuing the first credential.

Connect: in Claude.ai, add a custom connector pointing at `https://<domain>/mcp`. It discovers
`/.well-known/oauth-protected-resource`, redirects to `/authorize`, and you see the consent screen
from §2 in a browser. Pick a profile (Phase 2 spec §3 suggests `standard`) and approve.

**Claude.ai mobile** rides on the same account-synced connector once web is connected, with nothing
separate to configure.

**ChatGPT.** Do the ten-minute hands-on check before wiring this up for real: log into a personal
Plus/Pro account, Settings → Developer Mode, add a custom connector against a throwaway endpoint
with a plain `Authorization: Bearer` header. That single attempt settles whether ChatGPT accepts
static headers at all (it may not need OAuth) and confirms Developer Mode's tier requirement
before you spend time on the OAuth path. If it needs OAuth: same connector flow as Claude.ai,
pointing at `https://<domain>/mcp`; Phase 2 spec §3 suggests `narrow` as the starting profile,
and ChatGPT is the surface the system PRD is most explicit about keeping narrow. ChatGPT mobile is
unverified; repeat the same ten-minute check there before relying on it.

**Before any of the Claude.ai-family or ChatGPT steps**, also email `mcp-review@anthropic.com` for
the static-header beta if you have not. A grant there means four surfaces can skip OAuth
entirely, and it costs nothing to ask in parallel with setting this up.

## 6. What the server has to get right (and how to tell if it didn't)

From Phase 2 spec §2, the failures worth checking for by hand because they are silent otherwise:

- An unauthenticated request must come back `401` with a `WWW-Authenticate` header carrying the
  resource-metadata pointer, not a `200` with an error body. Claude Code's own fallback probing
  masks this class of bug: a green result from Claude Code proves nothing about Claude.ai or
  ChatGPT. Test against the real client, not just the CLI.
- `/register` takes JSON, `/token` takes form encoding. A client that gets a 415 from `/token`
  while `/register` succeeded reads as almost-working; it is actually completely broken.
- `/authorize` must advertise (and enforce) `S256` PKCE. `AuthorizeRequest::validate` refuses a
  missing or non-`S256` challenge method rather than falling back to `plain`.
- Ten seconds for discovery, registration and token calls. A cold start otherwise fails
  intermittently with no useful client-side error.
- IPv4, publicly routable, no CGNAT or private host. Localhost only ever serves Claude Code.

## Rolling back

Set `AUTH_MODE=token` (or `oidc`), restart. Static-token clients were never affected; browser
clients that were mid-session lose their connection and need to reconnect once you switch back.
