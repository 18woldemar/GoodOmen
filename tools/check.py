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
    ("every level starts at every checkpoint",
     ["boot.py", "extracted", "--resources", "--expect", "129",
      "--events", "--expect-handlers", "45559"],
     "scripts/level1.lua"),
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

# needs the game executable and unicorn, so it is opt-in
SLOW = [
    ("texture codec matches the original",
     ["texdec.py", "extracted/base", "--check"], "base"),
    ("the controller walks every level",
     ["walksim.py", "extracted/base", "--resources", "extracted", "--all",
      "--expect-standing", "2556"], "base"),
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
    p = subprocess.run([PYTHON, str(ROOT / "tools" / argv[0])]
                       + argv[1:], cwd=ROOT, capture_output=True, text=True,
                       env=_env())
    tail = (p.stderr or p.stdout or "").strip().splitlines()
    return p.returncode == 0, tail[-1] if tail else ""


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
        for label, cmd, needs in CORPUS + (SLOW if args.slow else []):
            cmd = [c.replace("$MDK2_GOG", env.get("MDK2_GOG", ""))
                   for c in cmd]
            if needs is None:                       # a path in the game dir
                if not Path(cmd[1]).exists():
                    print(f"skip  {label} -- no {cmd[1]}")
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
