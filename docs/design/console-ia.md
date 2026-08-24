# Memory bank console: information architecture and flows

Design sprint, part one. Part two covers visual direction.

## The frame

The working hypothesis was "the UI is for what a folder of markdown cannot do." That is roughly
right and its imprecision would have cost real build time. The sharper frame:

> **The console is a disposition surface: a place to decide and to act. It is never a place to
> read.** Every screen either resolves something into a new state, or shows the consequence of a
> policy so a decision about it can be made. Nothing exists to be browsed.

That single rule does the scoping. It kills the file browser, the tag manager, the timeline
visualisation and the dashboard, all of which would otherwise arrive by reflex.

### Verdicts on the five candidate jobs

**Acting on facts: keep, this is the core.** `lumberroom forget <id>` means holding a uuid in your head,
so the CLI path is really "search, copy an id, delete, hope." Correcting is stronger still: a
correction is a supersede, and a supersede needs the old text visible while you type the new one.
That is a two-pane interaction and it is genuinely awkward in a terminal.

**Managing policy: keep, and build it first, but not the part you would expect.** The value is not
the grant editor; editing two glob lists is not hard and `AUTH_TOKENS` is already an editor. The
value is the **blast radius**. Today, "add `personal:*` to chatgpt's read list" is a change whose
effect you cannot see. With the console it reads *"+412 facts become visible to chatgpt, 3 of them
in personal:finance."* Nothing else can tell you that, because it requires running the proposed
grant against the live store before committing it. This is the highest-value thing in the document.

**Conflicts and staleness: keep, as a queue rather than a report.** `lumberroom review` is fine for one
item and falls apart at thirty, because resolving a conflict is a side-by-side comparison followed
by a fast keyboard verdict, repeated. That is email triage, and the terminal is worst at it.

**Search: keep, and upgrade the justification.** As stated it is `lumberroom search` in a browser, which
adds nothing. What it adds is the **client selector**: run this query *as chatgpt* and see what
chatgpt actually gets back, filtered by its real grant, with a count of what was withheld. That is
the interactive companion to `policy-test.sh`, the difference between a policy you asserted and one
you have watched work.

**Instrumentation: demote from a section to two lines, on the Clients screen.** `lumberroom stats` is a
good table and a browser copy is a worse one. What the browser adds is adjacency: the counts sitting
directly beneath the grant they belong to. "chatgpt may read `user:me` and `global`; chatgpt has read
340 times and written zero times in fourteen days" is one thought, and it is the thought that decides
whether the grant earns its risk.

**One job the hypothesis missed, which outranks two of the five.** The console is where **recall
misses are discovered**. Phase 4 wants an eval fixture seeded from real failures, and the moment you
notice the store failed you is the only moment you will ever bother to record it. So a weak result
set carries a `save this as a recall test case` action.

### What stays elsewhere

**Obsidian** keeps all reading, browsing, wandering, the graph view, and grep over history. It is
better at every one and it works on a plane.

**The CLI** keeps creating facts, setting registry values, export, backup, doctor, eval, and bulk
anything. **The console never grows a compose box.** Every write it performs is a response to a fact
already on screen: supersede, confirm, retire, delete. That constraint is what stops it becoming a
second, worse authoring surface.

---

## Jobs, ranked

| | Job | Cadence | State of mind |
|---|---|---|---|
| 1 | **Can I trust this surface with that?** Read a grant, see how far it reaches, change it, watch what changes | Bursty. Heavy the week a surface is wired, then quarterly | Deliberate, slightly paranoid. Wants to be shown |
| 2 | **That should not be in there.** Remove it now | Two or three times a year | Panic. Under thirty seconds, from any device |
| 3 | **Why does it think that?** Find the fact, see who wrote it and whether it was confirmed | Weekly, one to three times | Suspicious, mid-task, impatient |
| 4 | **Which of these two is true?** The conflict queue | The intended weekly ritual, fifteen minutes | Tidying. Wants speed and keyboard |
| 5 | **Is the loop working?** Per-client read, write and unprompted rates | Weekly for two months, then monthly | The founder checking their own thesis |
| 6 | **What would chatgpt get if it asked this?** | Twice per grant change | Verifying |
| 7 | **What did I used to believe?** The decision log | Monthly at best, and possibly a fiction | Reflective. Not a task |

