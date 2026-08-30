# 15. `SealedRepository` gains an enumeration method, for archive building alone

**Date:** 30 August 2026 · **Status:** accepted, design · **Decided by:** the owner

## Decision

`SealedRepository` gains `list_for_archive`, returning every sealed item for one tenant: namespace,
`key_hmac`, ciphertext, `alg`, `source_client`, `created_at`. Exactly one caller reaches
it, `services::archive::build`, and only after `services::reads_whole_store(&ctx.principal)` has
already passed.

## Context

`src/ports/sealed.rs:1` and `migrations/20260819000008_encryption.sql:46` both state the same design
property in the same words: the server cannot enumerate sealed items, keyed as they are by a
client-side HMAC of the canonical name for that reason. The port exposes `put`, `get`, `delete`,
`counts` and `namespaces`, and nothing that returns the rows themselves.

A bulk archive that skipped the `sealed_item` table would still call itself complete. The owner would
migrate everything else, discover their AWS credentials and their bank login did not come with them,
and find out after they had already cancelled the old install. A store the owner cannot take with
them in full is a worse product than a listing method that only fires behind the same gate a full
export already requires.

## What lost, and why

**Leaving sealed items out of the archive.** It keeps the non-enumerability property whole exactly as
written, and it turns the export into a partial copy that reads as a complete one. The header would
claim `format: lumberroom.archive` with no signal that a whole class of data stayed behind. Rejected
because the failure it produces is silent and discovered at the worst possible time.

**A client-side archive that fetches each item by key.** This would need the client to already hold
every key it wants archived, which is exactly the enumeration the server refuses to do on its own
behalf. It moves the problem, it does not solve it, and it requires a client-side index this design
does not otherwise need.

## Costs accepted

A caller holding a grant of `*` at sealed learns how many sealed items a tenant holds and under what
`key_hmac` each one sits. That caller already reads every other row in the store under the same
grant, `reads_whole_store` being exactly `admits(read, "*", Sealed)`. Nothing this method returns
lets the server read a blob or reverse a name: the ciphertext stays opaque, the HMAC stays one-way,
and the server holds no key before or after this change.

## What this is explicitly not for

Search, listing in the console, the digest inventory, or any read that is not `archive::build`.
`counts` and `namespaces` stay the honest answers for those, and neither one leaks a `key_hmac` or a
row's bytes. `list_for_archive` is not a general-purpose replacement for either.

## Reversal condition

If a grant narrower than `*` at sealed ever reaches `list_for_archive`, the method is wrong and the
service door in `archive::build` is what failed, not this record. The fix is to close that door, not
to narrow what the method returns.
