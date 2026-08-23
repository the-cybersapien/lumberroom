#!/usr/bin/env python3
"""Renders three layout systems over the same real console content.

One identity, three systems. Every direction keeps the committed tokens: the cream ground, the ink
ramp, the pencil accent, serif for headings and prose, sans for every control and label, mono for
identifiers. What differs is the layout system, the density, and what the extra width on a large
display is spent on.

The content is the real clients page plus one cleanup decision, so button placement is comparable
rather than described.
"""
import pathlib

# ── the shared system ────────────────────────────────────────────────────────────────────────────
# One type scale at a 1.2 ratio from a 15px base, which is the product register's advice and the
# opposite of the sixteen ad-hoc sizes in the current sheets. One spacing scale on a 4px grid,
# against twenty-five values today. Two button roles, against five geometries.
TOKENS = """
:root{
  --paper:#faf7f1; --paper-2:#f4efe5; --paper-3:#efe8da;
  --ink:#211c17; --ink-2:#4a423a; --ink-3:#6b6258;
  --rule:#ddd4c4; --rule-2:#c3b7a2; --rule-3:#9a8d78;
  --pencil:#a3341f; --pencil-bg:#fbeee9;
  --blue:#1c4f8f; --green:#2c5f34;

  --serif:"Iowan Old Style","Palatino Linotype",Palatino,"Book Antiqua",Georgia,serif;
  --sans:system-ui,-apple-system,"Segoe UI",Roboto,"Helvetica Neue",Arial,sans-serif;
  --mono:ui-monospace,"SF Mono",Menlo,Consolas,monospace;

  /* 1.2 ratio, 15px base. Fixed rem, never fluid: a heading that shrinks in a narrow column
     looks worse rather than better, and this is product UI at a consistent DPI. */
  --t-xs:0.688rem;   /* 11px  labels, kickers */
  --t-sm:0.813rem;   /* 13px  secondary, controls */
  --t-md:0.938rem;   /* 15px  body, data */
  --t-lg:1.125rem;   /* 18px  prose */
  --t-xl:1.375rem;   /* 22px  page heading */
  --t-2xl:1.688rem;  /* 27px  the one display size */

  /* 4px grid. Every gap below comes off this and nothing else. */
  --s-1:4px; --s-2:8px; --s-3:12px; --s-4:16px; --s-5:24px; --s-6:32px; --s-7:48px; --s-8:64px;

  --ease:cubic-bezier(0.16,1,0.3,1);
  --z-nav:10; --z-rail:20; --z-dialog:40;
}
*{box-sizing:border-box;margin:0;padding:0}
html,body{background:var(--paper);color:var(--ink);font:400 var(--t-md)/1.55 var(--sans)}
a{color:var(--blue)}
:focus-visible{outline:2px solid var(--blue);outline-offset:2px}

/* ── one button vocabulary ──────────────────────────────────────────────────────────────────────
   Two roles and two only. Primary is the act the screen exists for. Quiet is everything else,
   including anything destructive: destructive is carried by the word and its position, never by
   red, because red on this ground reads as the pencil accent and the accent means "attention"
   rather than "danger". */
.btn{font:600 var(--t-sm)/1 var(--sans);padding:var(--s-2) var(--s-4);border:1px solid var(--rule-3);
  background:var(--paper-2);color:var(--ink-2);cursor:pointer;
  transition:background 160ms var(--ease),color 160ms var(--ease),border-color 160ms var(--ease)}
.btn:hover{background:var(--paper-3);color:var(--ink)}
.btn:active{transform:translateY(0.5px)}
.btn[disabled]{opacity:.45;cursor:not-allowed}
.btn-primary{background:var(--ink);color:var(--paper);border-color:var(--ink)}
.btn-primary:hover{background:var(--ink-2);color:var(--paper)}
@media (prefers-reduced-motion:reduce){.btn{transition:none}.btn:active{transform:none}}

/* ── one form vocabulary ─────────────────────────────────────────────────────────────────────── */
.field{display:block;margin-block:var(--s-4)}
.field > span{display:block;font:600 var(--t-xs)/1.3 var(--sans);text-transform:uppercase;
  letter-spacing:.08em;color:var(--ink-3);margin-bottom:var(--s-1)}
.field input[type=text],.field select,.field textarea{width:100%;padding:var(--s-2) var(--s-3);
  border:1px solid var(--rule-3);background:var(--paper);color:inherit;
  font:400 var(--t-md)/1.4 var(--sans)}
.field input::placeholder{color:var(--ink-3)}
.hint{display:block;font:400 var(--t-sm)/1.5 var(--sans);color:var(--ink-3);margin-top:var(--s-1);
  max-width:62ch}
.choice{display:flex;gap:var(--s-2);align-items:flex-start;padding:var(--s-2) 0}
.choice input{margin-top:3px}
.choice b{font:600 var(--t-md)/1.4 var(--sans)}

/* ── chrome, shared by every direction ───────────────────────────────────────────────────────── */
.top{display:flex;align-items:center;gap:var(--s-5);flex-wrap:wrap;
  padding:var(--s-2) var(--s-5);border-bottom:1px solid var(--rule-2);background:var(--paper-2);
  position:sticky;top:0;z-index:var(--z-nav)}
.mark{font:600 var(--t-lg)/1 var(--serif)}
.mark em{font-style:normal;font:400 var(--t-xs)/1 var(--sans);color:var(--ink-3);
  text-transform:uppercase;letter-spacing:.1em;margin-left:var(--s-2)}
.nav{display:flex;flex:1;flex-wrap:wrap}
.nav a{font:500 var(--t-sm)/1 var(--sans);color:var(--ink-2);text-decoration:none;
  padding:var(--s-2) var(--s-3);border-bottom:2px solid transparent}
.nav a.on{color:var(--ink);font-weight:700;border-bottom-color:var(--ink)}
.health{font:400 var(--t-sm)/1.4 var(--mono);color:var(--ink-3)}
.health b{color:var(--green);font-weight:600}
h1{font:600 var(--t-xl)/1.2 var(--serif);letter-spacing:-.005em;text-wrap:balance}
h2{font:600 var(--t-lg)/1.3 var(--serif);text-wrap:balance}
.kicker{font:700 var(--t-xs)/1.3 var(--sans);text-transform:uppercase;letter-spacing:.09em;
  color:var(--ink-3)}
.lede{font:400 var(--t-lg)/1.6 var(--serif);color:var(--ink-2);max-width:64ch;text-wrap:pretty}
.id{font:400 var(--t-sm)/1.4 var(--mono);color:var(--blue);user-select:all}
.sim{font:400 var(--t-sm)/1.4 var(--mono);font-variant-numeric:tabular-nums;color:var(--ink-2)}
"""

