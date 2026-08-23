# 0006. The console decides the queue, and it is the same call the CLI makes

20 August 2026. Accepted, verified. Widened the same day by decision 0009's phase: the console now
also composes a memory and supersedes one, so the three queue routes below are no longer its only
write path. The argument holds unchanged, because both new routes call `services::write::run`, the
same function the MCP tool calls, and carry the same session-bound CSRF token.

## The decision

`/console/queue` gained three POST routes: approve, reject and return to queue. Each runs the same
`services::ingest::approve`, `reject` and `unreject` the admin routes run, behind the owner session
and a CSRF token bound to the session, the action and the row.

## The context that forced it

The queue page shipped read-only, and its own prose told the reader to go and run
`lumberroom ingest approve <id>`. The first real run put 221 rows in that queue. Reading a row in a
browser and then retyping its uuid into a terminal is not a review loop anybody finishes, and a
queue nobody finishes approves nothing, which is the failure the whole approval gate exists to avoid.

## What lost, and why

**Leaving it read-only.** The argument for that was written into the page: a console button is a
second path into the store and nothing tested it. The first half is answered by construction, since
the handler calls the same service function and holds no logic of its own. The second half was true
and is now false: `tests/console.rs` signs in, reads the token out of the rendered form, posts it,
and asserts the row moved and the memory row exists.

**A JSON endpoint with a script on the page.** The console fetches nothing and runs no JavaScript,
and a test asserts both over the rendered HTML. Plain forms keep that property.

**Reusing the consent form's CSRF token.** The label is `csrf-console`, not `csrf`. A token minted
to approve an OAuth client must not be spendable on the queue, and a token minted for one row must
not decide another. Both directions are pinned by tests.

## Costs accepted

The console now holds a principal that can write. `owner_approver` carries `*` at `sealed`, which
matches the ceiling the owner's own CLI credential already holds, because a narrower grant here
would refuse a row the CLI approves and read as a broken button. That is the blast radius: a CSRF
bypass on these routes writes memories at any level in any namespace the credential tripwire allows.
The mitigations are the session in front of it, `SameSite=Lax` on the cookie, the per-row token, and
the fact that `may_delete` and `registry_write` stay false, so nothing here removes or overwrites.

A token dies when the session's signature changes. Signing in again invalidates every button on a
page left open, and the reader gets a 403 telling them to reload. Correct, and it will look like a
bug the first time it happens.

## What this is not for

Bulk. One button decides one row. A backfill queue runs to hundreds and the command for that is
`lumberroom ingest approve --run <id>`, which prints what it will approve and asks once. The page says
so.

It is also not an editor. A proposal's content, namespace and sensitivity are what the extractor
produced and what `write::run` classifies. Changing a fact before approving it means writing it by
hand through `memory_write`.

## Reversal condition

Take the routes out if a second reader ever holds a console session. The whole argument rests on the
session belonging to the owner and nobody else, which is what `OWNER_PASSWORD_HASH` guarantees today
because there is exactly one of it.
