#!/usr/bin/env python3
"""
spawn.py -- the checkpoints a level starts you at, and whether they hold you up.

A level script carries its own restart points:

    Level["scenegraph"]["checkpoints"][1] = {"Room 1", {0, 124, 0.2},
                                             "sectionA", 0}

-- a label, a position, the section that has to be loaded, and a facing in
radians. `tools/luarun.py` runs the script for real and hands the table over,
so these are the game's own numbers rather than a re-parse of the text.

That makes them a test of everything under M8 at once. A checkpoint is where
the engine puts the player, so it has to be **in open space, with a floor
under it** -- and the floor comes from the `.bsp` trees of the objects the
scene graph places, which come from the scene graph the same script loads. If
any one of those readings were wrong the checkpoints would hang in the air or
sit inside a wall.

They very nearly do neither: of the 129 checkpoints across the 10 numbered
levels, **128 are in open space** and 109 have a floor under them. (129, not
127: the game's `override/` directory is a shipped patch and its `level1.lua`
adds two more. Anything that reads only the archives is reading the game as it
was before the patch -- see `override_dir()`.) The
exceptions are all explicable. `l7 cp3` is the one genuinely inside geometry.
The 19 with no floor are cut-scene camera positions parked outside the world
(`Intro Movie A` at y = -1000, `10-10 End Comic` at 10000, 10000, 10000),
rooms with no `.bsp` at all -- level 1's room 3 is the shaft you skydive
down, and it has no collision because there is nothing to stand on -- and
`zizzyroom`, whose arena is a separate scene graph loaded when that checkpoint
is reached.

**These are also what sized the player.** A body 4 units tall did not fit five
checkpoints; the smallest headroom over all of them is 2.9 units. Kurt's model is
1.86 units from sole to scalp and Max's 1.72, so a unit is about a metre and
`mod2html`'s controller now stands 1.7 -- at which every checkpoint but that
one fits.

Usage:
    python3 tools/spawn.py extracted --level 1
    python3 tools/spawn.py extracted --all
"""

from __future__ import annotations

import argparse
import base64
import json
import struct
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import mod2html as mh  # noqa: E402

# what the engine has already answered by the time a level script runs
BOOT = ["--boot", "mdk2",
        "--answer", "chGetGameWasReset=0",
        "--answer", "mdkLoadLevelIsInstant=1",
        "--set", "checkpoint=1",
        "--set", 'section="sectionA"']

EYE = 1.7          # the body in mod2html's controller, so the same numbers
STEP = 0.25        # how finely to march down looking for the floor
REACH = 200.0      # how far down to look before calling it a shaft


def override_dir() -> Path | None:
    """The game's `override/`, which the engine reads before the archives.

    It is a shipped patch, not a leftover: `level1.lua` there differs from the
    packed copy by 60 lines, adding two checkpoints and the encounter behind
    one of them. Anything that reads a script and ignores it is reading the
    game as it was before the patch.
    """
    import os
    root = os.environ.get("MDK2_GOG")
    if not root:
        local = Path(__file__).resolve().parent.parent / ".env.local"
        if local.is_file():
            import re
            m = re.search(r'MDK2_GOG\s*=\s*"?([^"\n#]+)', local.read_text())
            root = m.group(1).strip() if m else None
    if not root:
        return None
    d = Path(root) / "override"
    return d if d.is_dir() else None


def _lua(root: Path, script: Path, dump: str) -> object:
    cmd = [sys.executable, str(Path(__file__).resolve().parent / "luarun.py"),
           str(script), "--tree", str(root), *BOOT, "--dump", dump]
    over = override_dir()
    if over:
        cmd += ["--override", str(over)]
    p = subprocess.run(cmd, capture_output=True, text=True)
    if p.returncode:
        raise RuntimeError(p.stderr.strip().splitlines()[-1])
    return json.loads(p.stdout.strip() or "null")


def checkpoints(root: Path, level: int) -> tuple[str, list[dict]]:
    """-> (scene-graph name, checkpoints) for `scripts/level<N>.lua`."""
    script = root / "scripts" / f"level{level}.lua"
    graph = _lua(root, script, "Level.file")
    table = _lua(root, script, "Level.scenegraph.checkpoints") or {}
    out = []
    for key in sorted(table, key=int):
        row = table[key]
        pos = row.get("2") or {}
        out.append({
            "index": int(key),
            "label": row.get("1", "?"),
            "position": [float(pos.get(str(i + 1), 0.0)) for i in range(3)],
            "section": row.get("3"),
            "facing": row.get("4"),
        })
    return graph, out