NAV = """<header class="top"><div class="mark">lumberroom<em>notebook</em></div>
<nav class="nav"><a href="#">Reading</a><a href="#">Write</a><a href="#">Registry</a>
<a href="#">Aliases</a><a href="#">Queue</a><a href="#">Cleanup</a><a class="on" href="#">Clients</a></nav>
<div class="health">key <b>verified</b> &middot; embedder bge-base-en-v1.5 &middot; last write 7m ago</div></header>"""

CLIENTS = [
    ("claude-code-mac", "manual", "consented", "reads *@sealed, writes *@sealed, registryWrite", False),
    ("claude-desktop", "dcr", "consented", "reads *@sealed, writes *@open, no capabilities", False),
    ("cleanup", "manual", "consented", "reads nothing, writes nothing, mayIngest", False),
    ("chatgpt", "dcr", "awaiting consent", "reads *@open, writes nothing, no capabilities", False),
    ("an-old-laptop", "manual", "revoked", "reads *@sealed, writes *@open, no capabilities", True),
]

def rows(kind):
    out = []
    for i, (name, via, state, grant, revoked) in enumerate(CLIENTS):
        cid = f"{'abcdef0123456789'[i]*4}9c1e7b3d8a05f2{i}"
        act = "" if revoked else '<button class="btn">Revoke</button>'
        if kind == "ledger":
            out.append(f"""<article class="entry">
  <div class="entry-head"><h2>{name}</h2><span class="kicker">{state}</span>
    <span class="kicker">{via}</span></div>
  <code class="id">{cid}</code>
  <p class="grant">{grant}</p>
  <div class="acts">{act}</div>
</article>""")
        elif kind == "register":
            out.append(f"""<tr{' class="off"' if revoked else ''}>
  <td class="c-name">{name}</td><td><code class="id">{cid}</code></td>
  <td><span class="kicker">{state}</span></td><td class="c-grant">{grant}</td>
  <td class="c-act">{act}</td></tr>""")
        else:
            out.append(f"""<article class="entry{' off' if revoked else ''}">
  <div><h2>{name}</h2><code class="id">{cid}</code>
    <p class="grant">{grant}</p></div>
  <div class="entry-side"><span class="kicker">{state}</span>
    <span class="kicker">{via}</span>{act}</div>
</article>""")
    return "\n".join(out)

