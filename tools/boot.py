#!/usr/bin/env python3
"""
boot.py -- start every level at every checkpoint, the way the game does.

The engine's entry into a level is one function, and BioWare drew its call
graph in a comment above it (`mdk2.lua:354`):

    level()
        dofile('levelx.lua')
            - scene graph definition -
            PreInitLevel()
                doloadingscreen()          -- preload the section's resources
                dofile('lx.lua')           -- create the gobs
                ApplySceneGraph()          -- rooms, visibility, checkpoints
            - script definition -          -- hang handlers on those gobs
        Level.Init()                       -- checkpoint-specific setup
            CreateCharX()                  -- the player, camera, inventory

So there is nothing to reconstruct: `level(number, checkpoint, section)` is
the whole of starting a level, and it runs. **All ten levels start at all 129
checkpoints** on a stock `lua5.1` with `tools/luarun.py`'s stubs, 129 of 129,
no errors.

That run is what turns the 438-function catalogue in `docs/lua-api.md` into a
work list. Booting level 1 at checkpoint 1 touches **40 functions**, and they
fall into six groups:

    the scene       mdkRegisterObject x416, mdkGetScene, mdkGetGuiScene
    streaming       mdkSectionAddRes x252, mdkPreloadRes x118,
                    mdkPreloadHardCodedSound x25, chSndLoadBank(Pre),
                    mdkDumpMenuSounds, mdkShowLoadingScreen, mdkLoadLevelIsInstant
    rooms           mdkAddRoom x59, mdkGetRoomNum x59, mdkRoomAddVisible x350,
                    mdkRoomSetEnv/Music/Load/BB/Checkpoint
    checkpoints     mdkSetCheckpoint x14, mdkWarpToCheckpoint, mdkAdvanceCheckpoint
    the player      mdkCreateObjectLua, mdkKurtSetGuiGobs, mdkSetPlayModeGobs,
                    mdkSwitchPlayMode
    this level      mdkProxDoorLock, mdkBlowerDisable, mdkSpawnerSetSpawnedObject,
                    mdkGobSetSeekable, omGobEnterStasis x20, omGobSetTimer,
                    mdkSetLuaEvent, omSceneFade
    housekeeping    chSeedRand(127), chZeroGlobalTime, chGetGameWasReset,
                    mdkShowMouse, chControlAnimateImage, mdkGobEnableScript

All fourteen of level 1's checkpoints together need 51, and **all ten levels
at all 129 checkpoints need 68** -- the size of the engine M8 actually needs,
a seventh of what the scripts can call.

`levelchanged` and `sectionchanged` are set here because the engine sets
them, and without them `doloadingscreen` takes neither branch: no
`mdkPreloadRes`, no sound bank, no loading screen. That is half of what
starting a level *is*, and it would have been silently missing.

With them the boots name the resources each checkpoint demands, through
`mdkPreloadRes` and `mdkSectionAddRes`, and **all 2093 of them exist** in the
extracted data -- `--resources` checks it. A level start is therefore not
only a sequence that runs, it is one that can be satisfied.

Two of those counts check the room graph from the other side, and they agree
exactly: level 1 makes 59 `mdkAddRoom` calls for its 59 rooms, and 350
`mdkRoomAddVisible` calls for 291 authored visibility links plus the 59 rooms
that each add themselves. See tools/rooms.py.

**A checkpoint list has to come from the merged tree**, not from
`extracted/scripts`: `override/` is a shipped patch and it adds level 1's
checkpoints 13 and 14.

**After the boot, the handlers.** The level scripts hang 443 tables of them
on the object globals -- `OnEnterRoom`, `OnCreate`, `OnDie`, `OnUpdate`,
`OnTimer`, `OnDamage`, `OnCollision` and thirteen rarer ones -- and every one
of those tables names an object the scene graph registers. `--events` calls
each of them once, and **45559 of 49457 run to the end**. Objects and slots
are taken in name order rather than hash order, because a handler can install
another object's method and the tally would otherwise wobble by a few dozen
between runs.

That is a survey of the surface, not a simulation: a handler called out of
context is not doing what it would do in the game. Its worth is in the ~3900
that do not survive, because each one names something the engine has to hold.

Some of it turned out to be **holdable already**, which is what this driver
does beyond stubbing. `mdkGobDistance(gob, player)` and
`mdkGobDistancePoint(gob, "l1_r8wp")` are how this game triggers nearly
everything, and the scene graph carries every object's position and every
waypoint -- so the driver keeps positions on the gobs, answers both queries
for real, and warps the player to the checkpoint that `mdkSetCheckpoint`
recorded. That alone moved "compare nil with number" from 3410 failures to
145, and 3000 more handler calls now run.

What is left is mostly script state that only exists mid-game: `TutFuncs`,
`SetUpDoorAndEnemies`, `GenDone` are methods another object's handler
installs, so calling handlers in an arbitrary order is bound to miss them.
That bounds what this measurement can mean, and it is why the number is
pinned in `tools/check.py` rather than chased.

The boot needs 68 engine functions; the handlers reach for **65 more**.

Usage:
    python3 tools/boot.py extracted                  # all ten, all checkpoints
    python3 tools/boot.py extracted --level 1        # one level
    python3 tools/boot.py extracted --api            # the functions it needs
    python3 tools/boot.py extracted --events         # and fire the handlers
"""

