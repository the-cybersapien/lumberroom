# lumberroom memory bank — visual and interaction direction

Register: **product**. Design serves the task. No marketing surface exists or should.
Grounded in the repo: README, ROADMAP.md, DECISIONS.md, docs/specs/phase-2-surfaces.md,
docs/specs/phase-3-policy-encryption.md, db/migrations/001_init.sql.

## 1. Aesthetic position: The Apparatus

The interface is a **critical edition of the owner's own record**: a dense index of entries on the
left, a marginal apparatus on the right that carries provenance, variants and actions for whatever
the cursor is on.

This is not a metaphor for decoration. It is a structural match:
- supersession chains ARE an apparatus criticus — variant readings with dates and witnesses
- provenance (tool, conversation, date, confirmed, superseded) IS a siglum line
- namespace + tags ARE running heads
- the owner reads at speed and drills selectively — text block plus margin, not cards in a feed

### Verdicts on the five reference points
- **Terminal — rejected as a look, adopted as a contract.** Green-on-black, ASCII chrome and a
  fixed cell grid are costume; a third of this content is natural-language prose that a fixed grid
  sets badly and wastes horizontal room on. What is adopted: everything is reachable without a
  mouse, and every screen names the CLI command that does the same thing.
- **Professional audio tool — rejected.** Its vocabulary is continuous control under time pressure:
  meters, faders, gain staging. Nothing here is continuous. Every value is discrete, textual and
  edited rarely. Skeuomorphic meters would be decoration.
- **Scientific instrument — partially adopted.** Two properties: reserved signal colours that mean
  one thing and appear nowhere else, and a diagnostic overlay that exposes raw numbers on demand
  (`?`) without putting them in the resting state. Rejected: bezels, readout chrome, the
  instrument-panel look.
- **Well-set reference book — adopted, and it is the spine.** Measure, the type/provenance split,
  the apparatus, the variant stack.
- **Code editor — adopted for interaction, rejected for chrome.** Adopted: selection as a filled
  field rather than a glow; modal-ish keyboard verbs; a cursor that is always somewhere. Rejected:
  tabs, minimap, activity bar, a file tree for content that is not files.

### Why this is not the broadsheet default
The saturated AI look #3 is hairline rules, zero radius, dense serif columns — applied as style. The
difference here is that every device is load-bearing and would break the product if removed:
- the margin exists because the compact index truncates prose to one line and the full text has to
  live somewhere that never causes layout shift under `j`/`k`
- the serif/mono split is a provenance signal (see §2), not texture
- the variant stack is indented and collapsed because superseded facts must never be siblings of
  live ones — that is the four-nicknames bug in ROADMAP.md rendered as layout
- rules are hairlines because at 24px rows anything heavier out-weighs the type

### Signature element: the client lens
A persistent control that re-renders the entire index **as a named client sees it**. Denied rows do
not vanish; they collapse to a struck count line, because the owner needs to see the shape of what
is hidden. This is Phase 3's exit criterion ("ChatGPT provably cannot see a fact that Claude Code
can, and you have checked") turned into a screen. It is the one place the UI is allowed to be bold:
lens-on puts a full-width band across the top in the negative colour and desaturates nothing else.

## 2. Typography

**Rule: the family encodes who authored the string.**
Proportional serif = natural language a human or a model wrote. Monospace = identifiers the system
owns. You never have to wonder whether a string is a value or a description.

- **Source Serif 4** (variable, SIL OFL). Fact prose only. Real `opsz` axis — set it to the pixel
  size so 13px stays sturdy. In dark theme raise `wght` to 440 to counter halation; 400 in light.
- **Iosevka Fixed** (SIL OFL). Everything else: dotted keys, JSON, timestamps, client names,
  namespaces, tags, counts, nav, buttons, labels, table headers. Chosen for advance width — 0.5em
  against IBM Plex Mono's 0.6em. `credentials.postgres.location` is 29 characters; at 13px that is
  188px instead of 226px. Across a 5-column index that difference is a whole column.

Two families total, two woff2 files, no third face.

Scale (fixed rem, 16px root, ratio ≈1.15). No clamp() anywhere — this UI is viewed at one DPI.

| token | px | use |
|---|---|---|
| `--t-micro` | 11 | counts, band headers, axis labels |
| `--t-meta`  | 12 | namespace, client, date, tags |
| `--t-body`  | 13 | index prose (compact), JSON, keys |
| `--t-read`  | 15 | apparatus prose, comfortable-density prose |
| `--t-head`  | 17 | apparatus entry title, screen title |
| `--t-lead`  | 20 | the one number on the signal screen |

