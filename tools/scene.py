#!/usr/bin/env python3
"""
scene.py -- read the level scene graphs: where every object in a level stands.

The `.mod` files hold geometry and the `.bsp` files hold collision, but
neither says what a level *contains*. That lives in Lua, as plain text, in the
`base/` half of the archive -- one file per level and per cut-scene, 56 in
all. Each is a flat list of waypoints and object registrations, and nothing
else: 5633 `mdkRegisterObject` calls across the corpus and not one other
statement.

    points.NAME = {x=..., y=..., z=..., f=...}      -- a waypoint, f = facing

    mdkRegisterObject(name, type, scene, parent, group,
                      x, y, z, qw, qx, qy, qz,
                      resource, a, b, c, d,
                      bbox_min, bbox_max, flag)

`type` is an `OBJ_*` constant -- OBJ_ROOM, OBJ_SCENERY, OBJ_TURRET,
OBJ_PROXDOOR1, OBJ_SPAWNER -- which the engine defines, not Lua; none of the
132 of them appears on the left of an assignment anywhere in the shipped
scripts. 82 are placed in the scene graphs, the rest only ever spawned at run
time by `mdkCreateObjectLua`. `parent`
is a bare identifier naming an object registered earlier in the same file, so
the file is a topologically sorted tree: 4789 of the 5633 have a parent, the
remaining 844 hang off the scene root.

`resource` is polymorphic and the type decides what it names -- see
`resolve()`. The four fields after it are a per-type payload: for a light,
packed colour then radius; for an ambient sound the (min distance, max
distance, volume) triple the `.mod` sound channels also use.

OBJ_STATICLIGHT, and only OBJ_STATICLIGHT, omits the trailing flag: 2080 of
the calls take 19 arguments and the other 3553 take 20, and the split is
exactly by type.

Checked to the project's 100% rule -- `--validate` reports zero complaints
over all 54 files and 5633 objects:

  * every call parses, at 19 or 20 arguments by the rule above;
  * every rotation is a unit quaternion to within 1e-4;
  * every parent reference resolves to an object registered earlier;
  * every resource name resolves, to a file in the archive or to a waypoint.

The bounding box is an authored volume, not a derived one. Of the 1114 calls
that carry a box and name a model, 723 hold exactly the model's own bounds --
the exporter's default -- 313 hold a looser box that still encloses the model,
and 78 hold something unrelated: `l10_ladpad01` draws `l9ladbase`, which spans
1.7 units, inside a box of +-0.001. Those are triggers, and they are the types
one would guess: OBJ_PROXDOOR1, OBJ_TURRET, OBJ_BLOWERCYLINDER.

**The box is in the model's frame, not the object's.** Not one of the 1114
matches after adding the object's position, and 723 match without it. This is
the same fact `posed()` records for static geometry: a level's models are
authored already standing where they stand, and the position in the scene
graph is not a second translation to apply to them.

Usage:
    python3 tools/scene.py extracted/base/l1.lua
    python3 tools/scene.py extracted/base --validate --resources extracted
    python3 tools/scene.py extracted/base/l1.lua --json > l1.json
"""

from __future__ import annotations

import argparse
import json
import math
import re
import struct
import sys
from pathlib import Path

CALL = "mdkRegisterObject("
POINT = re.compile(r"^points\.(\w+)\s*=\s*\{x=([^,]+),y=([^,]+),z=([^,]+),f=([^}]+)\}")

# argument positions, after the name
I_TYPE, I_SCENE, I_PARENT, I_GROUP = 1, 2, 3, 4
I_POS, I_QUAT, I_RES, I_PAYLOAD = 5, 8, 12, 13
I_BBOX = 17

# every type whose `resource` names a sound rather than a model
SOUND_TYPES = {"OBJ_AMBIENTSOUND"}


class SceneError(ValueError):
    pass


def _split(args: str) -> list[str]:
    """Split a Lua argument list on commas that are not inside {} or ''."""
    out, depth, quoted, start = [], 0, False, 0
    for i, ch in enumerate(args):
        if quoted:
            quoted = ch != "'"
        elif ch == "'":
            quoted = True
        elif ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
        elif ch == "," and depth == 0:
            out.append(args[start:i].strip())
            start = i + 1
    out.append(args[start:].strip())
    return out


def _value(tok: str):
    """A Lua literal, or the token itself when it is a bare identifier."""
    if tok == "nil":
        return None
    if tok.startswith("'"):
        return tok[1:-1]
    if tok.startswith("{"):
        return [float(p) for p in tok[1:-1].split(",")]
    try:
        return float(tok)
    except ValueError:
        return tok


