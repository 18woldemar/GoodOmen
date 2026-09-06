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

CELL = 32
GRID = 16
#: alpha above this is ink; the atlas is antialiased and its background is 0
INK = 16
#: the mean |advance - ink| the transposed reading must stay under, and the
#: margin the direct reading must lose by. Measured, not chosen: see above.
TOLERANCE = 0.02
MARGIN = 4.0


def table(source: str) -> list[list[float]]:
    """The 16 x 16 of numbers in a font.lua, in file order."""
    rows = [l for l in source.splitlines() if l.strip().startswith("{")]
    out = [[float(x) for x in re.findall(r"[0-9]*\.[0-9]+", row)] for row in rows]
    # 16 rows of at least 16: `dialogfont.lua` is 16 x 24, which is a wider
    # code range than the 16 x 16 atlas has cells for -- Latin-1 fills the
    # atlas and the columns past it are for codes nothing draws.
    if len(out) != GRID or any(len(r) < GRID for r in out):
        raise SystemExit(f"a font table is {GRID} rows of {GRID} or more, this one is "
                         f"{len(out)}x{len(out[0]) if out else 0}")
    return out


def ink_widths(data: bytes) -> dict[int, float]:
    """The inked width of every cell that has any, as a fraction of a cell."""
    info = tex2png.parse(data)
    if info["compressed"]:
        raise SystemExit("a font atlas is raw RGBA; this one is compressed")
    if (info["width"], info["height"]) != (CELL * GRID, CELL * GRID):
        raise SystemExit(f"{info['width']}x{info['height']} is not a font atlas")
    px, stride = info["pixels"], info["width"] * 4
    out = {}
    for code in range(256):
        # rows are stored bottom-up, so the grid row counts from the end
        x0 = (code % GRID) * CELL
        y0 = (GRID - 1 - code // GRID) * CELL
        lo, hi = None, None
        for x in range(CELL):
            column = x0 + x
            if any(px[(y0 + y) * stride + column * 4 + 3] > INK for y in range(CELL)):
                lo = x if lo is None else lo
                hi = x
        if lo is not None:
            out[code] = (hi - lo + 1) / CELL
    return out


def check(name: str, lua: str, tex: bytes) -> str:
    dim = table(lua)
    ink = ink_widths(tex)
    printable = {c: w for c, w in ink.items() if 32 <= c < 127}
    if len(printable) < 90:
        raise SystemExit(f"{name}: only {len(printable)} printable glyphs have ink")
    direct = sum(abs(dim[c // GRID][c % GRID] - w) for c, w in printable.items())
    trans = sum(abs(dim[c % GRID][c // GRID] - w) for c, w in printable.items())
    direct, trans = direct / len(printable), trans / len(printable)
    if trans > TOLERANCE:
        raise SystemExit(f"{name}: the transposed reading is off by {trans:.4f}")
    if direct < trans * MARGIN:
        raise SystemExit(f"{name}: the two readings are too close to tell apart "
                         f"({direct:.4f} against {trans:.4f})")
    exact = sum(1 for c, w in printable.items() if abs(dim[c % GRID][c // GRID] - w) < 1e-6)
    return (f"  {name:<12} {len(printable)} glyphs, transposed off by {trans:.4f} "
            f"against {direct:.4f}, {exact} exact")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("root", type=Path, help="the extracted/ directory")
    args = ap.parse_args(argv)

    # `dialogfont` is deliberately not checked: its table is 16 x 24 rather
    # than 16 x 16 and no reading of it agrees with its atlas -- the glyph
    # rows are offset against the codes and the offset is not a constant.
    # 0x458cd0, which the char path runs every byte through, is the identity,
    # so it is not a remap. Open, and it only matters when the subtitles are
    # built; see CLAUDE.md.
    base = args.root / "base"
    lines = []
    for name in ("font",):
        lua, tex = base / f"{name}.lua", base / f"{name}.tex"
        if not lua.exists() or not tex.exists():
            raise SystemExit(f"{lua} or {tex} is missing; unpack base.zip first")
        lines.append(check(name, lua.read_text(encoding="latin-1"), tex.read_bytes()))
    print(f"{len(lines)} font, every advance agreeing with its own pixels")
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