Line heights: mono 1.3 (1.25 compact), serif prose 1.5.
Measure: apparatus prose capped at **58ch** (~430px at 15px Source Serif). Facts are one to three
lines; a 65–75ch measure would let a three-line fact become two, which loses the shape of the
record. 58ch keeps facts looking like facts.
Numerals: `font-variant-numeric: tabular-nums` on every count, latency and date. Iosevka is already
tabular; the declaration guards the fallback.
Font loading: self-hosted, latin subset, `preload`, and `@font-face` metric overrides
(`size-adjust`, `ascent-override`) on the fallback so `swap` produces zero reflow in a 30-row grid.

## 3. Colour

Strategy: **Restrained**. Neutrals tinted 0.010 chroma toward hue 220/230 (cold slate, not warm
paper, not blue-black). Default dark, because the scene is concrete: this runs on the right half of
a 27-inch display at 23:40, beside a terminal and an editor that are both dark, when the owner wants
to know why a work agent read a personal fact. A white slab there is a flashbang. The light theme
exists and is fully specified because sensitivity review is also a thing you do deliberately in
daylight, and because screenshots of a policy screen end up in issues.

### The safety-critical rule
**The three sensitivity colours are reserved. They appear nowhere else in the interface.** Not as an
accent, not in a chart, not on a button. Reuse would degrade the signal, and this is the one axis
where getting it wrong has a privacy consequence.

Sensitivity is encoded on **five independent channels**, of which hue is one:
1. **Glyph** in a fixed 2ch gutter: open = nothing, private = `◆`, sealed = `▩`
2. **Word** in the meta line, always printed: `open` / `private` / `sealed`
3. **Luminance** — private and sealed are separated 1.79:1 (dark) / 1.62:1 (light) from each other,
   so they differ in a greyscale screenshot
4. **Ground** — private and sealed content sits on a tinted panel; open sits on the canvas
5. **Structure** — sealed has no plaintext at all; the content slot is a redaction block. The
   strongest signal is that the thing simply is not there.

Removing hue entirely leaves four working channels. That is the test.

### Tokens

```css
:root {
  color-scheme: dark;

  /* ---- surfaces ------------------------------------------------------- */
  --canvas:        oklch(0.205 0.010 220);  /* #12181a */
  --panel:         oklch(0.245 0.010 220);  /* #1b2224  index + apparatus */
  --panel-2:       oklch(0.285 0.010 220);  /* #252b2e  hover, header rows */
  --rule:          oklch(0.330 0.010 220);  /* #303739  1.47:1 on canvas — hairlines */

  /* ---- ink ------------------------------------------------------------ */
  --ink-hi:        oklch(0.960 0.004 220);  /* #eff2f4  15.93:1 on canvas */
  --ink:           oklch(0.890 0.006 220);  /* #d7dcdd  12.88:1 canvas / 11.66:1 panel */
  --ink-mid:       oklch(0.740 0.009 220);  /* #a5acaf   7.77:1 / 7.04:1  SUPERSEDED text */
  --ink-low:       oklch(0.640 0.011 220);  /* #858e91   5.34:1 / 4.83:1  meta line */
  --ink-faint:     oklch(0.545 0.011 220);  /* #697275   3.62:1 / 3.28:1  disabled only */

  /* ---- interaction (never semantic) ----------------------------------- */
  --sel-field:     oklch(0.400 0.060 248);  /* #2c4a67  ink-hi on it = 8.17:1 */
  --focus:         oklch(0.760 0.110 242);  /* #6eb9f1   8.44:1 on canvas */

  /* ---- sensitivity — RESERVED, used nowhere else ----------------------- */
  --sens-private:  oklch(0.845 0.115 078);  /* #f5c372  11.01:1 canvas, 8.86:1 own panel */
  --sens-private-bg: oklch(0.285 0.030 078);/* #322819  ink 10.36:1, ink-mid 6.26:1 */
  --sens-sealed:   oklch(0.690 0.125 305);  /* #ad86d9   6.15:1 canvas, 5.07:1 own panel */
  --sens-sealed-bg:  oklch(0.280 0.035 305);/* #2d2437  ink 10.62:1 */
  /* open has no colour token. Absence is the encoding. */

  /* ---- outcome (instrumentation, grants) ------------------------------ */
  --ok:            oklch(0.790 0.120 152);  /* #7cd194   9.69:1 canvas, 8.77:1 panel */
  --bad:           oklch(0.740 0.140 027);  /* #f6857a   7.31:1 canvas, 6.62:1 panel */

  /* ---- space (4px base) ----------------------------------------------- */
  --s1: 2px; --s2: 4px; --s3: 6px; --s4: 8px; --s5: 12px;
  --s6: 16px; --s7: 20px; --s8: 24px; --s9: 32px; --s10: 48px;

  /* ---- motion --------------------------------------------------------- */
  --dur-quick:  120ms;
  --dur-struct: 180ms;
  --ease: cubic-bezier(0.22, 1, 0.36, 1);   /* ease-out-quint */

  /* ---- z ------------------------------------------------------------- */
  --z-sticky: 10; --z-menu: 20; --z-backdrop: 30;
  --z-dialog: 40; --z-toast: 50; --z-tip: 60;

  --lens: 0;  /* set to 1 while the client lens is on */
}

:root[data-theme="light"] {
  color-scheme: light;
  --canvas:        oklch(0.988 0.003 230);  /* #f9fbfd */
  --panel:         oklch(0.962 0.005 230);  /* #eff3f5 */
  --panel-2:       oklch(0.930 0.006 230);  /* #e4e9eb */
  --rule:          oklch(0.860 0.008 230);  /* #ccd2d5  1.48:1 */
  --ink-hi:        oklch(0.200 0.012 230);  /* #11171a  17.47:1 */
  --ink:           oklch(0.290 0.013 230);  /* #252d31  13.60:1 / 12.62:1 */
  --ink-mid:       oklch(0.450 0.014 230);  /* #4e575c   7.16:1 /  6.64:1 */
  --ink-low:       oklch(0.510 0.014 230);  /* #5e686d   5.53:1 /  5.13:1 */
  --ink-faint:     oklch(0.635 0.013 230);  /* #838c91   3.30:1 /  3.06:1 */
  --sel-field:     oklch(0.895 0.048 248);  /* #c4e0fb  ink-hi 13.26:1 */
  --focus:         oklch(0.505 0.115 245);  /* #1a6aa1   5.63:1 */
  --sens-private:  oklch(0.505 0.100 070);  /* #895916   5.80:1 canvas, 5.26:1 own panel */
  --sens-private-bg: oklch(0.955 0.030 078);/* #fceeda */
  --sens-sealed:   oklch(0.405 0.150 305);  /* #5d2b88   9.40:1 canvas, 8.34:1 own panel */
  --sens-sealed-bg:  oklch(0.950 0.028 305);/* #f3eafe */
  --ok:            oklch(0.470 0.120 152);  /* #0a6d37   6.23:1 /  5.78:1 */
  --bad:           oklch(0.490 0.170 027);  /* #ac2724   6.61:1 /  6.13:1 */
}
```