Job 7 is the one to be suspicious of. The supersession chain is a lovely side effect and exactly the
kind of thing a builder over-invests in. It gets a widget, not a screen.

---

## Screens

Three screens and one panel. That is the whole product.

**Inbox** (landing): the decision queue plus a one-line health strip.
**Search**: semantic search with a client selector; also the recall-miss capture point.
**Clients**: one row per client: what it may see, and what it actually does.
**Fact panel**: opens from anywhere, carries provenance, history and the four actions.
Deep-linkable by id so `lumberroom` can print a URL.

No sidebar. Three items in a top bar, `⌘K` for a command palette, which is the primary navigation
for someone who types. No breadcrumbs, no nesting, **no settings page**: configuration is
environment variables on a box the owner owns, and a settings screen would be a lie about where the
truth lives.

### The landing view, and why

**The Inbox, whose empty state is the health readout.** The first screen answers "is there anything
to deal with," and when the answer is no it answers "is this thing working" with the same pixels.
Those are not two questions, because the only durable evidence of health is that nothing has gone
wrong and facts are still arriving.

```
Nothing to decide. 1,240 live facts · 412 retired · last write 4 minutes ago, claude-code-mac.
```

**The rejected alternative was landing on Search**, which is defensible since it works on day one
when the queue is empty by definition. Rejected because a search box as a landing view makes the
console a lookup tool, lookup is Obsidian's job, and it would be worse at it. Opening on a queue
teaches what the tool is for every time it loads.

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  lumberroom      Inbox 3     Search     Clients                     ⌘K      ● live  │
├──────────────────────────────────────────────────────────────────────────────┤
│  1,240 live · 412 retired · 7 clients · last write 4m ago, claude-code-mac    │
│  claude-code-mac ▁▃▅▇▅▃   openwebui ▁▁▂▁▃▁   chatgpt ▁▁▁▁▁▁  reads only ⚑    │
├──────────────────────────────────────────────────────────────────────────────┤
│  ⟂  CONFLICT                                              project:lumberroom       │
│     "The MCP server listens on 8787"      claude-code-mac · 12 Aug · you      │
│     "The MCP server listens on 8080"      chatgpt         · 17 Aug            │
│     near-identical wording, one digit apart                                   │
│     [n] newer wins   [o] older wins   [b] both true   [↵] open                │
│                                                                              │
│  ⚑  UNCONFIRMED                              global · registry · host        │
│     machines.desktop.os = "Ubuntu 26.04"      hermes · 16 Aug · never you     │
│     [c] confirm   [e] correct   [r] retire   [↵] open                        │
│                                                                              │
│  ⌛ NEVER READ, 5 MONTHS OLD                                    user:me       │
│     "I prefer pnpm over npm for new projects"   14 Mar · 0 reads              │
│     [k] still true   [r] retire   [x] delete                                 │
└──────────────────────────────────────────────────────────────────────────────┘
```

Queue item types, in priority order: **credential tripwire hits** (a write refused for looking like
a secret is a security event and sorts to the top), conflicts, unconfirmed registry values, stale
rows, and the first write from a newly wired client.

### Clients

The screen leads with the pairing deliberately: **what a client may see, and what it actually does,
on the same row.** Neither number means much alone. A wide grant on a client that has never written
is risk with no return; a narrow grant on the client doing all the work is a bottleneck.

```
  CLIENT             REACH                       LAST 14 DAYS
  claude-code-mac    ███  open private sealed    1,102 read  84 write
                     every namespace             61 of those unprompted
  chatgpt            █░░  open                     340 read   0 write   ⚑
                     user:me, global
  openwebui          █░░  open                      88 read  12 write
                     project:*                   all 12 unprompted (inlet)
  ──────────────────────────────────────────────────────────────────────────
  chatgpt
  Reads   user:me           up to  open      ▸ 612 facts admitted today
          global            up to  open      ▸ 190 facts admitted today
  Writes  user:me           up to  open
          Cannot decrypt sealed content.
  ⚑ 340 reads, 0 writes in 14 days. This surface consumes and never contributes.
  policy-test.sh: passed 17 Aug, which was before the last grant edit.  [ run it now ]
