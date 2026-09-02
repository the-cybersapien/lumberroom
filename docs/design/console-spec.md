# Console specification

**Date:** 19 August 2026 · **Status:** revised against five reviews; input to five competing visual
designs · **Supersedes the scoping rule in** [`console-ia.md`](console-ia.md)

This document says what the console has to do, ranked, with the evidence for each job and the
server work each one needs. It does not specify a visual style. Five designers will each propose
one. §7 lists the constraints every proposal satisfies, and §10 is the brief the five styles are
judged against, written to stand on its own.

Read §1 first. If you disagree with the verdict there, most of the ranking below changes.

---

## 1. What the owner rejected, and what that settles

The owner's verdict on the Phase 1 design, rendered as [`console-preview.html`](console-preview.html):
**"I did not like the design much, it is hard to read and operate."**

The rejected design scoped itself with one rule: *the console is a disposition surface, a place to
decide and to act, never a place to read.* That rule killed the file browser, the tag manager and
the timeline. The owner's first request had been *"some UI I can look at memories."*

Three findings settle whether the rule was wrong, badly executed, or both.

### 1.1 The rule's premise goes unmet in this tree

The rule survives only if something else already serves reading. Both design passes named the
Obsidian mirror, and [`docs/design/README.md`](README.md) puts it this way: *"Reading belongs to
the Obsidian mirror, which is better at it and works offline."*

The mirror that argument describes lives in [`src/services/export.rs`](../../src/services/export.rs).
It renders registry notes as well as memory notes, writes supersession as wikilinks so the graph
view carries the decision log, keeps a `lumberroom/index.md` with per-namespace counts, names files
`{date}-{slug}-{shortid}.md`, and defines a `tombstone()` for a note whose row has left the store.

**No route and no tool calls it.** `grep -rn "export::" src/` returns nothing. The only callers are
two assertions in [`tests/integration.rs:1247`](../../tests/integration.rs), so the renderer is
exercised and unreachable at once, which strengthens §6.2 item 11.
What the owner can run today is `lumberroom export --obsidian <path>`, which pages `GET /admin/export`
and writes its own
note in `writeObsidianNote` ([`lumberroom:392`](../../lumberroom)). That note carries `id`,
`namespace`, `sensitivity`, `source_client`, `created_at` and `tags`. It carries no confirmation
flag, no access count, no `supersedes` or `superseded_by`, no registry entry, no index, and no
tombstone. Every file is named after a uuid, so a vault listing is a wall of hex. A fact deleted
from the store keeps its note forever with nothing marking it.

The export has been credited with *"writing tombstones and never unlinking."* Half of that holds.
`writeObsidianNote` unlinks nothing, because it deletes nothing at all, and it writes no tombstone
either. Implemented and reachable are two claims, and the ledger in §6 keeps them apart everywhere.

The premise holds on paper and goes unmet in the tree. A reviewer defending the old rule has to
defend a mirror the owner cannot get.

### 1.2 The preview failed at reading inside its own rule

Rendered at 1440×900, dark theme, the preview breaks the readability constraints its own visual
document argued for. Each defect below became a constraint in §7.

- **Everything the owner operates sits in an 11px to 13px mono band, separated by ink steps.** Body
  text is 13px monospace, metadata 12px, labels 11px uppercase with 0.08em tracking, and five ink
  steps carry the hierarchy (15.9:1 down to 3.6:1). A second family and larger sizes do exist:
  `--serif` at 15px in `.claim .t` and `.prose`, `h1` at 17px
  ([`console-preview.html`](console-preview.html) lines 45, 86, 119, 188). They land on asides,
  document prose and single headings. The queue head, the verdict strip, the client table, the
  policy grid and the status strip are mono inside the three-size band, and lightness does the
  separating alone. A designer who reads this defect as "use a serif, go bigger" repeats the
  preview, which already did that on the rows above and got rejected anyway. The defect is one
  family and three sizes carrying every operating surface.
- **Provenance sits about 800px from its fact, in a column too narrow to hold it.** On the Inbox
  conflict item, `.claim` is a grid of `minmax(0,1fr) 27ch`
  ([`console-preview.html:186`](console-preview.html)). The arithmetic at 1440 wide: `main` is
  1180px border-box with 16px side padding, `.mock` adds a 1px border and `.q` 12px padding, which
  leaves a 1122px row. 27ch of 13px mono runs about 211px, so the flexible column ends near 903px
  while `The port is 8787` ends near 110px. The provenance string
  `claude-code-mac · 19 Aug · confirmed by you` is 43 characters and wraps onto two lines in a 27ch
  column. The one provenance fact the visual document called decision-changing is the hardest thing
  on the row to pair with its subject.
- **Annotation outweighs content.** Dotted `NOT BUILT` and `WIRED` chips share the box treatment,
  size and family of the keyboard verdicts beside them. On the credential-tripwire item, two of five
  lines are commentary about implementation status.
- **The loudest element on the triage screen is a configuration warning.** The KEK mismatch renders
  as a full-bleed salmon slab across the top of the Inbox, above every queue item
  ([`console-preview.html:381`](console-preview.html)), carrying 214 characters of prose before its
  monospace tail.
- **"Not granted" is nearly invisible and "sealed" is nearly illegible.** On the Policy grid an
  ungranted cell is a lone middle dot at `--ink-faint` (3.62:1 dark, 3.30:1 light). The widest and
  most dangerous grant prints the word `sealed` ten times across one row, in `--sens-sealed` on
  `--sens-sealed-bg`, measuring 5.07:1. The safest cell shouts and the riskiest one whispers.
- **The client lens claims something the screen does not do.** A band reads *"viewing the whole
  interface as chatgpt sees it, 1,204 of 14,382 facts visible"* while the table beneath it shows
  every client's grant, unchanged. A control that announces a re-render and re-renders nothing reads
  as a broken build.
- **Ten top-level sections, seven of them screens, one visible at a time, number keys to switch.**
  The other three are commentary about the design. The artifact is a specification wearing console
  chrome. The owner was asked to judge an interface and got a document about one.

### 1.3 What the rejection does not settle

The owner said hard to read and hard to operate. Nothing in that sentence endorses a browse-all
table with filters and pagination, which the old IA called the single most likely thing to be built
and the most useless. That warning stands. §3 keeps it by scoping the console's reading to the three
properties a markdown mirror cannot have.

### 1.4 The verdict

The scoping rule rested on a premise this tree does not meet, and it was executed in a way that
failed at reading inside its own terms. Wiring the mirror (§6.2 item 11) repairs the premise, so the
verdict cannot rest there. It rests on three properties no mirror holds at any price. **The console
reads,** and every read surface has to hold at least one of them:

1. **Live.** The store as it is now, not as the last export left it.
2. **Policy-aware.** What a named client would get, which no file on disk can express.
3. **Actionable.** Every row carries the verdict that disposes of it.

