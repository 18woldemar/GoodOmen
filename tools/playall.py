#!/usr/bin/env python3
"""playall.py -- open a window on every level and check each one plays.

The pinned window checks are two checkpoints of two levels, which is enough to
catch a change and not enough to catch a level. This walks all ten through the
same path a person uses -- `--play L 1 --window --for N` -- and fails if any
of them does not reach its own summary line.

It is slow (llvmpipe under Xvfb renders a level at a few frames a second), so
it lives in `check.py`'s `--slow` list.

Usage:
    python3 playall.py extracted --run "/path/to/MDK 2" [--seconds 8]
"""
import argparse
import os
import pathlib
import shutil
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
ENGINE = HERE.parent / "engine" / "Cargo.toml"


def headless() -> list[str]:
    """`xvfb-run` when there is no display, the way check.py does it."""
    if os.environ.get("DISPLAY") or not shutil.which("xvfb-run"):
        return []
    return ["xvfb-run", "-a", "-s", "-screen 0 1024x768x24"]


def main(argv=None) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("resources", type=pathlib.Path)
    ap.add_argument("--run", required=True, help="the game directory")
    ap.add_argument("--seconds", type=int, default=8)
    args = ap.parse_args(argv)

    played, faults = 0, []
    for level in range(1, 11):
        out = subprocess.run(
            headless()
            + ["cargo", "run", "--quiet", "--release", "--manifest-path",
               str(ENGINE), "--", args.run, "--play", str(level), "1",
               "--window", "--for", str(args.seconds)],
            capture_output=True, text=True)
        line = next((l for l in out.stdout.splitlines()
                     if l.startswith(f"l{level} cp1:") and "ran " in l), None)
        if line is None:
            faults.append(f"l{level}: no summary")
            continue
        played += 1
        # a level that draws nothing is a level that did not load
        if " 0 triangles" in line:
            faults.append(f"l{level}: drew nothing")
        # and one whose player has no hitpoints has no player
        if " you on 0 of 0 hitpoints" in line:
            faults.append(f"l{level}: nobody is being played")

    print(f"{played} of 10 levels played {args.seconds}s in a window"
          + (f", {len(faults)} faults" if faults else ", no faults"))
    for f in faults:
        print("  " + f)
    if played != 10 or faults:
        print(f"{played} played, {len(faults)} faults", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
