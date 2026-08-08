#!/usr/bin/env python3
"""
bsp.py -- read .bsp files: the collision / space-partition trees.

Despite the extension these hold no geometry. A `.bsp` is a flat array of
24-byte BSP nodes and nothing else — no header, no signature, no counts, so
the node count is simply `filesize / 24`. The renderable geometry of a level
lives in the big `.mod` files (`ml7z_castle.mod`, `r1_castle.mod`, ...); this
is the partition the engine uses for collision and visibility, matching the
debug paths `E:\\mdk2\\Omen\\omPolyhedron.c` and `omCollision.c`.

    struct node {          // 24 bytes
        float normal[3];   // unit plane normal
        float dist;        // plane distance from the origin
        u32   front;       // child index, 0xFFFFFFFF for a leaf
        u32   back;        // child index, 0xFFFFFFFF for a leaf
    };

Node 0 is the root, and a point is **inside** when it lies in front of every
plane down a chain -- that is, when the descent reaches a leaf through the
*front* child. `l6r8_Stack5.bsp` shows this with nothing to interpret: seven
planes forming one oriented 4.5-cube, every `back` a leaf, `front` running
0 -> 1 -> ... -> 6. It is a crate from a stack, and it is a crate.

**The query point must be negated.** The same file settles it: its box is
centred on (0.73, 219.73, -2.25) while `l6r8_Stack5.mod` is centred on
(-0.73, -219.73, 2.25) -- the exact opposite. So the tree is authored in a
mirrored frame, and `contains()` negates before descending. Testing points
either side of a face then separates them 48/48 on that crate and 799/800 on
`l3_maze`; without the negation it is 0/800, every point landing on the same
side.

Verified across the whole corpus, to the project's 100% rule:

  * all 692 files are an exact multiple of 24 bytes;
  * all 64387 normals are unit vectors to within 1e-3 — which is what
    identified the format in the first place;
  * all 692 trees are structurally sound: every child index in range, every
    node referenced exactly once, exactly one unreferenced node (the root).

Usage:
    python3 tools/bsp.py extracted/base/c10.bsp
    python3 tools/bsp.py extracted/base --validate
"""

from __future__ import annotations

import argparse
import math
import struct
import sys
from pathlib import Path

NODE_SIZE = 24
LEAF = 0xFFFFFFFF


class BspError(ValueError):
    pass


