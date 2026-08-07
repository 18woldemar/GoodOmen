#!/usr/bin/env python3
"""
walksim.py -- run the viewer's character controller without a browser.

`tools/mod2html.py --walk` puts a body in the level and lets you walk it. This
is the same controller in Python, against the same `.bsp` trees, so it can be
run over every level and counted instead of being played and eyeballed. It is
how the controller's three real bugs were found: a standing body that thought
it was obstructed and stepped two units into the air every frame, a fall that
moved a whole frame at once and passed through thin floors, and a run that
stepped over thin walls.

The measure is deliberately blunt. Drop a body on a surface, point it in a
random direction, hold forwards for two seconds and ask three things:

  * was it **ever inside geometry**? That must be zero. A body that walks
    through the world is the one failure that is not a matter of degree.
  * is it **still standing** at the end? Not all of them will be, and should
    not be: walking forwards with your eyes shut off a ledge is a fall, and
    MDK2's levels are largely shafts.
  * did it **meet a wall**? Some must, or the collision is not being consulted.

The body is sized from the game and not guessed: `kurt.mod` is 1.86 units from
sole to scalp, so a unit is about a metre, the eye sits at 1.7 and a step is
0.6. See `tools/spawn.py` for the rest of that argument.

Over all ten levels, 2557 spawn points, two seconds of held-forwards each:

    standing   2556 of 2557 still standing,    2 ever inside,    0 met a wall
    walking    1897 of 2557,                  24 ever inside,  305 met a wall
    running    1431 of 2557,                  30 ever inside,  562 met a wall

Standing is as near exact as it gets. The bodies that stop standing walk off
ledges, and the ones that end up inside geometry are wedged under overhangs
rather than passing through the world -- the body is a vertical segment with
no head clearance, so falling into a gap drives its head into the slab above.
A capsule sweep is the fix, and it is engine work.

Usage:
    python3 tools/walksim.py extracted/base/l1.lua --resources extracted
    python3 tools/walksim.py extracted/base --resources extracted --all
"""

from __future__ import annotations

import argparse
import base64
import math
import random
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import mod2html as mh  # noqa: E402

# the same constants the page uses; keep them in step
EYE, STEP, GRAVITY = 1.7, 0.6, 20.0
WALK, SPRINT = 4.0, 9.0
DT = 1 / 60


