# 16. The audience on an access token is checked where the token becomes a principal

Status: accepted, implemented. 8 September 2026.

## Decision

`OpaqueTokenAuthenticator::authenticate` compares the resource stored on an access token against the
resource this deployment serves, and refuses the token when they disagree. The comparison
canonicalises both sides per RFC 3986 §6.2.2 rather than comparing strings. `/oauth/token` stamps
this deployment's own resource on any token whose client named none, and refuses `invalid_target`
for one that named a resource this server does not serve. `OAUTH_RESOURCE_AUDIENCE` selects
`off`, `lenient` or `strict`, and defaults to `lenient`, which admits a token carrying no resource
at all.

## The context that forced it

The `resource` indicator has been written down since the authorization server shipped. It is parsed
at `/authorize`, validated as an absolute URI in `AuthorizeRequest::validate`, stored on
`oauth_code`, carried across the code exchange, stored on `oauth_token`, and read back into
`AccessTokenRecord.resource`. Then nothing reads it. Every surface in this repository authenticates
through one `Authenticator` chain, and the link that spends an opaque token walked past the field.

So a token minted for one resource authenticated against another. RFC 8707 exists to stop exactly
that, and this server did the bookkeeping and skipped the check. Two smaller holes sat beside it.
The token endpoint took the `resource` form field verbatim onto the token row whenever the code
carried none, with no URI validation anywhere on that path, so a client could store arbitrary text
as its audience. And the code-exchange comparison used `!=` on raw strings, which reads
`https://host/mcp` and `https://host/mcp/` as two different audiences.

## Why the check is in the authenticator and not on the routes

The MCP transport, the HTTP routes and the console all resolve a caller through
`adapters::auth::create`, and every mode in that chain produces one `Principal`. Putting the
comparison there means a new surface inherits it by existing. Putting it on each route means a new
route inherits nothing, and the failure is silent: the surface works, and the audience it was
supposed to check is the thing nobody notices is missing. The same argument already decided where
the sensitivity filter runs.

The check sits after expiry and revocation and before the client lookup, so a token for the wrong
audience never costs a second query.

## What happens to a token with no resource, and why

Every access token issued before this landed carries a NULL resource. So does every token the CLI in
this repository holds after its first refresh: `crates/lumberroom/src/client.rs` sends
`grant_type`, `refresh_token` and the client credentials on the refresh grant, and no `resource`, so
each rotation re-mints a token with no audience. `oauth_refresh` has no resource column, so the
rotated token has nothing to inherit either.

Refusing all of those is a self-inflicted outage on the deploy, and it lands on every live client at
once, including this project's own MCP server and CLI. Accepting them forever is a check that never
takes effect, because the CLI would keep manufacturing fresh NULLs on every refresh.

Neither, then. The token endpoint stamps this deployment's resource on anything the client did not
name, which is correct rather than invented: this server serves one resource, it is the resource the
client is already talking to, and it is the value the client would have discovered from the RFC 9728
document. That turns the NULLs into a draining population rather than a standing one. Every token
minted after the deploy carries an audience, so after one `OAUTH_ACCESS_TTL_SECS` no live token has
a NULL, and `OAUTH_RESOURCE_AUDIENCE=strict` becomes safe to set.

`lenient` is the default because the deploy itself has to be survivable. `strict` is the end state
and the operator flips it, one access-token lifetime later.

## What was considered, and why each lost

**Reject NULL from the start.** One line, no setting, no migration path. It logs out every client
holding a token issued by any earlier build, which on this deployment is all of them. A security fix
whose first act is a full outage gets reverted, and a reverted fix protects nothing.

**Accept NULL forever, with no setting.** The check would then only ever refuse a token that named
the wrong resource, and no client names the wrong resource by accident. It closes the replay case
and leaves the downgrade case open: a client that omits `resource` everywhere gets a token that
skips the check for its whole life. Combined with the stamp at issuance this would almost be
sufficient, but it leaves no way to assert the invariant, and an invariant nobody can turn on is a
comment.

**Backfill the NULLs in a migration.** `UPDATE oauth_token SET resource = ... WHERE resource IS
NULL` would make `strict` deployable immediately. It also writes an audience that was never asserted
at issuance, converting an unknown into a claim, and it entrenches whatever `PUBLIC_URL` happened to
say on the day it ran. The draining population gets to the same place within one token lifetime
without fabricating anything.

**Compare the strings.** Cheapest, and wrong in four ways a real client hits: scheme case, host
case, an explicit default port, and a trailing slash. Each reads as a different audience under `==`,
and the resulting refusal tells the operator nothing about which of two identical-looking strings to
change. `canonical_resource` runs `Url::parse`, which does RFC 3986 syntax-based normalisation, and
then strips a trailing slash from the path.

That last part is a deliberate departure. RFC 3986 does not make `/mcp` and `/mcp/` equivalent, and
for a document they are not. A resource indicator names a deployment, the difference identifies
nothing, and refusing on it is an outage nobody can read off the error. Path case stays significant,
because normalisation lowercases the scheme and the host and nothing else, and going further would
admit a resource the operator did not configure.

## What it costs, accepted

A deployment reachable under two hostnames, whose clients discover different resource URLs, now has
one of those hostnames refusing tokens. `off` is the escape hatch and it exists for that case.
`lenient` is weaker than the spec allows for as long as an operator leaves it there, and nothing in
the server nags about it.

`canonical_resource` returning `None` refuses the token. A resource the `url` crate cannot parse
therefore matches nothing, including an identical copy of itself. That is the intended direction of
the failure and it means a future `url` release that tightens parsing could refuse tokens that
worked yesterday.

## What this is not for

It does not authorize anything. A token that names the right resource still holds only the grant on
its client row, and the audience check refuses tokens rather than narrowing them. It says nothing
about static `AUTH_TOKENS` grants or `AUTH_MODE=oidc` JWTs: neither carries a resource indicator,
the OIDC path has its own `audience` check on the `aud` claim, and a static token is a line in the
environment with no issuance path to bind.

## Reversal condition

`OAUTH_RESOURCE_AUDIENCE=off` turns the comparison off without a redeploy of anything but the
environment. Take it if a legitimate deployment shape turns out to be one this cannot express, and
say which shape in an amendment here. The stamp at issuance has no switch, because a token that
records the audience it was minted for is correct whether or not anything checks it.
