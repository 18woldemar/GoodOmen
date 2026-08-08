#!/usr/bin/env python3
"""
health.py -- how much health everything in MDK2 has, out of the binary.

There is no health in the `.mod` files, none in the scene graphs, and no
constant anywhere in `mdk2Main.exe` that a search for a plausible number will
find. That is not because the game invents it: it is because health is **a
table scaled at run time**, and the two halves live apart.

**The table** is at 0x4ab2e8 -- 19 records of 0x88 bytes, ending at the first
whose type field is zero. Field 0 is the `OBJ_*` type, +0x04 a name string,
+0x3c the base hitpoints. The constructor at 0x42f2e0 walks it linearly for a
record matching the type it is building, hangs the name on the gob, and puts
+0x3c through the scaler.

**The scaler** is 0x42d020, which the scripts see as `mdkDiffScale`:

    max = (int)(2.0f * difficulty * base)
    if base > 0 and max == 0: max = 1        ; and -1 for a negative base

Two things in that are worth having and neither is guessable. The multiplier
is **twice** the difficulty, so the four settings `scripts/menu.lua` offers --
0.2, 0.35, 0.5 and 1.0, which `mdk2.str` calls Easy, Medium, Hard and
"Jinkies!" -- come out as 0.4x, 0.7x, **1x** and 2x. The table below is
therefore what you fight on Hard. And a non-zero base is never scaled away to
zero; it is pushed back off it, in whichever direction it went.

The difficulty itself is the global float at 0x4bb71c, and the game leaves it
**uninitialised** -- the bytes in the file are `00 ff ff 00`. `mdkSetDifficulty`
from the new-game menu is its only writer, so a level reached without passing
through that menu scales its enemies by whatever happened to be in that
memory. The engine defaults to Hard instead, which is ours and is marked so.

Reading the table is a plain PE read. Checking the scaler is not, so it is
run under emulation the way `tools/rand.py` runs the generator -- a research
oracle, not engine code.

Usage:
    python3 tools/health.py "$MDK2_GOG/mdk2Main.exe"
    python3 tools/health.py "$MDK2_GOG/mdk2Main.exe" --engine
"""

from __future__ import annotations

import argparse
import os
import struct
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import refdec  # noqa: E402

TABLE = 0x004AB2E8
STRIDE = 0x88          # each record; the walk at 0x42f2e0 adds it
KEY, NAME, BASE = 0x00, 0x04, 0x3C
DIFF_SCALE = 0x0042D020
DIFFICULTY_GLOBAL = 0x004BB71C

# `scripts/menu.lua`, the only caller of mdkSetDifficulty in the whole game,
# with the names from mdk2.str 680, 681, 682 and 685 -- not 683, which is
# "Configure Joystick".
DIFFICULTIES = [("Easy", 0.2), ("Medium", 0.35), ("Hard", 0.5),
                ("Jinkies!", 1.0)]


def _sections(exe: Path):
    import pefile
    pe = pefile.PE(str(exe), fast_load=True)
    base = pe.OPTIONAL_HEADER.ImageBase
    return [(base + s.VirtualAddress, s.get_data()) for s in pe.sections]


def _read(secs, va: int, n: int) -> bytes:
    for start, data in secs:
        if start <= va < start + len(data):
            return data[va - start:va - start + n]
    raise ValueError(f"{va:#x} is in no section")


# The locomotion columns of the same record. 0x42fd0d indexes the four speeds
# by the gait -- `def[0x18 + gait * 4]`, still/walk/run/back, the last one
# negative so a walk clip plays in reverse -- and beside them are the turn rate
# in radians a second (0x42fc5d multiplies it by the frame time) and the strafe
# speed (0x42fd11 multiplies it by `walker + 0x10`).
WALK = (0x18, 0x1C, 0x20, 0x24, 0x28, 0x2C)


def table(exe: Path) -> list[tuple[int, str, int, tuple[float, ...]]]:
    """-> `(type, name, base hitpoints, locomotion)`, in table order."""
    secs = _sections(exe)
    out = []
    while True:
        r = _read(secs, TABLE + len(out) * STRIDE, STRIDE)
        key = struct.unpack_from("<I", r, KEY)[0]
        if key == 0:                       # the terminator the walk stops on
            return out
        name = r[NAME:BASE].split(b"\0")[0].decode("latin1")
        out.append((key, name, struct.unpack_from("<i", r, BASE)[0],
                    tuple(struct.unpack_from("<f", r, o)[0] for o in WALK)))


def scale(exe: Path, bases: list[int]) -> dict[tuple[float, int], int]:
    """Run 0x42d020 for each difficulty and base. `-> {(d, base): result}`."""
    from unicorn.x86_const import UC_X86_REG_ESP, UC_X86_REG_EIP, UC_X86_REG_EAX

    d = refdec.RefDecoder(exe)
    uc = d.uc
    sp = refdec.STACK + refdec.STACK_SIZE - 0x1000

    out = {}
    for _, difficulty in DIFFICULTIES:
        # the menu writes this global and nothing else does
        uc.mem_write(DIFFICULTY_GLOBAL, struct.pack("<f", difficulty))
        for base in bases:
            uc.mem_write(sp, struct.pack("<Ii", refdec.RET_MAGIC, base))
            uc.reg_write(UC_X86_REG_ESP, sp)
            uc.reg_write(UC_X86_REG_EIP, DIFF_SCALE)
            uc.emu_start(DIFF_SCALE, refdec.RET_MAGIC)
            out[(difficulty, base)] = struct.unpack(
                "<i", struct.pack("<I", uc.reg_read(UC_X86_REG_EAX) & 0xFFFFFFFF))[0]
    return out


