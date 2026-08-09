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

    standing   2556 of 2557 still standing,    0 ever inside,    0 met a wall
    walking    1897 of 2557,                   2 ever inside,  303 met a wall
    running    1430 of 2557,                   4 ever inside,  559 met a wall

The bodies that stop standing walk off ledges, which is what walking forwards
with your eyes shut does in a game made largely of shafts.

**Inside geometry went from 56 runs to 6**, and the two fixes are both about
checking the *whole* frame rather than a piece of it:

  * the lift out of the surface you land on now stops at the ceiling. Rising
    until the feet are clear is right in the open and wrong under an overhang,
    where it drove the head into the slab -- the two level 5 spawn points that
    stood inside geometry without moving at all are in a gap 1.7 units tall,
    exactly the body's height, so there was nowhere to lift them to. Refusing
    leaves the feet slightly in the floor, which is the lesser wrong.
  * the sideways move is checked against the height the body had *before* it
    settled, so walking fast under a lowering ceiling could pass a check that
    the finished position fails. Validating the finished position and giving
    the sideways move back when it fails is a cheap stand-in for a swept
    solve.

What is left is six *transient* clips of one to ten frames in runs of a
hundred and twenty, all of them ending with the body standing in a clear
column -- brushing through a tight spot rather than stuck in it. The body is
still a vertical segment; a capsule sweep is the real fix and it is engine
work.

## Replaying the demo

`--demo` drives the same controller from `base/demo1_5.omn`, the one recorded
demo the game ships, instead of from a held key. **The demo's command ids are
DirectInput scancodes**, which `scripts/defaultkeys.lua` is the key to:
`omBindCommandI(COM_FORWARD, 200)` and DIK_UP is 0xC8. Every id in the file
checks out against the DirectInput header, and so does every id in
`defaultkeys.lua`:

    200 DIK_UP  208 DIK_DOWN  203 DIK_LEFT  205 DIK_RIGHT
     57 DIK_SPACE = COM_SMTOGGLE, the sniper mode
     13 DIK_EQUALS   28 DIK_RETURN = COM_MENUSELECT   1 DIK_ESCAPE = COM_PAUSE
    1000..1007 are not scancodes: mouse buttons and the four half-axes,
    COM_SHOOT/COM_MELEE 1000, COM_JUMP/COM_JET/COM_CHUTE 1001,
    COM_MOUSERIGHT 1004, COM_MOUSELEFT 1005, COM_MLOOKDOWN 1006, MLOOKUP 1007

A record is present in **every frame its input is held**, not just on the
edge: forward is in 552 of the 1348 frames, in nine unbroken runs. The two
runs of 1001, fifteen frames and twenty-five, are the parachute; the four
short runs of 57 are the sniper going up and down.

Replayed from level 1's checkpoint 5 -- `demo%d_%d.omn` is the game's own
format string, `mdk2.lua` builds it for both `chRecordInput` and
`mdkPlayDemo` -- the body travels **131 units over the 45 seconds, ends
standing, and is never once inside geometry**. It also never leaves the room
it started in, which sounded wrong until the room was measured: `l1_r5` is
207 x 138 x 40 units, and the demo holds the fire button on 161 frames. This
is a fight in an arena, not a walk down a corridor.

The travel is a check on the walking speed rather than an accident of it.
Forward is held on 41% of the frames, so a 4 units-a-second walk predicts
about 74 units from forward alone; strafing carries the rest to 120, and the
figure barely moves across mouse gains from 0.2 to 1.5.

What this cannot do yet is *validate*: the mouse gain -- radians per unit of
axis -- is not in the data, and neither is the original's endpoint. The one
signal it does give is the floor: below about 0.2 rad per unit the body grinds
along walls and ends up inside geometry on hundreds of frames, and at 0.2 and
above it never does. What the replay is really for is the machinery, because
the day the engine plays this file back and lands where the original did is
the day the engine is right.