def parse(data: bytes) -> list[tuple[tuple[float, float, float], float,
                                     int, int]]:
    if not data or len(data) % NODE_SIZE:
        raise BspError(f"{len(data)} bytes is not a multiple of {NODE_SIZE}")
    out = []
    for i in range(len(data) // NODE_SIZE):
        nx, ny, nz, dist, front, back = struct.unpack_from(
            "<4f2I", data, i * NODE_SIZE)
        out.append(((nx, ny, nz), dist, front, back))
    return out


def validate(nodes: list) -> None:
    """Raise unless the array really is a well-formed tree of unit planes."""
    n = len(nodes)
    refs = [0] * n
    for i, (normal, _dist, front, back) in enumerate(nodes):
        length = math.sqrt(sum(c * c for c in normal))
        if abs(length - 1.0) > 1e-3:
            raise BspError(f"node {i}: normal length {length:.5f}")
        for child in (front, back):
            if child == LEAF:
                continue
            if child >= n:
                raise BspError(f"node {i}: child {child} out of range")
            refs[child] += 1
    if any(r > 1 for r in refs):
        raise BspError("a node is referenced more than once")
    roots = [i for i, r in enumerate(refs) if r == 0]
    if roots != [0]:
        raise BspError(f"expected exactly node 0 unreferenced, got {roots}")


def contains(nodes: list, point) -> bool:
    """Is the point inside solid geometry? Point is in *model* coordinates."""
    x, y, z = -point[0], -point[1], -point[2]
    i = 0
    while True:
        (nx, ny, nz), dist, front, back = nodes[i]
        side = nx * x + ny * y + nz * z - dist
        child = front if side >= 0 else back
        if child == LEAF:
            return side >= 0
        i = child


def crosses(nodes: list, a, b) -> bool:
    """Does the segment a->b pass through solid geometry? Model coordinates.

    The same descent as `contains`, split at the plane crossings instead of
    following one side: when the two ends fall on opposite sides of a node's
    plane the segment is cut there and both halves are tested. That makes it
    *exact for the tree* rather than a sampling of it -- a wall thinner than
    any step size still stops it.

    The leaf convention is `contains`'s: reaching a leaf on the front side
    (`side >= 0`) is solid, on the back side it is empty.

    What the original does at 0x471dc0 is not this -- it is a query against
    the engine's own world structure, reached from `mdkAILineOfSight`
    (0x402950) after the field-of-view test. The trees are the same trees, so
    this answers the same question; whether it answers it identically in every
    corner is not claimed.

    Iterative rather than recursive, like `contains`: the deepest tree in the
    game is `l4_r9.bsp` at 107, so recursion would in fact be safe, but a
    split pushes *two* halves and the branching is what would grow.
    """
    stack = [(0, tuple(a), tuple(b))]
    while stack:
        i, p, q = stack.pop()
        (nx, ny, nz), dist, front, back = nodes[i]
        dp = nx * -p[0] + ny * -p[1] + nz * -p[2] - dist
        dq = nx * -q[0] + ny * -q[1] + nz * -q[2] - dist
        if dp >= 0 and dq >= 0:
            if front == LEAF:
                return True
            stack.append((front, p, q))
        elif dp < 0 and dq < 0:
            if back != LEAF:
                stack.append((back, p, q))
        else:
            # the split point, on the plane
            t = dp / (dp - dq)
            m = tuple(p[c] + (q[c] - p[c]) * t for c in range(3))
            # whichever half lies on the front side is the one that can be
            # solid, and it is the first half when p is in front
            fp, fq = (p, m) if dp >= 0 else (m, q)
            bp, bq = (m, q) if dp >= 0 else (p, m)
            if front == LEAF:
                return True
            stack.append((front, fp, fq))
            if back != LEAF:
                stack.append((back, bp, bq))
    return False


def drop(nodes: list, point, floor: float, step: float = 0.25):
    """Lower the point along -Z until it enters solid. -> z of contact, or None.

    A crude but honest demonstration that the tree can be collided against:
    no sweeping, no radius, just a downward march.
    """
    x, y, z = point
    if contains(nodes, (x, y, z)):
        return None                      # started inside
    while z > floor:
        z -= step
        if contains(nodes, (x, y, z)):
            return z + step
    return None


def depth(nodes: list) -> int:
    """Deepest path from the root, iteratively — these trees get deep."""
    best, stack = 0, [(0, 1)]
    while stack:
        i, d = stack.pop()
        best = max(best, d)
        _n, _dist, front, back = nodes[i]
        for child in (front, back):
            if child != LEAF:
                stack.append((child, d + 1))
    return best


def probe_points(nodes: list) -> list[tuple[float, float, float]]:
    """Query points derived from the tree itself, so no other file is needed
    and two implementations can be asked exactly the same thing.

    The first are the feet of the planes, negated -- `contains` negates again,
    so each lands **exactly on** a plane, which is where two implementations
    would differ if either got the `>= 0` boundary wrong. The rest are a 4x4x4
    grid over the box those feet span. `engine/src/main.rs` builds the same
    list, in the same order.
    """
    out = [tuple(-normal[c] * dist for c in range(3))
           for normal, dist, _f, _b in nodes[:256]]
    lo = [min(p[c] for p in out) for c in range(3)]
    hi = [max(p[c] for p in out) for c in range(3)]
    for a in range(4):
        for b in range(4):
            for c in range(4):
                t = (a, b, c)
                out.append(tuple(lo[k] + (hi[k] - lo[k]) * t[k] / 3.0
                                 for k in range(3)))
    return out


def digest(files: list[Path]) -> int:
    """`name nodes depth inside crc32` a line, which is what the engine
    prints for `--bsp`."""
    import zlib
    for f in sorted(files, key=lambda p: p.name.lower()):
        nodes = parse(f.read_bytes())
        validate(nodes)
        points = probe_points(nodes)
        answers = bytes(contains(nodes, p) for p in points)
        # and the same points taken in consecutive pairs as segments, which
        # is what `crosses` is asked -- a line that starts on one plane and
        # ends on another crosses every boundary the tree has
        blocked = bytes(crosses(nodes, points[i], points[i + 1])
                        for i in range(len(points) - 1))
        print(f"{f.name.lower()} {len(nodes)} {depth(nodes)} "
              f"{sum(answers)} {zlib.crc32(answers):08x} "
              f"{sum(blocked)} {zlib.crc32(blocked):08x}")
    print(f"{len(files)} trees", file=sys.stderr)
    return 0


def compare(files: list[Path], gamedir: str) -> int:
    """Run the engine over `gamedir` and require the same answers.

    Not just the same parse: the `inside` answers come from points that lie
    exactly on the planes, so the `>= 0` boundary -- the one thing in this
    format that can be quietly wrong -- is what is being compared. The second
    pair of numbers is `crosses` over the same points taken as consecutive
    segments, which walks the split path rather than one side of it.
    """
    import subprocess
    import zlib
    root = Path(__file__).resolve().parent.parent
    out = subprocess.run(
        ["cargo", "run", "--quiet", "--release", "--manifest-path",
         str(root / "engine/Cargo.toml"), "--", gamedir, "--bsp"],
        capture_output=True, text=True, check=True).stdout
    theirs = {}
    for line in out.splitlines():
        f = line.split()
        if len(f) == 7:
            theirs[f[0]] = (int(f[1]), int(f[2]), int(f[3]), f[4],
                            int(f[5]), f[6])

    bad = []
    for f in files:
        name = f.name.lower()
        nodes = parse(f.read_bytes())
        validate(nodes)
        points = probe_points(nodes)
        answers = bytes(contains(nodes, p) for p in points)
        blocked = bytes(crosses(nodes, points[i], points[i + 1])
                        for i in range(len(points) - 1))
        mine = (len(nodes), depth(nodes), sum(answers),
                f"{zlib.crc32(answers):08x}",
                sum(blocked), f"{zlib.crc32(blocked):08x}")
        if theirs.get(name) != mine:
            bad.append(f"{name}: {mine} against the engine's "
                       f"{theirs.get(name)}")
    for name in sorted(set(theirs) - {f.name.lower() for f in files}):
        bad.append(f"{name}: the engine read it and this did not")
    for line in bad[:20]:
        print(f"MISMATCH {line}", file=sys.stderr)
    print(f"{len(files)} trees, {len(bad)} disagree", file=sys.stderr)
    return 1 if bad else 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path, help="a .bsp file or a directory")
    ap.add_argument("--validate", action="store_true",
                    help="check every file and report totals")
    ap.add_argument("--point", nargs=3, type=float, metavar=("X", "Y", "Z"),
                    help="report whether this point is inside solid geometry")
    ap.add_argument("--digest", action="store_true",
                    help="one line per tree, for --engine to diff")
    ap.add_argument("--engine", metavar="GAMEDIR",
                    help="run the engine over this installation and require "
                         "it to answer identically")
    args = ap.parse_args(argv)

    files = (sorted(args.src.rglob("*.bsp")) if args.src.is_dir()
             else [args.src])
    if not files:
        ap.error(f"no .bsp files in {args.src}")

    if args.digest:
        return digest(files)

    if args.engine:
        return compare(files, args.engine)

    if args.point:
        nodes = parse(files[0].read_bytes())
        validate(nodes)
        print(f"{files[0].name}: {tuple(args.point)} inside = "
              f"{contains(nodes, args.point)}")
        return 0

    total = leaves = 0
    bad = []
    for f in files:
        try:
            nodes = parse(f.read_bytes())
            validate(nodes)
        except BspError as e:
            bad.append((f.name, str(e)))
            continue
        total += len(nodes)
        leaves += sum(1 for _n, _d, a, b in nodes
                      for c in (a, b) if c == LEAF)
        if not args.validate:
            print(f"{f.name}: {len(nodes)} nodes, depth {depth(nodes)}, "
                  f"{sum(1 for _n, _d, a, b in nodes for c in (a, b) if c == LEAF)}"
                  " leaf links")
    for name, err in bad:
        print(f"ERROR {name}: {err}", file=sys.stderr)
    print(f"{len(files) - len(bad)}/{len(files)} trees valid, "
          f"{total} nodes, {leaves} leaf links", file=sys.stderr)
    return 1 if bad else 0


if __name__ == "__main__":
    raise SystemExit(main())