from __future__ import annotations

import argparse
import os
import re
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import luarun                                          # noqa: E402

CHECKPOINT = re.compile(
    r'Level\["scenegraph"\]\["checkpoints"\]\[(\d+)\]\s*=\s*\{\}')

# `level()` reads these before it starts, and the menu is what normally sets
# them. Nothing here needs a real save game.
#
# `chGetDeltaT` is here because the stub's rule -- a name with Get in it
# hands back a handle -- is wrong for the scalar getters, and the handlers
# do arithmetic on what it returns. That is the shape of nearly everything
# `--events` reports: an engine function that has to answer with a number.
ANSWERS = {"chGetGameWasReset": "0", "mdkLoadLevelIsInstant": "0",
           "chGetDeltaT": "0.0333"}

DRIVER = """
BOOTED, FAILED, WANTED, FIRED, PLAYED = {}, {}, {}, {}, {}
-- what each checkpoint demands: the argument, not just the call
function mdkPreloadRes(name, ...)
  CALLS[table.getn(CALLS) + 1] = "mdkPreloadRes"
  if name then WANTED[name] = (WANTED[name] or 0) + 1 end
end
function mdkSectionAddRes(section, name)
  CALLS[table.getn(CALLS) + 1] = "mdkSectionAddRes"
  if name then WANTED[name] = (WANTED[name] or 0) + 1 end
end

-- Positions, and the two queries the scripts lean on hardest. These are not
-- stubs: the scene graph carries every object's position and every waypoint,
-- so distance is a thing we can already answer for real, and proximity is
-- how this game triggers nearly everything.
local function _place(name, otype, sc, parent, group, x, y, z)
  local gob = {name = name, type = otype, x = x or 0, y = y or 0, z = z or 0}
  _G[name] = gob
  return gob
end
function mdkRegisterObject(name, ...)
  CALLS[table.getn(CALLS) + 1] = "mdkRegisterObject"
  return _place(name, unpack(arg))
end
function mdkCreateObjectLua(name, ...)
  CALLS[table.getn(CALLS) + 1] = "mdkCreateObjectLua"
  return _place(name, unpack(arg))
end
local function _dist(a, b)
  if type(a) ~= "table" or type(b) ~= "table" then return 1e9 end
  local dx, dy, dz = (a.x or 0) - (b.x or 0), (a.y or 0) - (b.y or 0),
                     (a.z or 0) - (b.z or 0)
  return sqrt(dx*dx + dy*dy + dz*dz)
end
function mdkGobDistance(a, b)
  CALLS[table.getn(CALLS) + 1] = "mdkGobDistance"
  return _dist(a, b)
end
function mdkGobDistancePoint(gob, name)
  CALLS[table.getn(CALLS) + 1] = "mdkGobDistancePoint"
  return _dist(gob, points and points[name])
end
-- the player is a gob the script creates and the engine then warps to the
-- checkpoint, so both halves are here rather than answered with a handle
function mdkGetPlayerGob()
  CALLS[table.getn(CALLS) + 1] = "mdkGetPlayerGob"
  return bob
end
CPS = {}
function mdkSetCheckpoint(n, x, y, z, facing, section)
  CALLS[table.getn(CALLS) + 1] = "mdkSetCheckpoint"
  CPS[n] = {x = x, y = y, z = z, facing = facing}
end
function mdkWarpToCheckpoint(gob, n)
  CALLS[table.getn(CALLS) + 1] = "mdkWarpToCheckpoint"
  local cp = CPS[n]
  if type(gob) == "table" and cp then
    gob.x, gob.y, gob.z = cp.x, cp.y, cp.z
  end
end
for _, job in ipairs(JOBS) do
  CALLS = {}
  -- the engine sets these; without them `doloadingscreen` takes neither of
  -- its branches and nothing is preloaded, which is exactly the streaming
  -- half of a level start
  levelchanged, sectionchanged = 1, 1
  local ok, err = pcall(level, job[1], job[2], nil)
  local key = "l" .. job[1] .. " cp" .. job[2]
  if ok then
    BOOTED[table.getn(BOOTED) + 1] = key
    local nboot = table.getn(CALLS)
    for i = 1, nboot do USED[CALLS[i]] = (USED[CALLS[i]] or 0) + 1 end
    if EVENTS then
      -- every handler the level script hung on an object global. Calling one
      -- out of context is a survey of the surface, not a simulation: what it
      -- is for is the engine functions the handlers reach for, and the ones
      -- that fail name the state the engine still has to hold.
      -- in name order: `pairs` over _G is hash order, and a handler that
      -- installs another object's method makes the result depend on it
      local names, slots = {}, {}
      for name, gob in pairs(_G) do
        if type(gob) == "table" and rawget(gob, "name") == name then
          names[table.getn(names) + 1] = name
        end
      end
      sort(names)
      for i = 1, table.getn(names) do
        local gob = _G[names[i]]
        slots = {}
        for slot, fn in pairs(gob) do
          if type(fn) == "function" and strfind(slot, "^On") then
            slots[table.getn(slots) + 1] = slot
          end
        end
        sort(slots)
        for j = 1, table.getn(slots) do
          local fine, ferr = pcall(gob[slots[j]], gob, gob, 1,
                                   "DAMAGE_NORMAL", 1)
          local k = slots[j] .. "|" .. (fine and "" or tostring(ferr))
          FIRED[k] = (FIRED[k] or 0) + 1
        end
      end
      -- what the handlers reach for, kept apart from what the boot needs
      for i = nboot + 1, table.getn(CALLS) do
        PLAYED[CALLS[i]] = (PLAYED[CALLS[i]] or 0) + 1
      end
    end
  else
    FAILED[table.getn(FAILED) + 1] = key .. ": " .. tostring(err)
  end
end
io.write("===\\n")
for k, n in pairs(FIRED) do io.write("ev\\t" .. k .. "\\t" .. n .. "\\n") end
for i = 1, table.getn(BOOTED) do io.write("ok\\t" .. BOOTED[i] .. "\\n") end
for i = 1, table.getn(FAILED) do io.write("no\\t" .. FAILED[i] .. "\\n") end
for name, n in pairs(USED) do io.write("fn\\t" .. name .. "\\t" .. n .. "\\n") end
for name, n in pairs(PLAYED) do io.write("ev-fn\\t" .. name .. "\\t" .. n .. "\\n") end
for name, n in pairs(WANTED) do io.write("res\\t" .. name .. "\\t" .. n .. "\\n") end
"""