Every ratio above is computed, not estimated (OKLCH → sRGB → WCAG 2.1 relative luminance).
Hex values are the sRGB fallback for the same colour. All are in gamut.

### The other semantic pairs
- **Live vs superseded** — not a hue. Superseded is `--ink-mid` (7.77:1 / 7.16:1 — still fully
  readable, because the decision log is content), plus indentation into a collapsed stack under its
  successor, plus a printed `retired <date> by <client>` clause. A superseded fact never appears as
  a sibling of a live one.
- **Grant allowed vs denied** — a filled cell carrying the ceiling word (`open` / `private` /
  `sealed`) versus an empty cell carrying `—`. Emptiness is the whole signal, and it is enough: the
  Phase 3 grant model is allowlist-only, so there is no explicit deny to distinguish from "not
  granted" and no red belongs on this screen. If an explicit-deny rule is ever added to the grant
  model, `--bad` is the token for it, and that is a grant-model change, not a design one.
- **Success vs failure** — `--ok` / `--bad`, always with the count beside them. A zero-failure bar
  is drawn in `--ink-low`, not green: nothing happening is not success.

## 4. Density and layout

**Position:** compact is the resting state. Comfortable is a mode you enter to read, not the
default. A list that shows twelve rows is a browsing toy; year three has tens of thousands of facts
and the primary verb is *scan*.

Grid — three zones, no sidebar:
```
row 1  header            36px   product + section nav + lens + ⌘K
row 2  index / apparatus  1fr   grid-template-columns: minmax(420px,1fr) minmax(380px,520px)
row 3  status bar         24px  counts, connection, active namespace filter
```
Header is horizontal because dense text needs the width; a 200px left rail costs a whole metadata
column and holds four links.

Index row columns (compact), `subgrid` so every row aligns:
```
grid-template-columns: 2ch 7ch minmax(0,1fr) 14ch 18ch;
                       sens  date  content     client  namespace
```

Spacing scale is 4px-based (`--s1`…`--s10`) and deliberately non-linear at the top: 2/4/6/8 do all
the intra-row work, 16/24/32 separate zones. Rhythm comes from the alternation, not from a uniform
gap.

**The two densities, same content, 1440×900 (808px of list after chrome):**

Compact — 24px rows, metadata inline in aligned columns, prose 13px clamped to one line. **33 rows.**
```
   19 Aug  Prefers pnpm over npm for new repos; will not accept a …   claude-code   user:me
   19 Aug  Deploys go out Tuesday mornings, never Friday             claude-code   project:lumberroom
 ◆ 18 Aug  Retainer invoices go out on the 1st, net 15               chatgpt       personal:finance
   18 Aug  Desktop runs Ubuntu 26.04                                 lumberroom      global
       └   3 earlier values ▸
 ▩ 17 Aug  ▚▚▚▚▚▚▚▚▚▚  sealed · 2 lines                              —             credentials:aws
```

