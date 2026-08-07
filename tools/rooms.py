#!/usr/bin/env python3
"""
rooms.py -- the room graph: what is drawn, what is streamed, where you respawn.

`tools/scene.py` reads *where things are*. This reads *how the level is
divided*, which is a different table in a different file: `Level.scenegraph`
in `scripts/levelN.lua`, keyed by room name. `mdk2.lua:ApplySceneGraph` is
the whole contract and it is worth quoting, because it says exactly which
fields the engine consumes:

    mdkAddRoom(gob)                     -- a room *is* an object
    mdkRoomAddVisible(room, gob)        -- itself, then every `visible` entry
    mdkRoomSetLoad(room, sectmap[load]) -- start streaming this section
    mdkRoomSetMusic(room, music)
    mdkRoomSetEnv(room, env)
    mdkRoomSetBB(room, bmin..., bmax...)
    mdkRoomSetCheckpoint(room, checkpoint)

So `visible` is the **authored PVS**: standing in a room, the renderer draws
that room and the rooms it lists, and nothing else. That is how a 2000-era
engine culls 500 objects down to a dozen, and it is why the level scripts
carry 753 of these lists. It works: standing at level 1's first checkpoint,
`l1_r1` sees five of the level's fifty-nine rooms, and the viewer draws
12621 triangles of 74658.

**Authored, not computed** -- only 63% of the 3127 links between live rooms
are reciprocated (l10 as little as 19%). A computed potentially-visible set
would be near-symmetric; a designer walking the level and listing what he can
see from each doorway would not be. The engine does not care either way: the
list says what to draw from where you stand, and nothing reads it backwards.

**Eight fields are authored, seven are read.** `ambient` is the eighth, it
appears on 54 rooms of 823, no script anywhere reads it, and every one of the
54 is {0,0,0} or {0,0,0,0}. Dead editor output, not a feature.

**The scene graph agrees, independently.** It has a type for these objects:
`OBJ_ROOM`. Every one of the 677 live rooms in the ten levels is an OBJ_ROOM
object, and only seven OBJ_ROOM objects are not listed as rooms -- three
minigame arenas (`torpedogame`, `podgame`, `jimdandygame`, which the scripts
put together themselves) and four mirror or see-through rooms
(`l5_r6mirror`, `l10_tun13`, `l10r4_see`, `tun1_see`). Two files written by
different tools, naming the same 677 things.

**A room's box is its object's box** -- the authored bbox from the scene
graph, in world coordinates because room geometry is static (see the placement
rule in CLAUDE.md) -- unless `bmin`/`bmax` override it, which 55 rooms do.
The check that this is the right reading is the checkpoints: 125 of the game's
129 land inside a room of their own level, and **all four that do not have a
reason**: `l1 cp9 "Intro Movie A"` sits at y = -1000, `l9 cp13 "Intro Movie"`
at x = 10000 and `l10 cp10 "10-10 End Comic"` at (10000, 10000, 10000) -- all
three camera positions parked off the map -- while `l2 cp11 "Train Station"`
names a room that no longer exists. The seven live rooms with no box at all
are the `*_levellight` objects and `l5_levelstars`: lighting and sky, which
are everywhere and so are bounded by nothing.

**148 names in the room tables are registered by no scene graph at all.**
That is not a parsing failure, it is stale editor data the engine skips by
design: `ApplySceneGraph` has `if (gob) then ... else` with a commented-out
`print("room '%s' in scenegraph does not exist")` in the else, and
`mdkRoomAddVisible` is guarded the same way. One room does resolve, but in
another scene: l10 lists `zizroom`, which `zizzyroom.lua` registers -- the
Zizzy arena, loaded as a scene of its own.

**Rooms are also where the script logic hangs.** `handlers()` reads the
level script for `NAME.OnX = function` and finds 447 objects carrying one,
**182 of them rooms**, and 119 of those with `OnEnterRoom` -- so crossing a
room boundary is the single commonest thing that runs game logic in MDK2.
`tools/mod2html.py` puts that in the viewer's HUD: walk into a room and it
prints what the engine would dispatch there.

Usage:
    python3 tools/rooms.py extracted --check        # all ten levels
    python3 tools/rooms.py extracted --level 1
    python3 tools/rooms.py extracted --level 1 --at 0 124 0.2
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import scene                                              # noqa: E402

# ApplySceneGraph reads these seven; `ambient` is authored and read by nothing
FIELDS = ("visible", "load", "music", "env", "bmin", "bmax", "checkpoint")
NOT_A_ROOM = ("sections", "checkpoints")


class RoomError(ValueError):
    pass


def _list(t) -> list:
    """Lua's 1-based array, as luarun --dump encodes it: {"1": v, "2": v}."""
    if t is None:
        return []
    return [t[k] for k in sorted(t, key=int)]