A read screen holding none of those three belongs in Obsidian, and §8 keeps it out.

---

## 2. The jobs, ranked

Ranked by how often the owner does the job, whether anything serves it today, and what a failure
costs. Two jobs come first for different reasons: job 1 is why the owner opens the console, and job
2 is the one that must never be wrong.

The PRD's named product failure sits behind jobs 2 and 3. §8: *"It has failed if: everything ends up
in `open` because setting permissions is a chore."* §4.5: *"The design target is that you configure
this two or three times and then forget it."* A console that turns classification into a chore fails
the same way the store does. Jobs 2 and 3 exist to be finished and closed, not visited.

**The cadence column is this document's assumption, not a measurement.** Nobody has counted how
often the owner does any of these, one client is wired today (§9 Q8), and job 6's arrival rate was
already flagged as unmeasured. Read the column as a stated guess. The ranking survives the guess
going wrong, because the other two inputs carry it: whether anything serves the job today, and what
a failure costs, are both evidenced below.

| # | Job | Cadence (assumed) | Served today | Cost of getting it wrong |
|---|---|---|---|---|
| 1 | See what the store thinks it knows about me | Weekly | Almost nothing | The owner stops opening the console |
| 2 | Decide what a surface may see, and see the consequence first | Bursty, then quarterly | Half: the state is visible, the effect is not | A breach, which PRD §8 says has to be zero |
| 3 | Approve, refuse or revoke a client | Once per surface | Approval yes, revocation no | A stranger keeps reach, or a surface stays blocked |
| 4 | Get one thing out of the store, now | Two or three times a year | On a laptop, with a uuid | Sensitive content stays readable for hours |
| 5 | Understand why the store thinks that | Weekly | Two halves that never meet | The owner distrusts the store and stops writing |
| 6 | Triage conflicts, stale rows, unconfirmed values | Weekly ritual | `lumberroom review`, then copy uuids | The store rots, PRD §8's decay measure |
| 7 | Notice the store has stopped working | Never on purpose | A boolean and a log line | Private writes refused for weeks |
| 8 | See what one client would get for one question | Twice per grant change | Nothing | Job 2 stays an assertion |
| 9 | See what I used to believe | Monthly at best | Wikilinks nobody can reach | Nothing. It is a pleasure rather than a task |

**How to judge a design against this.** Jobs 1 through 4 have to work. A proposal that shines at job
6 and stumbles at job 1 has failed, and so has one that reads well and cannot delete on a phone.
Jobs 8 and 9 may be a control and a widget inside another screen.

### Job 1. See what the store thinks it knows about me

**Who:** the owner. **Cadence:** weekly, and after any session that wrote a lot.
**Trigger:** curiosity, or a model saying something about them that sounds off.
**Done looks like:** they have read the recent arrivals in each namespace, recognised the facts, and
either left them alone or acted on two.
**Today, without a console:** `lumberroom search` needs the query you already know. `lumberroom bootstrap`
prints the digest, which is a fixed sample. The Obsidian mirror is thin and its good renderer is
unreachable (§1.1). Nothing lists what arrived this week.

This is the owner's own request and the job with the weakest current surface. It has three distinct
readings, and a design that serves one and calls it done fails the other two:

- **the arrivals view:** what came in recently, by whom, in which namespace, at which sensitivity
- **the inventory view:** how much sits in each namespace, per both policy axes
- **the one-fact view:** the panel in job 5

### Job 2. Decide what a surface may see, and see the consequence before committing

**Who:** the owner. **Cadence:** heavy the week a surface is wired, then quarterly.
**Trigger:** connecting a client, or a suspicion that one reads too much.
**Done looks like,** for an OAuth client: the grant changed, and before committing the owner saw
how many facts the change admits, which namespaces they sit in, and a sample of the actual rows.
**For a bearer client on `AUTH_TOKENS`,** decision 0003 forbids the console writing the grant, so
done is the preview plus a clause block to paste into the file, and a later visit where the console
flags any difference between the live grant and the clause it last previewed. Half the job, and the
half that carries the value.
**Today, without a console:** for a bearer client, edit `AUTH_TOKENS` and restart. For an OAuth
client, pick one of three profiles at the consent screen. In both cases the effect on the store is
invisible. `lumberroom clients` prints the current state and cannot preview a change.

PRD §8: *"No tool ever sees outside its grant. This has to be perfect. One leak is a breach, not a
bug."* The blast radius is the only thing that turns that from an assertion into an observation.

### Job 3. Approve, refuse or revoke a client

**Who:** the owner. **Cadence:** once per surface, so seven or eight times, then on incident.
**Trigger:** a client registers itself and holds nothing until the owner decides.
**Done looks like:** the client holds a grant the owner chose, or holds nothing and cannot come
back.
**Today, without a console:** `POST /oauth/consent` does approval inside the authorization redirect,
with the password in front of it. Revocation has no path at all: `revoke_client` is implemented in
[`src/adapters/postgres/oauth.rs:160`](../../src/adapters/postgres/oauth.rs) and nothing calls it.
[`README.md`](../../README.md) says revocation is one `UPDATE` in psql, and it is.

Approval is the purest disposition act in the system: one decision that hands a stranger reach over
the whole record. The console's addition is out-of-band approval and revocation, because the
in-redirect path already works.

### Job 4. Get one thing out of the store, now

**Who:** the owner, often on a phone. **Cadence:** two or three times a year.
**Trigger:** realising something sensitive got written.
**Done looks like:** the server confirms the row is gone inside thirty seconds, and the owner reads
how many times it had been served and when it was last read. Naming the last reader needs the
migration in §6.2 item 7, and no design draws it as present.
**Today, without a console:** `lumberroom forget <id>` needs the uuid, or `--query` plus a typed `yes`,
from a laptop. Nothing tells them whether the fact was read before it went.

The phone case is the strongest mobile argument in the product: the surface that stored the thing is
usually on the same phone.

### Job 5. Understand why the store thinks that

**Who:** the owner, mid-task. **Cadence:** one to three times a week.
**Trigger:** a model stating something as settled.
**Done looks like:** they know which client wrote it, on what date, whether they confirmed it, what
it replaced, and how often it has been served.
**Today, without a console:** `lumberroom search` prints score, namespace and content. Provenance needs
`GET /admin/memory/{id}`, which needs an id you do not have. The two halves never meet.

Job 5 is one panel away from job 1. Serving job 1 well nearly serves this, which is why they rank
adjacent rather than being merged.

### Job 6. Triage conflicts, stale rows and unconfirmed registry values

**Who:** the owner. **Cadence:** the intended weekly ritual, fifteen minutes.
**Trigger:** deciding to tidy, or a conflict noticed during job 1.
**Done looks like:** the queue is shorter than when they opened it and no decision was reversed.
**Today, without a console:** `lumberroom review` prints all three lists well. Acting means copying two
uuids into `lumberroom supersede`. Fine at one item, awkward at thirty.

