# 0013. A delete splices the supersession chain, and revives a predecessor only under the caller's grant

23 August 2026. Accepted, implemented. Pinned by `tests/integration.rs`
(`deleting_a_correction_does_not_revive_a_row_the_caller_cannot_reach`,
`deleting_the_middle_of_a_chain_splices_the_ends_together`,
`no_foreign_key_into_memory_is_left_to_block_a_delete`) and by the pure `plan_for` tests in
`src/services/forget.rs`. Not yet run against a live server on this branch.

## The decision

Deleting a memory edits the chain around it in the delete's own transaction, under a plan the
service makes from the caller's grant:

- A row with a successor is spliced out. Every predecessor's `superseded_by` moves to the
  successor, which stays `supersedes` of the first of them. Nothing comes back to life, whatever
  namespace the predecessors sit in.
- A row with no successor is the head of its chain, and deleting it revives its predecessors. That
  is a write at each predecessor's own namespace and level, so the caller has to hold read and
  write on every one of them. If any predecessor is outside the grant, the delete is refused with
  the same message an unknown id gets: `memory {id} does not exist or is not yours to delete`.
- The outcome reports `revived`, `spliced` and `blocked` by id, beside `rows` and `count`.

`memory.supersedes` and `memory.superseded_by` keep `ON DELETE NO ACTION`. A predecessor the plan
did not account for makes the `DELETE` fail on the constraint, and that failure is mapped to a
conflict telling the caller to look again. The keys from `ingest_proposal` into `memory` moved to
`ON DELETE SET NULL` in migration 000019; they point at the row and revive nothing.

## The context that forced it

The first delete cleared every inbound `superseded_by` under the tenant alone, before removing the
row. A client holding `mayDelete` and an `open` ceiling on one namespace could delete a correction
it had written and revive the private fact the owner had retired behind it, in any namespace, with
no grant in the loop. The audit of 23 August 2026 filed it as `delete-revives-foreign-rows`.

The same audit found that a memory approved from the ingest queue could not be deleted at all
(`forget-fk-blocks-shred`): `ingest_proposal.memory_id` referenced it with `NO ACTION`, the delete
raised 23503, the transaction rolled back, and the caller got an internal error while the wrapped
DEK and the plaintext embedding stayed. A crypto-shred that fails on a foreign key is not a shred.

## What lost, and why

**`ON DELETE SET NULL` on `memory.superseded_by`.** The database would do the revival itself, for
every predecessor, with no principal to check. That is the leak the audit described, applied one
layer down where no service can refuse it. It is the obvious fix and the wrong one.

**Refusing every delete of a row that has predecessors.** Closes the leak and makes `memory_forget`
useless on exactly the rows people correct, which are the ones they later want gone.

**Reviving regardless of grant and writing a log line.** A leak with an audit trail is a leak.

**Evaluating the grant inside the delete statement.** The grant is a list of globs with ceilings
and the classification table is in config; neither can be expressed as a SQL term today without
pushing both in as arrays the way the queue reads do. `chain_neighbours` returns ids, namespaces
and levels and no content, so no row a caller may not read enters its process, and the
`NO ACTION` constraint stays as the backstop for a plan that missed a row.

## Costs accepted

**Two round trips per delete**: `chain_neighbours`, then `delete` under the plan. A supersede that
lands between them changes the chain the plan was made for; the constraint catches it and the
caller sees a 409 and retries. `by_query` plans each row separately for the same reason.

**Deleting the middle of a chain no longer revives the predecessor.** The old doc comment on the
adapter's `delete` said a middle delete put the predecessor beside the successor. It now stays
retired behind the successor. Two live versions of one fact was never what anyone wanted from a
delete.

**The splice writes to rows the caller holds nothing on.** Moving a predecessor's `superseded_by`
from the doomed row to its successor is a write in the predecessor's namespace. It changes no
content and revives nothing, which is why it runs without a grant; migration 000019 and the
adapter comment are the places that say so.

**The grant check runs in Rust, not in SQL.** A documented deviation from the filter-in-the-query
rule, accepted because nothing readable crosses the boundary and the constraint backs the plan.

## What this is explicitly not for

**Cross-tenant references.** The id space already rules them out; the plan does not look.

**Deciding which of two conflicting facts survives.** A delete removes the row it was given. It
never picks a winner among the rows it splices.

## The reversal condition

If a grant model arrives that can be evaluated inside the delete statement, the plan moves into SQL
and the two calls become one. If the 409 on a concurrent supersede shows up in practice rather than
in theory, the two calls move into one transaction with the doomed row locked first.
