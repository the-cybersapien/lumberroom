# Memory bank console — design

Two passes, run separately so the structure and the surface were argued independently rather than
by one opinion.

- [`console-ia.md`](console-ia.md) — jobs, information architecture, flows, and what not to build.
- [`console-visual.md`](console-visual.md) — aesthetic position, type, colour tokens with computed
  contrast ratios, the entry component in every state, motion, keyboard model, screens.

**Status: designed, not scheduled.** Both passes independently concluded the console should not be
built before Phase 3, and the reasoning is the same from either direction: four of its five ranked
jobs depend on the sensitivity axis and per-client grants, which do not exist yet. An admin UI built
early against a store of two hundred facts and one client is a screen with nothing to do, and unused
tools do not survive.

---

## What the two passes agreed on without coordinating

**The console is a disposition surface.** A place to decide and to act, never a place to read.
Reading belongs to the Obsidian mirror, which is better at it and works offline. Authoring and bulk
work belong to the CLI. That single rule kills the file browser, the tag manager, the timeline and
the dashboard, all of which would otherwise arrive by reflex.

**Policy is the reason it exists.** Not the grant editor, which is easy and which `AUTH_TOKENS`
already is, but the **blast radius**: today "add `personal:*` to chatgpt's read list" is a change
whose effect you cannot see, and afterwards it reads *"+412 facts become visible to chatgpt, 3 of
them in personal:finance."* The visual pass reached the same place from the other side and made the
**client lens** the signature element: a control that re-renders the whole interface as a named
client sees it, with denied rows collapsing to a struck count rather than vanishing, because the
owner needs to see the shape of what is hidden. Both are Phase 3's exit criterion turned into a
screen instead of an argument.

**Never show a cosine score.** It is precise, meaningless to a human, and shifts when the embedding
model changes. Instead: honest bands, with the third one saying `weak — matched only faintly` and
collapsed by default, and a relevance cliff drawn at the largest gap in *this* result set rather
than at a global threshold. Raw numbers exist behind a held key, for the one person who might want
them.

**Sensitivity is safety-critical, not decorative.** The visual pass encodes it on five independent
channels — glyph, printed word, luminance step, ground tint, and structural absence for sealed — so
that removing hue entirely leaves four working signals. The three sensitivity colours are reserved
and appear nowhere else in the interface, because reuse degrades the signal.

**Do not build the grant matrix as a matrix**, per the IA pass, and the reason is structural rather
than aesthetic: namespaces are glob patterns that overlap, nest and are open-ended, so the axis has
no enumerable row set. The visual pass does render a grid on the policy screen, which is a genuine
tension between the two documents. Resolve it in favour of the IA argument for the *editor*, where
patterns are authored, and allow the grid only as a read-only overview of concrete clients against
concrete namespaces.

---

## What this asks of the rest of the system

- **The console is a client of itself.** Its own credential, its own grant, its own row in the
  clients list, `sealed_capable: false`. It dogfoods the policy layer: the console cannot see what
  its own grant forbids, and the server path that returns ciphertext to a non-capable client is
  exactly the code the sealed state depends on.
- **Grants should move from `AUTH_TOKENS` to a versioned Postgres table in Phase 3.** Two-axis
  grants with per-clause ceilings have outgrown an environment variable, the blast-radius preview
  wants to be transactional with the change it previews, and a grant history is worth having for the
  same reason a memory history is. This also closes the grant-reload gap that fell between the
  Phase 2 and Phase 3 specs.
- **Two schema additions beyond current plans.** The *reading client* alongside `last_accessed_at`,
  which turns "read 14 times" into "read 14 times, last by chatgpt" and is what lets the delete flow
  tell you whether you were too late. And a persistent **not-a-conflict** record, without which the
  review queue cries wolf and stops being opened.
- **No framework.** Server-rendered HTML from a tagged template, one hand-written stylesheet, and
  roughly 300 lines of vanilla TypeScript for the keymap, a fragment swapper and the palette. Two
  self-hosted font files. Under 90KB to the client, most of it font. React and Tailwind are both
  explicitly rejected, the latter because scattering a safety-critical colour encoding across markup
  makes a wrong class invisible.