DECISION = """<section class="finding">
  <div class="finding-head"><span class="kicker pencil">contradiction</span>
    <span class="sim">0.954</span><span class="kicker">project:lumberroom</span>
    <span class="kicker">via glm-5.3</span></div>
  <p class="lede">The two statements give different internal nicknames (QUARTZLARK-8297b522 vs
    QUARTZLARK-17eb0cd1) for the same project, so they cannot both be true.</p>
  <div class="members">
    <div class="member"><span class="kicker">retire</span><code class="id">7b36ba97-335d-423f</code>
      <p>The internal nickname for the lumberroom project is QUARTZLARK-8297b522.</p></div>
    <div class="member"><span class="kicker">retire</span><code class="id">ffc4bdf0-9e85-416f</code>
      <p>The internal nickname for the lumberroom project is QUARTZLARK-17eb0cd1.</p></div>
  </div>
  <p class="hint">A contradiction names no survivor: which of two conflicting facts holds is yours to
    call. Keeping one retires the other into it, and the retired text stays readable.</p>
  <div class="acts acts-decide">
    <div class="acts-main">
      <button class="btn btn-primary">Keep QUARTZLARK-8297b522</button>
      <button class="btn btn-primary">Keep QUARTZLARK-17eb0cd1</button>
    </div>
    <form class="acts-quiet"><input type="text" placeholder="why not (optional)">
      <button class="btn">Reject</button></form>
  </div>
</section>"""

FORM = """<section class="compose">
  <h2>New client</h2>
  <label class="field"><span>Name</span><input type="text" placeholder="claude-desktop"></label>
  <span class="hint">What you will call it in this list. A label, never an identity.</span>
  <fieldset class="shapes"><legend class="kicker">Shape</legend>
    <label class="choice"><input type="radio" name="p"><span><b>Read only</b>
      <span class="hint">Reads every namespace at every level and writes nothing.</span></span></label>
    <label class="choice"><input type="radio" name="p" checked><span><b>Read and write</b>
      <span class="hint">Reads everything, writes at open. What a chat surface gets.</span></span></label>
    <label class="choice"><input type="radio" name="p"><span><b>Ingest bot</b>
      <span class="hint">Fills the proposal queue and decides nothing.</span></span></label>
    <label class="choice"><input type="radio" name="p"><span><b>Full</b>
      <span class="hint">Everything except deletion.</span></span></label>
  </fieldset>
  <details><summary>Adjust namespaces and capabilities</summary>
    <label class="field"><span>Reads</span><input type="text" placeholder="*@sealed"></label>
    <label class="field"><span>Writes</span><input type="text" placeholder="*@open"></label>
  </details>
  <div class="acts"><button class="btn btn-primary">Create it</button>
    <a href="#">Back to reading</a></div>
</section>"""

# ── A. Ledger ────────────────────────────────────────────────────────────────────────────────────
# One column, entries separated by rules, reading measure held at 68ch. Closest to the notebook it
# already is. The large display buys a wider measure and more entries on screen, never more margin.
LEDGER = """
.page{padding:var(--s-5) var(--s-5) var(--s-8);max-width:78ch;margin-inline:auto}
.pagehead{display:flex;align-items:baseline;gap:var(--s-3);flex-wrap:wrap;
  padding-bottom:var(--s-2);border-bottom:2px solid var(--rule-2)}
.entry{padding:var(--s-4) 0;border-bottom:1px solid var(--rule)}
.entry-head{display:flex;align-items:baseline;gap:var(--s-3);flex-wrap:wrap}
.entry .grant{font:400 var(--t-md)/1.5 var(--sans);color:var(--ink-2);margin-top:var(--s-1)}
.entry .id{display:block;margin-top:var(--s-1)}
/* One placement rule, everywhere: actions end the block, left aligned with its text, primary
   first. Never between two fields, which is what the cleanup row did. */
.acts{display:flex;gap:var(--s-3);align-items:center;flex-wrap:wrap;margin-top:var(--s-3)}
.acts-decide{flex-direction:column;align-items:flex-start;gap:var(--s-3)}
.acts-main{display:flex;gap:var(--s-2);flex-wrap:wrap}
.acts-quiet{display:flex;gap:var(--s-2);align-items:center;
  padding-top:var(--s-3);border-top:1px solid var(--rule);width:100%}
.acts-quiet input{flex:1;max-width:32ch;padding:var(--s-2) var(--s-3);border:1px solid var(--rule-3);
  background:var(--paper);font:400 var(--t-sm)/1.4 var(--sans)}
.finding{padding:var(--s-5) 0;border-bottom:1px solid var(--rule)}
.finding-head{display:flex;gap:var(--s-3);flex-wrap:wrap;align-items:baseline;
  margin-bottom:var(--s-2)}
.pencil{color:var(--pencil)}
.members{margin:var(--s-3) 0;border-left:1px solid var(--rule-2);padding-left:var(--s-4)}
.member{padding:var(--s-2) 0}
.member p{font:400 var(--t-md)/1.5 var(--serif);color:var(--ink-2);margin-top:var(--s-1)}
.compose{padding-top:var(--s-6)}
.shapes{border:0;margin:var(--s-4) 0}
details summary{font:600 var(--t-sm)/1.4 var(--sans);cursor:pointer;padding:var(--s-2) 0;
  color:var(--ink-2)}
@media (min-width:1600px){ .page{max-width:96ch;padding-inline:var(--s-7)} }
"""