# The shot table, found the same way the enemy table was: a run of plausible
# `OBJ_*` ids at a fixed stride. 69 records of 0x58, and `mdkBullet.c` --
# whose path string sits at 0x498f1c -- reads every field kept here.
SHOTS = 0x00497388
SHOT_STRIDE = 0x58
SHOT_COLUMNS = ((0x04, "<i"), (0x08, "<i"), (0x20, "<f"), (0x2C, "<f"))


def bullets(args) -> int:
    """Compare the engine's shot table against the binary's, column by column."""
    secs = _sections(args.exe)
    want = []
    i = 0
    while True:
        r = _read(secs, SHOTS + i * SHOT_STRIDE, SHOT_STRIDE)
        key = struct.unpack_from("<I", r, 0)[0]
        if not 100 <= key <= 999:
            break
        model = _read(secs, struct.unpack_from("<I", r, 0x30)[0], 40)
        row = [str(key), model.split(b"\0")[0].decode("latin1")]
        for off, fmt in SHOT_COLUMNS:
            v = struct.unpack_from(fmt, r, off)[0]
            row.append("%g" % v)
        want.append(row)
        i += 1

    if not args.engine:
        print("type model damage_type damage life speed")
        for row in want:
            print(" ".join(row))
        return 0

    out = subprocess.run(
        ["cargo", "run", "--quiet", "--release", "--manifest-path",
         str(Path(__file__).resolve().parent.parent / "engine/Cargo.toml"),
         "--", "--bullets"],
        capture_output=True, text=True, check=True).stdout
    got = [l.split() for l in out.strip().splitlines() if l.strip()]
    if got != want:
        for i, (a, b) in enumerate(zip(got, want)):
            if a != b:
                print(f"MISMATCH at shot {i}: the engine says {a}, "
                      f"the original {b}", file=sys.stderr)
                break
        print(f"{len(want)} shots from the binary, {len(got)} from the engine, "
              "and they differ", file=sys.stderr)
        return 1
    hardest = max(want, key=lambda r: int(r[3]))
    fastest = max(want, key=lambda r: float(r[5]))
    print(f"{len(want)} shot types, 4 columns each, identical to the original "
          f"({hardest[1]} {hardest[3]} damage and {fastest[1]} "
          f"{fastest[5]} a second the most)", file=sys.stderr)
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    # positional, because `check.py` decides whether a tool can run by asking
    # whether its first argument is a path that exists
    ap.add_argument("exe", type=Path, nargs="?",
                    default=Path(os.environ.get("MDK2_GOG", ".")) / "mdk2Main.exe",
                    help="mdk2Main.exe (GOG)")
    ap.add_argument("--bullets", action="store_true",
                    help="the shot table at 0x497388 instead of the enemy "
                         "table, and compare that")
    ap.add_argument("--engine", action="store_true",
                    help="compare the engine's own table against this one")
    args = ap.parse_args(argv)

    if not args.exe.is_file():
        print(f"skip: no {args.exe}", file=sys.stderr)
        return 0

    if args.bullets:
        return bullets(args)

    rows = table(args.exe)
    # bases too small for the doubling to survive truncation. The table has no
    # record that reaches them, so without these the rule that keeps a
    # non-zero base off zero would go unchecked on both sides.
    probes = [1, -1, -3]
    scaled = scale(args.exe, [b for _, _, b, _ in rows] + probes)

    def line(key, name, base, walk):
        return ([str(key), name, str(base)]
                + [str(scaled[(d, base)]) for _, d in DIFFICULTIES]
                + [("%g" % v if v != "-" else "-") for v in walk])

    want = ([line(k, n, b, w) for k, n, b, w in rows]
            + [line("-", "-", b, ["-"] * len(WALK)) for b in probes])

    if not args.engine:
        print("type name base " + " ".join(n for n, _ in DIFFICULTIES))
        for row in want:
            print(" ".join(row))
        return 0

    out = subprocess.run(
        ["cargo", "run", "--quiet", "--release", "--manifest-path",
         str(Path(__file__).resolve().parent.parent / "engine/Cargo.toml"),
         "--", "--health"] + [str(b) for b in probes],
        capture_output=True, text=True, check=True).stdout
    got = [l.split() for l in out.strip().splitlines() if l.strip()]

    if got != want:
        for i, (a, b) in enumerate(zip(got, want)):
            if a != b:
                print(f"MISMATCH at record {i}: the engine says {a}, "
                      f"the original {b}", file=sys.stderr)
                break
        print(f"{len(want)} rows from the binary, {len(got)} from the engine, "
              "and they differ", file=sys.stderr)
        return 1

    hardest = max(rows, key=lambda r: r[2])
    fastest = max(rows, key=lambda r: r[3][2])
    print(f"{len(rows)} enemy types and {len(probes)} bare bases at "
          f"{len(DIFFICULTIES)} difficulties, plus {len(WALK)} locomotion "
          f"columns, identical to the original "
          f"({hardest[1]} {hardest[2]} hitpoints and {fastest[1]} "
          f"{fastest[3][2]:g} a second the most)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