class Collision:
    """The scene's `.bsp` trees, queried exactly as the viewer queries them."""

    def __init__(self, graph: Path, resources: Path) -> None:
        packed = mh.scene_collision(graph, resources)
        raw = base64.b64decode(packed["data"]) if packed else b""
        self.nodes = [struct.unpack_from("<4f2I", raw, i * 24)
                      for i in range(len(raw) // 24)]
        self.trees = packed["trees"] if packed else []

    def solid(self, x: float, y: float, z: float) -> bool:
        qx, qy, qz = -x, -y, -z            # the trees are mirrored, see bsp.py
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

    def floor_under(self, x: float, y: float, z: float) -> float | None:
        for k in range(int(REACH / STEP)):
            if self.solid(x, y, z - k * STEP):
                return z - k * STEP
        return None

    def clear(self, x: float, y: float, z: float) -> bool:
        """Is a body standing here, rather than embedded in something?"""
        return not any(self.solid(x, y, z + h)
                       for h in (0.1, 0.5, 1.0, 1.4, EYE))


def waypoints(args) -> int:
    """The same body test, asked of every waypoint the scene graphs place.

    A waypoint is where the game **walks a character to**, so it is as much a
    statement about the collision reading as a checkpoint is -- and there are
    five times as many of them. The number that matters is the exceptions:
    **39 of the 625 waypoints across the ten levels sit inside a tree**, and
    they are not spread evenly. Levels 1, 6 and 10 have none at all; level 3
    has 19 and level 7 has 13, and in both the walkable surface is a separate
    object embedded in a larger mesh (`l3_piperoom`, `c9`), so a *point* query
    has nothing to prefer between them. Several of the rest are pens --
    `l2_penwp`, `l4_r2poopsypen`, `l5r3bbpen` -- which is a name for exactly
    this: a spot inside a thing.

    That is why a walker cannot be given collision as a point test. The
    original sweeps a body and resolves against the surface it actually
    touches (0x46de70), which is indifferent to being inside something else;
    a point test refuses the step and the character never moves again. This
    counts the ground that has to be made up first.
    """
    total = clear = 0
    worst: dict[str, int] = {}
    for n in range(1, 11) if args.all else [args.level or 1]:
        try:
            graph, _ = checkpoints(args.root, n)
        except Exception as e:
            print(f"level {n}: {e}", file=sys.stderr)
            continue
        path = args.root / "base" / f"{graph}.lua"
        if not path.is_file():
            continue
        points = _lua(args.root, path, "points") or {}
        coll = Collision(path, args.root)
        bad = []
        for name, p in sorted(points.items()):
            x, y, z = (float(p.get(k, 0.0)) for k in ("x", "y", "z"))
            total += 1
            if coll.clear(x, y, z):
                clear += 1
            else:
                bad.append(name)
        for name in bad:
            worst[f"l{n}"] = worst.get(f"l{n}", 0) + 1
        print(f"  l{n}: {len(points) - len(bad)} of {len(points)} clear"
              + (f"  ({', '.join(bad[:4])}{'...' if len(bad) > 4 else ''})"
                 if bad else ""))
    print(f"{total} waypoints: {clear} a body would stand clear at, "
          f"{total - clear} inside a tree", file=sys.stderr)
    if args.expect is not None:
        return 0 if clear == args.expect else 1
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("root", type=Path, help="the extraction root")
    ap.add_argument("--level", type=int, help="one level number")
    ap.add_argument("--all", action="store_true", help="every numbered level")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--expect", type=int, metavar="N",
                    help="succeed only if exactly N checkpoints are in open "
                         "space; pins the one known exception, l7 cp3")
    ap.add_argument("--waypoints", action="store_true",
                    help="ask the same question of every waypoint instead: "
                         "how many of the scene graph's own points would a "
                         "body stand clear at")
    args = ap.parse_args(argv)

    if args.waypoints:
        return waypoints(args)

    levels = range(1, 11) if args.all else [args.level or 1]
    total = clear = floored = 0
    dump = {}
    for n in levels:
        try:
            graph, points = checkpoints(args.root, n)
        except Exception as e:                    # a level with no script
            print(f"level {n}: {e}", file=sys.stderr)
            continue
        path = args.root / "base" / f"{graph}.lua"
        if not path.is_file():
            print(f"level {n}: no scene graph {graph}.lua", file=sys.stderr)
            continue
        coll = Collision(path, args.root)
        dump[n] = points
        for c in points:
            x, y, z = c["position"]
            ok = coll.clear(x, y, z)
            f = coll.floor_under(x, y, z)
            total += 1
            clear += ok
            floored += f is not None
            c["clear"] = ok
            c["drop"] = None if f is None else round(z - f, 2)
            if not args.json:
                print(f"  l{n} cp{c['index']:<3d} {c['label'][:22]:22s} "
                      f"({x:8.1f},{y:8.1f},{z:7.1f}) "
                      f"{'open' if ok else 'BLOCKED':7s} "
                      f"floor {'-' if f is None else f'{z - f:5.2f} below'}")
    if args.json:
        json.dump(dump, sys.stdout, indent=1)
        return 0
    print(f"{total} checkpoints: {clear} in open space, "
          f"{floored} with a floor within {REACH:.0f}", file=sys.stderr)
    if args.expect is not None:
        return 0 if clear == args.expect else 1
    return 0 if clear == total else 1


if __name__ == "__main__":
    raise SystemExit(main())