# ── B. Register ──────────────────────────────────────────────────────────────────────────────────
# Table-forward. Aligned columns make five clients scannable at a glance and fifty still readable.
# The large display gains columns rather than margin, and the compose form moves beside the table.
REGISTER = """
.page{padding:var(--s-5);max-width:1800px;margin-inline:auto}
.pagehead{display:flex;align-items:baseline;gap:var(--s-3);flex-wrap:wrap;
  padding-bottom:var(--s-2);border-bottom:2px solid var(--rule-2)}
.grid{display:grid;grid-template-columns:1fr;gap:var(--s-6);margin-top:var(--s-5)}
table{width:100%;border-collapse:collapse;font:400 var(--t-md)/1.45 var(--sans)}
th{font:700 var(--t-xs)/1.3 var(--sans);text-transform:uppercase;letter-spacing:.09em;
  color:var(--ink-3);text-align:left;padding:var(--s-2) var(--s-3) var(--s-2) 0;
  border-bottom:1px solid var(--rule-2)}
td{padding:var(--s-3) var(--s-3) var(--s-3) 0;border-bottom:1px solid var(--rule);
  vertical-align:top}
tr.off{color:var(--ink-3)}
.c-name{font-weight:600;white-space:nowrap}
.c-grant{color:var(--ink-2);font-size:var(--t-sm)}
.c-act{text-align:right;white-space:nowrap}
.acts{display:flex;gap:var(--s-3);align-items:center;flex-wrap:wrap;margin-top:var(--s-4)}
.acts-decide{flex-direction:column;align-items:flex-start;gap:var(--s-3)}
.acts-main{display:flex;gap:var(--s-2);flex-wrap:wrap}
.acts-quiet{display:flex;gap:var(--s-2);align-items:center;padding-top:var(--s-3);
  border-top:1px solid var(--rule);width:100%}
.acts-quiet input{flex:1;max-width:32ch;padding:var(--s-2) var(--s-3);border:1px solid var(--rule-3);
  background:var(--paper);font:400 var(--t-sm)/1.4 var(--sans)}
.finding{padding:var(--s-4);border:1px solid var(--rule-2);background:var(--paper-2);
  margin-top:var(--s-5)}
.finding-head{display:flex;gap:var(--s-3);flex-wrap:wrap;align-items:baseline;
  margin-bottom:var(--s-2)}
.pencil{color:var(--pencil)}
.members{margin:var(--s-3) 0;display:grid;gap:var(--s-3)}
.member{padding:var(--s-2) 0;border-top:1px solid var(--rule)}
.member p{font:400 var(--t-md)/1.5 var(--serif);color:var(--ink-2);margin-top:var(--s-1)}
.lede{max-width:70ch}
.compose{max-width:56ch}
.shapes{border:0;margin:var(--s-4) 0}
details summary{font:600 var(--t-sm)/1.4 var(--sans);cursor:pointer;padding:var(--s-2) 0;
  color:var(--ink-2)}
@media (min-width:1400px){ .grid{grid-template-columns:minmax(0,1fr) 46ch} }
"""

