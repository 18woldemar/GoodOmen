#!/usr/bin/env python3
"""
check.py -- run every check the project has, and say which ones ran.

There are two kinds. The **self-tests** need nothing but Python: they cover
the arithmetic that can be wrong quietly -- the texture block layouts, the
quaternion interpolation, the Lua 3 rewrites, the scene-graph parser. The
**corpus checks** need the game extracted, and they are the ones that carry
the project's rule that a format counts as solved only at 100%.

Anything whose inputs are missing is reported as skipped rather than silently
passed, because a green run that checked nothing is the failure mode worth
guarding against.

Usage:
    python3 tools/check.py                 # everything available
    python3 tools/check.py --quick         # self-tests only
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXTRACTED = ROOT / "extracted"

SELFTESTS = ["scene", "mod2obj", "mod2html", "texdec", "luaapi", "luarun",
             "rooms"]

# (name, argv, what has to exist first)
CORPUS = [
    ("models parse", ["mod2obj.py", "extracted/base", "--stats"],
     "base"),
    # the animation key channel (target kind 23): what a model fires, shakes
    # and spawns as it plays. 229 of the 2207 models carry one, and 819 of the
    # 1313 keys create an object -- which is where an enemy's shot comes from.
    ("animation keys carry what a model fires",
     ["mod2obj.py", "extracted/base", "--keys", "--expect-keys", "1313"],
     "base"),
    ("collision trees validate", ["bsp.py", "extracted/base", "--validate"],
     "base"),
    ("scene graphs validate",
     ["scene.py", "extracted/base", "--validate", "--resources", "extracted"],
     "base"),
    ("scene graphs run under Lua",
     ["luarun.py", "extracted/base", "--crosscheck"], "base"),
    ("scripts compile", ["luarun.py", "extracted/scripts", "--compile"],
     "scripts"),
    ("string table is byte-exact",
     ["strfile.py", "extracted/local/mdk2.str"], "local/mdk2.str"),
    ("star catalogue parses", ["stars.py", "extracted/base/stars.sta"],
     "base/stars.sta"),
    ("recorded demo parses", ["omn.py", "extracted/base/demo1_5.omn"],
     "base/demo1_5.omn"),
    ("sound headers are WAVC over Interplay ACM",
     ["wavc.py", "extracted", "--validate"], "sounds"),
    ("five language tables parse",
     ["strfile.py", "$MDK2_GOG/override/english/mdk2.str", "--compare",
      "$MDK2_GOG/override/french/mdk2.str",
      "$MDK2_GOG/override/german/mdk2.str",
      "$MDK2_GOG/override/italian/mdk2.str",
      "$MDK2_GOG/override/spanish/mdk2.str"], None),
    ("music playlists parse", ["wavc.py", "$MDK2_GOG/Music", "--playlists"],
     None),
    ("checkpoints stand in open space",
     ["spawn.py", "extracted", "--all", "--expect", "128"],
     "scripts/level1.lua"),
    # the same body test over the waypoints instead, five times as many of
    # them. 39 of the 625 sit inside a tree, which is the number a walker's
    # collision has to survive before it can be a point query -- see the
    # docstring on `waypoints()`.
    # the map of the binary: every assert pushes its own source path, so the
    # push sites bracket the file. 39 of them, from mdkAI.c to omConsole.c.
    ("the binary still says which file each function came from",
     ["exe_recon.py", "$MDK2_GOG/mdk2Main.exe", "--files",
      "--expect-files", "39"], None),
    ("waypoints stand where a body would fit",
     ["spawn.py", "extracted", "--all", "--waypoints", "--expect", "586"],
     "scripts/level1.lua"),
    ("every level starts at every checkpoint",
     ["boot.py", "extracted", "--resources", "--expect", "129",
      "--events", "--expect-handlers", "8690"],
     "scripts/level1.lua"),
    ("the shader poses the animated objects like mod2obj",
     ["mod2html.py", "--scene", "extracted/base/l1.lua", "--resources",
      "extracted", "--movers"], "base"),
    ("key bindings are DirectInput scancodes",
     ["walksim.py", "extracted/base/l1.lua", "--resources", "extracted",
      "--keys"], "scripts/defaultkeys.lua"),
    ("the recorded demo replays without leaving the world",
     ["walksim.py", "extracted/base/l1.lua", "--resources", "extracted",
      "--demo", "extracted/base/demo1_5.omn"], "base/demo1_5.omn"),
    ("room graphs resolve",
     ["rooms.py", "extracted", "--check", "--expect", "823"],
     "scripts/level1.lua"),
]

# The engine is Rust and the tools are Python, and where both can do a thing
# they must agree. Skipped when cargo is not installed.
ENGINE = [
    ("the engine reads every container",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG"], None),
    ("the engine decodes every texture",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--tex", "--expect", "761"], None),
    ("the engine's models agree with mod2obj.py",
     ["modcheck.py", "extracted", "--run", "$MDK2_GOG"], "base"),
    ("the engine's collision trees agree with bsp.py",
     ["bsp.py", "extracted", "--engine", "$MDK2_GOG"], "base"),
    ("the Lua constants come out of the binary",
     ["luaconst.py", "--expect", "507", "--coverage", "extracted/scripts",
      "extracted/base", "--expect-undefined", "3"], "scripts"),
    ("the engine compiles every shipped script",
     ["luarun.py", "extracted", "--engine", "$MDK2_GOG",
      "--override", "$MDK2_GOG/override"], "scripts"),
    ("the engine runs every scene graph",
     ["scene.py", "extracted/base", "--engine", "$MDK2_GOG"], "base"),
    ("the renderer draws its first triangle",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "--triangle"], None),
    ("the mixer attenuates by the game's own distance model",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "--sound"], None),
    ("the music streams, loops and reaches the mixer",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "--music", "$MDK2_GOG",
      "--expect", "27"], None),
    ("the engine decodes every sound",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--wav", "--expect", "998"], None),
    ("every sound decodes exactly like ffmpeg",
     ["acmcheck.py", "extracted", "--run", "$MDK2_GOG"], "bin:ffmpeg"),
    ("the music decodes exactly like ffmpeg",
     ["acmcheck.py", "$MDK2_GOG/Music", "--music"], "bin:ffmpeg"),
    ("the engine starts every level at every checkpoint",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG", "--boot",
      "--expect", "129", "--expect-resources", "2093",
      "--expect-rooms", "677", "--expect-bindings", "59",
      "--events", "--expect-events", "8836",
      "--expect-survived", "8690", "--expect-plays", "253",
      "--expect-spawned", "152", "--expect-armed", "152",
      "--expect-destroyed", "19881", "--expect-roomless", "0",
      "--expect-alerted", "243"], None),
    ("the engine's controller replays the demo like walksim.py",
     ["walksim.py", "extracted/base/l1.lua", "--resources", "extracted",
      "--demo", "extracted/base/demo1_5.omn", "--engine", "$MDK2_GOG"],
     "base/demo1_5.omn"),
    ("the engine runs a level, driver and all",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--run", "1", "5", "45", "--expect-rooms", "1",
      "--expect-events", "18900", "--expect-survived", "17550",
      "--expect-touched", "1"], None),
    ("a run reaches a spawner and the enemies arrive with hitpoints",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--run", "2", "1", "30", "--expect-spawned", "17"], None),
    # level 7's task lists jump the sniper pilots onto their perches
    # (`{ mdkWalkerJumpToPoint, { "l7r6pilot12", 50 } }`), which is the first
    # thing in the game that moves a gob the player is not standing in.
    # ...and its walkers walk and its shots fly: 1826 object moves against the 901 a run makes
    # when only the player is moving, all of it non-player gobs turning toward
    # a waypoint and running at their own type's speed out of 0x4ab2e8.
    ("a run launches a walker along the arc the original solves for",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--run", "7", "1", "30", "--expect-jumps", "2",
      "--expect-keys", "0", "--expect-fighting", "1",
      "--expect-moves", "1826",
      "--expect-events", "8104", "--expect-survived", "8104"], None),
    # level 10's zizzy turrets shoot: nine bullets in thirty seconds, each one
    # carrying its damage, damage type, lifetime and speed out of the shot
    # table at 0x497388 rather than out of the call.
    ("a run fires shots that carry the table's own numbers",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--run", "10", "1", "30", "--expect-shots", "9",
      "--expect-events", "2751", "--expect-survived", "2751"], None),
    # and a shot reaches the player. `--hunt` steers the driver at the
    # nearest thing with hitpoints instead of holding forwards, which is what
    # it takes to get inside a turret's range at all: level 10's zizzy
    # turrets fire twelve and one lands.
    ("a shot fired in a level hits the player",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--run", "10", "1", "40", "--hunt", "--expect-shots", "12",
      "--expect-hits", "1", "--expect-events", "3667",
      "--expect-survived", "3667"], None),
    ("walking drives the player's own animation, and reaches the scripts",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--run", "6", "1", "40", "--expect-playing", "2",
      "--expect-moves", "1201", "--expect-touched", "1"], None),
    ("the room graph culls what the engine draws",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--play", "1", "1", "--expect-drawn", "14038",
      "--expect-sounds", "6"], None),
    ("the renderer draws a level",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--level", "l1.lua"], None),
    ("the Windows build runs the same",
     ["sh", "tools/winbuild.sh"], None),
    ("the engine's own tests pass",
     ["cargo", "test", "--lib", "--manifest-path", "engine/Cargo.toml"],
     None),
]

# needs the game executable and unicorn, so it is opt-in
SLOW = [
    ("texture codec matches the original",
     ["texdec.py", "extracted/base", "--check"], "base"),
    ("the engine's texture codec matches texdec.py",
     ["sh", "tools/texcheck.sh"], "base"),
    ("the random numbers are the original's",
     ["rand.py", "$MDK2_GOG/mdk2Main.exe", "--engine", "$MDK2_GOG",
      "--count", "2000"], None),
    ("the enemy health table is the original's",
     ["health.py", "$MDK2_GOG/mdk2Main.exe", "--engine"], None),
    ("the shot table is the original's",
     ["health.py", "$MDK2_GOG/mdk2Main.exe", "--bullets", "--engine"], None),
    # the fourth table: nine AI behaviours, and which of the 19 enemy types
    # index one. Kept in the tool rather than the engine, because nothing in
    # the engine reads it yet.
    ("nine AI behaviours, and nine enemies with none",
     ["health.py", "$MDK2_GOG/mdk2Main.exe", "--ai", "--expect-ai", "10"],
     None),
    ("the item table is the original's",
     ["health.py", "$MDK2_GOG/mdk2Main.exe", "--items", "--engine"], None),
    ("the controller walks every level",
     ["walksim.py", "extracted/base", "--resources", "extracted", "--all",
      "--expect-standing", "2556", "--expect-inside", "6"], "base"),
]


# the slow check imports unicorn, which lives in the project venv
VENV = ROOT / ".venv" / "bin" / "python"
PYTHON = str(VENV) if VENV.is_file() else sys.executable


def _env() -> dict:
    """The shell environment plus whatever .env.local sets, which is where
    MDK2_GOG lives and is not committed."""
    import os
    import re
    env = dict(os.environ)
    local = ROOT / ".env.local"
    if local.is_file():
        for line in local.read_text().splitlines():
            m = re.match(r'\s*(?:export\s+)?(\w+)\s*=\s*"?([^"#]*)"?', line)
            if m and m.group(2).strip():
                env.setdefault(m.group(1), m.group(2).strip())
    return env


def _run(argv: list[str]) -> tuple[bool, str]:
    # a `.py` name is one of our tools; anything else is a command, which is
    # how the Rust engine gets checked against the Python that defines it
    cmd = ([PYTHON, str(ROOT / "tools" / argv[0])] + argv[1:]
           if argv[0].endswith(".py") else argv)
    p = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True,
                       env=_env())
    out = (p.stderr or "") + (p.stdout or "")
    lines = [l for l in out.strip().splitlines() if l.strip()]
    # cargo prints a result line per target; the one that ran the tests is
    # the one worth showing
    real = [l for l in lines if l.startswith("test result:") and "0 passed" not in l]
    return p.returncode == 0, (real or lines or [""])[-1]


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("--quick", action="store_true",
                    help="self-tests only, no game files needed")
    ap.add_argument("--slow", action="store_true",
                    help="also check the texture codec against the emulated "
                         "original, which needs mdk2Main.exe and unicorn")
    args = ap.parse_args(argv)

    passed = failed = skipped = 0
    for name in SELFTESTS:
        ok, line = _run([f"{name}.py", "--selftest"])
        print(f"{'ok  ' if ok else 'FAIL'}  {name}.py self-test"
              + ("" if ok else f"\n        {line}"))
        passed += ok
        failed += not ok

    if not args.quick:
        env = _env()
        exe = Path(env.get("MDK2_GOG", "")) / "mdk2Main.exe"
        import shutil
        engine = ENGINE if shutil.which("cargo") else []
        if not engine:
            print("skip  the engine -- cargo is not installed")
            skipped += len(ENGINE)
        for label, raw, needs in CORPUS + engine + (SLOW if args.slow else []):
            cmd = [c.replace("$MDK2_GOG", env.get("MDK2_GOG", ""))
                   for c in raw]
            # a check that cannot run has not found anything wrong, so
            # anything missing is a skip and never a failure
            if "$MDK2_GOG" in " ".join(raw) and not exe.is_file():
                print(f"skip  {label} -- MDK2_GOG is not set to an "
                      "installation")
                skipped += 1
                continue
            if needs is None:
                # a tool given a path in the game directory; a command that
                # is not one of our tools brings its own inputs
                if cmd[0].endswith(".py"):
                    # This rule assumes the tool's first argument is that
                    # path. When it is a flag instead, the check is
                    # misconfigured and would skip for ever without anyone
                    # noticing -- which is exactly what happened to
                    # `rand.py` the first time it was added here.
                    if cmd[1].startswith("-"):
                        print(f"FAIL  {label} -- its first argument is "
                              f"{cmd[1]}, not a path this can test for")
                        failed += 1
                        continue
                    if not Path(cmd[1]).exists():
                        print(f"skip  {label} -- no {cmd[1]}")
                        skipped += 1
                        continue
            elif needs.startswith("bin:"):
                if not shutil.which(needs[4:]):
                    print(f"skip  {label} -- {needs[4:]} is not installed")
                    skipped += 1
                    continue
            elif not (EXTRACTED / needs).exists():
                print(f"skip  {label} -- no extracted/{needs}")
                skipped += 1
                continue
            if (label, exe.is_file()) == (SLOW[0][0], False):
                print(f"skip  {label} -- no {exe}")
                skipped += 1
                continue
            t = time.monotonic()
            ok, line = _run(cmd)
            print(f"{'ok  ' if ok else 'FAIL'}  {label} "
                  f"({time.monotonic() - t:.1f}s)\n        {line}")
            passed += ok
            failed += not ok

    print(f"\n{passed} passed, {failed} failed, {skipped} skipped")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