The old IA ranked this fourth and rested it on an unmeasured arrival rate. That unknown is still
open (§9), so this job earns a surface that is honest when empty rather than a landing page.

### Job 7. Notice the store has stopped working

**Who:** the owner. **Cadence:** never on purpose; the console has to volunteer it.
**Trigger:** none. That is the problem.
**Done looks like:** they learn that the key does not match, or a wired surface has gone silent, or
the embedder fell back to hash, on the visit they made for another reason.
**Today, without a console:** `kek_verified` is a boolean on `/readyz` and a line in the boot log. A
key mismatch keeps every open read and write working and refuses every private write, and the server
reports itself healthy.

### Job 8. See what one client would get for one question

**Who:** the owner. **Cadence:** twice per grant change, which makes it a companion to job 2.
**Trigger:** having just changed a grant, or doubting one.
**Done looks like:** they typed the question, chose the client, and read the rows that client would
have received, with a count of what was withheld.
**Today, without a console:** nothing. `scripts/policy-test.sh` asserts one case per run.

### Job 9. See what I used to believe

**Who:** the owner. **Cadence:** monthly at best.
**Trigger:** reflection.
**Done looks like:** they read one value's history as intervals.
**Today, without a console:** `memory_search` takes an `include_superseded` argument that the CLI
never sends, or the wikilinks in a mirror nobody can reach.

Rank it last and give it a widget inside the job 5 panel. The supersession chain is the kind of
thing a builder over-invests in, and Phase 4 refuses a supersede whose target is already superseded,
so the chain is linear and needs no graph.

### Jobs the CLI keeps, with the reason

A console job the CLI already serves needs an argument stronger than "a browser is nicer."

| Job | Stays in the CLI because |
|---|---|
| Creating a fact, setting a registry value | The console never grows a compose box. Break that and it becomes a second, worse note app |
| `lumberroom export --obsidian`, backups | Batch work with a filesystem destination |
| `lumberroom eval`, `lumberroom recall` | Runs against a fixture on disk, output is a number to compare over time |
| `lumberroom doctor`, `lumberroom login` | Diagnosing the transport the console would be riding |
| `lumberroom seal` / `lumberroom unseal` | The key is on the client. A browser must never hold it |
| Bulk anything | A rule with a dry run beats thirty checkboxes |

---

## 3. Reading: what Obsidian solves, and what it cannot

**What a wired mirror would solve.** Full-text search over every fact, a graph view over the
supersession wikilinks, backlinks, offline access, and grep. All four beat anything a console at
`/console` will do in a browser tab. If the owner wants to wander through the record, that is the
surface, and §6 prices wiring it as one of the cheapest items in this document.

**What no mirror can solve, at any price.**

1. **Freshness.** The vault is a snapshot. "What did the last hour write" is a question about the
   store, and the mirror answers it as of the last run.
2. **Policy.** A markdown file cannot express what ChatGPT would see. Jobs 2 and 8 are unreachable
   from a folder.
3. **Action.** Reading a fact and deciding it is wrong happen in the same second. The mirror is one
   way by design and states so at the top of every note.
4. **Private content.** `EXPORT_MAX_SENSITIVITY` defaults to `open`, and a vault synced to a third
   party defeats the encryption a private row was given. The design leaves anything above open out.

So the console reads, bounded by the three properties in §1.4, and the recommendation is to wire
`export::run` as well. They serve different halves and neither replaces the other.

---

## 4. Who the console is, and what has to change

The old design said the console is a client like any other, holding its own credential and grant,
`sealed_capable: false`, dogfooding the policy layer. Half of that survives contact with the
enforcement path.

### 4.1 A read-only console cannot do jobs 4 or 6

Every disposition action goes through a service that demands the **write** grant at the row's stored
sensitivity, in the row's namespace:

- `review::confirm` and `review::supersede` both call `writable_row`, which requires `can_read` and
  `can_write` ([`src/services/review.rs:225`](../../src/services/review.rs))
- `write::validate_supersedes` requires the same on the supersede target
  ([`src/services/write.rs:428`](../../src/services/write.rs))
- `forget::by_id` requires `may_delete` and then `deletable`, which is `can_read` and `can_write`
  ([`src/services/forget.rs:245`](../../src/services/forget.rs))
- `forget::sealed_item` requires `may_delete` and `can_write` at `sealed` in that namespace

"Read everywhere up to sealed, no writes" therefore buys a console that reads jobs 1, 5 and 7
and acts on nothing. The honest grant for the console in this specification is:

```json
{ "client": "lumberroom-ui", "token": "...",
  "read":  [{ "namespace": "*", "max": "sealed" }],
  "write": [{ "namespace": "*", "max": "sealed" }],
  "mayDelete": true, "sealedCapable": false, "registryWrite": true }
```

That is `AUTH_TOKENS`, and every field above parses today
([`src/config.rs:79`](../../src/config.rs)). Three consequences the owner should decide on rather
than inherit:

- **The console holds the widest grant in the system.** The dogfooding claim inverts: the console
  cannot demonstrate policy against itself, because its own grant admits everything. It demonstrates
  policy against *other* clients, through jobs 2 and 8.
- **`sealedCapable: false` is the one honest narrowing and it is worth keeping.** The server hands
  every caller ciphertext for a sealed item and the flag reports whether those bytes are of any use
  ([`src/services/mod.rs:157`](../../src/services/mod.rs)), so the console renders sealed rows as
  the redaction state and the flag makes that a server answer rather than a UI choice.
- **`registryWrite: true` is forced, and it is forced by a read.** `GET /oauth/clients` gates on
  `principal.registry_write` or an owner session cookie
  ([`src/authserver/routes.rs:870`](../../src/authserver/routes.rs)). Listing the clients is a read,
  and today it takes a write capability that also permits rewriting
  `services.postgres.endpoint` ([`src/services/registry.rs:157`](../../src/services/registry.rs)).
  Confirming an unconfirmed registry value in job 6 needs the same flag for a better reason, since
  that action is a registry upsert. Splitting the read gate belongs in the ledger regardless.

### 4.2 No OAuth profile expresses this, and the clause route wins

`GrantProfile` has three values ([`src/domain/oauth.rs:233`](../../src/domain/oauth.rs)). `Full`
grants `*` at sealed for read and write, plus `sealed_capable`, `may_delete` and `registry_write`.
`Standard` and `Narrow` cap at `open` and grant none of the flags. Read and write are the same set
at every profile.

So the console's credential can only be a static bearer token today, which means it changes with a
restart, and the editable authority (`set_client_grant`, per-clause and applied live) is reachable
only from the consent POST.

**Ruling: build the clause route and keep the three profiles as presets over it. No fourth
profile.** `set_client_grant` already takes clause lists rather than a profile, so the store is
ready; what is missing is a route that calls it outside a consent flow, with per-clause ceilings.
A `console` profile would be a second model of the same idea sitting beside a mechanism that
expresses all four shapes, and it turns into dead weight the day the route lands. One artifact
answers this section, ledger item 5 and ledger item 6 together.

