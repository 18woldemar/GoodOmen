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

Node 0 is the root. Verified across the whole corpus, to the project's 100%
rule:

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


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path, help="a .bsp file or a directory")
    ap.add_argument("--validate", action="store_true",
                    help="check every file and report totals")
    args = ap.parse_args(argv)

    files = sorted(args.src.glob("*.bsp")) if args.src.is_dir() else [args.src]
    if not files:
        ap.error(f"no .bsp files in {args.src}")

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
