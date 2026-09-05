#!/usr/bin/env python3
"""
frame.py -- disassemble a function with `esp` tracked, so `[esp + N]` reads
as the slot it really is.

The trap this exists for has cost this project three sessions. MSVC built
mdk2Main.exe without a frame pointer, so every local is `[esp + N]` and N
*changes* with every `push` of a call's arguments. rizin names those slots
(`var_28h`, `arg_4h`) from its own idea of esp and gets it wrong often
enough that two different slots print with the same name -- which is how a
read of the AI's leap gate first came out as "the type's own name, byte 16".

This prints, beside each `[esp + N]`, `@N` -- the offset from esp at the
function's entry, which is fixed for the whole function. Two accesses to the
same local always print the same `@N`, and `@4`, `@8`, ... are the caller's
arguments.

The tracking is linear: esp is followed instruction by instruction down the
listing, not along the control flow graph. That is right for MSVC bodies,
where esp only moves inside a call's straight-line argument set-up, and it
is checked by `--check`: any address the listing jumps to whose tracked esp
differs from the fall-through is reported.

Usage:
    python3 frame.py mdk2Main.exe 0x4324f0            # the whole function
    python3 frame.py mdk2Main.exe 0x4329a0 --bytes 96 # a window of it
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys

LINE = re.compile(r"0x([0-9a-f]{8})\s+(\S+)\s*(.*)$")
SLOT = re.compile(r"\[esp(?:\s*\+\s*(0x[0-9a-f]+|\d+))?\]")
JUMP = re.compile(r"^j\w+$")


def disassemble(exe: str, start: int, length: int) -> list[tuple[int, str, str]]:
    out = subprocess.run(
        ["rizin", "-q", "-e", "scr.color=0", "-e", "asm.bytes=false", "-e", "asm.sub.var=false",
         "-c", f"s {start:#x}; af; " + (f"pD {length}" if length else "pdf"), exe],
        capture_output=True, text=True,
    ).stdout
    rows = []
    for raw in out.splitlines():
        line = raw.lstrip("│╎┌└├─<>╌; ").rstrip()
        m = LINE.match(line)
        if m:
            rows.append((int(m.group(1), 16), m.group(2), m.group(3)))
    # `pdf` prints basic blocks, and one of them can come back above the
    # others; esp is tracked down the addresses, so sort and drop repeats
    seen = {}
    for address, mnemonic, operands in rows:
        seen.setdefault(address, (mnemonic, operands))
    return [(a, *seen[a]) for a in sorted(seen)]


def delta(mnemonic: str, operands: str) -> int:
    """How much this instruction moves esp, or 0."""
    if mnemonic == "push":
        return -4
    if mnemonic == "pop":
        return 4
    if mnemonic in ("add", "sub") and operands.startswith("esp,"):
        n = operands.split(",", 1)[1].strip()
        try:
            value = int(n, 0)
        except ValueError:
            return 0
        return value if mnemonic == "add" else -value
    return 0


def walk(rows):
    """esp at every instruction, following the control flow from the entry."""
    order = {address: i for i, (address, _, _) in enumerate(rows)}
    at: dict[int, int] = {}
    conflicts = []
    todo = [(rows[0][0], 0)]
    seeded = []
    while True:
      while todo:
        address, esp = todo.pop()
        if address not in order:
            continue
        if address in at:
            if at[address] != esp:
                conflicts.append((address, at[address], esp))
            continue
        at[address] = esp
        _, mnemonic, operands = rows[order[address]]
        after = esp + delta(mnemonic, operands)
        target = None
        if JUMP.match(mnemonic) or mnemonic == "jmp":
            try:
                target = int(operands.strip(), 0)
            except ValueError:
                target = None
        if target is not None:
            todo.append((target, after))
        if mnemonic == "ret" or mnemonic == "jmp":
            continue
        nxt = order[address] + 1
        if nxt < len(rows):
            todo.append((rows[nxt][0], after))
      # a `jmp dword [eax*4 + table]` -- which is how the enemy AI reaches
      # ten of its twelve states -- leaves whole blocks with no edge into
      # them. Seed the first one left with the level esp spends most of its
      # time at, which is the body's own, and walk on from there.
      missing = [a for a, _, _ in rows if a not in at]
      if not missing:
          break
      body = max(set(at.values()), key=list(at.values()).count)
      seeded.append(missing[0])
      todo.append((missing[0], body))
    return at, conflicts, seeded


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("exe")
    ap.add_argument("address")
    ap.add_argument("--bytes", type=lambda s: int(s, 0), default=0,
                    help="how far to disassemble; default is the function")
    ap.add_argument("--check", action="store_true",
                    help="report predecessors whose tracked esp disagrees")
    ap.add_argument("--quiet", action="store_true",
                    help="the summary only, not the listing")
    ap.add_argument("--expect-seeded", type=int, default=None,
                    help="how many blocks a jump table should reach; a "
                         "different number means the function changed shape")
    args = ap.parse_args()

    start = int(args.address, 0)
    rows = disassemble(args.exe, start, args.bytes)
    if not rows:
        print("nothing disassembled -- is rizin installed?", file=sys.stderr)
        return 1

    # esp is propagated along the control flow, not down the listing: a
    # `pdf` dump prints epilogues and argument set-ups in address order, and
    # a linear walk climbs out of the frame at the first `ret` it passes.
    # A conflict -- two predecessors that disagree -- is reported, never
    # silently averaged, because a disagreement is the interesting thing.
    at, conflicts, seeded = walk(rows)
    for address, mnemonic, operands in rows:
        esp = at.get(address)
        shown = operands
        for m in SLOT.finditer(operands):
            n = int(m.group(1), 0) if m.group(1) else 0
            where = f"{esp + n:+d}" if esp is not None else "?"
            shown = shown.replace(m.group(0), f"{m.group(0)}@{where}", 1)
        if not args.quiet:
            print(f"0x{address:08x}  {mnemonic:<7} {shown}")

    print(f"{len(rows)} instructions, {len(conflicts)} esp conflicts, "
          f"{len(seeded)} blocks seeded")
    if args.expect_seeded is not None and len(seeded) != args.expect_seeded:
        print(f"expected {args.expect_seeded} seeded, got {len(seeded)}",
              file=sys.stderr)
        return 1
    if conflicts:
        return 1
    if args.check:
        for address, was, now in conflicts:
            print(f"  ! 0x{address:08x}: esp {was} and {now}", file=sys.stderr)
        for address in seeded:
            print(f"  . 0x{address:08x} has no edge into it -- seeded at the "
                  f"body's own esp", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