The console's own credential stays a static bearer token either way, because decision 0003 keeps
bearer grants in the file. It changes with a restart, and it changes about never.

### 4.3 Session, and the boundary decision 0003 draws

The authorization server already has an owner browser session: HMAC over an expiry keyed by
`OAUTH_COOKIE_SECRET`, `HttpOnly`, `Secure` off loopback, `SameSite=Lax`, `Path=/oauth`
([`src/authserver/session.rs`](../../src/authserver/session.rs)). A console can reuse it, and the
cookie path has to widen or the console needs its own cookie signed by the same secret.

Reusing it puts a question in front of the owner rather than behind a layout choice: **the consent
screen is the one place the password is taken.** A console session that can grant reach is a session
whose theft grants reach. Whichever way the owner decides, the browser never holds the MCP token.

**Decision 0003 draws a boundary the console works inside.** A bearer client's grant lives in
`AUTH_TOKENS` and stays authoritative there, because *"a console that could rewrite them would put
the server in charge of its own access control."* The console therefore edits OAuth client grants
and, for bearer clients, shows the grant read-only with the file that owns it named on screen.

No route renders that today. `GET /oauth/clients` returns `list_clients` from the OAuth store alone
([`src/authserver/routes.rs:897`](../../src/authserver/routes.rs)), and `/admin/whoami` answers for
the presented credential only ([`src/http/mod.rs:409`](../../src/http/mod.rs)), so every bearer
client appears in no list at all, including the CLI and the console itself. Ledger item 14 funds it,
and once it lands the console shows up in its own client list, so every design should expect a
self-row. Job 2's value sits in the preview, and the preview works either way.

### 4.4 The console is a program, and it needs a decision before a layout

Three facts force a component this specification had left unnamed. `/admin/*` and `/mcp`
authenticate on a bearer token and nothing else ([`src/http/mod.rs:80`](../../src/http/mod.rs)), the
owner session cookie is `Path=/oauth`, and the browser never holds the MCP token. Two shapes satisfy
all three:

- **A backend for the front end.** A small server holds the console's bearer token, serves the
  console's HTML and JSON, and issues its own session cookie signed by `OAUTH_COOKIE_SECRET` with
  the password check delegating to the authorization server's verifier. Mounted same-origin behind
  the same proxy, it shares an origin without sharing a cookie path.
- **Session acceptance widened onto `/admin/*`.** One process cheaper. It also turns a stolen cookie
  into a store-wide credential over every operator route, and it puts the server's own access
  control behind a browser session, which is the shape decision 0003 refused for grants.

**Ruling: the backend for the front end.** It keeps the token out of the browser, it holds the
session's blast radius inside the console's own routes, and it leaves `/admin/*` bearer-only. Ledger
item 0 prices it. It also settles item 12: the console's bearer token already carries
`registry_write`, so `GET /oauth/clients` answers with no gate change, and splitting that read gate
becomes a cleanup rather than a dependency.

**Signing in from a phone, mid-panic.** Job 4's thirty-second clock is a sign-in clock as much as a
delete clock. The working case is a phone already signed in, with a session that outlives the gap
between panics, which for a job done twice a year is months. A password prompt on a signed-out phone
spends the whole budget before the owner reaches the row. Two knobs the owner sets rather than
inherits: the session TTL, and whether a phone gets a longer one than a laptop. Both sit beside §9
Q3, since a session that survives for months is a session whose theft survives for months.

---

## 5. What any design must be able to render, per job

The data contract, so five designers work against the same shape. No layout implied.

**A † marks a field that needs the numbered §6.2 item beside it before anything can render.** Lay
out the space for one if the design wants it, and never draw it populated. Everything unmarked
answers from a route that exists today or from a ledger item with no migration behind it.

| Job | Must be able to render |
|---|---|
| 1 | Per namespace: count, ceiling reached, most recent write. Per fact: date, namespace, sensitivity, source client, content, confirmed or not |
| 2 | Per client: read clauses with ceilings, write clauses with ceilings, `registry_write`, `sealed_capable`, `may_delete`, consent state, last used, how it registered, which authority owns the grant †14 for bearer clients. For a proposed change: facts admitted, namespaces they sit in, a sample of rows. For a bearer client: the same fields read-only, the clause block to paste, and a drift flag when the live grant differs from the change last previewed |
| 3 | Registration: client id, self-declared name, redirect URIs, how it registered, what it holds now. The three profiles with their one-line descriptions and what each admits. Allow, refuse, revoke |
| 4 | One fact with its full text, read count, last read time, sensitivity. A second deliberate act before the delete commits, which is the confirm gate §7 specifies. An after-state, taken from the server's answer, saying whether it was read before it went. The last reader by name †7 |
| 5 | Provenance as a sentence: who wrote it, when, whether the owner confirmed it, what it replaced. Value history as intervals, not events |
| 6 | Conflict pair side by side with both provenances and the similarity band. Stale row with age and read count. Unconfirmed registry value with key, value and review date. A both-true verdict †8 |
| 7 | Key state, embedder state, per-client call and write rates, last write per client †14, and a named silent surface †14 |
| 8 | Query, client selector, the rows that client would get, the count withheld, the namespaces withheld |
| 9 | One key's values as dated intervals with the client that changed each |

---

## 6. Capability ledger

Three columns per row: what already answers, what needs building, and how much. "How much" counts
artifacts rather than hours: ports, queries, routes, migrations, decisions. Nothing here is an
estimate of effort.

### 6.1 What answers today, with no server work

| Need | Route or tool | Bounds worth knowing |
|---|---|---|
| Per-client grant, consent state, last used, **for OAuth clients only** | `GET /oauth/clients` | Requires `registry_write` or the owner session. It returns `list_clients` from the OAuth store, so no `AUTH_TOKENS` client appears, the CLI and the console included. Item 14 covers the rest |
| Per-client calls, reads, writes, failures, sessions, unprompted rates, write-to-read ratio | `GET /statsz?by=client&hours=N` | Tenant scope needs `read *` at `sealed`; a narrower caller gets only its own row. A windowed `GROUP BY client` over `tool_calls` ([`src/adapters/postgres/tool_calls.rs:56`](../../src/adapters/postgres/tool_calls.rs)), so a client with no calls inside the window returns no row and vanishes from the screen rather than reading as silent |
| Store decay: live rows, never retrieved, percentage, median age, superseded count, oldest never retrieved | same call, `staleness` | Same gate. This is the junk detector, already built |
| Per-tool calls, failures, unprompted, p50, p95 | `GET /statsz` | |
| Both-axes namespace inventory, sealed inventory, counts, recent facts, registry summary | `context_bootstrap` over `POST /mcp` | Cached on client, project, readable namespaces and budget ([`src/services/bootstrap.rs:110`](../../src/services/bootstrap.rs)). The sample is fixed and offers no way to browse |
| Search with score, similarity, namespace, sensitivity, source client, tags, date, `primary` | `memory_search` over `POST /mcp` | As the console's own principal only |
| One fact with full provenance, access count, supersession links, decrypted if the grant allows | `GET /admin/memory/{id}` | 404 covers both "missing" and "not yours" |
| Conflict pairs, both sides, similarity | `GET /admin/review/conflicts` | Two extra round trips per pair by design |
| Stale rows | `GET /admin/review/stale?days=&limit=` | Never-retrieved and older than N |
| Registry due for review, non-canonical keys | `GET /admin/review/registry` | |
| Supersede, delete, sealed delete | `POST /admin/memory/{id}/supersede`, `DELETE /admin/memory/{id}`, `DELETE /admin/sealed` | Write grant at the row's level; delete also needs `may_delete` |
| Key state, embedder state, auth mode, db latency | `GET /readyz` | `kek_verified` is the field job 7 turns on |
| Effective grant for the presented credential | `GET /admin/whoami` | Answers from the enforcing path |