Comfortable — 40px rows, prose 15px Source Serif on its own line, metadata on a second line.
**20 rows.** Same data, same order, same gutter.
```
   Prefers pnpm over npm for new repos; will not accept a yarn.lock in review
   19 Aug · claude-code · user:me · tooling, preference

   Deploys go out Tuesday mornings, never Friday
   19 Aug · claude-code · project:lumberroom · process

 ◆ Retainer invoices go out on the 1st, net 15
   18 Aug · chatgpt · personal:finance · private · billing
```

Density is a persisted preference, toggled with `[` and `]`. There is no third mode; the apparatus
is the third mode.

Responsive is structural: <1100px the apparatus becomes a right-edge overlay opened by Enter;
<720px single column, index only, Enter pushes a full-screen apparatus view.

## 5. The entry (there is no card)

Cards are wrong here. A card implies an object you might drag, reorder or act on in isolation. A
fact is a **line in a register**: it belongs to a sequence, its meaning depends on its neighbours,
and its value comes from alignment with them. So the atom is a **row in a subgrid**, with borders
only between rows (1px `--rule`), no radius, no shadow, no nesting.

**Always visible:** sensitivity glyph, date, content (truncated to the column), source client,
namespace.
**On hover:** `--panel-2` background and nothing else. Hover does not change the apparatus — mousing
across a list while scrolling would strobe the margin. Hover is a hint, not a selection.
**On focus (`j`/`k`, or click):** `--sel-field` background, a 2px `--focus` bar in the gutter, and
the apparatus fills with the full entry. Nothing appears on focus that is not also reachable by
keyboard — there are no hover-only affordances anywhere in this UI.
**Takes an explicit keystroke:** `o` expand the variant stack · `y` copy id · `e` edit · `s`
supersede · `d d` delete · `L` change sensitivity · `?` diagnostic overlay.

Markup for the row — the gutter is a real element with a real text label, not a `::before` and not
a colour:

```html
<li class="row" data-sens="private" tabindex="-1" aria-current="false">
  <span class="gutter" aria-hidden="true">◆</span>
  <span class="sr-only">private</span>
  <time class="date" datetime="2026-08-18">18 Aug</time>
  <p class="content">Retainer invoices go out on the 1st, net 15</p>
  <span class="client">chatgpt</span>
  <span class="ns">personal:finance</span>
</li>
```

```css
.row { display: grid; grid-template-columns: subgrid; grid-column: 1 / -1;
       align-items: baseline; min-height: 24px; border-bottom: 1px solid var(--rule); }
.row .content { font: 400 13px/1.3 "Source Serif 4", Charter, Georgia, serif;
       font-variation-settings: "opsz" 13, "wght" 440;
       overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.row .date, .row .client, .row .ns {
       font: 400 12px/1.3 "Iosevka Fixed", ui-monospace, Menlo, monospace;
       color: var(--ink-low); font-variant-numeric: tabular-nums; }
/* meta on a tinted ground steps up one stop: ink-low measures 4.29:1 on the private
   ground and 4.40:1 on the sealed ground in dark. ink-mid gives 6.26:1 / 6.41:1. */
.row[data-sens="private"] :is(.date,.client,.ns),
.row[data-sens="sealed"]  :is(.date,.client,.ns) { color: var(--ink-mid); }

/* sensitivity: colour is the LAST channel added, never the only one */
.row[data-sens="open"]    .gutter { visibility: hidden; }
.row[data-sens="private"] { background: var(--sens-private-bg); }
.row[data-sens="private"] .gutter { color: var(--sens-private); }
.row[data-sens="sealed"]  { background: var(--sens-sealed-bg); }
.row[data-sens="sealed"]  .gutter { color: var(--sens-sealed); }
.row[data-sens="sealed"]  .content { font-family: "Iosevka Fixed", monospace;
       color: var(--sens-sealed); letter-spacing: 0.02em; }

.row:hover        { background-color: var(--panel-2); }
.row[aria-current="true"] { background: var(--sel-field); color: var(--ink-hi); }
.row[aria-current="true"] .gutter { box-shadow: inset 2px 0 0 var(--focus); }
```

Note the deliberate absence: no `border-left` accent stripe anywhere. Sensitivity is carried by a
glyph in a reserved column, a full-width ground tint, a word, and a luminance step.

### Every state

**live, open** — the baseline. No gutter glyph, canvas ground.
```
   19 Aug  Deploys go out Tuesday mornings, never Friday            claude-code   project:lumberroom
```

**superseded** — never a sibling. Collapsed under its successor, `--ink-mid`, with the retirement
clause. `o` expands.
```
   19 Aug  Desktop runs Ubuntu 26.04                                lumberroom      global
       ├  02 Aug  Ubuntu 25.10        retired 19 Aug by claude-code
       └  11 Jun  Ubuntu 24.04 LTS    retired 02 Aug by lumberroom
```

