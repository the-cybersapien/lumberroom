# 0010. The registry keeps what it replaces

21 August 2026. Accepted, implemented.

## The decision

A registry upsert writes the value it is about to replace into `registry_history` in the same
transaction. The history is readable through one admin route and one CLI command, behind
`may_read_history`, and through no MCP tool.

## The context that forced it

The upsert read `ON CONFLICT ... DO UPDATE SET value = EXCLUDED.value ... version = version + 1`.
Only a counter survived a change. The previous value was gone, with no copy anywhere.

Compare that with a memory. A superseded memory row is hidden by a filter and still on disk, so its
history was recoverable the whole time even before anything read it. The registry destroyed. So the
irreversible loss in this system sat in the component that decision 0008 was not going to touch,
while the recoverable loss sat in the one it did.

Seven reviewers looked at the valid-time plan and all seven independently reached the same
conclusion on this point, three of them after reading `registry.rs:109-121` to check. One called it
the worst footgun in the review and noted it was live: mistype a port today and there is no recovery
path at all.

## What lost, and why

**Deferring it with the rest of registry history.** The plan deferred a full design: a versioned
`tstzrange`, a `btree_gist` exclusion constraint keyed on the canonical key, and a query surface.
That deferral priced the whole feature to justify postponing a stop-loss that needs none of it. One
append-only table and one INSERT is not that feature.

**A trigger.** It would catch writes that bypass the adapter, and there are none: the adapter is the
only module holding registry SQL, which is a rule this codebase already enforces. A trigger would
put behaviour somewhere no Rust reader looks.

**An MCP tool for reading it.** The registry holds credential locations. A model asking what one
used to be is exactly what `may_read_history` being off by default guards against, and a capability
is a property of a client rather than a flag on a call.

## Costs accepted

The table grows without bound and nothing prunes it. At this store's size that is nothing, and a
retention rule needs a policy nobody has written; recording the growth beats guessing a limit.

A history row keeps the sensitivity its value had, so a row archived at `private` stays private and
the ceiling filter runs inside the query rather than over the results. That is the same rule every
other read here follows and it matters more here: a replaced credential location can be more
revealing than the one that replaced it.

The write shares the upsert's transaction. A history write that could fail on its own would produce
gaps nobody can detect, which is worse than no history at all.

## What this is not for

It does not answer "what did this key hold on 3 March". It answers "what did this key hold before
now", newest first. Point-in-time over the registry is the deferred design and this is its seed.

It is also not an undo. Reading what a key used to hold and writing it back are two acts, and the
second one goes through the same upsert with the same checks.

## Reversal condition

If the history table ever grows large enough to matter on a store this size, the answer is a
retention rule rather than removing the stop-loss. Take this out only if registry values stop being
things anybody would want back.
