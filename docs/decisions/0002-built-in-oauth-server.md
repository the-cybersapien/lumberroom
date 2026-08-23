# 2. Build the OAuth 2.1 authorization server into lumberroom

**Date:** 19 August 2026 · **Status:** accepted · **Decided by:** the owner

## Decision

lumberroom issues its own OAuth tokens. `AUTH_MODE=oauth` turns on authorization-server discovery
(RFC 8414), dynamic client registration (RFC 7591), PKCE with S256 as the only accepted method, an
owner login with a consent screen, and opaque access tokens stored as hashes. `AUTH_MODE=oidc`
stays as it is and validates an external issuer's JWTs.

## The context that forced it

The owner's requirement is an OAuth-enabled MCP server that comes up under `docker compose up`,
with no identity tenant to configure first. [`docs/specs/phase-2-surfaces.md`](../specs/phase-2-surfaces.md)
§2 settles the other half. A static bearer header reaches Claude Code, Hermes and OpenWebUI and
nothing else; the Claude.ai family, Cowork, mobile and probably ChatGPT need an authorization server
they can discover and register with. OAuth stopped being optional at that line.

## Where this departs from the spec

The Phase 2 spec says "Logto is the Phase 2 baseline, not a contingency", and
[`docs/research/client-capabilities.md`](../research/client-capabilities.md) §A concludes the same.
This decision supersedes both. What that research settled was *which surfaces force an
authorization server*, and that finding stands untouched. Whose authorization server it should be
was never examined; Logto came along from Phase 1's compose profile and was treated as given.

## What was considered, and why each lost

**Logto, self-hosted.** It needs its own hostname, its own certificate, an admin-console pass and a
client registration before lumberroom can store one memory, so the product does not work until the owner
has configured a second product. Its support for RFC 7591 dynamic registration was never confirmed,
and that is the one feature the browser clients need. [`deploy/logto.md`](../../deploy/logto.md) has
never been executed, so the estimate for that work is a guess.

**Any other external provider.** The same shape, with the same first step. Hosted tenants add an
account and a bill to a product whose point is that the owner holds their own facts.

**The static-header beta.** Anthropic's auth-type table includes `static_headers`, and it is
invite-gated behind an email to `mcp-review@anthropic.com`, with users still seeing OAuth-only
fields in August 2026. Worth asking for, and no basis for a plan.

**`oidc` mode alone, which already exists.** It keeps the "never issue tokens" rule from
[DECISIONS.md](../../DECISIONS.md) §2 and buys nothing here, because it makes the product depend on
the owner standing up an issuer before the first write.

## "Do not hand-roll OAuth", argued rather than waved away

The advice is sound and it is aimed at a service with a user population, a support burden and other
people's data behind its tokens. lumberroom has one owner, one tenant and no third party's data.

The argument that carries the decision is narrow. The alternative costs the owner a separate
identity provider to stand up, configure and keep patched before the product does anything, and the
blast radius of getting the token code wrong is one person's own server issuing tokens to their own
clients. That does not make hand-written OAuth safe. It makes the tradeoff decidable in a way it
would not be for a multi-tenant service.

Two design choices reduce the surface rather than the responsibility. Access tokens are opaque and
stored as hashes, so revocation is one `UPDATE` that takes effect on the next request and there is
no signing key to rotate or leak. Codes are single use, bound to one redirect URI and one PKCE
challenge, and a second exchange revokes what the first one issued.

## Why open dynamic registration is defensible

Registration is not authorization. A self-registered client is a row with an empty `grant_read`, an
empty `grant_write` and a null `consented_at` (migration `20260819000007_oauth.sql`). It can
register, start an authorization request and receive nothing. Only the consent screen attaches a
grant, and reaching that screen takes the owner's password, verified with argon2id against
`OWNER_PASSWORD_HASH`. `OAUTH_DCR_ENABLED=false` turns registration off and requires every client
to be issued by hand.

This answers the spec's "prefer CIMD or manually issued credentials over Dynamic Client
Registration". The spec's objection is that Claude and ChatGPT mint a fresh client on every
connection and phantom registrations pile up. Under the empty-grant rule a phantom registration is
a dead row that holds nothing, and `lumberroom clients` lists them for revocation. A manually issued
credential stays available, and stays the better choice for a surface the owner sets up once.

## What it costs, accepted

- **The project now maintains security-sensitive code**, and the maintenance does not stop when the
  feature works. Advisories against this class of code arrive on someone else's schedule.
- **Every RFC detail is a silent failure of a real client rather than a test that goes red.** The
  spec names the ones already known: `/token` must take form encoding while `/register` takes JSON,
  and `code_challenge_methods_supported` must advertise S256 or newer clients refuse to start.
  Claude Code's fallback probing hides this whole class of bug, so a green result there proves
  nothing about a browser.
- **The consent screen and the password path are new attack surface.** A login form, a signed
  cookie and a rate limiter that has to hold.
- **A subtle bug here is worse than a subtle bug in search.** A bad search result is a bad answer.
  A bad token check hands a client the owner's whole store.

## What this is not for

It is not a general-purpose identity provider, and nothing else should authenticate against it. It
is not multi-user: there are no user accounts, only one owner, and no sign-up path that could make a
second. The login exists so the owner can approve a client, and nothing else takes a password; the
MCP surface authenticates with tokens. It does not replace `AUTH_MODE=token` for the three
surfaces that already work with a header.

## Reversal condition

`AUTH_MODE=oidc` is the exit and it exists today. Take it when either of these arrives: a
vulnerability class the project cannot keep up with, such as a run of advisories in code-exchange or
redirect handling that an external provider would have absorbed, or a client that needs a
protocol feature an external provider already ships and this server would have to grow to match.
Reversing costs an issuer to configure and the grants to re-key onto its client ids. The schema, the
tools and the database are unaffected.