**private** — ochre glyph, ochre-tinted ground, word printed in the apparatus meta line. Content is
fully readable: private means *encrypted at rest and grant-limited*, not hidden from the owner.
```
 ◆ 18 Aug  Retainer invoices go out on the 1st, net 15              chatgpt       personal:finance
```

**sealed and unreadable** — a normal condition, not an error. There is no plaintext to show: the
spec says browser clients receive ciphertext permanently. The row shows the redaction texture, the
byte length, and the exact CLI command that reads it.
```
 ▩ 17 Aug  ▚▚▚▚▚▚▚▚▚▚▚▚  sealed · 412 B · key not in this browser   —             credentials:aws
```
Apparatus for a sealed entry:
```
  ▩ sealed
  credentials.aws.deploy-key            hmac 9f3a…c210
  written 17 Aug by lumberroom · confirmed by you

  This server holds no key for sealed items and neither does this
  browser. Read it where the key is:

      lumberroom open 9f3a…c210

  [c] copy command                             [d d] delete (crypto-shred)
```

**search hit** — lexical overlap emphasised in place (`--ink-hi` + weight, not a highlighter block,
which would collide with the sensitivity grounds). Purely semantic hits print `semantic only`.
```
   19 Aug  Deploys go out **Tuesday** mornings, never Friday         claude-code   project:lumberroom
   11 Jul  Release cadence is weekly           semantic only         claude-code   project:lumberroom
```

**conflict pair (or quartet)** — the four-nicknames failure documented in ROADMAP.md, made visible.
Conflict is a *group wrapper*, not a gutter state, because the gutter is reserved for sensitivity.
```
 ┌ conflict · 4 live facts make different claims about the same thing        [r] resolve
 │    19 Aug  The official nickname is "Bluefin"                    claude-code   user:me
 │    19 Aug  The official nickname is "Halyard"                    claude-code   user:me
 │    18 Aug  The official nickname is "Tidewater"                  claude-code   user:me
 │    18 Aug  The official nickname is "Kestrel"                    claude-code   user:me
 └ keep one; the other three become superseded by it
```
`r` puts the group into resolve mode: `j`/`k` picks the survivor, Enter writes the supersession
edges, the other three fold into its variant stack. One keystroke sequence turns a contradiction
into a decision log entry.

### Registry entries
Same row grammar, different columns, because the content is a key not a sentence:
```
grid-template-columns: 2ch 34ch minmax(0,1fr) 10ch 18ch;
                       sens  key   value        conf   namespace
```
```
   machines.desktop.os              "Ubuntu 26.04"                    ✓you   global
   services.postgres.endpoint       {host:"127.0.0.1", port:5432}     ✓you   global
   routes.coding.model              "claude-opus-5"                   —      user:me
 ▩ credentials.postgres.location    ▚▚▚▚▚▚▚▚                          ✓you   credentials:*
```
Dotted keys are set in Iosevka with the dots at `--ink-low` and the segments at `--ink`, so the
hierarchy reads without indentation. JSON in the index is single-line and elided; the apparatus
pretty-prints it with 2-space indent, keys `--ink-mid`, strings `--ink`, numbers/booleans `--ink-hi`
— three levels, not a rainbow. The five provenance fields are printed as a labelled block, never as
badges:
```
  written by   claude-code · session 0f21…
  on           18 Aug 2026, 23:41
  confirmed    yes, by you, 19 Aug
  supersedes   v3 · machines.desktop.os = "Ubuntu 25.10"
  version      4
```

## 6. Motion

Default: nothing moves. The list does not stagger in, rows do not fade, the apparatus does not
slide. Under `j`/`k` held down, any transition on the row or the apparatus smears — the correct
transition duration for keyboard navigation is 0ms, and that is what it is.

Four exceptions, each earning its place:

1. **Variant stack expand/collapse — 180ms `--ease` on grid-template-rows.** The user initiated a
   structural change; the motion carries the parent–child relationship that indentation alone
   states weakly. Reduced motion: instant.
2. **Hold-to-confirm delete — 600ms linear fill across the button.** Motion *is* the affordance;
   it is a progress indicator for an irreversible act on a system that has no delete path today and
   no undo. **The 600ms gate is enforced by a JS timer; the fill only displays it**, so no CSS rule
   and no user preference can shorten the gate to a tap. Reduced motion keeps the fill (progress is
   information, not decoration) and drops the easing; a numeric `3 · 2 · 1` accompanies it either
   way.
3. **Focus ring — 0ms, explicitly.** Stated as a rule so nobody adds a transition later. A focus
   ring that animates is a focus ring you can outrun.
4. **New-row tick — 500ms background wash from `--sel-field` to transparent, conditional.** Only if
   a live tail transport exists; nothing in the repo specifies SSE or polling today, so this is
   speced but not scheduled. It exists because an insertion above the fold in a silent list is
   invisible. Reduced motion: a static `new` marker in the gutter's second cell for 5s instead.

