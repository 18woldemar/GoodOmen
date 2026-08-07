#!/usr/bin/env python3
"""
stars.py -- read `.sta`: the sky. It is a real star catalogue, with a real bug.

`base/stars.sta` is the only file of its type, and it is as plain as a format
gets:

    0x00  u32   2004        resource type tag, after .tex 2001, .mod 2002,
                            .str 2003
    0x04  u32   1           version
    0x08  ...             3141 records of { float ra; float dec; float mag }

Right ascension and declination in **radians**, visual magnitude, sorted
brightest first. 37700 = 8 + 3141 x 12 exactly, with nothing over.

It decodes because the sky is checkable. Record 0 is RA 6.752 h, Dec -15.28,
magnitude -1.46: that is **Sirius**, whose magnitude is -1.46 and whose right
ascension is 6.752 h. Records 1 to 5 are Canopus, Arcturus, Alpha Centauri,
Vega and Capella, each with the right RA and the right magnitude.

**The southern declinations are wrong, in the shipped game.** Arcturus reads
+19.183 and is +19.183; Vega reads +38.784 and is +38.784. But Sirius reads
-15.284 where the true value is -16.716, Canopus -51.304 against -52.696,
Alpha Centauri -59.165 against -60.834. Every one of those three is the true
value with the arcminutes *added* instead of subtracted:

    Sirius     -16 deg 43 min  ->  -16 + 43/60  =  -15.283
    Canopus    -52 deg 42 min  ->  -52 + 42/60  =  -51.300
    Alpha Cen  -60 deg 50 min  ->  -60 + 50/60  =  -59.167

Whoever imported the catalogue applied the sign to the degrees and not to the
minutes, so every southern star sits up to one degree too far north. The
corpus agrees: under that arithmetic a true declination in [-1, 0) comes out
in [0, +1), so that band should hold twice its share of stars -- and it does,
56 against a neighbouring average of 28.

`GoodOmen` reproduces the sky as shipped by default, because that is what the
game looks like. `--fix` applies the correction, for comparison.

Usage:
    python3 tools/stars.py extracted/base/stars.sta
    python3 tools/stars.py extracted/base/stars.sta --brightest 20
    python3 tools/stars.py extracted/base/stars.sta --plot sky.png
"""

from __future__ import annotations

import argparse
import math
import struct
import sys
from pathlib import Path

TYPE_STA = 2004
HEADER = 8
RECORD = 12


class StaError(ValueError):
    pass


def parse(data: bytes) -> list[tuple[float, float, float]]:
    if len(data) < HEADER:
        raise StaError("file shorter than the header")
    tag, version = struct.unpack_from("<2I", data, 0)
    if tag != TYPE_STA:
        raise StaError(f"type tag {tag}, expected {TYPE_STA}")
    if version != 1:
        raise StaError(f"version {version}, expected 1")
    body = len(data) - HEADER
    if body % RECORD:
        raise StaError(f"{body} bytes is not a multiple of {RECORD}")
    stars = [struct.unpack_from("<3f", data, HEADER + i * RECORD)
             for i in range(body // RECORD)]
    for ra, dec, _mag in stars:
        if not 0 <= ra <= 2 * math.pi or abs(dec) > math.pi / 2:
            raise StaError(f"({ra}, {dec}) is not a direction on the sphere")
    return stars


def fix(star: tuple[float, float, float]) -> tuple[float, float, float]:
    """Undo the importer's sign bug: -D + m/60 was meant to be -(D + m/60).

    Stored is `-D + f` for whole degrees D and a fraction f, so D = -floor,
    f = stored + D, and the true value is -(D + f) = -2D - stored.

    It cannot recover everything. A star truly at -0 deg 30 min was stored as
    +0.5 and is now indistinguishable from one truly at +0 deg 30 min, which
    is exactly why the [0, +1) band holds twice its share. Those stay wrong.
    """
    ra, dec, mag = star
    if dec >= 0:
        return star
    deg = math.degrees(dec)
    whole = -math.floor(deg)                # -15.284 -> 16
    return ra, math.radians(-2 * whole - deg), mag


def direction(star, radius: float = 1.0):
    """-> a point on the sky sphere, z up, as the engine's frame has it."""
    ra, dec, _mag = star
    return (radius * math.cos(dec) * math.cos(ra),
            radius * math.cos(dec) * math.sin(ra),
            radius * math.sin(dec))


def plot(stars: list, path: Path) -> None:
    """An equirectangular sky, brighter stars drawn larger."""
    from PIL import Image, ImageDraw
    w, h = 1440, 720
    img = Image.new("RGB", (w, h), (6, 7, 11))
    d = ImageDraw.Draw(img)
    for ra, dec, mag in stars:
        x = w - ra / (2 * math.pi) * w          # RA increases to the left
        y = h / 2 - dec / math.pi * h
        r = max(0.5, (6.5 - mag) * 0.42)
        v = int(min(255, 90 + (6.5 - mag) * 26))
        d.ellipse((x - r, y - r, x + r, y + r), fill=(v, v, min(255, v + 12)))
    img.save(path)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path)
    ap.add_argument("--brightest", type=int, default=8,
                    help="how many to list")
    ap.add_argument("--fix", action="store_true",
                    help="correct the southern declinations")
    ap.add_argument("--plot", type=Path, help="write an all-sky map")
    args = ap.parse_args(argv)

    stars = parse(args.src.read_bytes())
    if args.fix:
        stars = [fix(s) for s in stars]

    if args.plot:
        plot(stars, args.plot)
        print(f"{args.plot}: {len(stars)} stars", file=sys.stderr)
        return 0

    for ra, dec, mag in stars[:args.brightest]:
        print(f"RA {ra / math.pi * 12:7.3f} h   "
              f"Dec {math.degrees(dec):+8.3f}   mag {mag:+.2f}")
    south = sum(1 for _r, d, _m in stars if d < 0)
    print(f"{len(stars)} stars, {south} south of the equator, "
          f"magnitude {min(m for _r, _d, m in stars):+.2f} to "
          f"{max(m for _r, _d, m in stars):+.2f}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