class World:
    def __init__(self, graph: Path, resources: Path) -> None:
        packed = mh.scene_collision(graph, resources)
        raw = base64.b64decode(packed["data"]) if packed else b""
        self.nodes = [struct.unpack_from("<4f2I", raw, i * 24)
                      for i in range(len(raw) // 24)]
        self.trees = packed["trees"] if packed else []

    def solid(self, x, y, z) -> bool:
        qx, qy, qz = -x, -y, -z
        for t in self.trees:
            b = t["box"]
            if not (b[0] <= x <= b[3] and b[1] <= y <= b[4]
                    and b[2] <= z <= b[5]):
                continue
            i = t["first"]
            while True:
                nx, ny, nz, dist, front, back = self.nodes[i]
                side = nx * qx + ny * qy + nz * qz - dist
                child = front if side >= 0 else back
                if child == 0xFFFFFFFF:
                    if side >= 0:
                        return True
                    break
                i = t["first"] + child
        return False

    def blocked(self, x, y, z) -> bool:
        """Only the body above step height stops it; below is a kerb."""
        h = STEP
        while h <= EYE + 1e-9:
            if self.solid(x, y, z - EYE + h):
                return True
            h += (EYE - STEP) / 2
        return False

    def footed(self, x, y, z) -> bool:
        return self.solid(x, y, z - EYE + 0.05)


def walk(w: World, start, yaw: float, frames: int, speed: float) -> dict:
    pos = list(start)
    vz, ground, hits, inside = 0.0, False, 0, 0
    fwd = (math.cos(yaw), math.sin(yaw))
    run = speed * DT
    for _ in range(frames):
        if run > 0:
            dx, dy = fwd[0] * run, fwd[1] * run
            if w.blocked(pos[0] + dx, pos[1] + dy, pos[2]):
                hits += 1
            pieces = max(1, math.ceil(run / 0.25))
            for _k in range(pieces):
                for ax, ay in ((dx, dy), (dx, 0), (0, dy)):
                    nx = pos[0] + ax / pieces
                    ny = pos[1] + ay / pieces
                    if not w.blocked(nx, ny, pos[2]):
                        pos[0], pos[1] = nx, ny
                        break
        vz -= GRAVITY * DT
        nz, left = pos[2], vz * DT
        while abs(left) > 1e-6 and not w.footed(pos[0], pos[1], nz):
            bit = max(-0.5, min(0.5, left))
            nz += bit
            left -= bit
        if w.footed(pos[0], pos[1], nz):
            lift = 0.0
            while lift < EYE and w.footed(pos[0], pos[1], nz + lift):
                lift += 0.05
            nz += lift
            ground, vz = True, 0.0
        elif ground and vz < 0:
            drop = 0.0
            while drop < STEP and not w.footed(pos[0], pos[1], nz - drop):
                drop += 0.05
            if drop < STEP:
                nz -= drop - 0.05
                vz = 0.0
            else:
                ground = False
        else:
            ground = False
        pos[2] = nz
        if w.blocked(pos[0], pos[1], pos[2]):
            inside += 1
    return {"end": pos, "ground": ground, "hits": hits, "inside": inside}


def surfaces(w: World, rng: random.Random, per_tree: int = 6) -> list:
    """Spawn points: march down each tree's box until something is solid."""
    out = []
    for t in w.trees:
        b = t["box"]
        for _ in range(per_tree):
            x = rng.uniform(b[0], b[3])
            y = rng.uniform(b[1], b[4])
            z = b[5]
            while z > b[2]:
                if w.solid(x, y, z):
                    if not w.blocked(x, y, z + EYE + 0.2):
                        out.append((x, y, z + EYE + 0.2))
                    break
                z -= 0.5
    return out


def survey(graph: Path, resources: Path, seed: int = 7,
           frames: int = 120) -> dict:
    w = World(graph, resources)
    rng = random.Random(seed)
    starts = surfaces(w, rng)
    rows = {}
    for speed, label in ((0.0, "standing"), (WALK, "walking"),
                         (SPRINT, "running")):
        r = [walk(w, s, rng.uniform(0, 2 * math.pi), frames, speed)
             for s in starts]
        rows[label] = {
            "standing": sum(1 for x in r if x["ground"]),
            "inside": sum(1 for x in r if x["inside"]),
            "walls": sum(1 for x in r if x["hits"]),
        }
    return {"trees": len(w.trees), "nodes": len(w.nodes),
            "starts": len(starts), "rows": rows}


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path, help="a scene .lua, or a directory")
    ap.add_argument("--resources", type=Path, default=Path("extracted"))
    ap.add_argument("--all", action="store_true",
                    help="every numbered level in the directory")
    ap.add_argument("--frames", type=int, default=120)
    ap.add_argument("--expect-standing", type=int, metavar="N",
                    help="succeed only if exactly N bodies are still standing "
                         "after the standing-still pass; pins the survey")
    args = ap.parse_args(argv)

    if args.all:
        files = [p for n in range(1, 11)
                 if (p := args.src / f"l{n}.lua").is_file()]
    else:
        files = [args.src]

    bad = upright = 0
    for f in files:
        s = survey(f, args.resources, frames=args.frames)
        if not s["starts"]:
            print(f"{f.stem}: no collision", file=sys.stderr)
            continue
        r = s["rows"]
        print(f"{f.stem:6s} {s['trees']:3d} trees {s['nodes']:7d} nodes "
              f"{s['starts']:4d} starts | " + " | ".join(
                  f"{k} {v['standing']:3d} up {v['inside']:2d} in "
                  f"{v['walls']:3d} wall" for k, v in r.items()))
        bad += sum(v["inside"] for v in r.values())
        upright += r["standing"]["standing"]
    print(f"{len(files)} levels, {upright} standing bodies stay up, "
          f"{bad} runs ever inside geometry", file=sys.stderr)
    if args.expect_standing is not None:
        return 0 if upright == args.expect_standing else 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