```css
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after { animation-duration: 1ms !important; transition-duration: 1ms !important; }
  .stack { transition: none; }
  .row--new { animation: none; }
  /* the delete gate is exempt: it is progress, and its duration is owned by JS anyway */
  .confirm-fill { transition-duration: 600ms !important; transition-timing-function: linear; }
}
```

Not present anywhere: page-load choreography, skeleton shimmer (skeletons are static hairlines),
hover lift, scroll reveals, toast slide-ins (toasts appear and disappear).

## 7. Search as an interaction

Semantic search has no zero-result state. Everything matches a little. The design problem is making
a weak result set *read* as weak without printing a cosine value the owner cannot calibrate.

**Input.** `/` focuses it from anywhere; it is a plain 15px field on the canvas, no icon, no border
until focus. Debounce 180ms, abort in-flight requests. The server already embeds in 11–16ms
(DECISIONS.md), so the round trip is ~60ms on the box.

**Bands, not scores.** Results are grouped under printed headers computed from the distribution
relative to the top hit plus an absolute floor — the same two numbers a person would use:
```
  strong · 4 results
   19 Aug  Deploys go out Tuesday mornings, never Friday             claude-code   project:lumberroom
   …
  related · 11 results
   …
  weak · 23 results — matched only faintly                                              ▸ show
```
The weak band is collapsed by default. That collapse is the honest presentation of "everything
matches a little": the results exist, they are one keystroke away, and they are not competing for
attention with the strong band.

**A weak set reads as weak because the band header becomes the answer.** If the top hit is below the
absolute floor, there is no strong band and the screen says so:
```
  Nothing matched well.
  The closest 5 are below. Try a different phrasing, or write this fact:

      lumberroom write "…" --namespace user:me
```

**Why it matched.** Every hit carries one of two clauses in its meta line: the lexical terms it
matched (emphasised inline in the content), or `semantic only`. The blend is 0.35-weighted ts_rank
over cosine (DECISIONS.md), and the owner should be able to see which half did the work. Facts from
outside the primary namespace set carry `other project` — the 0.85 penalty made visible, because a
silent penalty is a silent recall failure.

**Diagnostic overlay.** `?` reveals raw cosine, ts_rank, the blended score and the penalty, right-
aligned in a fifth column, for as long as it is held. This is the scientific-instrument concession:
the numbers exist and the owner is the one person on earth who might want them, but they are never
in the resting state because a number you cannot calibrate is noise that looks like signal.

## 8. Keyboard first

