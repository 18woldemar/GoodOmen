#!/usr/bin/env python3
"""
rand.py -- the game's own random numbers, run under emulation.

`mdk2.lua` calls `chSeedRand(127)` on every level start, so the original's
encounters are reproducible: the same checkpoint gives the same spawns and the
same taunts every time you play it. Reproducing that *sequence* is part of
reproducing the game.

It is **MT19937**, and the 1998 edition. `chRand` (0x41ccb0) hands its work to
0x452a80, whose tempering is unmistakable once the compiler's rewriting is
undone -- `and eax, 0xff3a58ad; shl eax, 7` is `(y << 7) & 0x9d2c5680` with
the low seven bits masked off early. The seeding at 0x452920 settles the
edition: `mt[0] = seed | 1` and `mt[i] = 69069 * mt[i-1]`, which is Matsumoto
and Nishimura's `sgenrand` of 1998, not the 2002 replacement.

This is a **research oracle, not engine code** -- the same standing as
`tools/refdec.py`, and for the same reason: the routines are pure, so mapping
the PE and calling them is enough. What it produces is the sequence
`engine/src/game/rand.rs` is checked against.

Usage:
    python3 tools/rand.py --seed 127 --count 8
    python3 tools/rand.py --engine "$MDK2_GOG"     # against the engine's
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

SEED = 0x00452920
NEXT = 0x00452A80
# The two places 0x452a80 has finished tempering and is about to convert the
# result to a double: one on the path that reloads the block, one on the path
# that takes the next word out of it. Stopping there and reading the stack
# gets the raw 32-bit value without going through the x87 registers, which
# unicorn hands back in a form that varies by version.
FILD = (0x00452AA1, 0x00452AF4)


def sequence(exe: Path, seed: int, count: int) -> list[int]:
    """-> `count` raw 32-bit outputs after seeding with `seed`."""
    from unicorn.x86_const import UC_X86_REG_ESP, UC_X86_REG_EIP

    d = refdec.RefDecoder(exe)
    uc = d.uc
    sp = refdec.STACK + refdec.STACK_SIZE - 0x1000

    uc.mem_write(sp, struct.pack("<2I", refdec.RET_MAGIC, seed))
    uc.reg_write(UC_X86_REG_ESP, sp)
    uc.reg_write(UC_X86_REG_EIP, SEED)
    uc.emu_start(SEED, refdec.RET_MAGIC)

    out = []
    for _ in range(count):
        uc.mem_write(sp, struct.pack("<I", refdec.RET_MAGIC))
        uc.reg_write(UC_X86_REG_ESP, sp)
        uc.reg_write(UC_X86_REG_EIP, NEXT)
        stopped = {}

        def at_fild(uc_, address, size, _user, stopped=stopped):
            if address in FILD:
                stopped["esp"] = uc_.reg_read(UC_X86_REG_ESP)
                uc_.emu_stop()

        from unicorn import UC_HOOK_CODE
        handle = uc.hook_add(UC_HOOK_CODE, at_fild)
        uc.emu_start(NEXT, refdec.RET_MAGIC)
        uc.hook_del(handle)
        if "esp" not in stopped:
            raise RuntimeError("0x452a80 did not reach its conversion")
        out.append(struct.unpack("<I", uc.mem_read(stopped["esp"], 4))[0])
    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("--exe", type=Path,
                    default=Path(os.environ.get("MDK2_GOG", ".")) / "mdk2Main.exe")
    ap.add_argument("--seed", type=int, default=127,
                    help="127 is what every level start uses")
    ap.add_argument("--count", type=int, default=8)
    ap.add_argument("--engine", metavar="GAMEDIR",
                    help="compare the engine's sequence against this one")
    args = ap.parse_args(argv)

    if not args.exe.is_file():
        print(f"skip: no {args.exe}", file=sys.stderr)
        return 0

    want = sequence(args.exe, args.seed, args.count)
    if not args.engine:
        for v in want:
            print(v)
        return 0

    out = subprocess.run(
        ["cargo", "run", "--quiet", "--release", "--manifest-path",
         str(Path(__file__).resolve().parent.parent / "engine/Cargo.toml"),
         "--", "--rand", str(args.seed), str(args.count)],
        capture_output=True, text=True, check=True).stdout
    got = [int(v) for v in out.split()]
    if got != want:
        for i, (a, b) in enumerate(zip(got, want)):
            if a != b:
                print(f"MISMATCH at {i}: the engine says {a}, the original {b}",
                      file=sys.stderr)
                break
        print(f"{len(want)} numbers, the sequences differ", file=sys.stderr)
        return 1
    print(f"{len(want)} numbers from seed {args.seed}, identical to the original",
          file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