**On the missing search route.** There is no HTTP search endpoint, and that does not block anything.
A console backend calls `POST /mcp` with its own bearer token, which is what `lumberroom`
does through `callTool`. `allowed_hosts` already admits loopback and `PUBLIC_URL`
([`src/http/mod.rs:146`](../../src/http/mod.rs)), so a console on the same box works. The real gap
is impersonation, not reachability.

**On browsing with today's routes.** `GET /admin/export` is the closest thing to a list
endpoint, and
its shape decides what job 1 can show without new SQL. `list_for_export` is live rows only
(`superseded_by IS NULL`), **oldest first**, offset paged, with no namespace filter in SQL and a
ceiling bounded by `EXPORT_MAX_SENSITIVITY`, which defaults to `open`
([`src/adapters/postgres/memory.rs:1144`](../../src/adapters/postgres/memory.rs)). "The newest fifty
facts in `personal:finance`" is therefore not available. Job 1's arrivals view needs a query.

### 6.2 What needs server work

Ordered by the job it unblocks. No row here is a migration unless it says so.

| # | Capability | Job | What exists | What to build | Size |
|---|---|---|---|---|---|
| 0 | **The console application itself** | all | Nothing. `/admin/*` and `/mcp` take a bearer token; the owner session cookie is `Path=/oauth` | The backend ruled in §4.4. It holds the console's bearer token, serves the console, issues its own session cookie signed by `OAUTH_COOKIE_SECRET`, and delegates the password check to the authorization server's verifier. Plus a serving story and the session TTL knobs in §4.4 | 1 process, 1 session route pair, 1 deploy change. No migration |
| 1 | **Recent facts, filtered and paged** | 1 | `list_for_export` (oldest first, live only) | One port method: newest first, optional namespace and sensitivity filter, keyset on `(created_at, id)`, live and superseded selectable. One route. Both axes inside the query. It sits beside `list_for_export` rather than replacing it, because the two contracts agree on nothing: the export method is oldest first, offset paged and ceilinged by `EXPORT_MAX_SENSITIVITY`, this one is newest first, keyset paged and filtered on both axes. Narrow per-consumer port methods are the house style, and merging them later would hand the mirror a policy filter it must not have | 1 port method, 1 query, 1 route. No migration |
| 2 | **Blast radius: what a hypothetical grant admits** | 2 | The digest's `inventory` is already the both-axes filtered count, keyed to the caller ([`src/services/bootstrap.rs:54`](../../src/services/bootstrap.rs)) | The same computation over a grant passed in as an argument: `count_visible(tenant, &[NamespaceCeiling]) -> HashMap<String, i64>`. It shares the digest's grant-filter SQL rather than copying its shape; the digest carries seven grant-filtered subqueries and the tests count the joins, so a second copy is the duplication the trap log warns about. Guard it on `reads_whole_store`, since asking about another client's reach is a whole-store read. One route | 1 port method, 1 query, 1 guard, 1 route. No migration |
| 3 | **Client lens: search as a named client** | 8, 2 | `SearchQuery.primary` already carries `Vec<NamespaceCeiling>`, so the query layer needs nothing | Three couplings to the caller, not one, and an earlier draft of this row priced only the first. Substitute the lens ceilings at `filter_readable(&ctx.principal, ...)` in `search::run` ([`src/services/search.rs:94`](../../src/services/search.rs)) **and** at the second call inside `other_namespaces` ([line 216](../../src/services/search.rs)), which builds the secondary candidate set when the query names no namespace. Suppress `touch_accessed` ([line 180](../../src/services/search.rs)) for a lens query, or every rehearsal ages the store, corrupting both the staleness detector and job 4's read history. Then a resolver from a client id to a grant, spanning two authorities with no single home: bearer grants come from `grant_for` in `src/adapters/auth`, OAuth grants from the client store, and decision 0003 means it reads `AUTH_TOKENS` without writing it. Same `reads_whole_store` guard | 2 substitution sites, 1 touch suppression, 1 resolver, 1 guard, 1 route. No migration |
| 4 | **Approve and revoke out of band** | 3 | `set_client_grant` takes clause lists; `revoke_client` is implemented with no caller | Two routes behind the owner session, and the decision in §4.3 about what a console session may grant | 2 routes, 1 security decision. No migration |
| 5 | **Per-clause grant editing** | 2 | The store is ready; only the consent POST writes a grant | A route that takes clauses with per-clause ceilings, plus the profiles-as-presets decision. Bearer clients stay read-only per decision 0003 | 1 route, 1 model decision |
| 6 | **A console grant expressible without a restart** | all | `AUTH_TOKENS` expresses the shape; no OAuth profile does | Nothing. §4.2 rules that the console keeps its bearer token and changes on restart, and item 5 covers every other client | Closed by §4.2 |
| 7 | **"It may already have been read, last by chatgpt"** | 4 | `access_count` and `last_accessed_at` exist | The reading client beside `last_accessed_at`, written by `touch_accessed` | **1 migration**, 1 port signature change, 1 adapter change |
| 8 | **"Both true" survives the night** | 6 | `conflicts()` recomputes on demand from similarity | A not-a-conflict record keyed on the pair, honoured by `conflicts()`. Without it the queue re-raises every dismissed pair and stops being opened | **1 migration**, 1 port method, 1 query change |
| 9 | **Credential tripwire hits as a queue item** | 6, 7 | `write.rs` logs the rule and returns a message; `tool_calls` records `succeeded: false` with no reason | A refusal record carrying client, namespace, rule and time, and never the matched text. The old IA sorted this above everything else in the queue and it has nothing to read | **1 migration**, 1 port method, 1 route |
| 10 | **Why a hit matched** | 8, 1 | `Hit` carries `score`, `similarity` and `primary`; the lexical component is not returned | The `ts_rank` contribution alongside the blend, so "wording" and "meaning" are an observation. Without it, drop the claim | 1 field through the search path |
| 11 | **A reachable Obsidian mirror** | 1, 9 | `export::run` renders registry notes, wikilinks, tombstones, an index and a manifest, and nothing calls it | One route returning files and manifest, and a CLI writer that honours the manifest contract: write every file, tombstone what is missing, unlink nothing | 1 route, 1 CLI rewrite. No migration |
| 12 | **Listing clients without a write capability** | 2 | `GET /oauth/clients` gates on `registry_write` | Nothing blocking. The §4.4 backend presents the console's bearer token, which already carries `registry_write`, so the route answers as it stands. Splitting the read gate from the write flag stays the right cleanup and gates nothing | 1 gate change, optional |
| 13 | **Classification rules, read-only** | 2 | Rules resolve once at boot beside the key check, and `SENSITIVITY_DEFAULTS` overrides the table outright | Show the resolved rules and name the authority. Editing is dishonest until the resolution stops being boot-time. A console writing rows the server ignores is worse than no editor | 1 route, read-only |
| 14 | **Bearer clients as data, and last write per client** | 2, 3, 7 | Nothing lists an `AUTH_TOKENS` client, and `statsz` drops any client that stayed silent through the window | One route rendering the parsed bearer grants, roughly `/admin/whoami`'s shape over the whole list, which doubles as job 7's roll of expected clients. Plus an unwindowed `max(created_at)` per client, so silence renders as a date instead of an absence. The console appears in its own list | 1 route, 1 query. No migration |
| 15 | **Undoing a supersede** | 6 | Nothing clears the links. `review::supersede` refuses the direct reversal, since a superseded row cannot be the replacement, and deleting the winner clears the links as a side effect ([`src/adapters/postgres/memory.rs:987`](../../src/adapters/postgres/memory.rs)) while destroying the correction | One route clearing `superseded_by`, `superseded_at` and `supersedes` across the pair. The only recovery in the triage flow that leaves the replacement row alive, and the reason §7 can charge one deliberate act for a supersede instead of two | 1 route, 1 port method, 1 query. No migration |
| 16 | **An undo window on delete** | 4 | Nothing. `DELETE /admin/memory/{id}` is a hard delete with the chain links cleared first ([`src/adapters/postgres/memory.rs:989`](../../src/adapters/postgres/memory.rs)); the tree holds no soft delete, no tombstone and no pending state | A `deleted_at` column, a hold window, a reaper, and a live predicate in every query that reads memory. §7 rules delete has no undo, so this row prices the alternative rather than proposing it | **1 migration**, plus a predicate in every read path. Priced to be declined |