def parse(text: str) -> dict:
    points, objects = {}, []
    for lineno, line in enumerate(text.splitlines(), 1):
        line = line.strip()
        m = POINT.match(line)
        if m:
            points[m.group(1)] = {
                "x": float(m.group(2)), "y": float(m.group(3)),
                "z": float(m.group(4)), "facing": float(m.group(5)),
            }
            continue
        if not line.startswith(CALL):
            continue
        a = [_value(t) for t in _split(line[len(CALL):line.rindex(")")])]
        if len(a) not in (19, 20):
            raise SceneError(f"line {lineno}: {len(a)} arguments")
        light = a[I_TYPE] == "OBJ_STATICLIGHT"
        if light != (len(a) == 19):
            raise SceneError(f"line {lineno}: {a[I_TYPE]} with {len(a)} args")
        objects.append({
            "name": a[0],
            "type": a[I_TYPE],
            "parent": a[I_PARENT],
            "group": int(a[I_GROUP]),
            "position": a[I_POS:I_POS + 3],
            "rotation": a[I_QUAT:I_QUAT + 4],      # (w, x, y, z), as in .mod
            "resource": a[I_RES],
            "payload": a[I_PAYLOAD:I_PAYLOAD + 4],
            "bbox_min": a[I_BBOX],
            "bbox_max": a[I_BBOX + 1],
            "flag": None if light else int(a[I_BBOX + 2]),
            "line": lineno,
        })
    return {"points": points, "objects": objects}


def _model_bbox(path: Path):
    """The union of the .mod's node boxes -- the model's own bounds."""
    data = path.read_bytes()
    off = struct.unpack_from("<I", data, 0x20)[0]      # section 0, the nodes
    count = struct.unpack_from("<12H", data, 8)[5]
    if off == 0xFFFFFFFF or not count:
        return None
    boxes = [(struct.unpack_from("<3f", data, off + i * 136 + 0x1c),
              struct.unpack_from("<3f", data, off + i * 136 + 0x28))
             for i in range(count)]
    return (tuple(min(b[0][c] for b in boxes) for c in range(3)),
            tuple(max(b[1][c] for b in boxes) for c in range(3)))


def validate(scene: dict, resources: Path | None = None) -> list[str]:
    """-> a list of complaints, empty when the file is sound."""
    bad, seen = [], set()
    for o in scene["objects"]:
        where = f"{o['name']} (line {o['line']})"
        length = math.sqrt(sum(c * c for c in o["rotation"]))
        if abs(length - 1.0) > 1e-4:
            bad.append(f"{where}: rotation length {length:.6f}")
        if o["parent"] is not None and o["parent"] not in seen:
            bad.append(f"{where}: parent {o['parent']} not registered yet")
        seen.add(o["name"])
        if resources is None or o["resource"] is None:
            continue
        if resolve(o, scene, resources) is None:
            bad.append(f"{where}: {o['resource']} is neither a resource "
                       f"in the archive nor a waypoint in this file")
    return bad


def resolve(o: dict, scene: dict, resources: Path) -> Path | str | None:
    """What an object's `resource` field names -- a file, or a waypoint.

    The slot is polymorphic, and the type decides. Most objects name a `.mod`.
    OBJ_AMBIENTSOUND names a `.wav` in `sounds/`. OBJ_STARS names a `.tex`,
    the flare sprite. And every character type -- OBJ_DOGANBOY, OBJ_HOSER,
    OBJ_POOPSY, OBJ_BIF, OBJ_INVISOGRUNT, OBJ_BIRDBRAIN1, OBJ_CONEHEADCIV2,
    OBJ_ULTRADOGANBOY -- names a *waypoint* declared earlier in the same file,
    the spot it guards: `l1_r2dgnwp`, `l3_r9doganpen`, `l9r8dogpen01a`. Their
    model comes from the type instead, which is why the type list is so long
    and so specific.
    """
    name = o["resource"]
    for ext in (".wav",) if o["type"] in SOUND_TYPES else (".mod", ".tex"):
        found = _find(resources, name + ext)
        if found is not None:
            return found
    return name if name in scene["points"] else None


