#!/usr/bin/env python3
"""Maps the three stylesheets onto one scale, keeping every selector.

A rewrite would lose classes silently. This transforms values instead: every font size snaps to the
nearest step of a 1.2 ratio scale off a 15px base, and every spacing value snaps to the 4px grid.
Selectors are untouched, so the class-coverage tests still hold afterwards and anything that moves
is a value rather than a rule that vanished.
"""
import re, pathlib, sys

# 1.2 ratio, 15px base. Six steps, against the sixteen ad-hoc sizes measured today.
SCALE = [11, 13, 15, 18, 22, 27]
def snap_font(px: float) -> int:
    return min(SCALE, key=lambda s: abs(s - px))

# 4px grid. Eight steps, against twenty-five values today.
GRID = [0, 4, 8, 12, 16, 24, 32, 48, 64]
def snap_space(px: int) -> int:
    return min(GRID, key=lambda s: abs(s - px))

def retheme(style: str) -> tuple[str, dict]:
    moved = {"font": 0, "space": 0}

    def font_sub(m):
        px = float(m.group(1))
        s = snap_font(px)
        if abs(s - px) > 0.01:
            moved["font"] += 1
        return f"{s}px"

    # Only inside a font shorthand or font-size, so a 12px border radius is not mistaken for type.
    def in_font(m):
        return re.sub(r'(\d+(?:\.\d+)?)px', font_sub, m.group(0))
    style = re.sub(r'font(?:-size)?:[^;}]+', in_font, style)

    def space_sub(m):
        px = int(m.group(1))
        s = snap_space(px)
        if s != px:
            moved["space"] += 1
        return f"{s}px"

    def in_space(m):
        return re.sub(r'(\d+)px', space_sub, m.group(0))
    style = re.sub(r'(?:padding|margin|gap|row-gap|column-gap)[^:]*:[^;}]+', in_space, style)
    return style, moved

# The three sheets are plain .css files now rather than Rust string literals, so this reads each one
# whole and no longer has to find where a literal ends by walking escapes.
total = {"font": 0, "space": 0}
for f in ["src/console/console.css", "src/console/aliases.css", "src/console/cleanup.css"]:
    p = pathlib.Path(f)
    new, moved = retheme(p.read_text())
    p.write_text(new)
    total["font"] += moved["font"]; total["space"] += moved["space"]
    print(f"  {f.split('/')[-1]:<14} {moved['font']:>3} type values snapped, {moved['space']:>3} spacing values snapped")
print(f"\n  total: {total['font']} type, {total['space']} spacing")