### 6.3 Ranking the server work against the rest

**Item 0 comes first, because nothing else runs without it.** The backend ruled in §4.4 is the
console, and whether job 4 works from a phone at all is decided there.

Items 1, 2 and 3 unblock jobs 1, 2 and 8 with no migration between them. Items 2 and 3 are the two
ideas worth keeping from the old design, and both cost less than that design assumed: the counting
is the digest's own arithmetic over a grant passed in, and the lens is a parameter on a query that
already takes per-namespace ceilings. Item 3 costs more than the first draft of this ledger claimed,
and it is still the cheapest way to turn PRD §8's zero-leak requirement into something the owner
watches happen.

Item 14 ranks with them. No migration, it repairs three sections that assumed a route which does not
exist, and job 7's whole failure mode is an absence the owner never notices on their own.

Items 7, 8 and 9 each need a migration and each buys one sentence or one queue item. Item 8 gates a
control rather than decorating one: until it lands, a both-true verdict is a promise the queue
breaks overnight, so the verdict ships with the migration or waits.

Item 15 has no migration and it is the only recovery in the triage flow. Rank it above every
migration item.

Item 11 stays the highest ratio in the table. One route and a CLI rewrite deliver the mirror PRD
§4.8 already promises, two integration assertions already exercise the renderer, and it was the
surface the rejected design leaned on while nobody could reach it. Keeping both a console and a
mirror alive costs upkeep on two reading surfaces, which §9 Q1 puts to the owner.

Item 16 is priced to be declined. Buying a ten-second window with a `deleted_at` predicate in every
read path is the wrong trade, and §7 rules the interface accordingly.

---

## 7. What every visual proposal must satisfy

No style is prescribed. Each constraint below has a test, and each one comes from a defect in §1.2
or from a job in §2.

Every constraint here is a floor. Clearing all seventeen earns a proposal the right to be considered
and nothing else, and five proposals that clear them and look alike have wasted the exercise. The
owner rejected the last design on taste as much as on function, so each proposal owes one decision
it would defend against the other four. §10.8 asks for it by name.

**Reading density.** The primary read surface shows at least 20 facts on a 1440×900 viewport and at
least 6 on a 390×844 phone, with fact text at 15px or larger on both. Fact prose caps at 75
characters per line. A design that needs a comfortable mode to be readable has the wrong resting
state.

**Hierarchy on more than lightness.** At least two of size, weight and family separate each level:
fact text, metadata, chrome, section label. Test: convert a screenshot to greyscale and flatten it
to three tones. The levels stay distinguishable.

**Provenance adjacent to its fact.** The client, date and confirmation for a fact sit within 20
characters of the fact's own text block and never wrap into a ragged column. Test: cover every row
but one, and that row still reads as a unit.

**Private and sealed without leaking.** A private row shows its content. Private means encrypted at
rest and limited by grant, and the owner is the one party it was never hidden from. §9 Q2 records
this as a default the owner vetoes in one line, so no design hedges it and all five draw the same
product. A sealed row shows the namespace, the byte length and the command that reads it where the
key is, and never a stand-in for content. Sensitivity travels on at least three channels beyond
hue, so a greyscale screenshot still carries it. Test: print the screen in black and white and sort the rows by sensitivity.

**Granted and not granted, both legible.** An ungranted cell reads at 4.5:1 or better. The widest
grant is not the hardest text on the screen to read. Test: measure the ceiling word on the widest
row and the empty marker on the narrowest.

**Destructive actions carry no undo, and the interface says so.** `DELETE /admin/memory/{id}` is a
hard delete and the tree holds no tombstone (§6.2 item 16), so an undo window can only defer the
request in the browser or delete and rewrite. Deferring loses the row when a phone locks inside the
window, in the one job whose done state is the row being gone. Rewriting mints a new id, a new
`created_at`, fresh classification, no access history and no supersession links, and it can trip the
credential tripwire on content the store already held. Both fail the owner, so the rules are:

- **Every destructive verb takes a second deliberate act,** on pointer and on keyboard alike. That
  act is the confirm gate §5 asks for on job 4. No held key ever reaches a destructive verb.
- **Delete fires on that act and the after-state comes from the server's answer.** It carries the
  read count and the last read time, because the anxiety is that the fact is still out there and a
  confirmed absence with its read history attached is what answers it.