```

That last line matters: Phase 3 says run the policy test after every grant edit, and an interface
that lets you forget undoes the phase's exit criterion.

---

## Flows worth recording

**Correcting a fact.** The panel splits, old text dimmed above, an editable field below pre-filled
with the old text so a one-word fix is a one-word edit. Sensitivity may be raised, never lowered,
surfaced as a disabled option with a reason rather than an error after the fact. On save the panel
does not close: it re-renders as the new fact with the history strip one segment longer. Seeing the
chain grow is the confirmation; a toast saying "saved" is not.

If the target was already superseded by another session, the write is rejected and the panel jumps
to the head of the chain, explaining who corrected it and when, then offers to correct that one
instead.

**Deleting.** Four actions from cold: `⌘K`, type a fragment, top result selected, `x`. For `open`
and `private` the row disappears immediately with a ten-second undo, because the anxiety is that the
fact still exists and the interface must show it gone at once. **For `sealed` there is no optimism
and no undo**: an explicit confirm naming the key, because deleting a sealed row destroys the only
copy by construction.

The toast's final frame carries the sentence that actually matters: **"deleted. It may already have
been read: 3 reads, last by chatgpt on 17 Aug."** A deletion does not un-read a fact, and the
console is the only thing that can tell you whether you were too late.

**Resolving a conflict.** Four verdicts: newer wins, **older wins** (common and easy to forget to
build, because the newest write often comes from the client that knows least), both are true, and
open. Nothing in this flow deletes, which is what makes it safe to work fast. Group actions exist
but as a **rule with a dry run**, never a checkbox selection.

**Changing a grant.** The grant reads as clauses, each showing what it currently admits. Editing
shows the **diff, not the state**:

```
chatgpt · read · user:me · open → private

+412 facts become visible to chatgpt.
     3 of them are in personal:finance.        [ show me ]
