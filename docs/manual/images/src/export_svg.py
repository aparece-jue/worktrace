#!/usr/bin/env python3
"""Export a diagram HTML (inline SVG) to a standalone .svg, with a hard XML gate.

Usage:
    python3 export_svg.py <src.html> [out.svg]      # one figure
    python3 export_svg.py --all [src-dir]            # every *.html in src-dir
                                                     # (out = ../<slug>.svg)
Layout convention (Worktrace manual figures):
    docs/manual/images/src/<slug>.html   ->  docs/manual/images/<slug>.svg

The HTML source is the single source of truth; the .svg is derived. A standalone
.svg is parsed as *strict* XML by browsers: one stray quote makes the reader's
whole page fail, so every export is re-parsed with xml.etree.ElementTree and the
run aborts on the first failure. Gates:

  1. XML        -- ET.fromstring on the written file (hard gate)
  2. a11y       -- <title> first child, role="img", aria-labelledby -> title+desc,
                   slug-prefixed ids (no bare title/desc)
  3. connectors -- orthogonal only: <line> axis-aligned, <path> M/H/V/Q with
                   quarter-arc corners (r=8); no diagonal slants
  4. budget     -- node boxes <= 9, connectors <= 12 (diagram-design budget)
"""

from __future__ import annotations

import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

SVG_NS = "http://www.w3.org/2000/svg"
FONT_IMPORT = (
    "@import url('https://fonts.googleapis.com/css2?family=Geist:wght@400;500;600"
    "&amp;family=Geist+Mono:wght@400;500"
    "&amp;family=Instrument+Serif:ital@0;1&amp;display=swap');"
)
RGBA_RE = re.compile(
    r'(fill|stroke)="rgba\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d*\.?\d+)\s*\)"'
)
TRANSPARENT_RE = re.compile(r'(fill|stroke)="transparent"')
TAG_RE = re.compile(r"<[^>]+>")


class GateError(RuntimeError):
    """A hard gate failed; the export must not be published."""


def extract_svg(html: str) -> str:
    match = re.search(r"<svg\b.*?</svg>", html, re.S)
    if not match:
        raise GateError("no <svg>...</svg> block found in the HTML source")
    return match.group(0)


def make_standalone(svg: str) -> str:
    if 'xmlns="http://www.w3.org/2000/svg"' not in svg:
        svg = svg.replace("<svg", f'<svg xmlns="{SVG_NS}"', 1)
    if "viewBox=" not in svg.split(">", 1)[0]:
        raise GateError("the <svg> has no viewBox; sizes would be guessed")

    style = f"<style>{FONT_IMPORT}</style>"
    if "<defs" in svg:
        svg = re.sub(r"(<defs[^>]*>)", r"\1" + style, svg, count=1)
    else:
        svg = re.sub(r"(<svg[^>]*>)", r"\1<defs>" + style + "</defs>", svg, count=1)

    svg = RGBA_RE.sub(
        lambda m: '{0}="#{1:02x}{2:02x}{3:02x}" {0}-opacity="{4}"'.format(
            m.group(1), int(m.group(2)), int(m.group(3)), int(m.group(4)), m.group(5)
        ),
        svg,
    )
    svg = TRANSPARENT_RE.sub(r'\1="none"', svg)
    return '<?xml version="1.0" encoding="UTF-8"?>\n' + svg + "\n"