- **Delete of a sealed row names the key in the confirm** and says it destroys the only copy.
- **Supersede is a destructive verb.** Phase 4 never re-raises a resolved pair, `review::supersede`
  refuses the direct reversal, and the console has no compose box to retype the losing text into, so
  a verdict on the wrong pair stands until §6.2 item 15 lands. Triage keeps held-key speed for
  navigation and loses it for verdicts.
- **A both-true verdict ships with §6.2 item 8 or waits for it.** Without the not-a-conflict record
  the pair returns tomorrow, and a queue that cries wolf stops being opened.

**Keyboard and pointer at parity.** Nothing is reachable by hover alone. Every keyboard verb has a
visible control, because job 4 happens on a phone and job 1 happens one-handed. Focus never lands on
`<body>` after an action. `Escape` steps out one level and destroys nothing.

**Above the fold, per job.** Job 1: the arrivals list and the per-namespace inventory. Job 2: the
client's read clauses, its write clauses and its 14-day read and write counts, together, without
scrolling. Job 4: the fact, its read count and the delete control. Job 7: one line, on whatever
screen the owner opened.

**Phone.** Jobs 1, 4, 5 and 7 work end to end. Job 6 is readable and not actionable. Grant editing
is absent, because a fat-fingered ceiling change is the mistake this system exists to prevent and
it is never urgent.

**Error states, drawn rather than improvised.** Three of them, each with copy: the console cannot
reach the server, a route answered 403 because the console's grant is narrower than §4.1 assumes,
and a destructive act that failed after the owner committed to it. Job 7 exists to notice the store
has stopped working, and a phone mid-panic is the worst place to invent the offline screen. Test:
pull the server down and read every screen the owner could be standing on.

**Empty state on a healthy store.** Job 6's queue is empty most weeks. The empty state answers the
other question with the same pixels: live rows, retired rows, last write and its client. It never
reads as an error and never reads as an achievement. Test: a store with 1,240 clean facts and
nothing to decide still fills the screen with something true.

**Empty state on a new store.** Different copy, and it names the CLI command that writes the first
fact plus the hook that makes Claude Code write on its own.

**Signal proportional to the job.** No status band outweighs the content of the screen it sits on. A
key mismatch is one line on every screen and a full treatment on one.

**A control that claims a re-render does one.** If a lens says the view is another client's, the
rows change and the withheld ones collapse to a counted, struck line so the shape of what is hidden
stays visible. Two inputs the design answers rather than leaves undefined: an empty query, and the
lens pointed at the console's own credential, which sees everything and says so instead of reading
as a bug.

**No annotation in the product.** Implementation-status chips belong in this document, never on a
screen the owner operates.

**Never a cosine score in the resting state.** Bands with printed headers, and the third band says
`nothing matched well` in those words. Raw numbers behind a held key, and only the ones the server
returns (§6.2 item 10).

**Motion.** Nothing moves by default. Under held `j`/`k` the correct transition duration is zero.
The exceptions earn their place one at a time, and a hold-to-confirm gate is timed in JavaScript so
no CSS preference can shorten it to a tap.

---

## 8. Out of scope, and why

Each of these would otherwise arrive by reflex. The list is the old IA's, minus the entry §1
overturns.

**A browse-all table of every fact with filters and pagination.** Job 1 shows arrivals and
inventory, both scoped and both live. Unbounded browsing is the mirror's job.

**A compose box.** Every write the console performs answers a fact already on screen: confirm,
supersede, retire, delete. Break that and it becomes a second, worse note app.

**A graph view.** Supersession chains are linear by construction, since Phase 4 refuses a supersede
whose target is already superseded. Branching is defined as an error, so a graph would draw nothing.

**The grant matrix as an editing surface.** Namespaces are glob patterns that overlap, nest and are
open-ended, so the axis has no enumerable row set. Rows-as-patterns leaves the cell where
`project:*` and `project:lumberroom` disagree undefined; rows-as-namespaces grows forever and stops
showing the grant that was written. Clauses, counters and a diff instead. A read-only grid of
concrete clients against concrete namespaces is allowed, and §1.2 shows how badly it reads if
nobody works at it.

**An export or backup UI, a settings screen, a theme gallery, a tag manager, a namespace manager.**
Configuration is environment variables on a box the owner owns, and a settings screen would be a lie
about where the truth lives. Namespaces are created by writing to them.

**A conversation viewer.** It would mean storing transcripts.

**Editing the classification table.** Read-only until the rules stop resolving at boot (§6.2 item
13).

**Notifications, digests, onboarding tours, tooltips explaining what a namespace is.** The queue
count is the notification.

**Anything multi-user.** One owner, one tenant.

---

## 9. Open questions only the owner can answer

Questions 1 and 2 forked the product while five designers were drawing it, so each now carries a
default this specification rules and the owner overturns in one line. Nothing waits on either.

1. **Does the console read?** **Ruled yes,** on the three properties in §1.4, which survive item 11
   landing. Wiring the mirror stays recommended alongside, and the two surfaces cost upkeep: a route
   plus a CLI writer for the vault, and a console that keeps telling the truth about a live store.
   To overturn, the owner says "mirror only", item 11 moves to the top of §6.2, and job 1 shrinks to
   arrivals and inventory.
2. **May private content render in a browser?** **Ruled yes.** Private means encrypted at rest and
   limited by grant. The console's threat model is a session on a box the owner owns, where the
   vault's is a folder syncing to a third party, which is why `EXPORT_MAX_SENSITIVITY` stays at
   `open` there. Most of what the owner opens the console to read is private, and redacting it
   leaves a screen of redaction with job 1 dead inside it. To overturn, the owner says so and
   redaction becomes a session toggle defaulting to shown, never a second design.
3. **May a console session grant reach, and for how long?** Folding consent into the console means
   the session that reads the record can also widen a grant. §4.4 adds the second half: a session
   long enough to survive job 4's clock on a phone is a session whose theft lasts as long. The owner
   sets the TTL and decides whether a phone gets a longer one. Neither is a layout question.
4. **Closed.** §4.2 rules the clause route with the three profiles as presets over it. The console
   keeps a bearer token that changes on restart.
5. **Keyboard first, or pointer first?** The rejected design assumed a typist and spent its budget
   on a palette and a keymap. Job 4 is a phone job.
6. **How many items land in the triage queue per week?** Nobody has measured it. Under one a week
   and job 6 gets a widget rather than a screen. Settled by dumping live pairs above 0.85 similarity
   and counting how many sit in the 0.90 to 0.97 band, divided by the weeks the store has existed.
7. **Is KEK escrow decided?** Decision 0004 leaves it open. A console that invites a private write
   while escrow is undecided helps the owner strand data.
8. **Which surfaces exist by the time this ships?** Every ranking above assumes the owner's seven or
   eight surfaces. Today one is wired, and jobs 2, 3 and 8 have one client to reason about.

---

## 10. The design brief

