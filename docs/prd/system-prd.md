# System PRD: Agent Memory Control Plane

**Owner:** maintainer · **Date:** 2026-08-18 · **Status:** ready to build · **Scope:** the whole system, all phases

Companion docs: `north-star.md` (thesis), `agentic-memory-design.md` (architecture), `build-decision.md` (hosting and scope), `claude/phase-1-prd.md` (the first build slice). This document describes what the system does and what it is for. It stays out of implementation detail on purpose.

Status: see `docs/decisions/0002-built-in-oauth-server.md` through `0005-private-drops-lexical-search.md` for what the build settled that this document left open.

---

## 1. What this is

One memory that every AI tool you use can read from and write to, with you deciding what each one is allowed to see.

Today you run seven AI surfaces. Each keeps its own memory. None of them share. You re-explain your setup to ChatGPT, then again to Claude, then again to your coding agent. When you correct one of them, the others never find out.

This system holds the facts once, in a place you control, and every tool reads from it.

---

## 2. Who it is for

You. One user, six or seven surfaces, personal use. Building this as a product for other people is out of scope, though nothing in the design blocks that later.

That decision changes what matters. Onboarding, billing, tenant isolation and moat all drop off the list. What stays: the thing has to work well enough that you keep using it, and it has to be safe enough that you are willing to put real facts in it.

---

## 3. The three layers

The system does three separate jobs and confusing them is how similar products go wrong.

**Memory remembers.** Fuzzy, semantic recall. You said something six weeks ago, a tool asks a vaguely related question, the right thing comes back. This layer is a commodity. Half a dozen funded startups compete on it. Build the thinnest possible version and keep it swappable.

**Registry knows.** Exact, structured facts with a source. Which machine runs which OS. Which model you route which task to. Where a credential lives. Not "Dana mentioned the GPU box runs Ubuntu" but a record that says `machines.desktop.os = Ubuntu 26.04`, confirmed by you, on a date, superseding an older value. This layer answers "how do you know that?" and fuzzy memory cannot.

**Policy decides.** Which tool sees which facts. Your work coding agent should see project context and never see personal finance. ChatGPT on a browser should not get everything Claude Code gets. This is the layer that makes you willing to store anything sensitive at all.

The value sits in the second and third layers. The first is table stakes.

---

## 4. Capabilities

### 4.1 One identity across every surface

Every tool connects to the same memory through a standard connector protocol. Claude.ai on web and mobile, ChatGPT on web, both Claude Code installs, Hermes, OpenWebUI, Cowork. A fact written by any of them is visible to all of them, subject to policy.

The system lives on an always-on cloud instance so that browser and mobile clients can reach it. Nothing depends on your Ubuntu box being switched on.

### 4.2 Automatic recall at the start of work

One primitive, `context_bootstrap`, returns a compact digest: who you are, what you are working on, recent decisions, a registry summary. Cheap and fast, so tools call it without friction.

On tools with lifecycle hooks (Claude Code, Hermes) this fires automatically at session start. On browser tools it fires when the model decides to call it, which is a real limitation covered in section 6.

### 4.3 Write-back without being asked

When a conversation establishes something durable, a decision, a preference, a fact about your setup, the tool saves it. No prompting from you, no announcement.

This is the half of the loop that decides whether the system lives or dies. A memory that gets read constantly and written rarely goes stale within weeks and stops being worth consulting.

### 4.4 A registry of exact facts

Structured records with a canonical key, a value, and provenance: which tool wrote it, from which conversation, on what date, whether you confirmed it, and which earlier record it replaced.

Canonical keys matter from day one. Six tools left to their own devices will invent `desktop.gpu`, `machines.desktop.gpu` and `hardware.desktop.gpu` for the same fact. A minimum naming scheme prevents the mess rather than cleaning it up later.

### 4.5 Per-tool permissions

Each connected tool gets a grant that says what it may read and write. Two independent axes:

**Sensitivity:** `open`, `private`, `sealed`.
**Namespace:** personal, work, per-project, global.

A single sensitivity ceiling is not enough. Work notes and personal finances can both be `private` while a work agent must see one and never the other. Grants combine both axes.

The design target is that you configure this two or three times and then forget it. If using the system means classifying every sentence, the system has failed at the product level regardless of how well it works technically.

### 4.6 Three sensitivity levels, honestly described

**Open.** Any tool you have authorized can read it, within its namespace grant. Searchable.

**Private.** Encrypted at rest, served only to tools whose grant allows it. Searchable. The honest limit: a searchable index sits next to the encrypted text, and that index leaks a good deal of the meaning. [Editorial correction, decision [0005](../decisions/0005-private-drops-lexical-search.md): understated. A Postgres tsvector is not an index that leaks the document. It is the document, stemmed, and recovering it needs no attack and no model. The lexical index is dropped for private content as a result; the embedding stays plaintext and is what actually leaks the gist, per `docs/research/encryption-and-sensitivity.md` §1.] This defends against a stolen database, not against the server itself. Say so rather than implying more.

**Sealed.** The server holds no key and cannot read the content. Retrievable by exact key, never searchable, and only usable by tools that can decrypt locally. That means Claude Code and Hermes. On browser tools, sealed content can only ever come back as ciphertext.

