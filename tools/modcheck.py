#!/usr/bin/env python3
"""
modcheck.py -- hold the engine's .mod reader to the Python one that defined it.

`tools/mod2obj.py` is the reference. The engine prints eleven numbers per
model (`goodomen --mod`) and this recomputes them here and compares:

    name  nodes groups vertices refs animations triangles
          sum(x) sum(y) sum(z)  sum(quaternions) sum(offsets)

Sums rather than checksums, because the arithmetic crosses a slerp and two
implementations of `acos` need not agree in the last bit. They are taken over
every vertex and every node, so the errors that matter -- a hierarchy not
walked, a quaternion read (x,y,z,w), a strip wound the wrong way, a static
model wrongly offset -- move them by orders of magnitude, not by ulps.

The quaternion and offset sums come from **animation 0 at t = 0.5**: t = 0
would sample the first key of every channel and never exercise the
interpolation, which is the part most likely to differ.

Usage:
    python3 tools/modcheck.py extracted engine-output.txt
    python3 tools/modcheck.py extracted --run "$MDK2_GOG"   # runs the engine
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import mod2obj  # noqa: E402

# the engine prints %.6f, so anything below that is rounding rather than
# disagreement; the relative term carries the large sums
ABS_TOL = 1e-4
REL_TOL = 1e-9


def measure(path: Path) -> list:
    m = mod2obj.Model(path.read_bytes())
    verts, tris = m.posed()
    p = [sum(v[0][c] for v in verts) for c in range(3)]
    anims = m.animations()
    q = o = 0.0
    if anims:
        for quat, off in m.node_world(anims[0], 0.5):
            q += sum(quat)
            o += sum(off)
    return [len(m.nodes), len(m.groups), len(m.vertices), len(m.refs),
            len(anims), len(tris), p[0], p[1], p[2], q, o]


def close(a: float, b: float) -> bool:
    # NaN on both sides is agreement, not a disagreement that cannot be
    # spelled: six models carry an uninitialised translation on their root
    # node and it propagates -- see docs/journal.md, 2026-08-07.
    if a != a and b != b:
        return True
    return abs(a - b) <= max(ABS_TOL, REL_TOL * max(abs(a), abs(b)))


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path, help="the extracted resources")
    ap.add_argument("engine", type=Path, nargs="?",
                    help="a file of `goodomen --mod` output")
    ap.add_argument("--run", metavar="GAMEDIR",
                    help="run the engine over this installation instead")
    ap.add_argument("--limit", type=int, default=0)
    args = ap.parse_args(argv)

    if args.run:
        out = subprocess.run(
            ["cargo", "run", "--quiet", "--release", "--manifest-path",
             str(Path(__file__).resolve().parent.parent / "engine/Cargo.toml"),
             "--", args.run, "--mod"],
            capture_output=True, text=True, check=True).stdout
    elif args.engine:
        out = args.engine.read_text()
    else:
        ap.error("need a file of engine output, or --run GAMEDIR")

    theirs = {}
    for line in out.splitlines():
        f = line.split()
        if len(f) == 12:
            theirs[f[0]] = [int(v) for v in f[1:7]] + [float(v) for v in f[7:]]

    files = sorted(args.src.rglob("*.mod"), key=lambda p: p.name.lower())
    if args.limit:
        files = files[:args.limit]

    bad, nan = [], []
    for f in files:
        name = f.name.lower()
        want = theirs.get(name)
        if want is None:
            bad.append(f"{name}: the engine did not read it")
            continue
        got = measure(f)
        if any(v != v for v in got[6:]):
            nan.append(name)
        for k, (a, b) in enumerate(zip(got, want)):
            if (a != b) if k < 6 else not close(a, b):
                bad.append(f"{name}: field {k}, {a} against the engine's {b}")
                break

    if not args.limit:
        missing = set(theirs) - {f.name.lower() for f in files}
        for name in sorted(missing):
            bad.append(f"{name}: the engine read it and this did not")

    for line in bad[:20]:
        print(f"MISMATCH {line}", file=sys.stderr)
    print(f"{len(files)} models, {len(bad)} disagree"
          + (f", {len(nan)} pose to NaN on both sides ({', '.join(nan)})"
             if nan else ""),
          file=sys.stderr)
    return 1 if bad else 0


if __name__ == "__main__":
    raise SystemExit(main())