# ── C. Sheet ─────────────────────────────────────────────────────────────────────────────────────
# Content in a held measure with a side rail that carries state and every action. Placement stops
# being a per-screen decision because there is exactly one place an action can go.
SHEET = """
.page{padding:var(--s-5);max-width:1700px;margin-inline:auto}
.pagehead{display:flex;align-items:baseline;gap:var(--s-3);flex-wrap:wrap;
  padding-bottom:var(--s-2);border-bottom:2px solid var(--rule-2)}
.entry{display:grid;grid-template-columns:1fr;gap:var(--s-3);padding:var(--s-4) 0;
  border-bottom:1px solid var(--rule)}
.entry.off{color:var(--ink-3)}
.entry .grant{font:400 var(--t-md)/1.5 var(--sans);color:var(--ink-2);margin-top:var(--s-1)}
.entry .id{display:block;margin-top:var(--s-1)}
.entry-side{display:flex;gap:var(--s-2);align-items:center;flex-wrap:wrap}
.acts{display:flex;gap:var(--s-3);align-items:center;flex-wrap:wrap;margin-top:var(--s-4)}
.acts-decide{display:grid;gap:var(--s-3)}
.acts-main{display:grid;gap:var(--s-2)}
.acts-quiet{display:flex;gap:var(--s-2);align-items:center;padding-top:var(--s-3);
  border-top:1px solid var(--rule)}
.acts-quiet input{flex:1;padding:var(--s-2) var(--s-3);border:1px solid var(--rule-3);
  background:var(--paper);font:400 var(--t-sm)/1.4 var(--sans)}
.finding{display:grid;gap:var(--s-4);padding:var(--s-5) 0;border-bottom:1px solid var(--rule)}
.finding-head{display:flex;gap:var(--s-3);flex-wrap:wrap;align-items:baseline}
.pencil{color:var(--pencil)}
.members{border-left:1px solid var(--rule-2);padding-left:var(--s-4);display:grid;gap:var(--s-2)}
.member p{font:400 var(--t-md)/1.5 var(--serif);color:var(--ink-2);margin-top:var(--s-1)}
.lede{max-width:64ch}
.compose{padding-top:var(--s-6);max-width:60ch}
.shapes{border:0;margin:var(--s-4) 0}
details summary{font:600 var(--t-sm)/1.4 var(--sans);cursor:pointer;padding:var(--s-2) 0;
  color:var(--ink-2)}
@media (min-width:1280px){
  .entry{grid-template-columns:minmax(0,1fr) 24ch;align-items:start}
  .entry-side{flex-direction:column;align-items:flex-end;text-align:right}
  .finding{grid-template-columns:minmax(0,1fr) 28ch}
  .finding-head{grid-column:1/-1}
  .acts-decide{align-content:start}
}
"""

DIRECTIONS = [
    ("a-ledger", "Ledger", LEDGER, "ledger",
     "One column, entries divided by rules, measure held at 68ch. The notebook it already is, "
     "with a scale under it. A wide display buys a wider measure and more entries, never margin."),
    ("b-register", "Register", REGISTER, "register",
     "Table forward. Aligned columns make five clients scannable and fifty still readable. "
     "A wide display puts the compose form beside the table instead of below it."),
    ("c-sheet", "Sheet", SHEET, "sheet",
     "Content in a held measure with a side rail carrying state and every action. Placement stops "
     "being a per-screen decision: there is one place an action can go."),
]

for slug, title, css, kind, blurb in DIRECTIONS:
    listing = rows(kind)
    if kind == "register":
        listing = ("<table><thead><tr><th>Name</th><th>Client id</th><th>State</th>"
                   "<th>Grant</th><th></th></tr></thead><tbody>" + listing + "</tbody></table>")
    body = f"""{NAV}
<main class="page">
  <div class="pagehead"><h1>Clients</h1><span class="kicker">5 registered</span></div>
  <p class="lede">Every surface that reaches this store, and what each may do. A client created
    here is consented to already; one that registered itself waits at the consent screen.</p>
  <div class="grid">
    <div>{listing}
      {DECISION}
    </div>
    {FORM}
  </div>
</main>"""
    html = f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>lumberroom console: {title}</title>
<style>{TOKENS}{css}</style></head>
<body>{body}</body></html>
"""
    pathlib.Path(f"design/{slug}.html").write_text(html)
    print(f"wrote design/{slug}.html  ({title}: {blurb.split('.')[0]}.)")