Sealed is a capability of the tool, not a property of the memory. Credentials and anything under NDA live here, and you accept that ChatGPT will never see them.

### 4.7 Corrections that stick

Every write can retire an earlier fact. Correct something once and the old version is marked superseded rather than sitting alongside the new one, contradicting it. This gives you a decision log as a side effect: not just what is true, but what used to be true and when it changed.

### 4.8 Full inspection and export

Read everything, export everything, delete anything. No opaque storage. An Obsidian vault mirrors the registry as plain markdown, one note per fact with its provenance, so you can browse and search the whole thing without going through an AI tool at all. The database stays the record of truth; the vault is a window onto it.

---

## 5. What it is not

**Not a memory extraction engine.** Turning conversations into clean facts is a crowded and unsolved problem. This system depends on that layer and does not compete in it.

**Not a knowledge graph.** One can sit underneath later if recall quality demands it.

**Not a document store.** Small durable facts, not your PDFs.

**Not an agent runtime.**

**Not a promise that tools will behave.** See below.

---

## 6. The central limitation, stated plainly

Connector protocols standardize how a tool talks to the system. They do not standardize whether it bothers.

**Enforceable:** ChatGPT cannot read your personal namespace. A tool cannot write to the registry unless granted. A grant caps at a sensitivity level. These hold on the server and no client can talk its way around them.

**Not enforceable:** "Claude must remember this." "ChatGPT must check memory before answering." The tool's host decides whether to invoke anything. Nothing compels it.

Three levers push against this, none of them guarantees:

Tool descriptions land in the model's context on every request, on every client, including mobile. Instructions in Claude.ai Projects and ChatGPT Custom Instructions, phrased as triggers rather than suggestions. Lifecycle hooks on the tools that support them, which genuinely are automatic.

Phase 2 measures how well this works. A read and write rate per client tells you which surfaces carry the system and which need help. If browser clients read but rarely write, the fallback is sharper instructions, and after that a browser extension that writes automatically.

---

## 7. Build sequence

Each phase ends with a capability you can use, not a milestone you can report.

**Phase 1. One tool, working end to end.**
The server, sign-in, storage, four operations: bootstrap, search, write, registry lookup. Claude Code on the Mac wired first, with automatic recall at session start. No encryption, no permissions, everything at `open`.
*Ends when:* you tell Claude Code a fact on Monday and a fresh session on Wednesday recalls it without prompting.

**Phase 2. Every surface connected.**
Claude.ai, ChatGPT, OpenWebUI, the second Claude Code, Hermes, Cowork. Each with its own grant. Hermes keeps its learning loop but stops being the only place its findings live.
*Ends when:* a fact you tell ChatGPT shows up in Claude Code the next day, and you notice you did not repeat yourself.

**Phase 3. Permissions and encryption.**
The three sensitivity levels, per-tool grants enforced, encryption at rest, sealed storage for credentials.
*Ends when:* ChatGPT provably cannot see a fact that Claude Code can, and you have checked.

**Phase 4. Quality.**
Supersession working properly, duplicate detection, ageing out stale facts, and a way to test recall against your own real memories rather than a benchmark. The Obsidian mirror lands here.
*Ends when:* a correction you make once does not resurface as a contradiction later.

**Phase 5. Cut.** Multi-user hardening, only if this ever becomes a product.

---

## 8. How you know it is working

**The main one: you stop repeating yourself.** You told one tool something, a different tool needed it later, and it knew. Count those.

**Writes keep pace with reads.** If the store is read often and written rarely, it is decaying and will be stale in a month.

**No tool ever sees outside its grant.** This has to be perfect. One leak is a breach, not a bug.

**You still use it after three months.** The honest test for a personal tool.

**It has failed if:** everything ends up in `open` because setting permissions is a chore, or recall is bad enough that you needed an extraction engine rather than a control plane.

---

## 9. Known risks

**Browser tools may not write on their own.** The largest unknown. Phase 2 measures it. Fallbacks exist and none is free.

**Permissions could become admin work.** Nobody wants to file every sentence into a taxonomy. Most classification has to be inferred, with manual control reserved for exceptions.

**Private is thinner than it sounds.** The searchable index leaks meaning that the encryption was meant to protect. Documented in section 4.6 rather than buried.

**Keeping facts canonical pulls toward building an extraction engine.** Every conflict between two tools writing contradictory values is a small extraction problem. Resist the drift; accept some mess.

**Prior art is unresearched.** Supermemory, Mem0, Zep, Letta, Basic Memory and a dozen others occupy nearby ground. Worth an afternoon before Phase 2 to check whether someone already ships exactly this, and if so, whether their permission model is any good.

---

## 10. Decisions made

Hosted on an always-on cloud instance rather than your own hardware, because the Ubuntu box switches off and browser clients cannot reach it when it does.

Personal utility rather than product, which removes multi-user machinery from every phase.

Straight to a real build with instrumentation, skipping the throwaway measurement rig, because the fallback position is a tool you keep for yourself rather than a failed launch.

Sealed storage stays local-only and that limitation is permanent, not a gap to close.