def dump(script: Path, tree: Path, override: Path | None = None) -> dict:
    """Run a level script and hand back its `Level.scenegraph`."""
    cmd = [sys.executable, str(Path(__file__).parent / "luarun.py"), str(script),
           "--tree", str(tree), "--boot", "mdk2",
           "--answer", "chGetGameWasReset=0", "--set", "checkpoint=1",
           "--dump", "Level.scenegraph"]
    if override:
        cmd += ["--override", str(override)]
    out = subprocess.run(cmd, capture_output=True, text=True).stdout
    lines = [l for l in out.splitlines() if l.startswith("{")]
    if not lines:
        raise RoomError(f"{script.name}: the script produced no scenegraph")
    return json.loads(lines[-1])


def rooms(graph: dict, objects: dict, others: dict | None = None) -> dict:
    """Merge the room table with the objects it names. -> {name: room}.

    Names resolve the way the engine's `getglobal` does: through whatever the
    scene graphs registered. `live` is False only for a name **no** graph
    registers -- `ApplySceneGraph` guards those with `if (gob) then`, and so
    must anything built on this. `foreign` marks the one case of a room that
    belongs to another scene, l10's `zizzyroom`.
    """
    others = others or {}
    out = {}
    for name, v in graph.items():
        if name in NOT_A_ROOM:
            continue
        o = objects.get(name) or others.get(name)
        box = None
        if "bmin" in v and "bmax" in v:
            box = (_list(v["bmin"])[:3], _list(v["bmax"])[:3])
        elif o and o["bbox_min"] is not None:
            box = (o["bbox_min"], o["bbox_max"])
        out[name] = {
            "live": o is not None,
            "foreign": name not in objects and name in others,
            "box": box,
            "boxed": "bmin" in v,          # overridden rather than inherited
            "visible": _list(v.get("visible")),
            "load": v.get("load") or None,
            "music": v.get("music"),
            "env": v.get("env"),
            "checkpoint": v.get("checkpoint"),
        }
    return out


def track(music) -> str | None:
    """The music a room asks for, as the file it names. -> "Track18" or None.

    `mdkRoomSetMusic(room, 18)` and `chSndSwitchMusic(18)` both mean
    `Music/Track18`, and the mapping is the identity rather than an offset:
    **the highest index anything uses is 27, and Track27 is the last that
    exists** -- there is no Track28 for an off-by-one to land on. 27 is also
    `StartFunnyTrack()`, the joke that plays at the end of the game if the
    language is English, which is where the last track belongs.

    Rooms use 1-5, 8-20 and 22-25; the scripts add 7 and 27; 0 and -1 mean
    stop. Only Track06, Track21 and Track26 are never named by either.
    """
    if music is None or music <= 0:
        return None
    return f"Track{int(music):02d}"


HANDLER = re.compile(r"^\s*([A-Za-z_]\w*)\.(On\w+)\s*=\s*function", re.M)


def handlers(level: int, tree: Path, override: Path | None = None) -> dict:
    """Which events each object has a handler for. -> {object: [event, ...]}.

    Read out of the level script's text rather than by running it, because
    what is wanted is the static shape: the engine dispatches `OnEnterRoom`
    to the room's own object, and this says which rooms would answer. The
    dynamic form, `mdkSetLuaEvent(gob, "OnX", fn)`, is not here.
    """
    script = Path(override or "") / f"level{level}.lua"
    if not script.is_file():
        script = tree / "scripts" / f"level{level}.lua"
    out: dict[str, list[str]] = {}
    for m in HANDLER.finditer(script.read_text(errors="replace")):
        if m.group(1) != "Level":
            out.setdefault(m.group(1), []).append(m.group(2))
    return out


def where(table: dict, point) -> list[str]:
    """Which rooms' boxes contain this point. Boxes do overlap."""
    hits = []
    for name, r in table.items():
        box = r["box"]
        if not r["live"] or box is None:
            continue
        if all(box[0][c] <= point[c] <= box[1][c] for c in range(3)):
            hits.append(name)
    return hits


def visible_from(table: dict, name: str) -> list[str]:
    """The room itself plus its authored PVS, dead names dropped.

    `ApplySceneGraph` calls `mdkRoomAddVisible(room, gob)` for the room's own
    object before walking the list, so a room always sees itself.
    """
    r = table.get(name)
    if r is None:
        return []
    return [name] + [v for v in r["visible"]
                     if table.get(v, {}).get("live") or v in table]


_others: dict[str, dict] = {}


def _all_scenes(tree: Path) -> dict:
    """Every object every scene graph registers, for the cross-scene case."""
    if not _others:
        for g in sorted((tree / "base").glob("*.lua")):
            for o in scene.parse(g.read_text(errors="replace"))["objects"]:
                _others.setdefault(o["name"], o)
    return _others


def load(level: int, tree: Path, override: Path | None = None) -> tuple:
    """-> (room table, checkpoint table, the level's objects by name)."""
    graph = dump(tree / "scripts" / f"level{level}.lua", tree, override)
    objects = {o["name"]: o for o in scene.parse(
        (tree / "base" / f"l{level}.lua").read_text(errors="replace"))["objects"]}
    return (rooms(graph, objects, _all_scenes(tree)),
            graph.get("checkpoints", {}), objects)


