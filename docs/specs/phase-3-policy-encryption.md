# Phase 3 — Permissions and encryption

> **Ends when:** ChatGPT provably cannot see a fact that Claude Code can, and you have checked.
> — system PRD §7

Status: implemented 19 August 2026. Design was settled before the build; the research it depended
on is in [`docs/research/encryption-and-sensitivity.md`](../research/encryption-and-sensitivity.md).
Verification is pending. `scripts/policy-test.sh` is the gate for §6 and has not yet run against a
live server, and no private row has been encrypted and read back outside a unit test. Decisions
[0004](../decisions/0004-kek-provider.md) and
[0005](../decisions/0005-private-drops-lexical-search.md) supersede parts of §3 and answer part of
§7; see the notes there.

This is the phase that decides whether you are willing to put real facts in the system. The exit
criterion is a test you run, not a property you argue for.

**One correction to the PRD before anything else.** §4.6 describes the searchable index as leaking
"a good deal of the meaning" of private content. That is an understatement, and it changes the
design. A Postgres `tsvector` is not an index that leaks the document; it is the document, stemmed.
Recovering private content from it needs no attack and no model. **Private content therefore drops
lexical search and becomes semantic-only.** The embedding stays plaintext because search has to
work, and that leaks the gist, which is a claim we can defend. The evidence, including the
embedding-inversion literature, is in the research doc.

---

## 1. The two-axis grant model

Namespace answers *whose facts*. Sensitivity answers *how exposed*. The PRD is explicit that one
ceiling is not enough: work notes and personal finance can both be `private` while a work agent must
see one and never the other.

**Today.** A grant is two glob lists:

```json
{ "client": "chatgpt", "token": "...", "read": ["user:me", "global"], "write": ["user:me"] }
```

**Phase 3.** Each entry gains a ceiling. A bare string keeps meaning what it means today, so every
Phase 1 grant stays valid and the migration is additive:

```json
{
  "client": "chatgpt",
  "read":  [{ "namespace": "user:me", "max": "open" },
            { "namespace": "global",  "max": "open" }],
  "write": [{ "namespace": "user:me", "max": "open" }],
  "sealed_capable": false
}
```

```json
{
  "client": "claude-code-mac",
  "read":  [{ "namespace": "*", "max": "sealed" }],
  "write": [{ "namespace": "*", "max": "sealed" }],
  "sealed_capable": true
}
```

Rules:

- A bare string `"user:me"` expands to `{ "namespace": "user:me", "max": "open" }`. Defaulting to the
  lowest ceiling means a grant written before this phase never silently gains access.
- Ceilings are ordered `open < private < sealed`. A grant admits everything at or below its ceiling
  for that namespace.
- `sealed_capable` is a property of the *client*, not of the grant: it asserts the client can decrypt
  locally. A client without it can hold a `sealed` ceiling and still only ever receive ciphertext.

**Enforcement points.** Every one of these already exists for the namespace axis and gains the second
axis. The list is exhaustive on purpose, because a policy that holds in three places out of four is
not a policy:

| Path | Check |
|---|---|
| `memory_search` | Namespace intersection, then sensitivity ceiling, applied in SQL rather than after |
| `context_bootstrap` | The same, in **every** subquery. Phase 1 shipped a bug where the profile and project subqueries skipped the namespace filter; the test that caught it is the template |
| `registry_get` | The same, per namespace in the precedence walk |
| `memory_write` | Namespace and sensitivity both asserted before the insert. Denial is loud, 403, never a silent drop |
| `sealed` reads | Served as ciphertext unless the client is `sealed_capable` |
| `/statsz`, `/admin/*` | Unchanged; already authenticated |

Filtering must happen in the query, not in the application after fetching. A row that a client may
not see should never enter that client's process memory.

---

## 2. Classification, which is the part that can fail at the product level

The PRD is blunt about this: "The design target is that you configure this two or three times and
then forget it. If using the system means classifying every sentence, the system has failed at the
product level regardless of how well it works technically." §9 repeats it as a risk.

