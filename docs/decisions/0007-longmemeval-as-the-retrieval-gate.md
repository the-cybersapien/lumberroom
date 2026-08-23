# 7. LongMemEval-S retrieval recall as the standing retrieval gate

**Date:** 20 August 2026 · **Status:** accepted · **Decided by:** the owner

## Decision

LongMemEval-S retrieval recall becomes lumberroom's standing retrieval gate. The harness runs on
`all-MiniLM-L6-v2` rather than lumberroom's own `bge-base-en-v1.5`, because MiniLM is what the one
published comparison that exists, agentmemory's, was measured on, and a number run on a different
embedder measures the embedder rather than the search stack. `docs/eval-longmemeval.md` carries the
protocol, the environment, and every deviation from agentmemory's harness that keeps the two numbers
from being the same measurement wearing different clothes.

## Context that forced this

lumberroom had no retrieval number that survived being looked at. The recall monitor built earlier had
published figures, and they were withdrawn: `SET LOCAL` on a pooled connection with no open
transaction is a silent no-op, so the monitor's exact arm was comparing HNSW against itself for a
whole phase, and everything downstream of that arm was worthless. There was no other retrieval
measurement in the system.

Meanwhile a competing memory engine publishes LongMemEval-S retrieval numbers and a documented
protocol for reproducing them. Improving lumberroom's search without a scoreboard is guessing, and the
only scoreboard on offer that anyone else has published is this one.

## What lost, and why

**A curated in-house fixture.** Hand-picking sessions and questions that exercise cases the author
already suspects matter measures whether the author's model of the system is self-consistent, not
whether the system retrieves well on data it did not choose. LongMemEval-S is adversarial in a way a
self-authored fixture cannot be: the author did not write the haystacks and does not know which
session is gold until the file is opened.

**The official QA-accuracy metric.** LongMemEval's own metric retrieves, generates an answer, and
scores the answer with a GPT-4o judge. That adds a generation step and a judge model between the
number and the thing being tested, and it means a regression in retrieval and a regression in
answer generation produce the same drop in the same number. Retrieval recall isolates the one layer
lumberroom actually owns: what search surfaces, not what an LLM does with what it was handed.

## Costs accepted

- A 265MB dataset that has to be fetched and kept somewhere, not checked into the repository.
- A full run measured in tens of minutes against a live server: 500 questions, each writing roughly
  53 sessions and then searching, over real HTTP into real Postgres.
- An embedder, `all-MiniLM-L6-v2`, carried in the deployed binary for benchmark parity alone. lumberroom's
  own default is `bge-base-en-v1.5`; MiniLM earns its place in the model list only because the one
  external number worth comparing against was run on it.
- `SENSITIVITY_TRIPWIRE` switched off for the run. LongMemEval's synthetic sessions contain
  credential-shaped strings by construction, as part of what the benchmark exercises, and the
  tripwire firing on them would shrink the haystack for a reason that has nothing to do with search
  quality. The tripwire runs at its normal setting everywhere else.

## What this is explicitly not for

It does not prove lumberroom is better than agentmemory or anything else at being a memory system. It
measures one thing: whether search surfaces the right session on synthetic multi-session chat logs
built for this benchmark. It says nothing about supersession, about the two-axis policy model, about
the registry, or about provenance, which is where lumberroom's actual value over a plain vector store
sits. A team that starts citing this number as a product claim rather than an engineering gate has
misread it.

## Reversal condition

If the harness starts pulling search design toward beating LongMemEval-S rather than toward serving
the owner's own corpus, the harness goes. A retrieval gate that reshapes the product around a
synthetic benchmark's blind spots has stopped doing the job it was built for.