def checkpoints(script: Path) -> list[int]:
    """The checkpoint numbers a level script defines, in order."""
    return sorted({int(m) for m in
                   CHECKPOINT.findall(script.read_text(errors="replace"))})


def boot(tree: Path, override: Path | None, levels: list[int],
         events: bool = False, extra: dict | None = None) -> dict:
    """Run `level(n, cp, nil)` for every checkpoint of every level.

    One Lua process for all of them: preparing the script tree costs more
    than the boots do, and `level()` clears `Level` and `Save` itself, so
    the runs do not contaminate each other.
    """
    tmp = Path(tempfile.mkdtemp(prefix="goodomen-boot-"))
    roots = [d for d in (tree / "scripts", tree / "base", override)
             if d and d.is_dir()]
    luarun.prepare(roots, tmp, set())

    jobs = []
    for n in levels:
        script = tmp / f"level{n}.lua"
        if not script.is_file():
            continue
        jobs += [[n, cp] for cp in checkpoints(script)]
    if not jobs:
        raise SystemExit("no level scripts under " + str(tree))

    answers = "".join(f'ANSWERS["{k}"] = {v}\n'
                      for k, v in {**ANSWERS, **(extra or {})}.items())
    stubs = ("ANSWERS = {}\n" + answers + luarun.stub_source()
             + f"LUADIR = [[{tmp}]]\nUSED = {{}}\n"
             + f"EVENTS = {'1' if events else 'nil'}\n"
             + "JOBS = {" + ",".join(f"{{{n},{cp}}}" for n, cp in jobs) + "}\n")
    source = (tmp / "mdk2.lua").read_text()
    out = luarun.run(source + DRIVER, stubs)

    booted, failed, used, wanted, fired, played = [], [], {}, {}, {}, {}
    for line in out.split("===\n", 1)[-1].splitlines():
        kind, _, rest = line.partition("\t")
        if kind == "ok":
            booted.append(rest)
        elif kind == "no":
            failed.append(rest)
        elif kind in ("fn", "ev-fn", "res"):
            name, _, count = rest.partition("\t")
            {"fn": used, "ev-fn": played, "res": wanted}[kind][name] = int(count)
        elif kind == "ev":
            slot, _, count = rest.rpartition("\t")
            event, _, err = slot.partition("|")
            fired[(event, err)] = int(count)
    return {"jobs": jobs, "booted": booted, "failed": failed,
            "used": used, "wanted": wanted, "fired": fired, "played": played}


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("tree", type=Path, help="the extracted/ directory")
    ap.add_argument("--level", type=int, help="just this one")
    ap.add_argument("--api", action="store_true",
                    help="list the engine functions the boots called")
    ap.add_argument("--answer", action="append", default=[],
                    metavar="NAME=VALUE",
                    help="what an engine function returns, for the run")
    ap.add_argument("--events", action="store_true",
                    help="also fire every handler the boot leaves hanging on "
                         "the objects, and report what stops it")
    ap.add_argument("--resources", action="store_true",
                    help="check that every resource the boots demand exists")
    ap.add_argument("--expect-handlers", type=int, metavar="N",
                    help="fail unless exactly N handler calls survive")
    ap.add_argument("--expect", type=int, metavar="N",
                    help="fail unless exactly N checkpoints boot")
    args = ap.parse_args(argv)

    gog = os.environ.get("MDK2_GOG")
    over = Path(gog) / "override" if gog else None
    r = boot(args.tree, over if over and over.is_dir() else None,
             [args.level] if args.level else list(range(1, 11)), args.events,
             dict(a.split("=", 1) for a in args.answer))

    if args.events:
        by_event: dict[str, list[int]] = {}
        reasons: dict[str, int] = {}
        for (event, err), n in r["fired"].items():
            hit = by_event.setdefault(event, [0, 0])
            hit[0] += n
            hit[1] += n if not err else 0
            if err:
                reasons[re.sub(r"^.*?:\d+: ", "", err)] = \
                    reasons.get(re.sub(r"^.*?:\d+: ", "", err), 0) + n
        for event, (all_, ok) in sorted(by_event.items(),
                                        key=lambda kv: -kv[1][0]):
            print(f"{event:16s} {ok:5d}/{all_:<5d} run to the end")
        survived = sum(v[1] for v in by_event.values())
        print(f"\n{survived}/{sum(v[0] for v in by_event.values())} handler "
              f"calls survive; what stops the rest:")
        for why, n in sorted(reasons.items(), key=lambda kv: -kv[1])[:15]:
            print(f"  {n:5d}x  {why}")
        if args.expect_handlers and survived != args.expect_handlers:
            print(f"expected {args.expect_handlers} handler calls to survive",
                  file=sys.stderr)
            return 1

    if args.api:
        for name, n in sorted(r["used"].items(), key=lambda kv: -kv[1]):
            print(f"{n:6d}x  {name}")
    if args.resources:
        have = {p.name.lower() for p in args.tree.rglob("*") if p.is_file()}
        missing = sorted(n for n in r["wanted"] if n.lower() not in have)
        for n in missing[:20]:
            print(f"  missing: {n}", file=sys.stderr)
        print(f"{len(r['wanted']) - len(missing)}/{len(r['wanted'])} demanded "
              f"resources exist", file=sys.stderr)
        if missing:
            return 1
    for line in r["failed"]:
        print(f"  {line}", file=sys.stderr)
    print(f"{len(r['booted'])}/{len(r['jobs'])} checkpoints boot through "
          f"level(), {len(r['used'])} engine functions used"
          + (f", {len(set(r['played']) - set(r['used']))} more reached for by "
             f"the handlers" if args.events else ""), file=sys.stderr)
    if args.expect and len(r["booted"]) != args.expect:
        print(f"expected {args.expect}", file=sys.stderr)
        return 1
    return 1 if r["failed"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