def _override(tree: Path) -> Path | None:
    gog = os.environ.get("MDK2_GOG")
    p = Path(gog) / "override" if gog else None
    return p if p and p.is_dir() else None


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("tree", type=Path, help="the extracted/ directory")
    ap.add_argument("--level", type=int, help="one level, listed")
    ap.add_argument("--check", action="store_true", help="validate all ten")
    ap.add_argument("--at", nargs=3, type=float, metavar=("X", "Y", "Z"),
                    help="which room contains this point, and what it sees")
    ap.add_argument("--expect", type=int, metavar="N",
                    help="fail unless the ten levels list exactly N rooms")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)

    over = _override(args.tree)
    levels = [args.level] if args.level else list(range(1, 11))
    total = dead = boxed = pvs = 0
    homeless, extra, bad = [], [], []

    for n in levels:
        table, cps, objects = load(n, args.tree, over)
        known = _all_scenes(args.tree)
        # the scene graph types rooms as OBJ_ROOM, so the two lists must agree
        live = {r for r, v in table.items() if v["live"]}
        typed = {k for k, o in objects.items() if o["type"] == "OBJ_ROOM"}
        extra += [f"l{n} {r}" for r in sorted(typed - live)]
        untyped = live - typed - {r for r, v in table.items() if v["foreign"]}
        if untyped:
            bad.append(f"l{n}: {sorted(untyped)} is a room but not OBJ_ROOM")
        dead_here = {r for r, v in table.items() if not v["live"]}
        dead_here |= {v for r in table.values() for v in r["visible"]
                      if v not in known}
        # a room with no box cannot be entered, and a live room always has one
        boxless = [r for r, v in table.items() if v["live"] and v["box"] is None]
        inside = sum(1 for c in cps.values()
                     if where(table, _list(c["2"])[:3]))
        homeless += [f"l{n} cp{c} {v['1']!r}" for c, v in cps.items()
                     if not where(table, _list(v["2"])[:3])]
        total += len(table)
        dead += len(dead_here)
        boxed += sum(1 for r in table.values() if r["boxed"])
        pvs += sum(len(r["visible"]) for r in table.values())

        if args.at and len(levels) == 1:
            here = where(table, args.at)
            print(f"at {tuple(args.at)}: {here or 'no room'}")
            for h in here:
                print(f"  {h} sees {len(visible_from(table, h))}: "
                      f"{', '.join(visible_from(table, h))}")
            return 0
        if args.json:
            json.dump(table, sys.stdout, indent=1)
            return 0
        if args.level:
            for name, r in sorted(table.items()):
                flags = "".join(c for c, on in
                                (("D", not r["live"]), ("B", r["boxed"]),
                                 ("L", r["load"]), ("M", r["music"]),
                                 ("C", r["checkpoint"])) if on)
                print(f"{name:24s} {flags:5s} sees {len(r['visible']):3d}"
                      + (f"  {track(r['music']) or 'music off'}"
                         if r["music"] else ""))
        print(f"l{n}: {len(table)} rooms, {len(dead_here)} dead names, "
              f"{len(boxless)} live but boxless, "
              f"{sum(len(r['visible']) for r in table.values())} visibility links, "
              f"{inside}/{len(cps)} checkpoints in a room", file=sys.stderr)

    if len(levels) > 1:
        print(f"{total} rooms over {len(levels)} levels, {dead} dead names, "
              f"{boxed} with an explicit box, {pvs} visibility links",
              file=sys.stderr)
        print(f"checkpoints outside every room: {', '.join(homeless)}",
              file=sys.stderr)
        print(f"OBJ_ROOM objects the scenegraph does not list as rooms: "
              f"{', '.join(extra)}", file=sys.stderr)
    if args.expect and total != args.expect:
        bad.append(f"{total} rooms, expected {args.expect}")
    for line in bad:
        print(line, file=sys.stderr)
    return 1 if bad else 0


def selftest() -> None:
    g = {"sections": {}, "checkpoints": {},
         "a": {"visible": {"1": "b"}, "music": 3},
         "b": {"bmin": {"1": 0, "2": 0, "3": 0},
               "bmax": {"1": 1, "2": 1, "3": 1}},
         "gone": {"visible": {"1": "a"}}}
    objs = {"a": {"bbox_min": [-1, -1, -1], "bbox_max": [1, 1, 1]},
            "b": {"bbox_min": [-9, -9, -9], "bbox_max": [9, 9, 9]}}
    t = rooms(g, objs)
    assert t["a"]["box"] == ([-1, -1, -1], [1, 1, 1]), t["a"]
    assert t["b"]["box"] == ([0, 0, 0], [1, 1, 1]), "bmin overrides the object"
    assert t["b"]["boxed"] and not t["a"]["boxed"]
    assert not t["gone"]["live"] and t["a"]["live"]
    assert where(t, (0.5, 0.5, 0.5)) == ["a", "b"], "boxes overlap"
    assert where(t, (5, 5, 5)) == []
    assert visible_from(t, "a") == ["a", "b"], "a room sees itself first"
    print("rooms.py: self-test passed")


if __name__ == "__main__":
    if "--selftest" in sys.argv:
        selftest()
    else:
        raise SystemExit(main())