def local(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def gate_a11y(root: ET.Element, slug: str) -> list[str]:
    notes: list[str] = []
    children = list(root)
    if not children or local(children[0].tag) != "title":
        raise GateError("<title> must be the first child of <svg>")
    if root.get("role") != "img":
        raise GateError('<svg> needs role="img"')
    ids = [local(c.tag) for c in children]
    if "desc" not in ids:
        raise GateError("<svg> needs a <desc>")
    title = children[0]
    desc = children[ids.index("desc")]
    title_id, desc_id = title.get("id", ""), desc.get("id", "")
    if not title_id.startswith(slug) or not desc_id.startswith(slug):
        raise GateError(f"title/desc ids must be prefixed with '{slug}' (got {title_id!r}/{desc_id!r})")
    if title_id in {"title", "desc"} or desc_id in {"title", "desc"}:
        raise GateError("bare title/desc ids are banned")
    if root.get("aria-labelledby", "").split() != [title_id, desc_id]:
        raise GateError("aria-labelledby must name <title> then <desc>")
    if not (title.text or "").strip() or not (desc.text or "").strip():
        raise GateError("<title> and <desc> must both be filled in")
    notes.append(f"a11y ok ({title_id} / {desc_id})")
    return notes


def check_path_d(d: str, where: str) -> None:
    """Every segment must be axis-aligned; corners are quarter arcs (r=8)."""
    tokens = re.findall(r"([MHQVZ])([^MHQVZ]*)", d)
    if not tokens or tokens[0][0] != "M":
        raise GateError(f"{where}: path must start with M")
    pos = None
    for cmd, raw in tokens:
        nums = [float(n) for n in re.findall(r"-?\d+(?:\.\d+)?", raw)]
        if cmd == "M":
            if len(nums) != 2:
                raise GateError(f"{where}: M takes exactly 2 numbers")
            pos = (nums[0], nums[1])
        elif cmd in "HV":
            if len(nums) != 1 or pos is None:
                raise GateError(f"{where}: {cmd} takes exactly 1 number")
            pos = (nums[0], pos[1]) if cmd == "H" else (pos[0], nums[0])
        elif cmd == "Q":
            if len(nums) != 4 or pos is None:
                raise GateError(f"{where}: Q takes exactly 4 numbers")
            cx, cy, x, y = nums
            start = pos
            if not ((cx == start[0] and cy == y) or (cy == start[1] and cx == x)):
                raise GateError(f"{where}: Q corner is not a right angle")
            if abs(cx - start[0]) + abs(cy - start[1]) != 8 or abs(x - cx) + abs(y - cy) != 8:
                raise GateError(f"{where}: Q corner radius must be 8")
            pos = (x, y)
        elif cmd == "Z":
            raise GateError(f"{where}: connector paths must not be closed")
    return None


def gate_connectors(root: ET.Element) -> tuple[list[str], int]:
    notes: list[str] = []
    connectors = 0
    for el in root.iter():
        tag = local(el.tag)
        if tag == "path" and el.get("marker-end"):
            connectors += 1
            label = el.get("id") or f"path#{connectors}"
            check_path_d(el.get("d", ""), label)
        elif tag == "line":
            x1, y1, x2, y2 = (float(el.get(k, 0)) for k in ("x1", "y1", "x2", "y2"))
            if x1 != x2 and y1 != y2:
                raise GateError(f"{el.get('id') or 'line'}: diagonal <line> is banned")
            if el.get("marker-end"):
                connectors += 1
    notes.append(f"connectors orthogonal ok ({connectors} arrows)")
    return notes, connectors


def count_nodes(root: ET.Element) -> int:
    """Node boxes = rounded rects (rx=6) at node size; chips use rx<=4."""
    nodes = 0
    for el in root.iter():
        if local(el.tag) != "rect":
            continue
        rx = float(el.get("rx", 0))
        w = float(el.get("width", 0))
        h = float(el.get("height", 0))
        if rx == 6 and w >= 140 and h >= 60:
            nodes += 1
    return nodes


def export_one(src: Path, out: Path) -> list[str]:
    slug = src.stem
    html = src.read_text(encoding="utf-8")
    svg = make_standalone(extract_svg(html))
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(svg, encoding="utf-8")

    # --- hard XML gate: exactly what a browser does with a standalone .svg ---
    try:
        root = ET.fromstring(out.read_text(encoding="utf-8"))
    except ET.ParseError as exc:  # pragma: no cover - the gate is the point
        out.unlink(missing_ok=True)
        raise GateError(f"{out.name}: strict XML parse failed: {exc}") from exc
    if local(root.tag) != "svg":
        raise GateError(f"{out.name}: root element is <{local(root.tag)}>, not <svg>")

    notes = [f"XML ok ({out.stat().st_size} bytes)"]
    notes += gate_a11y(root, slug)
    conn_notes, arrows = gate_connectors(root)
    notes += conn_notes
    nodes = count_nodes(root)
    if nodes > 9:
        raise GateError(f"{out.name}: {nodes} node boxes > budget 9")
    if arrows > 12:
        raise GateError(f"{out.name}: {arrows} connectors > budget 12")
    notes.append(f"budget ok (nodes={nodes}, arrows={arrows})")
    return notes


def main(argv: list[str]) -> int:
    if len(argv) >= 2 and argv[1] == "--all":
        src_dir = Path(argv[2]) if len(argv) > 2 else Path(__file__).resolve().parent
        sources = sorted(p for p in src_dir.glob("*.html"))
        if not sources:
            print(f"FAIL no .html under {src_dir}")
            return 1
        targets = [(p, src_dir.parent / f"{p.stem}.svg") for p in sources]
    elif len(argv) >= 2:
        src = Path(argv[1]).resolve()
        out = Path(argv[2]).resolve() if len(argv) > 2 else src.parent.parent / f"{src.stem}.svg"
        targets = [(src, out)]
    else:
        print(__doc__)
        return 2

    failed = 0
    for src, out in targets:
        try:
            notes = export_one(src, out)
        except GateError as exc:
            failed += 1
            print(f"FAIL {src.name}")
            print(f"  - {exc}")
            continue
        print(f"OK   {src.name} -> {out.name}")
        for note in notes:
            print(f"  - {note}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
