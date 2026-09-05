#!/usr/bin/env python3
"""
spawncheck.py -- can the player stand where each checkpoint puts him?

Two questions over the game's 129 checkpoints, both asked against the same
collision the engine uses:

  * is he **inside geometry** the moment he appears? Exactly one is, and it
    is level 7's third, inside the spire `c9`.
  * is there **any floor beneath him**? Twenty-four have none within 400
    units, and a run of those checkpoints is a two-minute fall.

Neither number is an engine defect and the second one was suspected of being
one. The original culls a collision query by the model's **per-node** boxes
(0x471930 tests `node + 0x1c` against `node + 0x28` before anything else) and
`Collision::load` bounds each tree by the union of exactly those, so the
engine's bound is never the tighter of the two. The 24 have no floor in the
original either: they are respawn points, not starts.

What this pins is the *fact*, so that it is noticed if it moves.

Usage:
    python3 spawncheck.py extracted [--expect-floorless 24] [--expect-inside 1]
"""

from __future__ import annotations

import argparse
import importlib.util
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent


def _load(name: str):
    spec = importlib.util.spec_from_file_location(name, HERE / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("resources", type=Path)
    ap.add_argument("--expect-floorless", type=int, default=None)
    ap.add_argument("--expect-inside", type=int, default=None)
    ap.add_argument("--deep", type=int, default=400,
                    help="how far under a checkpoint to look for a floor")
    args = ap.parse_args()

    sys.path.insert(0, str(HERE))
    walksim = _load("walksim")
    spawn = _load("spawn")

    floorless, inside, total = [], [], 0
    for n in range(1, 11):
        graph = args.resources / "base" / f"l{n}.lua"
        if not graph.is_file():
            continue
        world = walksim.World(graph, args.resources)
        _, points = spawn.checkpoints(args.resources, n)
        for cp in points:
            x, y, z = cp["position"]
            total += 1
            # the body's position is its **head**; a checkpoint's is its feet
            if world.blocked(x, y, z + walksim.EYE):
                inside.append(f"l{n} cp{cp['index']}")
            if not any(world.solid(x, y, z - d) for d in range(args.deep)):
                floorless.append(f"l{n} cp{cp['index']}")

    print(f"{total} checkpoints: {len(inside)} start inside geometry, "
          f"{len(floorless)} have no floor within {args.deep}")
    print(f"  inside:    {', '.join(inside) or 'none'}")
    print(f"  floorless: {', '.join(floorless) or 'none'}")
    for got, want, what in (
        (len(floorless), args.expect_floorless, "floorless"),
        (len(inside), args.expect_inside, "inside geometry"),
    ):
        if want is not None and got != want:
            print(f"{got} {what}, expected {want}", file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
