#!/usr/bin/env python3
"""
refdec.py -- reference decoder for .tex levels, by emulating the original.

This is a **research oracle, not engine code.** It runs the original's own
decode routine under a CPU emulator so we have ground truth to check a
from-scratch decoder against, and so the block format can be probed by
experiment (feed a synthetic block, observe the pixels) instead of by reading
bit-shift assembly. Nothing here goes into the engine; see rule 2 in docs/rules.md.

The routine is reachable because it is pure: it touches only its arguments,
the compressed block, and static tables in `.data`. So mapping the PE's
sections at their virtual addresses and calling the function is enough — no
imports, no relocation, no initialisation.

    mdk2Main.exe (GOG)
      fcn.0045e1e0(desc, src)   level decoder, loops over 8x4 blocks
        fcn.0045e0c0            mode dispatch on the top 3 bits of block[3]
          0x45d660 / 0x45d8c0 / 0x45da90 / 0x45dc70    the four block modes

`desc` is the destination descriptor built by chVideo.cpp:

    +0x00  unused by the decoder
    +0x04  width
    +0x08  height
    +0x0c  width * height * 4
    +0x10  destination buffer

Output is **BGRA**, 8 bits per channel.

Usage:
    python3 tools/refdec.py extracted/base/angel.tex -o angel.png
    python3 tools/refdec.py extracted/base --check       # against the 4x4 tail
"""

from __future__ import annotations

import argparse
import struct
import sys
from pathlib import Path

DECODE_LEVEL = 0x0045E1E0
IMAGE_BASE = 0x00400000

STACK = 0x70000000
STACK_SIZE = 0x100000
HEAP = 0x10000000
HEAP_SIZE = 0x1000000
RET_MAGIC = 0x0BADF00D  # sentinel return address; hitting it stops emulation

TAIL_BYTES = 84

# the container arithmetic belongs to the decoder, not to the oracle
from texdec import levels  # noqa: E402


def _align(x: int, a: int = 0x1000) -> int:
    return (x + a - 1) & ~(a - 1)


class RefDecoder:
    """Wraps one loaded copy of the executable; reusable across textures."""

    def __init__(self, exe: Path) -> None:
        import pefile
        from unicorn import Uc, UC_ARCH_X86, UC_MODE_32

        pe = pefile.PE(str(exe))
        raw = exe.read_bytes()
        self.uc = Uc(UC_ARCH_X86, UC_MODE_32)

        base = pe.OPTIONAL_HEADER.ImageBase
        if base != IMAGE_BASE:
            raise RuntimeError(f"unexpected image base 0x{base:x}")
        # headers plus every section, at its virtual address
        self.uc.mem_map(base, _align(0x1000))
        self.uc.mem_write(base, raw[:0x1000])
        for s in pe.sections:
            va = base + s.VirtualAddress
            size = _align(max(s.Misc_VirtualSize, s.SizeOfRawData))
            self.uc.mem_map(va, size)
            body = raw[s.PointerToRawData:s.PointerToRawData + s.SizeOfRawData]
            if body:
                self.uc.mem_write(va, body)

        self.uc.mem_map(STACK, STACK_SIZE)
        self.uc.mem_map(HEAP, HEAP_SIZE)

    def decode_level(self, width: int, height: int, blocks: bytes) -> bytes:
        """Run the original decoder over one level. -> width*height*4 BGRA."""
        from unicorn.x86_const import UC_X86_REG_ESP, UC_X86_REG_EIP

        nbytes = width * height * 4
        desc = HEAP
        dest = HEAP + 0x100
        src = dest + _align(nbytes)
        if src + len(blocks) > HEAP + HEAP_SIZE:
            raise RuntimeError("level does not fit in the emulator heap")

        self.uc.mem_write(desc, struct.pack("<5I", 0, width, height,
                                            nbytes, dest))
        self.uc.mem_write(dest, b"\0" * nbytes)
        self.uc.mem_write(src, blocks)

        # cdecl: push args right to left, then the sentinel return address
        sp = STACK + STACK_SIZE - 0x1000
        self.uc.mem_write(sp, struct.pack("<3I", RET_MAGIC, desc, src))
        self.uc.reg_write(UC_X86_REG_ESP, sp)
        self.uc.reg_write(UC_X86_REG_EIP, DECODE_LEVEL)
        self.uc.emu_start(DECODE_LEVEL, RET_MAGIC)
        return self.uc.mem_read(dest, nbytes)


