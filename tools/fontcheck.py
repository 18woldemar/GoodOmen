#!/usr/bin/env python3
"""
fontcheck.py -- the font's advance table against its own pixels.

MDK2 ships each font as a pair: `font.tex` is a 512x512 raw RGBA atlas of a
**16 x 16 grid of 32-pixel cells**, and `font.lua` is a 16 x 16 table of
per-glyph advances as a fraction of a cell. The engine reads the pair in
`engine/src/render/overlay.rs`.

The question the file cannot answer is which way round the table is indexed.
0x462cf0 reads a flat 256-entry array at `font + 4` and the loader that
filled it is not in the Lua-visible half of the binary, so the reading is
settled here instead, against the art:

    advance[c] = dim[c % 16 + 1][c / 16 + 1]      -- transposed

Measure the inked width of every printable cell and compare it with each
candidate. An advance is the ink plus a small right-hand bearing, so the
right reading is *slightly above* the ink on average and the wrong one is
scattered. It is not close: over the 94 printable glyphs the transposed
reading is off by a mean of 0.0066 and the direct one by 0.0921.

Usage:
    python3 tools/fontcheck.py extracted
"""

from __future__ import annotations

import argparse
import re
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import tex2png  # noqa: E402

#: every atlas is sixteen glyphs across; the number of rows differs, and
#: the table says which -- 16 x 16 for the menu font, 16 x 24 for the
#: dialogue font, whose cells are 32 by 21.33
COLUMNS = 16
#: alpha above this is ink; the atlas is antialiased and its background is 0
INK = 16
#: the mean |advance - ink| the transposed reading must stay under, and the
#: margin the direct reading must lose by. Measured, not chosen: see above.
TOLERANCE = 0.02
MARGIN = 4.0


def table(source: str) -> tuple[list[list[float]], int]:
    """The numbers in a font.lua, in file order, and how many rows its atlas
    has -- which is how many numbers each line holds."""
    rows = [l for l in source.splitlines() if l.strip().startswith("{")]
    out = [[float(x) for x in re.findall(r"[0-9]*\.[0-9]+", row)] for row in rows]
    if len(out) != COLUMNS or len({len(r) for r in out}) != 1:
        raise SystemExit(f"a font table is {COLUMNS} rows of one length, this one is "
                         f"{len(out)}x{len(out[0]) if out else 0}")
    return out, len(out[0])


def ink_widths(data: bytes, rows: int) -> dict[int, float]:
    """The inked width of every cell that has any, as a fraction of a cell."""
    info = tex2png.parse(data)
    if info["compressed"]:
        raise SystemExit("a font atlas is raw RGBA; this one is compressed")
    w, h = info["width"], info["height"]
    px, stride = info["pixels"], w * 4
    cw = w / COLUMNS
    out = {}
    for code in range(COLUMNS * rows):
        # rows are stored bottom-up, so the grid row counts from the end
        column, row = code % COLUMNS, rows - 1 - code // COLUMNS
        x0, x1 = round(column * cw), round((column + 1) * cw)
        # a pixel in from each edge, because a cell height of 21.33 rounds
        # into its neighbour
        y0, y1 = round(row * h / rows) + 1, round((row + 1) * h / rows) - 1
        lo, hi = None, None
        for x in range(x0, x1):
            if any(px[y * stride + x * 4 + 3] > INK for y in range(y0, y1)):
                lo = x if lo is None else lo
                hi = x
        if lo is not None:
            out[code] = (hi - lo + 1) / cw
    return out


def check(name: str, lua: str, tex: bytes, least_exact: int) -> str:
    """The two readings against the art. An advance is the ink plus a right
    bearing, so the mean error is not the test -- **the exact ties are**: a
    wrong reading has none, and both fonts have dozens."""
    dim, rows = table(lua)
    ink = ink_widths(tex, rows)
    printable = {c: w for c, w in ink.items() if 33 <= c < 127}
    if len(printable) < 90:
        raise SystemExit(f"{name}: only {len(printable)} printable glyphs have ink")
    direct = sum(abs(dim[c // COLUMNS][c % COLUMNS] - w) for c, w in printable.items())
    trans = sum(abs(dim[c % COLUMNS][c // COLUMNS] - w) for c, w in printable.items())
    direct, trans = direct / len(printable), trans / len(printable)
    if direct <= trans:
        raise SystemExit(f"{name}: the direct reading is no worse "
                         f"({direct:.4f} against {trans:.4f})")
    exact = sum(1 for c, w in printable.items()
                if abs(dim[c % COLUMNS][c // COLUMNS] - w) < 1e-6)
    if exact < least_exact:
        raise SystemExit(f"{name}: only {exact} advances are exactly their ink")
    return (f"  {name:<12} {COLUMNS}x{rows}, {len(printable)} glyphs, {exact} advances "
            f"exactly their ink, transposed off by {trans:.4f} against {direct:.4f}")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("root", type=Path, help="the extracted/ directory")
    args = ap.parse_args(argv)

    base = args.root / "base"
    lines = []
    for name, least_exact in (("font", 70), ("dialogfont", 35)):
        lua, tex = base / f"{name}.lua", base / f"{name}.tex"
        if not lua.exists() or not tex.exists():
            raise SystemExit(f"{lua} or {tex} is missing; unpack base.zip first")
        lines.append(check(name, lua.read_text(encoding="latin-1"), tex.read_bytes(),
                           least_exact))
    print(f"{len(lines)} fonts, both tables agreeing with their own pixels")
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
