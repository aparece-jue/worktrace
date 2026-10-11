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
  4. geometry   -- label masks must not overlap a node box; no connector may run
                   through the interior of a box that is not its own endpoint
  5. budget     -- node boxes <= 9, connectors <= 12 (diagram-design budget)
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
    """Node boxes = rounded rects (rx 6 or 8) at node size, without a zone wash.

    Chips are rx=4 and small; zone backgrounds carry fill-opacity; the status /
    legend strips are too flat (h < 60) to be nodes.
    """
    return len(node_rects(root))


def node_rects(root: ET.Element) -> list[tuple[float, float, float, float]]:
    boxes: list[tuple[float, float, float, float]] = []
    for el in root.iter():
        if local(el.tag) != "rect" or el.get("fill-opacity"):
            continue
        rx = float(el.get("rx", 0))
        x, y = float(el.get("x", 0)), float(el.get("y", 0))
        w, h = float(el.get("width", 0)), float(el.get("height", 0))
        if rx in (6, 8) and 140 <= w <= 960 and 60 <= h <= 260:
            boxes.append((x, y, x + w, y + h))
    return boxes


def label_masks(root: ET.Element) -> list[tuple[float, float, float, float]]:
    """Arrow-label masks are paper-filled rects that are not node boxes."""
    masks: list[tuple[float, float, float, float]] = []
    for el in root.iter():
        if local(el.tag) != "rect" or el.get("fill") != "#FBF9F6":
            continue
        x, y = float(el.get("x", 0)), float(el.get("y", 0))
        w, h = float(el.get("width", 0)), float(el.get("height", 0))
        if w <= 400 and h <= 24:
            masks.append((x, y, x + w, y + h))
    return masks


def overlap(
    a: tuple[float, float, float, float], b: tuple[float, float, float, float], inset: float = 0.0
) -> float:
    x0 = max(a[0], b[0] + inset)
    y0 = max(a[1], b[1] + inset)
    x1 = min(a[2], b[2] - inset)
    y1 = min(a[3], b[3] - inset)
    if x1 <= x0 or y1 <= y0:
        return 0.0
    return (x1 - x0) * (y1 - y0)


def path_polyline(d: str) -> list[tuple[float, float]]:
    """Orthogonal path -> polyline; a Q corner contributes its corner point."""
    points: list[tuple[float, float]] = []
    pos: tuple[float, float] | None = None
    for cmd, raw in re.findall(r"([MHQVZ])([^MHQVZ]*)", d):
        nums = [float(n) for n in re.findall(r"-?\d+(?:\.\d+)?", raw)]
        if cmd == "M":
            pos = (nums[0], nums[1])
            points.append(pos)
        elif cmd == "H":
            pos = (nums[0], pos[1])  # type: ignore[index]
            points.append(pos)
        elif cmd == "V":
            pos = (pos[0], nums[0])  # type: ignore[index]
            points.append(pos)
        elif cmd == "Q":
            pos = (nums[2], nums[3])
            points.append(pos)
    return points


def on_border(point: tuple[float, float], box: tuple[float, float, float, float], tol: float = 3.0) -> bool:
    x, y = point
    inside_x = box[0] - tol <= x <= box[2] + tol
    inside_y = box[1] - tol <= y <= box[3] + tol
    if not (inside_x and inside_y):
        return False
    near_v = abs(x - box[0]) <= tol or abs(x - box[2]) <= tol
    near_h = abs(y - box[1]) <= tol or abs(y - box[3]) <= tol
    return near_v or near_h


