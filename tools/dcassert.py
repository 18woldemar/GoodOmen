#!/usr/bin/env python3
"""
dcassert.py -- the asserts BioWare shipped in the Dreamcast build.

The three PC editions compile their asserts out. The **Dreamcast 1.100 US**
build does not, and every surviving site is a call of the shape

    chAssert("<the condition, as written in the source>", __FILE__, __LINE__)

so the binary carries a few hundred lines of BioWare's own source text:
identifiers, struct fields, macro names and line numbers. That is the largest
body of engine vocabulary in any of the four editions, and it is what lets a
field found by offset in the GOG build be given the name its author gave it.

**How the arguments are recovered.** SH-4 has no 32-bit immediates: a constant
lives in a literal pool near the code and is loaded with

    mov.l @(disp,pc),Rn      1101 nnnn dddddddd   ((pc + 4) & ~3) + disp * 4
    mov.w @(disp,pc),Rn      1001 nnnn dddddddd   (pc + 4) + disp * 2
    mov   #imm,Rn            1110 nnnn iiiiiiii   sign-extended

and the first four arguments go in r4..r7. So walking the code linearly while
tracking r4, r5 and r6 through those three forms is enough -- no disassembler
is needed, and none of the float instructions have to be understood.

Two details decide whether the scan is right:

  - **A `jsr` has a delay slot**, and the compiler routinely parks the last
    argument in it. The slot has to be executed before the call is read.
  - **A register still holding a value from before the previous call is not
    this call's argument.** Clearing the three at every call and every `rts`
    is what separates 277 real sites from the ~1700 a carrying scan invents.

Recognising a site needs no knowledge of where chAssert lives: r5 pointing at
one of the 42 `*.c` strings in the image is the signature, and those strings
exist because __FILE__ was used.

The same scan yields the **module map**: the filename strings are referenced
in strictly increasing address order with no interleaving, so each module's
code range is bracketed by its first and last reference. The Dreamcast link
order is the source order.

**What this tool is not for.** rizin 0.8.2's SH plugin decodes the integer ISA
but not the FPU, so every float routine reads as `invalid`. The Dreamcast
build is a legend for the GOG build, never a source of behaviour -- and the
project's rule that the engine is written only from `mdk2Main.exe` stands.

Usage:
    python3 tools/dcassert.py 1ST_READ.BIN            # the corpus
    python3 tools/dcassert.py 1ST_READ.BIN --map      # module ranges only
    python3 tools/dcassert.py --selftest              # arithmetic only
"""

from __future__ import annotations

import argparse
import re
import struct
import sys
from collections import defaultdict

# A Dreamcast 1ST_READ.BIN is loaded at the start of system RAM, uncached.
BASE = 0x8C010000

# Past this the image is data: strings, tables and the ASCII-art debug font.
# The last code reference to a filename is rendermodel.c at 0x8c06a7a4.
CODE_END = 0x6F000

FILENAME = re.compile(rb"[A-Za-z0-9_]{3,30}\.(?:c|cpp|h)\x00")


def pc_long(va: int, op: int) -> int:
    """Where `mov.l @(disp,pc),Rn` at va reads. The pc is 4 ahead, aligned."""
    return ((va + 4) & ~3) + (op & 0xFF) * 4


def pc_word(va: int, op: int) -> int:
    """Where `mov.w @(disp,pc),Rn` at va reads. No alignment for a word."""
    return (va + 4) + (op & 0xFF) * 2


class Image:
    def __init__(self, data: bytes):
        self.data = data
        self.files = {BASE + m.start(): m.group()[:-1].decode()
                      for m in FILENAME.finditer(data)}

    def long(self, va: int) -> int:
        return struct.unpack_from("<I", self.data, va - BASE)[0]

    def short(self, va: int) -> int:
        return struct.unpack_from("<h", self.data, va - BASE)[0]

    def cstr(self, va: int) -> str | None:
        off = va - BASE
        if not 0 <= off < len(self.data):
            return None
        end = self.data.find(b"\0", off)
        if end < 0 or end - off > 400:
            return None
        try:
            return self.data[off:end].decode("ascii")
        except UnicodeDecodeError:
            return None

    def asserts(self) -> list[tuple[str, int, int, str]]:
        """(file, line, call address, condition) for every surviving site."""
        reg: dict[int, int] = {}
        out = []

        def load(va: int, op: int) -> None:
            n = (op >> 8) & 0xF
            if op >> 12 == 0xD:
                reg[n] = self.long(pc_long(va, op))
            elif op >> 12 == 0x9:
                reg[n] = self.short(pc_word(va, op)) & 0xFFFFFFFF
            elif op >> 12 == 0xE:
                reg[n] = struct.unpack("b", bytes([op & 0xFF]))[0] & 0xFFFFFFFF

        off, end = 0, min(len(self.data), CODE_END)
        while off < end - 2:
            va = BASE + off
            op = struct.unpack_from("<H", self.data, off)[0]
            if (op & 0xF0FF) == 0x400B or op == 0x000B:      # jsr @Rn / rts
                load(va + 2, struct.unpack_from("<H", self.data, off + 2)[0])
                name = self.files.get(reg.get(5))
                text = self.cstr(reg.get(4, 0))
                line = reg.get(6, 0)
                if name and text:
                    out.append((name, line if 0 < line < 20000 else 0,
                                va, text))
                reg.clear()
                off += 4
                continue
            load(va, op)
            off += 2
        return out

    def modules(self) -> list[tuple[str, int, int, int]]:
        """(file, first reference, last reference, references) per module."""
        seen = defaultdict(list)
        for off in range(0, len(self.data) - 4, 4):
            name = self.files.get(struct.unpack_from("<I", self.data, off)[0])
            if name:
                seen[name].append(BASE + off)
        return sorted(((n, v[0], v[-1], len(v)) for n, v in seen.items()),
                      key=lambda r: r[1])