```

`show me` opens Search as chatgpt with the newly admitted set. The preview is not a summary, it is
the actual rows.

---

## Four design decisions worth keeping

**Provenance is a sentence, not a field list.** Fields fall out of the grammar when they are
ordinary, so a long sentence means something unusual happened.

```
claude-code-mac wrote this on 12 Aug and you confirmed it. It replaced a value from 2 Mar.
hermes wrote this on 16 Aug. You have never confirmed it.
You wrote this on 4 Jul.
```

**Exactly one distinction is coloured: confirmed by you, or not.** It is the only provenance fact
that routinely changes a decision. Five colour-coded chips would be five things to learn.

**Supersession is a value strip, with intervals rather than events.**

```
●  8787      now, since 12 Aug
○  8080      2 Mar  – 12 Aug     claude-code-mac
○  3000      18 Jan – 2 Mar      you
```

"8080 was true from 2 March to 12 August" is what a decision log is for; a list of write timestamps
is an audit trail, and `tool_calls` already is one. **It never becomes a graph**: chains are linear
by construction, because Phase 4 rejects a supersede whose target is already superseded. The only
structure a graph would add is branching, and branching is defined as an error.

**Do not build the grant matrix, and the reason is structural rather than aesthetic.** Sensitivity
has three values and could be an axis. Namespaces cannot: they are glob patterns that overlap, nest
and are open-ended. A matrix needs an enumerable row set and this axis has none. You would have to
choose between rows-as-patterns, where `project:*` and `project:lumberroom` contradict each other and the
overlapping cell is undefined, or rows-as-namespaces, which grows forever and no longer shows the
grant you wrote. Clauses plus live counters plus the diff, instead.

**Never show a cosine score.** It is precise, meaningless to a human, and varies by embedding model,
so the same 0.71 means different things after a provider change. Instead: three bands, of which the
third is the point, `NOTHING STRONG: this is the best of a bad set`, said in those words. Plus a
**relevance cliff** drawn at the largest gap between consecutive results, which is honest in a way a
threshold is not because it describes *this* result set rather than asserting a constant. Plus two
tokens saying why it matched: `meaning + wording`.

---

## What not to build

Each of these would otherwise arrive by reflex:

A browse-all table of every fact with filters and pagination, which is the single most likely thing
to be built and the most useless. A graph view. A stats page. **A compose box**: break that and the
console becomes a second-rate note app. A registry CRUD form. A tag manager. An export or backup UI.
A settings screen. A theme toggle. Onboarding, tours, tooltips explaining what a namespace is, empty
state illustrations. A namespace management screen, since namespaces are created by writing to them. A
word-level diff of two one-sentence facts. Websockets. Anything multi-user. A conversation viewer,
which would mean storing transcripts. Notifications and weekly digests: the Inbox count is the
notification.

---

## Mobile

One job fully, one at a glance, nothing else. **The panic delete is the strongest mobile argument in
the product**: the realisation that something sensitive got stored does not wait for a laptop, and
the surfaces most likely to have stored it are on the same phone.

Keep: the health strip, the Inbox read-only, search with the client selector, the fact panel, and
delete. **Drop grant editing entirely.** A fat-fingered ceiling change is exactly the mistake this
system exists to prevent, and it is never urgent.

---

## What this asks of the rest of the system

- **The console is a client of itself.** Its own credential, its own grant, its own row in the
  Clients list, every action landing in `tool_calls` as `lumberroom-web`. It dogfoods the policy layer: the
  console cannot see what its own grant forbids, and if that is inconvenient then the grant is the
  thing to fix.
- **OAuth grants moved to a versioned Postgres table; bearer grants did not.** [Decision
  0003](../decisions/0003-grants-in-the-database.md) draws that boundary, and
  [`console-spec.md`](console-spec.md) §4.2 rules against a fourth OAuth profile to route around it:
  the console edits OAuth clients through a clause route and shows a bearer client's grant read-only,
  with the file that owns it named on screen. A grant history is still missing, so a change leaves no
  record of what a client used to hold.
- **Two schema additions beyond current plans.** The *reading client* alongside `last_accessed_at`,
  one column, which turns "read 14 times" into "read 14 times, last by chatgpt" and is what makes
  the delete flow able to tell you whether you were too late. And a persistent **not-a-conflict**
  record, one table, without which the queue cries wolf and stops being opened.
- **Build order: the Clients screen with Phase 3**, at the moment the owner first decides whether to
  store something sensitive. Search alongside it, since impersonation is how a grant gets verified.
  The Inbox with Phase 4, because until supersession retires and conflict candidates are returned on
  write, the queue has nothing to hold.

---

## Least confident decisions

**The Inbox as landing view rests on a rate nobody has measured.** How many conflicts, stale rows
and unconfirmed values arrive per week? Five and it is right; one a month and the console opens
empty every time, teaching the owner there is nothing here. **Settled by** running the dedupe
calibration early: dump live-row pairs above 0.85 similarity, count how many land in the 0.90 to
0.97 conflict band, divide by the weeks the store has existed. Under one per week and Search becomes
the landing view. One route change, worth deferring until the number exists.

**Whether impersonated search is used twice or fifty times.** If twice per grant change and never
otherwise, it is a button on the Clients screen rather than a mode of Search. Settled by counting
searches with a non-`me` client selected over the first month.

**Provenance as a sentence may not survive real data.** English degrades when fields combine
unexpectedly. Settled by rendering fifty real rows including the ugliest and reading them all.

**Whether the console should be built before Phase 3 at all.** Four of the five ranked jobs depend on
features that do not exist yet, and an admin UI built early against 200 facts and one client is a
screen with nothing to do. Unused tools do not survive. The recommendation is to hold it.
