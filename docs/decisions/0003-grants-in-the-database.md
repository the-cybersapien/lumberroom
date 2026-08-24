# 3. OAuth client grants live in Postgres, environment clients stay in the environment

**Date:** 19 August 2026 · **Status:** accepted · **Decided by:** the owner

## Decision

A grant for an OAuth client is a row in `oauth_client`, editable without a restart. A grant for a
static bearer client stays in `AUTH_TOKENS` and stays authoritative there. Neither authority writes
into the other.

`POST /console/clients/{id}/access` is the surface that writes that row today: the owner's console
session posts a grant through `set_client_grant`, under the same CSRF check the rest of the console
uses. Nothing else edits `oauth_client`'s grant columns.

## The context that forced it

Phase 1 put every grant in `AUTH_TOKENS`, and the Phase 2 spec recorded the limit rather than fixing
it: "Grant changes take effect on restart. Grants live in `AUTH_TOKENS`. That is fine for a handful
of clients and becomes annoying at the point where grants change often, which is Phase 3's problem to
solve, not this one's." Phase 3 is the point where grants change often, because each one gains a
sensitivity ceiling per namespace and the exit test is meant to run after every edit.

The harder fact is decision 0002. A dynamically registered client cannot live in an environment
variable, because it does not exist until it registers, and it registers while the server is
running. The owner approves it at a consent screen and assigns it a grant in the same action. There
is no restart in that flow and there should not be one.

## What was considered, and why each lost

**Everything in the environment.** It cannot express a client that registered itself five seconds
ago. It also turns each consent into a file edit plus a restart, which interrupts every connected
client, so the owner learns to avoid tightening a grant.

**Everything in Postgres, seeded from the environment.** This is the tempting one and it has two
failure modes, both quiet. If the environment value wins at boot, the owner narrows a grant in the
console, the container restarts for an unrelated reason, and the old grant comes back with no
notice. If the row wins, the file on disk says the ChatGPT client reads `user:me` and `global`, the
row says otherwise, and the file is a lie that the owner will read and believe. Either way the
question "what may this client see" has two answers at the moment it matters most.

So authority follows the credential. A token that came from `AUTH_TOKENS` carries its grant in
`AUTH_TOKENS`. A `client_id` this server issued carries its grant in its row. Every credential has
exactly one home, and no code path copies a grant from one home to the other.

## What it costs, accepted

**Two places to look when a grant is wrong.** That is the real cost and there is no way to price it
down to zero while both credential types exist.

The mitigation is that the effective grant is queryable from the enforcing path rather than
reconstructed by reading config. `/admin/whoami` answers for the credential you present and returns
the client, the resolved read and write lists, the registry-write flag and the auth mode that
produced them, so the answer comes from the code that enforces it. `lumberroom clients` lists the OAuth
rows with how each one registered and whether the owner has consented. The exit test the Phase 3
spec specifies runs per credential, so it covers both authorities without knowing about either.

**A row can change while requests are in flight.** The next request sees the new grant. That is the
behaviour asked for, and it removes the window where a client the owner just cut back keeps its old
access until the next deploy.

## What this is not for

It does not move bearer clients into the database. Those are configured by the person who runs the
box, in the file that person already edits, and a console that could rewrite them would put the
server in charge of its own access control.

It is not a policy store. Namespace defaults for sensitivity live in `sensitivity_default`
(migration `20260819000004_sensitivity.sql`) and apply to writes rather than to clients. Mixing them
into the grant row would make one client's grant able to change what another client's writes mean.

## Reversal condition

If diagnosing a wrong grant across two authorities costs real time in practice, collapse to one
store, and the survivor is Postgres. `AUTH_TOKENS` then becomes a first-boot seed with one rule: it
creates a row when none exists for that client and never updates one that does. That keeps a working
deploy for someone who has not opened the console yet.

That is not the shipping design because the rule has to be remembered, and a seed that looks
authoritative in a file the owner edits is the failure this record exists to avoid. Take the change
when the owner would rather remember one rule than check two places.