Five styles, one product. This section is what each proposal is judged against, so the owner can
reject a style for a reason. It repeats §5 and §7 on purpose: a designer who reads only §10 draws
the same product as one who reads the whole document.

### 10.1 What you are designing

A console served by the backend in §4.4, for one owner, one store, and one wired client as of today.
It reads the live store, it shows what a named client would get, and every row it shows carries the
verdict that disposes of it. Jobs 1 through 4 in §2 have to work. Job 4 has to work on a phone.

Two rulings are already fixed, so all five styles draw the same product:

- **A private row shows its content.** A sealed row shows the namespace, the byte length and the
  command that reads it where the key is.
- **The console reads.** A read screen that is not live, not policy-aware and not actionable belongs
  in the Obsidian mirror, and §8 keeps it out.

**Where the styles may differ:** keyboard-first or pointer-first (§9 Q5). Floor 6 forces parity and
§10.3 forces the phone coverage, so the failure that sank the last design is fenced off and letting
the proposals split on this axis is the cheapest way to answer the question.

### 10.2 Readability floors

"Hard to read and operate" killed the last design. Each floor has a test that settles it.

| # | Floor | Test |
|---|---|---|
| 1 | 20 facts on 1440×900 and 6 on 390×844, fact text at 15px or larger, prose capped at 75 characters per line | Count them at the resting state. A design that needs a comfortable mode to be readable has the wrong resting state |
| 2 | Two of size, weight and family separate each level: fact text, metadata, chrome, section label | Greyscale the screenshot and flatten to three tones. The levels stay apart |
| 3 | Client, date and confirmation sit within 20 characters of the fact's own text block and never wrap into a ragged column | Cover every row but one. That row still reads as a unit |
| 4 | Sensitivity travels on three channels beyond hue | Print in black and white, then sort the rows by sensitivity |
| 5 | An ungranted cell reads at 4.5:1 or better, and the widest grant is not the hardest text on the screen | Measure the ceiling word on the widest row against the empty marker on the narrowest |
| 6 | Nothing reachable by hover alone. Every keyboard verb has a visible control, focus never lands on `<body>` after an action, `Escape` steps out one level and destroys nothing | Unplug the mouse, then unplug the keyboard |
| 7 | No status band outweighs the content of the screen it sits on. A key mismatch is one line everywhere and a full treatment on one screen | Compare the band's area and contrast against the content below it |
| 8 | Nothing moves by default. Under held `j`/`k` the transition duration is zero, and a hold-to-confirm gate is timed in JavaScript so no CSS preference shortens it to a tap | Hold the key and watch. Then set `prefers-reduced-motion` and try to tap through the gate |
| 9 | No cosine score in the resting state. Bands with printed headers, the third reading `nothing matched well` in those words. Raw numbers behind a held key, and only the ones the server returns | Look for a number no route returns |
| 10 | No implementation-status chips and no annotation about what is built | Search the mockup for the word `wired` |

### 10.3 Above the fold, per job

| Job | Together, without scrolling |
|---|---|
| 1 | The arrivals list and the per-namespace inventory |
| 2 | The client's read clauses, its write clauses, and its 14-day read and write counts |
| 4 | The fact, its read count and the delete control |
| 7 | One line, on whatever screen the owner opened |

**On a phone,** jobs 1, 4, 5 and 7 work end to end. Job 6 is readable and not actionable. Grant
editing is absent, because a fat-fingered ceiling change is the mistake this system exists to
prevent and it is never urgent.

### 10.4 States every style has to draw

Five states that go wrong when a designer improvises them at the end:

1. **A healthy empty queue.** Job 6's queue is empty most weeks. Answer the other question with the
   same pixels: live rows, retired rows, last write and its client. Never an error, never an
   achievement. Test: 1,240 clean facts with nothing to decide still fills the screen with something
   true.
2. **A new store.** Different copy, naming the CLI command that writes the first fact and the hook
   that makes Claude Code write on its own.
3. **The console cannot reach the server.**
4. **A route answered 403,** because the console's grant is narrower than §4.1 assumes.
5. **A destructive act failed** after the owner committed to it.

### 10.5 Destructive acts

No undo anywhere, for the reasons in §7. Every destructive verb takes a second deliberate act on
pointer and keyboard alike, no held key reaches one, and the after-state comes from the server's
answer. A sealed delete names the key and says it destroys the only copy. Supersede counts as
destructive, so triage keeps its held-key speed for moving through the queue and loses it for
verdicts.

### 10.6 Demo data

Draw the store the owner has. One wired client, the namespaces that exist, the counts that exist. A
mockup showing seven clients and 14,382 facts asks the owner to judge a world they do not live in,
which is part of how the last artifact died. Where floor 1 needs more rows than the store holds,
label the filler rows in the submission notes and never on the screen. Client counts are never
padded.

### 10.7 Capability ledger, condensed

No style proposes something the server cannot do without saying so. §6 carries the detail and the
sizes.

**Answers today, with no server work.** Per-client grant and consent state for OAuth clients,
per-client call and write rates, store staleness, per-tool latency, the both-axes namespace
inventory, search as the console's own principal, one fact with full provenance and access count,
conflict pairs with similarity, stale rows, registry rows due for review, supersede, delete, sealed
delete, key and embedder state, and the effective grant for the presented credential.

**Priced in §6.2, so a design may draw it and names the item it rides on.** The console backend (0),
newest-first filtered listing (1), blast radius (2), the client lens (3), out-of-band approval and
revocation (4), per-clause grant editing (5), the last reader by name (7, migration), a durable
both-true verdict (8, migration), the credential tripwire as a queue item (9, migration), the
lexical contribution to a score (10), a reachable Obsidian mirror (11), the classification rules
read-only (13), bearer clients as data with last write per client (14), undoing a supersede (15).

**Never drawn as present.** A named last reader, a both-true verdict that survives the night, a
tripwire refusal in the queue, a wording-versus-meaning explanation of a hit, and any client list
that includes a bearer client. Each waits on the item beside it above.

**Out of scope, and not a gap to fill.** A browse-all table with filters and pagination, a compose
box, a graph view, the grant matrix as an editing surface, an export or backup UI, a settings
screen, a theme gallery, a tag manager, a namespace manager, a conversation viewer, an editor for
the classification table, notifications, digests, onboarding tours, and anything multi-user. §8
carries the reasoning for each one.

### 10.8 What each proposal submits

1. The job 1 arrivals and inventory screen, at 1440×900 and at 390×844.
2. The job 2 client screen with a grant change previewed, including the bearer case: the grant
   read-only, the clause block to paste, and the drift flag.
3. The job 4 delete path on the phone, end to end, including the after-state.
4. The job 6 queue with items, and the same screen with the queue empty.
5. One line per floor in §10.2 saying how the design meets it, with the test result.
6. **One decision the proposal would defend against the other four,** in two sentences. Clearing the
   floors is the entry fee. The owner has already rejected one compliant design.