Two dispatchers, cleanly separated, because conflating them is the usual mistake:
- **`/` searches content.** Nouns. It queries facts.
- **`⌘K` / `Ctrl+K` runs verbs and navigates.** It never returns a fact. It returns actions
  ("supersede this", "grant chatgpt read on project:lumberroom", "export namespace", "switch lens to
  chatgpt", "copy the CLI equivalent") and destinations.

The palette earns its place precisely because it is scoped this way: the action surface here is
genuinely wide (grants × clients × namespaces × sensitivity is a combinatorial space no menu can
hold) while the fact surface is deep, and a single palette doing both would make every search a
disambiguation.

```
 g f  facts        g r  registry     g p  policy      g s  signal
 j k  move cursor      g g / G  first / last     Ctrl-d/u  half page
 Enter open in apparatus   Esc  leave pane / clear filter / close
 Tab  index ⇄ apparatus    o  expand variant stack   [ ]  density
 /    search               ?  diagnostic overlay (hold)
 y    copy id              Y  copy the CLI command for this row
 e    edit      s  supersede      r  resolve conflict     d d  delete
 L    sensitivity → 1 open · 2 private · 3 sealed  (lowering is refused, per policy)
 v    client lens          V  clear lens
 ⌘K   command palette
```

Rules: Escape always steps out one level and never destroys work. Every destructive verb needs a
second keystroke, and `d d` shows the hold-to-confirm. Focus is never lost — after a delete the
cursor lands on the next row, not on `<body>`. A visible focus ring on every interactive element,
2px `--focus`, at 0ms. The `?` sheet is generated from the same keymap table that binds the handlers,
so it cannot drift.

## 9. States

- **Loading** — no spinners. The index paints its hairline grid with the row count it already knows
  from the status bar, ink at `--ink-faint`, no shimmer. The apparatus keeps the previous entry
  until the new one arrives; blanking it would flash on every `j`.
- **Empty (no facts yet)** — teaches the CLI, which is the right first move for this owner:
  `No facts in user:me yet.` followed by the literal `lumberroom write "…" --namespace user:me` and the
  note that Claude Code writes here on its own once the SessionStart hook is installed.
- **Empty (filter matched nothing)** — different copy, different fix: names the filter and offers to
  drop it. Never reuses the onboarding empty state.
- **Error** — a full-width band at the top of the index in `--bad`, carrying the HTTP status, the
  request id, and what to do. `Search failed · 503 · req 8f21c0 · the embedder fell back to hash;
  /readyz will confirm.` No apology, no illustration. The last good result set stays on screen
  underneath, because stale data beats no data when you are diagnosing.
- **Permission denied** — this appears in two shapes and they must not look alike. (a) The owner's
  own UI is denied by the server: that is a bug or a misconfigured `lumberroom-ui` grant, and it is an
  error band. (b) The client lens is on and rows are outside that client's grant: that is the
  feature working, and it renders as an elision line inside the flow, in `--ink-mid`:
  ```
  ⋯ 14 facts here are outside chatgpt's grant (personal:finance, credentials:*)
  ```
- **Sealed** — specified in §5. Not an error, not empty, not denied. It is a fourth thing: content
  that exists, that this viewer is authorised to know about, and that no browser will ever decrypt.
  The copy must say that plainly and hand over the command.

## 10. Implementation

**Build it with:** Express serving server-rendered HTML from a tagged-template `html` helper with
escaping (~80 lines), one hand-written `lumberroom.css` (~600 lines) carrying the token block, and roughly
300 lines of vanilla TypeScript: a keymap dispatcher, a fragment swapper
(`fetch` → `DOMParser` → `replaceChildren`), and the palette. Two self-hosted subset woff2 files.
Total client payload target: **under 90KB, of which ~60KB is font.**

- **Lists:** keyset pagination on `(created_at, id)`, 200 rows per page, `IntersectionObserver`
  appends, and pages trimmed off the top past ~2000 rows. Windowing by page, not row virtualization
  — virtualization breaks find-in-page, and this owner uses find-in-page.
- **The apparatus** is an HTML fragment endpoint. One template, rendered server-side, used for both
  the initial paint and the swap. No duplicated client renderer.
- **Auth:** the UI is its own client identity (`lumberroom-ui`) with `sealed_capable: false`, so the
  server-side path that returns ciphertext to a non-capable client is the same code the design
  depends on. Session cookie in front of the existing bearer/JWT path; the browser never holds the
  MCP token.
- **CSP** `default-src 'self'; script-src 'self'; style-src 'self'` with a nonce for the one inline
  theme-bootstrap script that sets `data-theme` before first paint. `Cache-Control: no-store` on
  every fragment carrying fact content. `Referrer-Policy: no-referrer`.
- **Charts:** none. The signal screen draws bars as `<div>` widths and one sparkline as a 30-line
  inline SVG polyline. A charting library on this box would be the single largest dependency in the
  UI.

**Avoid:** React/Next (a build toolchain, a `node_modules` tree and hydration cost on a 4-core A1,
for one user and zero client state worth reconciling); Tailwind (the sensitivity encoding is
safety-critical and must be auditable in one file — utility classes scatter it into markup where a
wrong class is invisible); any icon library (every glyph here comes from Iosevka); web fonts from a
CDN (an outbound dependency on a box the deploy story is proud of not needing); service workers;
client-side routing; analytics of any kind.

## 11. Screens

### A. Facts — the index, compact, dark, client lens off
```
┌────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ lumberroom   facts  registry  policy  signal                      lens: off      ⌘K   ?              ◐ theme      │
├──────────────────────────────────────────────────────────────────┬─────────────────────────────────────────┤
│ / deploy schedule_                                    38 results │  Deploys go out Tuesday mornings,       │
├──────────────────────────────────────────────────────────────────┤  never Friday                           │
│ strong · 4                                                       │                                         │
│    19 Aug  Deploys go out Tuesday mornings, never Friday   …     │  open · project:lumberroom                    │
│    02 Aug  Release train is cut Monday 18:00 UTC           …     │  tags   process, release                │
│  ◆ 28 Jul  Prod deploys need Marisol's ack after 16:00     …     │                                         │
│    14 Jul  Never deploy the hour before standup            …     │  written by   claude-code               │
│ related · 11                                                     │  on           19 Aug 2026, 09:12        │
│    11 Jul  Release cadence is weekly     semantic only     …     │  id           7c41e8a2                  │
│    09 Jul  CI runs on the ARM builder    other project     …     │  supersedes   —                         │
│    …                                                             │  superseded by —                        │
│ weak · 23 — matched only faintly                          ▸ show │                                         │
├──────────────────────────────────────────────────────────────────┤  [e] edit  [s] supersede  [y] copy id   │
│ ⋯ 2 facts here are sealed and cannot be shown in a browser       │  [Y] copy CLI     [d d] delete          │
├──────────────────────────────────────────────────────────────────┴─────────────────────────────────────────┤
│ 14,382 facts · user:me + global + project:lumberroom · connected · p50 44ms          compact  [ ]  comfortable    │
└────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

### B. Policy — the grant matrix with the client lens armed
```
┌────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ lumberroom   facts  registry  policy  signal                      lens: chatgpt   ⌘K   ?             ◐ theme      │
├────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ ▚▚ viewing the whole interface as chatgpt sees it · 1,204 of 14,382 facts visible · [V] clear ▚▚            │
│    (band ground --bad, text --canvas: 7.31:1 dark / 6.61:1 light. ink-hi on --bad is 2.18:1 and is banned)  │
├────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│                        user:me        global        project:*      personal:finance   credentials:*        │
│                        r      w       r      w      r      w       r      w           r      w             │
│  claude-code-mac      sealed sealed  sealed sealed sealed sealed  sealed sealed      sealed sealed         │
│  chatgpt              open   open    open   —      open   —       —      —           —      —              │
│  claude-web           open   open    open   —      open   open    —      —           —      —              │
│  openwebui            open   —       open   —      —      —       —      —           —      —              │
│  lumberroom-ui              sealed —       sealed —      sealed —       sealed —           sealed —              │
│                                                                                                            │
│  chatgpt · sealed_capable false — a sealed ceiling would still deliver ciphertext only                     │
│                                                                                                            │
│  ◆ private and ▩ sealed cells are the only coloured cells in this interface.                              │
│  An empty cell is not granted. The model is allowlist-only: there is no deny rule,                        │
│  and nothing here is red.                                                                                  │
│                                                                                                            │
│  [Enter] edit cell   [v] lens to this client   [t] test: assert a fact id is invisible to this client      │
├────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ 5 clients · 2 with write outside user:me · last grant change 18 Aug                                        │
└────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```
`[t]` is Phase 3's exit criterion as a button: pick a fact, pick a client, get a live assertion
rather than an argument.

### C. Signal — instrumentation
```
┌────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ lumberroom   facts  registry  policy  signal          window: 1h  24h  [7d]  30d                  ◐ theme         │
├────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│                                                                                                            │
│   unprompted share            0.31        ▁▂▂▃▃▂▄▅▄▅▆▅▆▇▆▇▇▆▇█▇  7d                                       │
│   the share of calls a model made on its own, not forced by a hook                                         │
│                                                                                                            │
│   client            calls   failed   unprompted   p50     p95                                              │
│   claude-code-mac   2,411        0   ████████░░░░░░░░ 0.41   44ms   238ms                                 │
│   chatgpt             318       12   ██░░░░░░░░░░░░░░ 0.09   61ms   410ms                                 │
│   claude-web          204        0   █████░░░░░░░░░░░ 0.28   52ms   198ms                                 │
│   openwebui             0        —   ░░░░░░░░░░░░░░░░ —      —       —                                     │
│                                                                                                            │
│   tool                calls    failed                                                                      │
│   memory_search       1,904         3      p50  44ms   p95 238ms                                           │
│   context_bootstrap   1,012         0      p50   4ms   p95  30ms                                           │
│   memory_write          412         9      p50 184ms   p95 197ms                                           │
│   registry_get          105         0      p50   6ms   p95  22ms                                           │
│                                                                                                            │
│   openwebui has made no calls in 7 days. Wired but silent — check the grant or the client.                 │
│                                                                                                            │
├────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ 2,933 calls · 12 failed · lumberroom stats --hours 168                                                           │
└────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```
Filled/unfilled bar segments carry the unprompted share, so it survives greyscale. A zero-call
client gets a sentence, not a green tick — the interesting instrumentation finding is always a
surface that has gone quiet.

## 12. Least confident

1. **Iosevka Fixed at 12px for metadata.** The width argument is real and the density win is
   measurable, but 0.5em advance at 12px is genuinely narrow and the owner may find sustained
   scanning of client names tiring. Fallback: IBM Plex Mono at the same size, costing about one
   column of width across the index. Decide by looking at it, not by arguing.
2. **Mono for all chrome including nav and buttons.** It keeps the "machine vs language" rule pure
   and ships two font files, but mono buttons drift toward the terminal costume this direction
   explicitly rejects. The alternative — a third family for chrome — costs a font file and muddies
   the rule.
3. **Dark as default.** The scene sentence forces it, but sensitivity review is the task where
   getting it wrong matters most, and the light theme has better colour separation
   (sealed at 9.40:1 vs 6.15:1). A defensible alternative is to force light whenever the client lens
   is on. Untested.
4. **Compact as the resting density.** 33 rows of one-line-truncated prose may make the index feel
   like a log rather than a record, and the apparatus dependency is total: if the margin is ever
   slow, compact becomes unusable. Comfortable at 20 rows is still well above the twelve-row
   failure line, and might simply be the right default.
5. **`--sens-private` amber at hue 78.** It is the one reserved colour a red-green deficient viewer
   could confuse with the `--bad` outcome red under poor conditions. The four non-hue channels cover
   it, but if a fifth is wanted, moving private toward hue 95 (further from red) is cheap and only
   slightly muddier.
6. **The weak-band collapse threshold.** Whether "strong / related / weak" maps cleanly onto the
   blended score distribution is an empirical question that needs a real store behind it. The band
   scheme is right; the cut points are a guess until there are ten thousand facts to test against.