So classification is inferred by default and manual only by exception.

**Default by namespace.** One table, edited about twice a year:

```
namespace            default sensitivity
global               open
project:*            open
user:me              open
personal:finance     private
personal:health      private
credentials:*        sealed
```

A write with no explicit sensitivity takes its namespace default. Nobody classifies anything in the
normal case, which is the entire point.

**Explicit override.** `memory_write` gains an optional `sensitivity` parameter. A model may raise
the level and may never lower it below the namespace default, because a tool that can downgrade
classification is a tool that can leak by mistake.

**A tripwire, because inference will occasionally be wrong in the expensive direction.** Content
matching credential-shaped patterns (private key headers, long high-entropy strings, known token
prefixes) is refused at `open`. The write fails with a message naming the pattern and suggesting
`sealed`. This is a cheap regex pass, not extraction, and it stays inside the "no LLM in the write
path" constraint.

The tripwire is a backstop, not a guarantee. It catches the obvious shapes and will miss prose that
happens to be sensitive, which is what the namespace defaults are for.

---

## 3. Encryption

Full design, evidence and threat model in the research doc. The specification, in short:

**open.** Unchanged. Plaintext content, plaintext embedding, lexical index. Defends nothing beyond
the grant, which is the point of the level existing.

**private.** Per-row envelope encryption. A fresh 256-bit DEK per row, content encrypted with
AES-256-GCM, the DEK wrapped by a KEK that never touches the disk. Per-row grain makes a delete a
crypto-shred and makes KEK rotation cheap. The lexical index is generated for open rows only. The
embedding stays plaintext.

**sealed.** A separate table holding client-encrypted blobs, keyed by an HMAC of the canonical name
computed client-side. The server holds no key and can never read the content, including under full
compromise. Not searchable, retrievable only by exact key, and returned to browser clients as
ciphertext permanently rather than pending a future fix.

> **Superseded.** Decision [0004](../decisions/0004-kek-provider.md) replaced the single Vault
> design below with a provider abstraction (`KeyProvider`, `KEK_PROVIDER=none|file|env`) so the
> product can boot wherever compose runs, not only on the one target this section assumed. Nobody
> verified the pricing claim this section flags. The local providers it shipped instead defend a
> stolen dump or backup, not a stolen disk or a live compromise. That is a weaker threat model than
> the Vault design below, which stays the recommended option for a deployment that needs it, and is
> now the first KMS implementation to write behind the trait rather than the default.

**Where the KEK lives.** The deployment target's Always Free ARM shape has no vTPM, so every
TPM-sealed design is out. OCI Vault software-protected keys are free on that platform and are the
recommendation: the app authenticates with an instance principal scoped to this one instance and this
one key, unwraps the KEK into memory at start, and never writes it to disk. A stolen disk image then
contains no key material at all. *Verify the pricing claim before building on it; it carries the
whole design.*

This does not defend against a live compromise of the box, which can call the same API with the same
identity. Nothing software-only can. Say so in the docs rather than implying otherwise.

**Backups change too.** Phase 1's plaintext dumps expose private content exactly as a stolen disk
would. Dumps get `age`-encrypted to a recipient whose private key lives on the Mac and one offline
copy, and that key is deliberately *not* the KEK.

---

## 4. Deleting

The PRD asks for "delete anything" in §4.8, Phase 1 has no delete path at all, and this is the phase
where sensitive content becomes possible. The two facts belong together.

**CLI, unrestricted.** `lumberroom forget <id>` and `lumberroom forget --query "..."` with a confirmation prompt
and a dry run that prints exactly what would go. This is the operator path and it hard-deletes.

**MCP, off by default.** A fifth tool, `memory_forget(id, reason)`, available only to clients whose
grant carries `"delete": true`. No client has it initially. A model that can silently delete memories
is a worse failure than a model that hoards them, and the asymmetry justifies making this opt-in per
client rather than available by default.