def selftest() -> None:
    """The address arithmetic, and one hand-built call of each shape."""
    # 0x8c05f278 in the retail image is `49d5`, a long load of displacement
    # 0x49; the pool slot it reaches holds the pointer to "omPhysics.c".
    assert pc_long(0x8C05F278, 0xD549) == 0x8C05F3A0
    # The odd instruction two bytes earlier aligns down before it adds.
    assert pc_long(0x8C05F276, 0xD44B) == 0x8C05F3A4
    # A word load does not align, and steps by two.
    assert pc_word(0x8C056F0C, 0x9339) == 0x8C056F82

    # The pool sits at 0x40, the condition at 0x60 and the filename at 0x70,
    # so every hand-built call below can use the same two displacements.
    def build(*words: int) -> bytes:
        code = b"".join(struct.pack("<H", w) for w in words).ljust(0x40, b"\0")
        code += struct.pack("<2I", BASE + 0x60, BASE + 0x70)
        code = code.ljust(0x60, b"\0")
        code += b"gp != NULL\0".ljust(0x10, b"\0")
        return code + b"omPhysics.c\0"

    def call_at(va: int) -> tuple[int, int]:
        """The two displacements that reach the pool from a call at va."""
        return ((BASE + 0x40 - ((va - 4 + 4) & ~3)) // 4,
                (BASE + 0x44 - ((va - 2 + 4) & ~3)) // 4)

    d4, d5 = call_at(BASE + 4)
    # The line in the delay slot: the shape a scan without delay slots loses.
    assert Image(build(0xD400 | d4, 0xD500 | d5, 0x420B, 0xE600 | 89)) \
        .asserts() == [("omPhysics.c", 89, BASE + 4, "gp != NULL")]
    # A nop there and nothing else setting r6: the site still counts, with no
    # line, rather than borrowing a number from whatever ran before it.
    assert Image(build(0xD400 | d4, 0xD500 | d5, 0x420B, 0x0009)) \
        .asserts() == [("omPhysics.c", 0, BASE + 4, "gp != NULL")]
    # And a line left in r6 by an earlier call does not leak into a later one:
    # two calls, only the first with a line, and the second reports none.
    a4, a5 = call_at(BASE + 6)
    b4, b5 = call_at(BASE + 0xE)
    two = Image(build(0xE600 | 77, 0xD400 | a4, 0xD500 | a5, 0x420B, 0x0009,
                      0xD400 | b4, 0xD500 | b5, 0x420B, 0x0009)).asserts()
    assert [r[1] for r in two] == [77, 0], two
    print("dcassert: self-tests pass")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[1])
    ap.add_argument("image", nargs="?", help="the Dreamcast 1ST_READ.BIN")
    ap.add_argument("--map", action="store_true", help="module ranges only")
    ap.add_argument("--expect", type=int, help="fail unless this many sites")
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()

    if args.selftest or not args.image:
        selftest()
        return 0

    img = Image(open(args.image, "rb").read())
    if args.map:
        print(f"{len(img.modules())} modules, link order is source order\n")
        for name, first, last, n in img.modules():
            print(f"  {name:18s} {first:#x}..{last:#x}  {n:3d} refs")
        return 0

    rows = img.asserts()
    per = defaultdict(list)
    for name, line, va, text in rows:
        per[name].append((line, va, text))
    print(f"{len(rows)} assert sites in {len(per)} source files\n")
    for name in sorted(per, key=lambda k: min(v[1] for v in per[k])):
        for line, va, text in sorted(per[name]):
            print(f"  {name}:{line:<5} {va:#x}  {text}")
    if args.expect is not None and len(rows) != args.expect:
        print(f"FAIL: expected {args.expect} sites, found {len(rows)}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
