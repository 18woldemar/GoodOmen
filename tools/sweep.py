#!/usr/bin/env python3
"""sweep.py -- run every checkpoint of every level and add up what is lost.

One checkpoint of one level always looks healthy. The faults that matter here
are the size of a single object, and the only way they show is to ask the
whole game the same question at once: 129 runs of sixty seconds is about half
a minute of machine time.

Two numbers come back:

  * **bodies that leave the world**, by name. The counter in the engine's own
    report includes the player, and on 24 of the 129 checkpoints the player
    has no floor to begin with -- so the names are the signal and the count is
    not. See `spawncheck.py` for the floorless list.
  * **walker frames finished inside geometry**, worst object first.

Usage:
    python3 sweep.py extracted --run "/path/to/MDK 2" [--expect-lost 23]
"""
import argparse
import collections
import pathlib
import re
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
ENGINE = HERE.parent / "engine" / "Cargo.toml"

LOST = re.compile(r"\((\d+) left the world(?:: ([a-z0-9_ ]+))?\)")
INSIDE = re.compile(r"\((\d+) inside(?:: ([a-z0-9_: ]+))?\)")


def one(exe_root: str, level: int, checkpoint: int, seconds: int):
    """A single run, or None when the level has no such checkpoint."""
    out = subprocess.run(
        ["cargo", "run", "--quiet", "--release", "--manifest-path", str(ENGINE),
         "--", exe_root, "--run", str(level), str(checkpoint), str(seconds),
         "--roam"],
        capture_output=True, text=True)
    for line in out.stdout.splitlines():
        if line.startswith(f"l{level} cp{checkpoint}:"):
            return line
    return None


def main(argv=None) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("resources", type=pathlib.Path)
    ap.add_argument("--run", required=True, help="the game directory")
    ap.add_argument("--seconds", type=int, default=60)
    ap.add_argument("--expect-lost", type=int, default=None,
                    help="fail unless exactly this many named bodies leave")
    args = ap.parse_args(argv)

    lost = collections.Counter()
    inside = collections.Counter()
    runs = 0
    for level in range(1, 11):
        for checkpoint in range(1, 21):
            line = one(args.run, level, checkpoint, args.seconds)
            if line is None:
                continue
            runs += 1
            m = LOST.search(line)
            if m and m.group(2):
                for name in m.group(2).split():
                    lost[name] += 1
            m = INSIDE.search(line)
            if m and m.group(2):
                for pair in m.group(2).split():
                    name, _, frames = pair.partition(":")
                    inside[name] += int(frames or 0)

    print(f"{runs} checkpoints of {args.seconds}s: {len(lost)} bodies leave "
          f"the world, {len(inside)} finish a frame inside geometry")
    print("  leave:  " + ", ".join(f"{n} x{c}" for n, c in lost.most_common())
          or "  leave:  none")
    print("  inside: " + ", ".join(f"{n}:{c}" for n, c in inside.most_common(6))
          or "  inside: none")
    if args.expect_lost is not None and len(lost) != args.expect_lost:
        print(f"{len(lost)} bodies leave the world, expected "
              f"{args.expect_lost}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
