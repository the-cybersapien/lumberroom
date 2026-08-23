# 0009. Two names for one thing is an alias, and for retrieval it is query expansion

20 August 2026. Accepted, implemented.

## The decision

A name that denotes the same subject as another name is recorded as an alias with valid time, in its
own table, and search expands a query over the alias group before it runs. Memories gain no edges
and no entity column, and nothing links a row to an entity.

## The context that forced it

Six live rows in this store describe one project under three names. Warden was renamed Quill, Quill
was renamed Lumen, and the store holds facts filed under all three with nothing connecting them. A
search for Lumen finds four; a search for Warden finds two; neither says they are the same thing.
Worse, Warden is recorded at one path and Lumen's content at another, so one of those facts is
stale and the store cannot tell which.

Supersession is the wrong instrument and reaching for it would destroy data. The Warden facts are
true and they are about the same subject. Retiring them would hide facts that still hold and lose
the history that the project was ever called Warden.

## What lost, and why

**An entity graph, with a table of entities and an edge from every memory to one.** It is the
answer people reach for and it is the expensive one. Every row already in the store would need
relinking before it helped anything, and the six rows this decision exists to fix are exactly the
rows that would stay broken longest. Query expansion fixes them with no backfill.

Evidence beyond the argument: agentmemory ships the full version, entity extraction plus a Dijkstra
traversal to depth two. Reading its source, the graph contributes rank only, at weight 0.3, wrapped
in a handler that swallows its own errors. Meanwhile the LongMemEval measurement on this store says
the right document already reaches the top twenty for 98.4% of questions, so the whole gap is
ordering. A graph does not fix ordering.

**Extracting entities from free text with a model.** This system refuses that pattern everywhere:
the credential tripwire exists because content-derived classification is a guess stored as a fact.
An alias is recorded because somebody stated it, or it is not recorded.

**Reusing `registry_alias`.** It has the right shape and the wrong scope. It maps registry keys, it
carries no valid time, and widening it would put two different lifetimes in one table. The new table
copies its shape and adds the dates.

## Costs accepted

Expansion is lexical. It matches whole words in the query against known aliases, so a question that
refers to the project without naming it expands to nothing. That is the honest limit, it is written
at the function, and the alternative was the model-driven extraction rejected above.

An alias group has to be recorded by hand or derived from a stated fact. Nothing discovers a rename
on its own. For a store this size that is a small cost and it keeps the table trustworthy.

Aliases are lowercased on the way in, so a name whose case carries meaning collapses. No name in
this store depends on case.

## What this is not for

It does not model relationships between facts. "A caused B" and "A is part of B" are edges between
memories and this decision deliberately builds none of them. It answers one question: do these names
denote the same subject.

It also does not decide which of two conflicting facts is current. Expanding a search over Warden
and Lumen surfaces both path facts. Resolving them is supersession's job, and surfacing the
contradiction at all is more than the store could do before.

## Reversal condition

If a group ever needs to be discovered rather than stated, or if expansion has to reach a question
that never names the subject, this becomes retrieval over an entity index and the table becomes its
seed. The stored aliases carry over unchanged.