Usage:
    python3 tools/walksim.py extracted/base/l1.lua --resources extracted
    python3 tools/walksim.py extracted/base --resources extracted --all
    python3 tools/walksim.py extracted/base/l1.lua --resources extracted \\
            --demo extracted/base/demo1_5.omn --checkpoint 5
"""

from __future__ import annotations

import argparse
import base64
import math
import random
import re
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import mod2html as mh  # noqa: E402

# the same constants the page uses; keep them in step
EYE, STEP, GRAVITY = 1.7, 0.6, 20.0
WALK, SPRINT = 4.0, 9.0        # test speeds; a player takes PLAYER_SPEED
DT = 1 / 60

# The mouse turn, and none of it is a gain of ours any more. Three pieces:
#
#   * the axis (0x46f110). Its raw value is the positive half's command minus
#     the negative half's -- `MOUSEX+` and `MOUSEX-`, the pair `mdk2.lua`
#     declares and the pair the demo records. If the axis's +0x20 is not 1.0
#     it is then shaped by `sign(raw) * pow(|raw|, +0x20)`, and the turn
#     axis's exponent is **0.8**: a compressive curve. Then times the gain at
#     +0x1c, and zeroed below a dead zone at +0x18, which is 0.
#   * the gain, from `mdkSetTurnSensitivity` (0x43ab60 into 0x42d050):
#     **n * n + 0.05**, the 0.05 being the float at 0x48f3c4.
#   * Kurt (0x419d1e): `axis(1) * 45.0 * 1/60`, the 45.0 being the first float
#     of the block at `gob + 0x64`.
#
# `defaultopt.lua` ships `mdkSetMouseSensitivity(0.5)`, so that is the
# default here. The curve is why no single "radians per unit" ever fitted.
SENSITIVITY = 0.5
TURN_EXPONENT = 0.8
TURN_FLOOR = 0.05
TURN_RATE = 45.0


def turn_from_axis(raw: float, sensitivity: float = SENSITIVITY) -> float:
    """The original's turn, and **not yet what the replay uses**.

    At the shipped sensitivity it turns about a quarter as fast as the flat
    factor of 1.0 the replay still carries, and replaying the demo that slowly
    ends with the body inside geometry on 30 frames where it was never once
    inside. That is the body failing, not this: it is a vertical segment 1.7
    tall with no width, and Kurt's is 2.0 by 0.8 (0x416863). The fast turn was
    hiding it by swinging away from every wall.
    """
    shaped = abs(raw) ** TURN_EXPONENT * (1.0 if raw >= 0 else -1.0)
    return shaped * (sensitivity * sensitivity + TURN_FLOOR) * TURN_RATE / 60.0


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


def _settle(w: World, x: float, y: float, z: float, ground: bool,
            vz: float) -> tuple[float, bool, float]:
    """Rise out of the surface landed on, or stay glued over a kerb.

    The lift **stops at the ceiling**. Rising until the feet are clear is
    right in the open and wrong under an overhang, where it drives the head
    into the slab: the two level 5 spawn points that stood inside geometry
    without moving are in a gap 1.7 units tall, which is exactly the body's
    height, so the lift had nowhere to put it that was not solid. Refusing to
    lift into a ceiling leaves the feet a little in the floor, which is the
    lesser wrong and the one a player cannot see.
    """
    if w.footed(x, y, z):
        lift = 0.0
        while (lift < EYE and w.footed(x, y, z + lift)
               and not w.blocked(x, y, z + lift + 0.05)):
            lift += 0.05
        return z + lift, True, 0.0
    if ground and vz < 0:
        drop = 0.0
        while drop < STEP and not w.footed(x, y, z - drop):
            drop += 0.05
        if drop < STEP:
            return z - (drop - 0.05), True, 0.0
        return z, False, vz
    return z, False, vz


def _land(w: World, was, x: float, y: float, z: float, ground: bool,
          vz: float):
    """Take the frame back if the settled position is inside the world.

    A frame moves sideways and then vertically, and only the sideways part is
    checked -- against the height the body had *before* it settled. Walk fast
    under a lowering ceiling and the two disagree. Validating the finished
    position and giving up the sideways move when it fails is what a swept
    solve would do, cheaply, and it takes the wedges from 9 runs to 6.
    """
    if not w.blocked(x, y, z):
        return x, y, z
    back = _settle(w, was[0], was[1], z, ground, vz)[0]
    if w.blocked(was[0], was[1], back):
        return x, y, z                 # no better there; do not trade a wedge
    return was[0], was[1], back


def walk(w: World, start, yaw: float, frames: int, speed: float) -> dict:
    pos = list(start)
    vz, ground, hits, inside = 0.0, False, 0, 0
    fwd = (math.cos(yaw), math.sin(yaw))
    run = speed * DT
    for _ in range(frames):
        was = (pos[0], pos[1])
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
        nz, ground, vz = _settle(w, pos[0], pos[1], nz, ground, vz)
        pos[0], pos[1], pos[2] = _land(w, was, pos[0], pos[1], nz, ground, vz)
        if w.blocked(pos[0], pos[1], pos[2]):
            inside += 1
    return {"end": pos, "ground": ground, "hits": hits, "inside": inside}


# Anchors that make the DirectInput reading certain rather than plausible:
# each is a command whose *name* fixes which key it must be, and every one
# lands on that key's DIK_ scancode. `defaultkeys2.lua` is the WASD scheme,
# and W-A-S-D really are 17-30-31-32.
DIK_ANCHORS = {
    "COM_PAUSE": (1, "ESCAPE"), "COM_MENUSELECT": (28, "RETURN"),
    "COM_CONSOLE": (41, "GRAVE"), "COM_SCREENSHOT": (68, "F10"),
    "COM_SMTOGGLE": (57, "SPACE"),
}
DIK_WASD = {"COM_FORWARD": (17, "W"), "COM_STRAFELEFT": (30, "A"),
            "COM_BACKWARD": (31, "S"), "COM_STRAFERIGHT": (32, "D")}
DIK_MAX = 0xED                      # the last scancode DirectInput defines
PSEUDO = range(1000, 1008)          # mouse buttons and the four half-axes
BIND = re.compile(r"omBindCommandI\(\s*(\w+)\s*,\s*(\d+)\s*\)")


def keycheck(scripts: Path) -> tuple[int, int, list[str]]:
    """Are the binding ids DirectInput scancodes? -> (bindings, ids, faults)"""
    binds, faults = [], []
    for name in ("defaultkeys.lua", "defaultkeys2.lua"):
        text = (scripts / name).read_text(errors="replace")
        wasd = name.endswith("2.lua")
        for m in BIND.finditer(text):
            command, code = m.group(1), int(m.group(2))
            binds.append((command, code))
            if not (1 <= code <= DIK_MAX or code in PSEUDO):
                faults.append(f"{command} = {code}, neither a scancode nor "
                              f"a mouse id")
            want = (DIK_WASD if wasd else DIK_ANCHORS).get(command)
            if want and code != want[0]:
                faults.append(f"{command} = {code}, but DIK_{want[1]} "
                              f"is {want[0]}")
    return len(binds), len({c for _, c in binds}), faults


# the demo's ids, from scripts/defaultkeys.lua and the DirectInput scancodes
FORWARD, BACKWARD, LEFT, RIGHT = 200, 208, 203, 205
TURN_RIGHT, TURN_LEFT, LOOK_DOWN, LOOK_UP = 1004, 1005, 1006, 1007
JUMP = 1001
JUMP_SPEED = 7.0                       # the same as the page's

# A playable character's speed is a 3x3 table read at `table[fwd * 3 + side]`,
# with the forward table and the strafe table 0x24 apart. mdkKurt.c reads
# Kurt's at 0x4179cf; mdkMax, mdkHyde and mdkDoctor do the same at 0x421cb6,
# 0x414710 and 0x40954a. The whole 18-float block is byte-identical in the
# 2011 HD build (VA 0x4c3adc), indexed there by the same `eax*3 + [ebp+4]`.
# `engine/src/game/world.rs` carries the same numbers and `health.py --speed`
# reads them out of the binary to hold both to it.
PLAYER_SPEED = {
    100.0: (  # OBJ_KURT, 0x48f828 / 0x48f84c
        (10.6, 15.0, 10.6, 0.0, 0.0, 0.0, -8.5, -12.0, -8.5),
        (-10.6, 0.0, 10.6, -15.0, 0.0, 15.0, -8.5, 0.0, 8.5), 21.0),
    103.0: (  # OBJ_HYDE, 0x48f6e0 / 0x48f704
        (7.5, 10.0, 7.5, 0.0, 0.0, 0.0, -4.25, -6.0, -4.25),
        (-4.5, 0.0, 4.5, -6.0, 0.0, 6.0, -4.25, 0.0, 4.25), 14.0),
    101.0: (  # OBJ_MAX, 0x48fa28 / 0x48fa4c
        (8.5, 12.0, 8.5, 0.0, 0.0, 0.0, -7.0, -10.0, -7.0),
        (-8.5, 0.0, 8.5, -12.0, 0.0, 12.0, -7.0, 0.0, 7.0), 14.0),
    102.0: (  # OBJ_DOC, 0x48f3d0 / 0x48f3f4
        (8.5, 10.0, 8.5, 0.0, 0.0, 0.0, -5.0, -6.0, -5.0),
        (-6.0, 0.0, 6.0, -7.0, 0.0, 7.0, -6.0, 0.0, 6.0), 14.0),
}
ACCELERATE, BRAKE = 60.0, 120.0        # 0x40ef40's two rates, at all four sites


def player_speed(kind: float, ahead: int, side: int):
    """-> (forward, strafe, the mover's fourth argument)."""
    forward, strafe, arg = PLAYER_SPEED[kind]
    i = (1 - max(-1, min(1, ahead))) * 3 + (1 + max(-1, min(1, side)))
    return forward[i], strafe[i], arg


def approach(current: float, target: float, dt: float) -> float:
    """One step of 0x40ef40.

    The low rate is taken **only** while the value is short of the target and
    already on the target's side of zero; braking and reversing both take the
    high one. Reading that off the four arms rather than assuming
    "accelerate slowly, brake hard" is the whole of it.
    """
    if target > 0:
        gaining = current < target and current >= 0
    else:
        gaining = current > target and current <= 0
    rate = ACCELERATE if gaining else BRAKE
    moved = current + rate * dt * (1.0 if target > current else -1.0)
    if abs(moved - target) < 1e-12 or (target > current) == (moved > target):
        return target
    return moved


def replay(w: World, start, yaw: float, frames: list, mouse: float = 1.0,
           kind: float = 100.0) -> dict:
    """Drive the controller from a parsed `.omn`. Same physics as walk().

    The speeds are the character's own table, smoothed the way 0x40ef40 does;
    the demo is Kurt's. `mouse` is radians per unit of the demo's axis values,
    and it is still a guess: the file does not carry the sensitivity and
    neither does any script. The turn rate itself is *not* a guess any more —
    it is 45.0 at 0x416ad5, times 1/60 — but binding a demo command to an axis
    index is not done, so this stays where it was.
    """
    pos = list(start)
    forward = strafe = 0.0
    vz, ground, hits, inside = 0.0, False, 0, 0
    path = [tuple(pos)]
    for f in frames:
        was = (pos[0], pos[1])
        dt = max(1e-4, min(0.2, f["dt"]))
        held = {}
        for cmd, v in f["input"]:
            held[cmd] = v
        # still the flat factor and not turn_from_axis; see its docstring and
        # the engine's `Body::replay` for the measurement that says why
        yaw -= mouse * held.get(TURN_RIGHT, 0.0)
        yaw += mouse * held.get(TURN_LEFT, 0.0)
        fx, fy = math.cos(yaw), math.sin(yaw)
        want_f, want_s, _ = player_speed(
            kind, (FORWARD in held) - (BACKWARD in held),
            (RIGHT in held) - (LEFT in held))
        forward = approach(forward, want_f, dt)
        strafe = approach(strafe, want_s, dt)
        # right is (-sin, cos), the same hand the engine uses. The table's own
        # entries carry the diagonal, so this must not normalise them again.
        dx = forward * fx - strafe * fy
        dy = forward * fy + strafe * fx
        speed = math.hypot(dx, dy)
        length = speed
        if length > 0:
            run = speed * dt
            dx, dy = dx / length * run, dy / length * run
            if w.blocked(pos[0] + dx, pos[1] + dy, pos[2]):
                hits += 1
            pieces = max(1, math.ceil(run / 0.25))
            for _k in range(pieces):
                for ax, ay in ((dx, dy), (dx, 0), (0, dy)):
                    nx, ny = pos[0] + ax / pieces, pos[1] + ay / pieces
                    if not w.blocked(nx, ny, pos[2]):
                        pos[0], pos[1] = nx, ny
                        break
        if ground and JUMP in held:
            vz, ground = JUMP_SPEED, False
        vz -= GRAVITY * dt
        nz, left = pos[2], vz * dt
        while abs(left) > 1e-6 and not w.footed(pos[0], pos[1], nz):
            bit = max(-0.5, min(0.5, left))
            nz += bit
            left -= bit
        nz, ground, vz = _settle(w, pos[0], pos[1], nz, ground, vz)
        pos[0], pos[1], pos[2] = _land(w, was, pos[0], pos[1], nz, ground, vz)
        if w.blocked(pos[0], pos[1], pos[2]):
            inside += 1
        path.append(tuple(pos))
    travelled = sum(math.dist(a, b) for a, b in zip(path, path[1:]))
    return {"end": pos, "ground": ground, "hits": hits, "inside": inside,
            "path": path, "travelled": travelled,
            "drift": math.dist(path[0], pos)}


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


def _engine(args) -> int:
    """Replay the demo here and in the engine, and require the same body.

    The start comes from the level's checkpoint table, which the engine
    cannot read yet -- it needs the level script, not the scene graph -- so it
    is computed here and handed over. Everything after that is each side's own
    controller against its own collision world, and they must land on the same
    numbers: the trees loaded, the distance travelled, the drift, whether it
    ends standing, the frames that met a wall, and the frames inside geometry.
    """
    import subprocess
    import omn
    import rooms as rm

    level = int(re.search(r"l(\d+)", args.src.stem).group(1))
    frames = omn.parse(args.demo.read_bytes())[1:]
    table, cps, _ = rm.load(level, args.resources, rm._override(args.resources))
    cp = cps[str(args.checkpoint)]
    start = rm._list(cp["2"])[:3]
    yaw = cp.get("4") or 0.0
    mine = replay(World(args.src, args.resources),
                  [start[0], start[1], start[2] + EYE], yaw, frames, args.mouse)

    root = Path(__file__).resolve().parent.parent
    out = subprocess.run(
        ["cargo", "run", "--quiet", "--release", "--manifest-path",
         str(root / "engine/Cargo.toml"), "--", args.engine,
         "--demo", args.src.name, args.demo.name,
         "--from", ",".join(repr(c) for c in start),
         "--yaw", repr(yaw), "--mouse", repr(args.mouse)],
        capture_output=True, text=True, check=True).stdout

    want = {"frames": len(frames),
            "travelled": round(mine["travelled"]),
            "drift": round(mine["drift"]),
            "standing": mine["ground"],
            "hits": mine["hits"],
            "inside": mine["inside"]}
    m = re.search(r"(\d+) frames.*travelled (\d+) units, (\d+) from where it "
                  r"started, (standing|in the air) at the end, met a wall on "
                  r"(\d+) frames, inside geometry on (\d+)", out)
    if not m:
        print(f"the engine said something else:\n{out}", file=sys.stderr)
        return 1
    got = {"frames": int(m.group(1)), "travelled": int(m.group(2)),
           "drift": int(m.group(3)), "standing": m.group(4) == "standing",
           "hits": int(m.group(5)), "inside": int(m.group(6))}
    bad = [k for k in want if want[k] != got[k]]
    for k in bad:
        print(f"MISMATCH {k}: {want[k]} here, {got[k]} in the engine",
              file=sys.stderr)
    print(f"{len(frames)} frames replayed both ways, {len(bad)} disagree "
          f"({want['travelled']} units, {want['hits']} wall frames, "
          f"{want['inside']} inside)", file=sys.stderr)
    return 1 if bad or want["inside"] else 0


def _replay(args) -> int:
    """Replay a demo and say where it went, and through which rooms."""
    import omn
    import rooms as rm

    level = int(re.search(r"l(\d+)", args.src.stem).group(1))
    frames = omn.parse(args.demo.read_bytes())[1:]     # frame 0 is the load
    table, cps, _ = rm.load(level, args.resources, rm._override(args.resources))
    cp = cps[str(args.checkpoint)]
    start = rm._list(cp["2"])[:3]
    start = [start[0], start[1], start[2] + EYE]

    w = World(args.src, args.resources)
    r = replay(w, start, cp.get("4") or 0.0, frames, args.mouse)

    seen, order = set(), []
    for p in r["path"]:
        for name in rm.where(table, p):
            if name not in seen:
                seen.add(name)
                order.append(name)
    print(f"{args.demo.name}: {len(frames)} frames, "
          f"{sum(f['dt'] for f in frames):.1f}s from l{level} cp"
          f"{args.checkpoint} {cp['1']!r}")
    print(f"  travelled {r['travelled']:.0f} units, "
          f"{r['drift']:.0f} from where it started, "
          f"{'standing' if r['ground'] else 'in the air'} at the end")
    print(f"  met a wall on {r['hits']} frames, inside geometry on "
          f"{r['inside']}")
    print(f"  {len(order)} rooms: {', '.join(order)}")
    # the invariant, and the only one this can assert without the original:
    # replaying the game's own input must never put the body in the world
    return 1 if r["inside"] else 0


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
    ap.add_argument("--expect-inside", type=int, metavar="N",
                    help="succeed only if exactly N runs ever end up inside "
                         "geometry; the number that must not creep up")
    ap.add_argument("--keys", action="store_true",
                    help="check the key bindings against DirectInput")
    ap.add_argument("--demo", type=Path, metavar="OMN",
                    help="replay a recorded demo through the controller")
    ap.add_argument("--checkpoint", type=int, default=5,
                    help="which checkpoint the demo starts at "
                         "(demo1_5.omn means level 1, checkpoint 5)")
    ap.add_argument("--engine", metavar="GAMEDIR",
                    help="replay the demo in the engine too and require the "
                         "same body")
    ap.add_argument("--mouse", type=float, default=1.0,
                    help="radians per unit of the demo's axis value; flat, and not what the original does -- see turn_from_axis")
    args = ap.parse_args(argv)

    if args.keys:
        n, ids, faults = keycheck(args.resources / "scripts")
        for f in faults:
            print(f"  {f}", file=sys.stderr)
        print(f"{n} key bindings over {ids} distinct ids, all DirectInput "
              f"scancodes or mouse ids, {len(faults)} faults", file=sys.stderr)
        return 1 if faults else 0
    if args.demo:
        return _engine(args) if args.engine else _replay(args)

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
    if args.expect_standing is not None and upright != args.expect_standing:
        print(f"expected {args.expect_standing} standing", file=sys.stderr)
        return 1
    if args.expect_inside is not None and bad != args.expect_inside:
        print(f"expected {args.expect_inside} runs inside", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