def box_fit(o: dict, resources: Path) -> str | None:
    """How an object's authored box relates to its model's own bounds.

    -> "exact", "looser", "authored", or None when there is nothing to
    compare. The point of the measurement is the negative result: *no* object
    matches once its position is added to the model bounds, which is what says
    the box is stored in the same frame as the model rather than relative to
    the object.
    """
    if o["bbox_min"] is None or o["resource"] is None:
        return None
    if o["type"] in SOUND_TYPES:
        return None
    found = _find(resources, o["resource"] + ".mod")
    box = _model_bbox(found) if found else None
    if box is None:
        return None
    lo, hi = o["bbox_min"], o["bbox_max"]
    if all(abs(box[0][c] - lo[c]) < 1e-2 and abs(box[1][c] - hi[c]) < 1e-2
           for c in range(3)):
        return "exact"
    if all(lo[c] <= box[0][c] + 1e-2 and hi[c] >= box[1][c] - 1e-2
           for c in range(3)):
        return "looser"
    return "authored"


_index: dict[str, Path] = {}


def _find(root: Path, name: str) -> Path | None:
    """Case-insensitive lookup; the Lua spells names in a case of its own."""
    if not _index:
        for p in root.rglob("*"):
            if p.is_file():
                _index.setdefault(p.name.lower(), p)
    return _index.get(name.lower())


SAMPLE = """
points.wp1={x=1,y=2,z=3,f=-90.0456}
mdkRegisterObject('r1', OBJ_ROOM, scene, nil, -1, 0,0,0, 1,0,0,0, 'l1_r1',0,0,0,0, {-1,-2,-3}, {1,2,3}, 0)
mdkRegisterObject('lt', OBJ_STATICLIGHT, scene, r1, -1, 1,2,3, 1,0,0,0, nil,15906150,80,1,10, nil,nil)
"""


def selftest() -> None:
    """The parser's corner cases, without needing the game installed."""
    s = parse(SAMPLE)
    assert s["points"] == {"wp1": {"x": 1.0, "y": 2.0, "z": 3.0,
                                   "facing": -90.0456}}, s["points"]
    room, light = s["objects"]
    assert room["bbox_min"] == [-1.0, -2.0, -3.0], room     # braces, not commas
    assert room["flag"] == 0 and light["flag"] is None      # the 19/20 split
    assert light["parent"] == "r1" and room["parent"] is None
    assert light["payload"] == [15906150.0, 80.0, 1.0, 10.0], light
    assert validate(s) == []
    s["objects"][1]["rotation"] = [1.0, 1.0, 0.0, 0.0]
    assert len(validate(s)) == 1
    print("scene.py: self-test passed")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path, nargs="?",
                    help="a scene .lua file or a directory")
    ap.add_argument("--selftest", action="store_true",
                    help="check the parser against a built-in sample")
    ap.add_argument("--validate", action="store_true",
                    help="check every file and report totals")
    ap.add_argument("--resources", type=Path,
                    help="extraction root, to resolve resource names")
    ap.add_argument("--json", action="store_true", help="dump the scene")
    args = ap.parse_args(argv)
    if args.selftest:
        selftest()
        return 0
    if args.src is None:
        ap.error("a scene .lua file or a directory is required")

    if args.src.is_dir():
        files = [p for p in sorted(args.src.glob("*.lua"))
                 if CALL in p.read_text(errors="replace")]
    else:
        files = [args.src]
    if not files:
        ap.error(f"no scene graphs in {args.src}")

    if args.json:
        json.dump(parse(files[0].read_text(errors="replace")),
                  sys.stdout, indent=1)
        return 0

    total = points = complaints = 0
    types: dict[str, int] = {}
    fit: dict[str, int] = {}
    for f in files:
        scene = parse(f.read_text(errors="replace"))
        total += len(scene["objects"])
        points += len(scene["points"])
        for o in scene["objects"]:
            types[o["type"]] = types.get(o["type"], 0) + 1
            if args.resources:
                how = box_fit(o, args.resources)
                if how:
                    fit[how] = fit.get(how, 0) + 1
        bad = validate(scene, args.resources)
        complaints += len(bad)
        if not args.validate:
            print(f"{f.name}: {len(scene['objects'])} objects, "
                  f"{len(scene['points'])} waypoints")
        for line in bad:
            print(f"  {f.name}: {line}", file=sys.stderr)
    if args.validate:
        for name, n in sorted(types.items(), key=lambda kv: -kv[1])[:12]:
            print(f"{n:6d}  {name}")
        if fit:
            print("boxes vs the model's own bounds: " + ", ".join(
                f"{n} {k}" for k, n in sorted(fit.items())))
    print(f"{len(files)} scene graphs, {total} objects, {points} waypoints, "
          f"{len(types)} types, {complaints} complaints", file=sys.stderr)
    return 1 if complaints else 0


if __name__ == "__main__":
    raise SystemExit(main())