**What deletion means per level.** For `open` and `private`, the row goes and, for private, the DEK
goes with it, so the ciphertext in any older backup is already unreadable. For `sealed`, deleting the
row removes the only copy: the server cannot help recover it, by construction.

Deletions are recorded in `tool_calls` like any other operation, so "what happened to that fact" has
an answer.

---

## 5. Migration

1. Add the sensitivity column with `DEFAULT 'open'` and a CHECK constraint. Every existing row is
   `open`, which is what it already effectively was.
2. Make the lexical index conditional on `sensitivity = 'open'`. On an all-open store this is a
   no-op, which is the cheapest moment to make the change.
3. Add the encryption columns, nullable. Nothing populates them yet.
4. Stand up the Vault key and the instance principal. Verify unwrap-at-boot works, including across a
   reboot, before any row depends on it.
5. Turn on encryption for new private writes. Existing rows stay open until reclassified.
6. Reclassify by hand, which is a small job on a store this size and the last moment it will be small.

Steps 1 to 3 are safe to ship ahead of the rest. Step 4 is the one that can strand data: **do not
write an encrypted row until a restart has proved the key can be recovered.**

---

## 6. Exit test

The criterion says "and you have checked", so the deliverable is a script, run against the live
deployment with the real credentials, not a unit test with mocks.

`scripts/policy-test.sh`:

1. Write a nonce-bearing fact at `private` in a namespace ChatGPT's grant excludes, using the Claude
   Code credential.
2. Using ChatGPT's actual credential:
   - `memory_search` for it. Assert absent.
   - `context_bootstrap`. Assert the nonce appears nowhere in the digest, including the namespace
     inventory line.
   - `registry_get` for the equivalent key. Assert not found.
   - Attempt a write into that namespace. Assert 403.
3. Using the Claude Code credential, assert the fact is visible.
4. Write a `sealed` item. Assert a non-`sealed_capable` client receives ciphertext and cannot
   decrypt it, and that a capable client can.
5. Assert the denied attempts appear in `tool_calls`, so refusals are observable rather than silent.

Step 2's bootstrap check matters more than it looks. The Phase 1 bug that tests caught was exactly
this: a digest subquery that skipped the grant filter. The leak path in a memory system is the
convenience surface, not the obvious one.

Run this after every grant change. It is cheap, and grants are edited by hand.

---

## 7. Open questions carried from the research

These need the owner's answer before implementation, not during it.

1. **Answered.** ~~Confirm private drops lexical search.~~ If exact-phrase search over private
   notes is genuinely needed, the honest position is that private gains little over open against a
   stolen database, and the docs must say that instead. Decision
   [0005](../decisions/0005-private-drops-lexical-search.md) answers this: private drops lexical
   search, and the honest-limit language in §4.6 of the system PRD is corrected to match.
2. **KEK escrow.** Losing the Vault key makes every private row permanently unreadable, backups
   included. Decide whether a wrapped offline copy exists.
3. **Disk encryption on root.** Recommendation is to skip it; the alternatives cost either an SSH
   unlock at every reboot or a second always-on host.
4. **Sealed recipients.** Which local clients hold keys, and whether an offline-only fourth recipient
   exists as insurance against losing all of them.
5. **Vault IAM.** Who may `manage` the key rather than only `use` it. This decides whether an OCI
   account compromise alone is enough to read private content.

---

## Order of work

1. Sensitivity column, CHECK constraint, conditional lexical index. Safe, additive, no behaviour
   change.
2. Two-axis grants, parser and enforcement, with the Phase 1 grant format still valid.
3. `policy-test.sh` against the live box, before any encryption exists. The permission half is the
   exit criterion; encryption is what makes it worth having.
4. Namespace defaults and the credential tripwire.
5. Vault, instance principal, KEK unwrap at boot, proven across a restart.
6. Private encryption on the write and read paths.
7. Encrypted backups, and a restore drill that actually decrypts a private row.
8. Sealed: table, client tooling, key distribution.
9. The delete path.