def decode_top(dec: RefDecoder, data: bytes) -> tuple[int, int, bytes]:
    off, size, width, height = levels(data)[0]
    if size != width * height // 2:
        raise ValueError(f"level 0 is {size} B, expected {width*height//2}")
    return width, height, dec.decode_level(width, height,
                                           data[off:off + size])


def to_png(width: int, height: int, bgra: bytes, channels: int) -> bytes:
    import io
    from PIL import Image
    img = Image.frombytes("RGBA", (width, height), bytes(bgra), "raw", "BGRA")
    img = img.convert("RGBA" if channels == 4 else "RGB")
    buf = io.BytesIO()
    img.save(buf, "PNG")
    return buf.getvalue()


def check(dec: RefDecoder, data: bytes) -> float:
    """Decode level 0, box-filter it down to 4x4, compare with the stored 4x4.

    The 4x4 tail level is raw BGRA written by the original tool, so it is
    independent ground truth: a correct decode must reproduce it.
    Returns RMS error per channel.
    """
    width, height, bgra = decode_top(dec, data)
    ref = data[-TAIL_BYTES:-20]
    err = 0.0
    bw, bh = width // 4, height // 4
    for ry in range(4):
        for rx in range(4):
            acc = [0, 0, 0]
            for y in range(ry * bh, (ry + 1) * bh):
                row = y * width
                for x in range(rx * bw, (rx + 1) * bw):
                    p = (row + x) * 4
                    for c in range(3):
                        acc[c] += bgra[p + c]
            n = bw * bh
            for c in range(3):
                err += (acc[c] / n - ref[(ry * 4 + rx) * 4 + c]) ** 2
    return (err / 48) ** 0.5


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path, help="a .tex file or a directory")
    ap.add_argument("-o", "--out", type=Path)
    ap.add_argument("--exe", type=Path,
                    default=Path(__import__("os").environ.get("MDK2_GOG", "."))
                    / "mdk2Main.exe")
    ap.add_argument("--check", action="store_true",
                    help="score against the stored 4x4 level")
    ap.add_argument("--limit", type=int, default=0)
    args = ap.parse_args(argv)

    dec = RefDecoder(args.exe)
    files = sorted(args.src.glob("*.tex")) if args.src.is_dir() else [args.src]
    if args.limit:
        files = files[:args.limit]

    if args.check:
        errs, failed = [], 0
        for f in files:
            data = f.read_bytes()
            try:
                errs.append((check(dec, data), f.name))
            except Exception as e:  # emulation or format failure
                failed += 1
                print(f"FAIL {f.name}: {e}", file=sys.stderr)
        errs.sort(reverse=True)
        if errs:
            vals = [e for e, _ in errs]
            print(f"{len(errs)} decoded, {failed} failed", file=sys.stderr)
            print(f"RMS vs stored 4x4: mean {sum(vals)/len(vals):.2f}, "
                  f"max {vals[0]:.2f} ({errs[0][1]})", file=sys.stderr)
            print("worst 5: " + ", ".join(f"{e:.1f} {n}" for e, n in errs[:5]),
                  file=sys.stderr)
        return 1 if failed else 0

    if not args.out:
        ap.error("need -o OUT or --check")
    out_dir = args.out if len(files) > 1 else None
    if out_dir:
        out_dir.mkdir(parents=True, exist_ok=True)
    for f in files:
        data = f.read_bytes()
        channels = struct.unpack_from("<I", data, 0x10)[0]
        width, height, bgra = decode_top(dec, data)
        png = to_png(width, height, bgra, channels)
        (out_dir / (f.stem + ".png") if out_dir else args.out).write_bytes(png)
    print(f"{len(files)} decoded", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