def gate_geometry(root: ET.Element) -> list[str]:
    boxes = node_rects(root)
    for i, a in enumerate(boxes):
        for b in boxes[i + 1 :]:
            if overlap(a, b, inset=1.0) > 0:
                raise GateError(f"two node boxes overlap: {a} / {b}")

    for mask in label_masks(root):
        for box in boxes:
            if overlap(mask, box, inset=1.0) > 0:
                raise GateError(f"label mask {mask} overlaps node box {box}")

    for el in root.iter():
        if local(el.tag) != "path" or not el.get("marker-end"):
            continue
        points = path_polyline(el.get("d", ""))
        if len(points) < 2:
            continue
        start, end = points[0], points[-1]
        for box in boxes:
            if on_border(start, box) or on_border(end, box):
                continue
            for p, q in zip(points, points[1:]):
                seg = (
                    min(p[0], q[0]),
                    min(p[1], q[1]),
                    max(p[0], q[0]) or min(p[0], q[0]) + 0.001,
                    max(p[1], q[1]) or min(p[1], q[1]) + 0.001,
                )
                if overlap(seg, box, inset=2.0) > 4.0:
                    raise GateError(
                        f"connector {el.get('d')} runs through a non-endpoint node {box}"
                    )
    return [f"geometry ok ({len(boxes)} node boxes, {len(label_masks(root))} label masks)"]


def segments(points: list[tuple[float, float]]) -> list[tuple[tuple[float, float], tuple[float, float]]]:
    return list(zip(points, points[1:]))


def gate_no_overlap(root: ET.Element) -> list[str]:
    """No two connectors may share a stroke path or cross without a hop."""
    paths: list[str] = []
    for el in root.iter():
        if local(el.tag) == "path" and el.get("marker-end"):
            paths.append(el.get("d", ""))
    drawn = [(d, segments(path_polyline(d))) for d in paths]
    for i, (da, sa) in enumerate(drawn):
        for db, sb in drawn[i + 1 :]:
            for p1, p2 in sa:
                for q1, q2 in sb:
                    a_h = abs(p1[1] - p2[1]) < 0.5
                    b_h = abs(q1[1] - q2[1]) < 0.5
                    if a_h and b_h and abs(p1[1] - q1[1]) < 0.5:
                        lo = max(min(p1[0], p2[0]), min(q1[0], q2[0]))
                        hi = min(max(p1[0], p2[0]), max(q1[0], q2[0]))
                        if hi - lo > 4:
                            raise GateError(f"connectors overlap on a horizontal segment: {da} / {db}")
                    elif (not a_h) and (not b_h) and abs(p1[0] - q1[0]) < 0.5:
                        lo = max(min(p1[1], p2[1]), min(q1[1], q2[1]))
                        hi = min(max(p1[1], p2[1]), max(q1[1], q2[1]))
                        if hi - lo > 4:
                            raise GateError(f"connectors overlap on a vertical segment: {da} / {db}")
    return [f"no shared connector strokes ({len(paths)} paths)"]


def gate_no_crossings(root: ET.Element) -> list[str]:
    """Two connectors may not cross: a crossing would need a hop/bridge."""
    drawn: list[tuple[str, list[tuple[tuple[float, float], tuple[float, float]]]]] = []
    for el in root.iter():
        if local(el.tag) == "path" and el.get("marker-end"):
            d = el.get("d", "")
            drawn.append((d, segments(path_polyline(d))))
    for i, (da, sa) in enumerate(drawn):
        for db, sb in drawn[i + 1 :]:
            for p1, p2 in sa:
                for q1, q2 in sb:
                    a_h = abs(p1[1] - p2[1]) < 0.5
                    b_h = abs(q1[1] - q2[1]) < 0.5
                    if a_h == b_h:
                        continue
                    h = (p1, p2) if a_h else (q1, q2)
                    vt = (q1, q2) if a_h else (p1, p2)
                    hx0, hx1 = sorted((h[0][0], h[1][0]))
                    vy0, vy1 = sorted((vt[0][1], vt[1][1]))
                    x, y = vt[0][0], h[0][1]
                    if hx0 + 4 < x < hx1 - 4 and vy0 + 4 < y < vy1 - 4:
                        raise GateError(f"connectors cross at ({x:.0f},{y:.0f}): {da} / {db}")
    return [f"no connector crossings ({len(drawn)} paths)"]


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
    notes += gate_geometry(root)
    notes += gate_no_overlap(root)
    notes += gate_no_crossings(root)
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
