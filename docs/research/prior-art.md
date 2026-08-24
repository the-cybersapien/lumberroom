# Research: prior art

The system PRD names this as an unresearched risk and asks for it before Phase 2: "Supermemory,
Mem0, Zep, Letta, Basic Memory and a dozen others occupy nearby ground. Worth an afternoon before
Phase 2 to check whether someone already ships exactly this, and if so, whether their permission
model is any good."

Surveyed August 2026: Mem0, OpenMemory, Zep, Graphiti, Letta (MemGPT), Supermemory, Basic Memory,
Cognee, Memoripy, txtai, plus Redis Agent Memory Server, Compartment, LangMem and LlamaIndex memory.

---

## The landscape

| Product | What it is | Remote MCP + OAuth | Permission model | Structured facts | Provenance and supersession | Server-blind option | Health |
|---|---|---|---|---|---|---|---|
| **Mem0** | Library, cloud, self-host, hosted MCP | Yes, browser OAuth or bearer | `user_id`/`agent_id`/`app_id` are **scoping tags, explicitly not an ACL** in their own docs | Mostly one blob; optional graph; no canonical keys | `history()` audit log; supersession soft, contradictions coexist | No | Apache-2.0, very active |
| **OpenMemory** (Mem0's) | Was a local MCP server with per-app ACL, pause/revoke, audit log | Local only | **The closest thing in this survey to a Policy layer. Discontinued.** Replacement has per-user API keys only, a regression | Inherited | Inherited | No | Removed from main |
| **Zep Cloud** | Hosted; free self-host edition deprecated April 2025 | Yes, Enterprise plan, OIDC SSO | ABAC on API keys, genuinely enforced, **Enterprise-only and not wired into the MCP layer**. MCP grain is per-user, binary read/write | Typed graph with custom ontologies | Bi-temporal, inherited from Graphiti | AES + KMS BYOK, no zero-knowledge | Server is closed and cloud-only now |
| **Graphiti** | OSS library plus reference MCP server | MCP server exists, **no auth at all** | **None.** `group_id` is namespacing, not enforcement | Real typed graph | **Best supersession model found:** `valid_at`/`invalid_at`/`created_at`/`expired_at` per edge, invalidate rather than delete, point-in-time queries | No | Apache-2.0, very active |
| **Letta (MemGPT)** | V1 server **retired March 2026**, "should not be used in production" | Bearer only; OAuth claim unsubstantiated | Shared blocks are **all-or-nothing**; their docs state you cannot make a block read-only for some agents and writable for others | Freeform text blocks, no canonical keys | Weak; git history as a side effect | None found | Org active, the self-hostable server archived |
| **Supermemory** | Cloud plus self-hostable binary plus MCP | **Yes, genuine**, standards-based OAuth via `/.well-known/oauth-protected-resource` | **containerTag plus scoped API keys.** Real, revocable, endpoint-restricted. One axis, not two | Extracted facts, no canonical keys, extraction mandatory | Automatic timestamp-driven supersession, algorithmic rather than confirmed; no queryable chain | SOC2 II, no client-side encryption | MIT, very active; self-host licensing ambiguous |
| **Basic Memory** | Local-first MCP over markdown files, SQLite index | Yes for cloud | **Process-level isolation only.** Denying one client means running a second server | **Best direct structured-write story:** entities with frontmatter, typed observations, `[[wikilinks]]`, no forced extraction | **Confirmed absent:** no timestamps, no attribution, no correction markers | Local files, plaintext | AGPL-3.0, very active |
| **Cognee** | Library, server, MCP | Plain bearer, no OAuth | Real ACL primitives in the core library, **not wired into the MCP layer**; open issues on the isolation boundary, including deleted data surviving a hard delete | Graph extraction with ontologies | Weak, no temporal model | No | Apache-2.0, very active |
| **Memoripy v4** | Local library, optional service, MCP | Bearer with read/write/admin scopes | Scopes across user, agent, run, project, org and namespace at once. **Closest to a two-axis design found anywhere** | **Best registry design found:** typed kinds with `observed_at`/`recorded_at`/`valid_from`/`valid_to`/`trust_level` | Bitemporal, immutable history, non-LLM authoritative writes, an admission policy that quarantines untrusted content | Absent | **Two days old, one developer, one squashed commit.** Watch, do not build on |
| **Compartment** | Offline AEAD-encrypted vector memory | Local only | None | Vector only | Hash-chained audit log, per-record crypto-shred | **Best encryption story in the survey, including the embeddings** | Apache-2.0, brand new |
| **txtai** | Embeddings database and agent framework | No OAuth either direction | None | Unopinionated | None | Not documented | Apache-2.0, mature |

---

## A. Does anything already ship this?

**No.** Nothing combines multi-surface shared memory over remote MCP with OAuth, a registry of
canonical-keyed facts with provenance and confirmed supersession, and per-client grants on two
independent axes.

The near misses, each missing a different piece:

- **Zep Cloud** is closest on deployment shape: several surfaces genuinely pointed at one shared
  graph over OAuth-backed remote MCP. But grants are per *user*, not per *client*. Their own blog:
  all authenticated users on that connection reach the same graph, with no per-agent
  differentiation. No sensitivity axis, and the one real grant mechanism is Enterprise-only.
- **Graphiti's bi-temporal model** is the closest thing to the registry's supersession semantics.
  Zero access control, and its maintainers have an open issue admitting contradiction detection
  fails silently on non-reasoning models.
- **Supermemory's scoped keys** are the closest shipped, production-hardened policy layer. One
  axis, and no registry layer at all.
- **OpenMemory** once had genuine per-app ACL with pause, revoke and an audit log, the closest
  historical match to the policy layer, and it is dead.
- **Memoripy v4** is the closest match to the registry *design*, and is a two-day-old prerelease.

**Bottom line: this is not a rebuild of something that exists.** The pieces are scattered across
five products, never combined, and the one that came closest on policy was discontinued.

---

## B. Whose permission model is worth copying

Synthesise three, adopt none wholesale.

**Supermemory, for enforcement mechanics.** Two key tiers: a root key that mints and revokes, and
scoped keys bound to containers and restricted to an explicit endpoint allow-list, individually and
immediately revocable. That is the right shape for cheaply provisioned, cheaply revoked per-client
grants enforced at the boundary. Our token to client to namespace-glob mapping is structurally this
already. What to keep that they lack: the second axis.

**Zep's ABAC, for filtering by content attribute rather than by bucket.** Zep can restrict a key to
`data_class: [support]`, a policy that filters on metadata carried by the artifact rather than on
which container it sits in. That is much closer to a sensitivity axis than Supermemory's container
model. Combined: namespace is the bucket a scoped key may touch, sensitivity is a row attribute the
same key's policy admits or refuses.

**Letta, as the anti-pattern.** Their docs state plainly that a shared block cannot be read-only for
one agent and writable for another. That collapses two axes into "in the pool or out of it." Phase 1
is already ahead of every product surveyed on this specific point, because a request resolves to a
principal and a namespace glob list as a first-class check rather than an application convention.

**Concretely:** a grant is (client) × (namespace pattern) × (sensitivity ceiling), checked once
before every read and write rather than scattered through query logic, with sensitivity evaluated
per row as an attribute. Nobody ships this combination.

**One warning from OpenMemory's death and Letta's retirement.** Both attempts at a policy-heavy
product were walked back or discontinued, and both companies pivoted toward agent-harness products.
That is not evidence the idea is bad; it is evidence the fine-grained policy layer was not where
their revenue was. For a personal tool with no revenue to chase, that is a point in favour: you can
keep the layer they abandoned.

---

## C. Build, adopt, or wrap, per layer

**Memory: build thin, as planned.** Every product does this competently and none differentiates on
it. Adopting any of Mem0, Supermemory or Cognee drags in an LLM extraction pipeline the PRD
explicitly rejects. Budget to swap the retrieval strategy later; do not take a dependency.

**Registry: build, but steal two data-model ideas.**

- Graphiti's four-timestamp edge model separates *when a fact became true* from *when the system
  learned it*. That distinction is what makes "how do you know that?" answerable, which is the whole
  purpose of the registry layer. Worth adding `valid_from` and `invalid_at` alongside the existing
  `supersedes` key **before real data accumulates**.
- Memoripy's typed-kind taxonomy (fact, preference, policy, commitment, decision, procedure, belief)
  is worth cross-checking against our `kind` enum before it calcifies.

Neither project is adoptable: Graphiti has no policy layer and known contradiction failures,
Memoripy has no track record at all.

**Policy: build. This is the open ground.** No product combines two axes, the one with a real
per-app policy layer is discontinued, and the best mechanics are single-axis and, in Zep's case,
Enterprise-only and not MCP-integrated. Treat this layer as the actual differentiation.

**Wrap: nowhere.** Every candidate either forces unwanted extraction, has a coarser policy model, or
has had its relevant product line discontinued underneath adopters.

---

## D. Failure modes documented elsewhere, worth not relearning

**Recall-reinforcement duplication is real and expensive.** Mem0's audited production store held
"User prefers Vim" **808 times**, because recalled memories were re-extracted as new memories on the
next turn. The pipeline could not tell "this came back from a search" from "this is new." Anything
that ever writes back from context must exclude what the digest and search results put there. This
is the single most directly applicable finding in the survey.

**Silent contradiction resolution is worse than none.** Mem0 has a documented bug where two
contextually different but textually similar preferences are judged contradictory and one is
silently deleted, with no stack trace. Graphiti's maintainers admit the same on weaker models.
Storing `supersedes` explicitly and refusing to auto-consolidate is the right instinct; any
automatic, LLM-judged supersession needs a human-visible diff step.

**"Everything ends up in the loosest bucket" is the industry's actual failure mode.** Basic Memory's
project-only isolation, Letta's all-or-nothing sharing and Zep's per-user grain all converge on the
same regression: fine-grained policy is hard to keep fine-grained under real usage, so products
retreat to coarser boundaries. The PRD names this risk; the survey confirms it is not hypothetical.

**Extraction audits are damning when anyone runs one.** The one rigorous audit found (Mem0, 10,134
entries over 32 days) found 97.8% junk: boilerplate, heartbeat noise, and a small model inventing a
fictional user profile. Strong support for staying out of the extraction business.

**"Forget" does not always forget.** Cognee has an open issue where deleted data survives
`delete_data(mode="hard")`. Given the PRD's promise to delete anything, the delete path needs a test
that the row is gone from every index, not just the primary table.

**Extraction cost compounds.** Zep retrofitted deterministic MinHash dedup specifically to cut the
LLM call volume their extraction generated per episode. A practical argument for keeping the write
path free of model calls.

**Products in this space do not stay put.** Zep Community Edition deprecated, Letta's V1 server
archived, OpenMemory sunset, all within roughly twelve months. Any future "wrap something existing"
decision should budget for the wrapped thing being discontinued.

---

## E. Where the prior art suggests the design may be wrong

- **The two-axis grant model is genuinely unsolved, not merely unbuilt.** Nobody does it. The
  strongest single-axis implementations are each half of it. That is a reason to budget Phase 3
  generously rather than to assume it is composition of two known patterns; no one has debugged this
  combination in production.
- **"Private is thinner than it sounds" is more candid than any competitor's marketing.** Keep it.
  Compartment goes further than we plan to, encrypting the embeddings too; worth a look if `sealed`
  ever needs stronger technical backing.
- **Add the bi-temporal columns before data accumulates.** Retrofitting `valid_at` and `invalid_at`
  onto a registry with real history is worse than adding unused columns now.
- **The no-auth MCP server is a common pattern and a bad one.** Graphiti, one of the more
  sophisticated products here, ships its official MCP server with no authentication. Read that as a
  warning about where teams converge under convenience pressure, not as permission.
- **Nobody serves competing vendors' surfaces from one identity.** Zep's multi-surface server is the
  only place one store is reachable from competing products, and even there grants are per-user.
  Treat cross-vendor behaviour as an open question rather than an assumed win. Phase 2's per-client
  instrumentation is how that gets answered.

---

## Native vendor memory

**Unresolved.** The thread covering Anthropic's memory tool and Claude.ai memory, and OpenAI's
ChatGPT memory and Connectors, did not complete with live sources. What follows is from training
knowledge with a January 2026 cutoff, roughly seven months stale, and should be verified by hand.

Reasonably confident: Anthropic's memory tool is an API-level primitive where the *storage is
developer-managed*, so architecturally it could sit in front of our registry rather than compete
with it, but it only serves the API surface. Claude.ai's consumer memory and ChatGPT's saved
memories are first-party silos with no evidence of third-party read/write, and neither vendor's
consumer memory flows through to its own API. OpenAI Connectors is a retrieval framework, not a
memory system, and conflating the two would be a mistake.

Explicitly unknown: whether either vendor has since shipped a bridge across its own surfaces, which
would be the most important fact for the PRD's risk section.

**The structural conclusion holds regardless.** Native memory is single-vendor and usually
single-surface. One fact visible to Claude Code, Claude.ai, ChatGPT and OpenWebUI under one policy
requires those vendors to interoperate with each other's competing products, which neither is
positioned to do. **Twenty minutes of checking both settings screens by hand is worth doing before
Phase 2** and would settle it better than more searching.
