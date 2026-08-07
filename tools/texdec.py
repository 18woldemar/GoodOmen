#!/usr/bin/env python3
"""
texdec.py -- decode the `.tex` block codec. Written from scratch, no emulator.

This is the engine's own decoder, and it closes the debt `tools/refdec.py` was
standing in for: that one runs the original's routine under `unicorn` and is a
research oracle, not code that can ship. This one is checked *against* it.

The codec packs **8x4 pixels into 16 bytes -- 4 bits per pixel** -- and every
block is really two independent **4x4 sub-blocks**, left then right. The top
bits of the last dword choose one of four layouts, which is why fitting a
single model to it (DXT1, or two endpoints and 3-bit weights) never worked:

    bit 127          == 1   ->  A   a ramp per 4x4, two bits a pixel
    bits 127,126     == 00  ->  B   one ramp for the block, three bits a pixel
    bits 127,126,125 == 010 ->  C   four colours, stored outright
    bits 127,126,125 == 011 ->  D   colours with alpha

Bit 124 then picks a sub-mode within A and within D; C ignores it and B has no
room for it. So there are six layouts, not four. Over all 4205514 blocks in
the game: B 43.8%, A four-colour 26.0%, A three-colour 13.9%, D ramp 7.5%,
C 5.6%, D palette 3.2%.

Indices are stored per sub-block: sub-block 0 from bit 0, sub-block 1 from bit
32 (A, C, D, two bits each) or bit 48 (B, three bits each), and within a
sub-block they run row by row. Colours are BGR555, expanded by replication --
`v << 3 | v >> 2`, exact at both ends: 0 -> 0 and 31 -> 255.

**A** gives each 4x4 its own pair, 15 bits each: c1 at bit 64 + 30s and c0
fifteen bits after it.

    bit 124 == 0:  four colours,  entry k = ((3-k)*c0 + k*c1 + 1) / 3
    bit 124 == 1:  three colours, entry k = ((2-k)*c0 + k*c1) / 2,
                   entry 3 transparent

and green is six bits rather than five -- but only sometimes, and never
symmetrically. c1's green LSB is bit 125+s. c0's is `bit(125+s) XOR
bit(32s+1)`, which means it shares a bit with pixel 0's high index bit; and in
the three-colour ramp c0 does not get the sixth bit at all, keeping the plain
five-bit expansion. The original reaches that by jumping over one fix-up, and
the asymmetry is real enough to show up as 994 wrong blocks in twelve
textures if you assume otherwise.

**B** spends its bits the other way: one pair of endpoints for all 32 pixels,
c0 at bit 96 and c1 at 111, with three-bit indices -- seven steps
`((6-k)*c0 + k*c1 + 2) / 6`, index 7 transparent.

**C** stores four BGR555 outright, at bits 64, 79, 94 and 109. No
interpolation at all.

**D** is the alpha mode, and its two sub-modes are the least guessable part of
the format. With bit 124 clear it is three BGRA colours -- BGR at 64, 79, 94
and a 5-bit alpha at 109, 114, 119 -- plus a transparent fourth. With bit 124
set it is a ramp again, but a shared one: each 4x4 keeps its own first colour
(bits 64 / 94, alpha 109 / 119) while **both share the last** (bits 79, alpha
114), so 45 bits of colour buy two four-step ramps instead of one.

Verified the only way that counts: **every one of the 4205514 blocks in all
753 compressed textures decodes byte-for-byte identically to the original
routine under emulation.** The two fonts are stored as raw BGRA and have no
blocks.

Usage:
    python3 tools/texdec.py extracted/base/angel.tex -o angel.png
    python3 tools/texdec.py extracted/base --check      # against refdec.py
"""

from __future__ import annotations

import argparse
import struct
import sys
from pathlib import Path

# where each sub-block's colour fields and index bits start
COLOUR_AT = (64, 94)          # handler A, from the table at 0x4b8150
INDEX_AT = (0, 32)            # handlers A, C, D, from the table at 0x4b8160
INDEX_AT_B = (0, 48)          # handler B, three bits a pixel

DATA_OFFSET = 0x68            # where the mip chain starts, past the header


def levels(data: bytes) -> list[tuple[int, int, int, int]]:
    """-> [(offset, size, width, height)], largest level first.

    Derived analytically rather than from the 0x3c table: levels of 8x8 and
    up take w*h/2 bytes, the rest are raw BGRA, and they follow each other
    from 0x68. The table only holds the *last* nine levels — a 1024x1024
    texture has eleven, so its two largest are absent from it — which makes
    the arithmetic the more reliable source. The table is checked against it.
    """
    width, height = struct.unpack_from("<2I", data, 8)
    out, off = [], DATA_OFFSET
    while True:
        size = (width * height // 2 if width >= 8 and height >= 8
                else width * height * 4)
        out.append((off, size, width, height))
        off += size
        if width == 1 and height == 1:
            break
        width, height = max(1, width // 2), max(1, height // 2)
    if off != len(data):
        raise ValueError(f"levels total {off}, file is {len(data)}")
    table = [o for o in struct.unpack_from("<9I", data, 0x3c) if o]
    starts = [o for o, _, _, _ in out]
    if sorted(table) != starts[-len(table):]:
        raise ValueError("offset table disagrees with the level arithmetic")
    return out



def _bits(block: int, start: int, width: int) -> int:
    return (block >> start) & ((1 << width) - 1)


def _e5(v: int) -> int:
    """5 bits to 8, by replication. Exact at both ends: 0 -> 0, 31 -> 255."""
    return (v << 3) | (v >> 2)


def _e6(v: int) -> int:
    return (v << 2) | (v >> 4)


def _bgr555(block: int, at: int) -> list[int]:
    return [_e5(_bits(block, at, 5)),
            _e5(_bits(block, at + 5, 5)),
            _e5(_bits(block, at + 10, 5)),
            255]


def _palette_a(block: int, sub: int) -> list[list[int]]:
    at = COLOUR_AT[sub]
    c1 = _bgr555(block, at)
    c0 = _bgr555(block, at + 15)
    # green is six bits; the extra one lives outside the field, and the two
    # endpoints take it differently -- see the module docstring
    high = _bits(block, 125 + sub, 1)
    c1[1] = _e6((_bits(block, at + 5, 5) << 1) | high)
    if _bits(block, 124, 1):
        # in the three-colour ramp c0 keeps its five-bit green: the original
        # jumps over that one fix-up, and the asymmetry is visible in the data
        pal = [[((2 - k) * c0[c] + k * c1[c]) // 2 for c in range(3)] + [255]
               for k in range(3)]
        return pal + [[0, 0, 0, 0]]
    c0[1] = _e6((_bits(block, at + 20, 5) << 1)
                | (high ^ _bits(block, INDEX_AT[sub] + 1, 1)))
    return [[((3 - k) * c0[c] + k * c1[c] + 1) // 3 for c in range(3)] + [255]
            for k in range(4)]


def _palette_b(block: int) -> list[list[int]]:
    c0 = _bgr555(block, 96)
    c1 = _bgr555(block, 111)
    pal = [[((6 - k) * c0[c] + k * c1[c] + 2) // 6 for c in range(3)] + [255]
           for k in range(7)]
    return pal + [[0, 0, 0, 0]]


def _palette_c(block: int) -> list[list[int]]:
    return [_bgr555(block, 64 + 15 * k) for k in range(4)]


def _bgra(block: int, at: int, alpha_at: int) -> list[int]:
    colour = _bgr555(block, at)
    colour[3] = _e5(_bits(block, alpha_at, 5))
    return colour


def _palette_d(block: int, sub: int) -> list[list[int]]:
    if not _bits(block, 124, 1):
        return [_bgra(block, 64 + 15 * k, 109 + 5 * k) for k in range(3)] \
            + [[0, 0, 0, 0]]
    # each 4x4 keeps its own first colour and they share the last one, so a
    # block spends 45 bits on colour instead of 60 and still gets two ramps
    c0 = _bgra(block, 64 if sub == 0 else 94, 109 if sub == 0 else 119)
    c1 = _bgra(block, 79, 114)
    return [[((3 - k) * c0[c] + k * c1[c] + 1) // 3 for c in range(4)]
            for k in range(4)]


def decode_block(block: int) -> list[list[int]]:
    """-> 32 BGRA pixels, row-major over the 8x4 block."""
    out: list[list[int]] = [None] * 32       # type: ignore[list-item]
    if block >> 127:
        for sub in (0, 1):
            pal = _palette_a(block, sub)
            base = INDEX_AT[sub]
            for i in range(16):
                out[(i // 4) * 8 + (i % 4) + 4 * sub] = \
                    pal[_bits(block, base + 2 * i, 2)]
        return out
    if not _bits(block, 126, 1):
        pal = _palette_b(block)
        for sub in (0, 1):
            base = INDEX_AT_B[sub]
            for i in range(16):
                out[(i // 4) * 8 + (i % 4) + 4 * sub] = \
                    pal[_bits(block, base + 3 * i, 3)]
        return out
    palette_c = not _bits(block, 125, 1)
    for sub in (0, 1):
        pal = _palette_c(block) if palette_c else _palette_d(block, sub)
        base = INDEX_AT[sub]
        for i in range(16):
            out[(i // 4) * 8 + (i % 4) + 4 * sub] = \
                pal[_bits(block, base + 2 * i, 2)]
    return out


def decode_level(width: int, height: int, data: bytes) -> bytearray:
    """-> width*height*4 bytes of BGRA."""
    if width * height // 2 != len(data):
        raise ValueError(f"{len(data)} bytes for {width}x{height}")
    out = bytearray(width * height * 4)
    n = 0
    for by in range(0, height, 4):
        for bx in range(0, width, 8):
            lo, hi = struct.unpack_from("<QQ", data, n)
            n += 16
            pixels = decode_block(lo | (hi << 64))
            for i, p in enumerate(pixels):
                o = ((by + i // 8) * width + bx + i % 8) * 4
                out[o:o + 4] = bytes(p)
    return out


# One block per layout, with the answer the original gives. Recorded from
# tools/refdec.py so the self-test does not need the game installed.
VECTORS = (
    ("A four-colour", 0x8abcdef013579bdf02468acefedcba98,
     "7baa4affd3db39ffa7c242ffd3db39ff10ccdeff00f3f7ff317dadff00f3f7ff"
     "d3db39ffd3db39fffff331ffd3db39ff10ccdeff10ccdeff317dadff10ccdeff"
     "7baa4afffff331ffa7c242fffff331ff10ccdeff21a4c6ff317dadff21a4c6ff"
     "d3db39fffff331fffff331fffff331ff10ccdeff317dadff317dadff317dadff"),
    ("A three-colour", 0x9abcdef013579bdf02468acefedcba98,
     "7bad4afffff331ffbdd03dfffff331ff00f3f7ff00000000317badff00000000"
     "fff331fffff331ff00000000fff331ff00f3f7ff00f3f7ff317badff00f3f7ff"
     "7bad4aff00000000bdd03dff0000000000f3f7ff18b7d2ff317badff18b7d2ff"
     "fff331ff00000000000000000000000000f3f7ff317badff317badff317badff"),
    ("B", 0x3abcdef013579bdf02468acefedcba98,
     "84bdbdffa98bd6ff9d9cceffc26ae7ffce5aefff84bdbdff90acc5ff90acc5ff"
     "a98bd6ff90acc5ff00000000ce5aefff84bdbdffce5aefff00000000ce5aefff"
     "ce5aefff00000000a98bd6ff00000000a98bd6ffa98bd6ffce5aefffa98bd6ff"
     "b57bdeffc26ae7ff9d9cceffb57bdeffc26ae7ffce5aefffb57bdeff84bdbdff"),
    ("C", 0x4abcdef013579bdf02468acefedcba98,
     "fff731ff00f7f7ff7bad4aff00f7f7ff00f7f7ff317badfffff731ff317badff"
     "00f7f7ff00f7f7ff317badff00f7f7ff00f7f7ff00f7f7fffff731ff00f7f7ff"
     "fff731ff317badff7bad4aff317badff00f7f7ff7bad4afffff731ff7bad4aff"
     "00f7f7ff317badff317badff317badff00f7f7fffff731fffff731fffff731ff"),
    ("D palette", 0x6abcdef013579bdf02468acefedcba98,
     "fff7313100f7f7ad7bad4a7b00f7f7ad00f7f7ad00000000fff73131 00000000"
     "00f7f7ad00f7f7ad0000000000f7f7ad00f7f7ad00f7f7adfff7313100f7f7ad"
     "fff73131000000007bad4a7b0000000000f7f7ad7bad4a7bfff731317bad4a7b"
     "00f7f7ad00000000000000000000000000f7f7adfff73131fff73131fff73131"),
    ("D ramp", 0x7abcdef013579bdf02468acefedcba98,
     "fff73131a7c64262d3de394aa7c6426252c6848c7bad4a7b00f7f7ad7bad4a7b"
     "a7c64262a7c642627bad4a7ba7c6426252c6848c52c6848c00f7f7ad52c6848c"
     "fff731317bad4a7bd3de394a7bad4a7b52c6848c29debd9c00f7f7ad29debd9c"
     "a7c642627bad4a7b7bad4a7b7bad4a7b52c6848c00f7f7ad00f7f7ad00f7f7ad"),
)


def decode_texture(data: bytes) -> list[bytes]:
    """-> every level of a .tex as BGRA, largest first.

    Levels of 8x8 and up are coded; the 4x4, 2x2 and 1x1 tail is raw BGRA and
    is passed through, as are the two fonts, which have no blocks at all
    (the u32 at 0x24 is 0 rather than 32).
    """
    if struct.unpack_from("<I", data, 0x24)[0] != 32:
        width, height = struct.unpack_from("<2I", data, 8)
        return [data[0x2c:0x2c + width * height * 4]]
    out = []
    for off, size, w, h in levels(data):
        raw = data[off:off + size]
        out.append(bytes(decode_level(w, h, raw)) if size == w * h // 2
                   else raw)
    return out


def digest(files: list[Path]) -> int:
    """One CRC32 per texture over all of its decoded levels, so that a
    disagreement with another implementation names the texture it is in.

    The engine prints the same lines (`goodomen --tex`) and
    `tools/texcheck.sh` diffs the two; this side is the reference, being the
    one checked block for block against the original under emulation.
    """
    import zlib
    blocks = 0
    for f in sorted(files, key=lambda p: p.name.lower()):
        data = f.read_bytes()
        crc = 0
        for level in decode_texture(data):
            crc = zlib.crc32(level, crc)
        if struct.unpack_from("<I", data, 0x24)[0] == 32:
            blocks += sum(size // 16 for _, size, w, h in levels(data)
                          if size == w * h // 2)
        print(f"{f.name.lower()} {crc:08x}")
    print(f"{len(files)} textures, {blocks} blocks", file=sys.stderr)
    return 0


def selftest() -> None:
    for name, block, want in VECTORS:
        got = b"".join(bytes(p) for p in decode_block(block))
        if got != bytes.fromhex(want.replace(" ", "")):
            raise AssertionError(f"{name}: {got.hex()}")
    # C ignores bit 124; it is the one spare bit in the whole format
    assert decode_block(VECTORS[3][1]) == \
        decode_block(VECTORS[3][1] | (1 << 124)), "bit 124 is spare in C"
    print("texdec.py: self-test passed")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path, nargs="?",
                    help="a .tex file or a directory")
    ap.add_argument("--selftest", action="store_true",
                    help="check all six layouts against recorded vectors")
    ap.add_argument("-o", "--out", type=Path)
    ap.add_argument("--check", action="store_true",
                    help="compare every level against refdec.py")
    ap.add_argument("--digest", action="store_true",
                    help="one CRC32 per texture over all its levels, for "
                         "tools/texcheck.sh to diff against the engine")
    ap.add_argument("--exe", type=Path,
                    default=Path(__import__("os").environ.get("MDK2_GOG", "."))
                    / "mdk2Main.exe")
    ap.add_argument("--limit", type=int, default=0)
    args = ap.parse_args(argv)
    if args.selftest:
        selftest()
        return 0
    if args.src is None:
        ap.error("a .tex file or a directory is required")

    if args.digest:
        # a directory of extracted resources, which is two directories deep:
        # 755 textures in base/ and 6 in local/
        files = (sorted(args.src.rglob("*.tex")) if args.src.is_dir()
                 else [args.src])
        return digest(files[:args.limit] if args.limit else files)

    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import refdec

    files = sorted(args.src.glob("*.tex")) if args.src.is_dir() else [args.src]
    if args.limit:
        files = files[:args.limit]

    if args.check:
        oracle = refdec.RefDecoder(args.exe)
        blocks = bad = 0
        skipped = 0
        for f in files:
            data = f.read_bytes()
            if struct.unpack_from("<I", data, 0x24)[0] != 32:
                skipped += 1        # the two fonts are stored as raw BGRA
                continue
            for off, size, w, h in refdec.levels(data):
                if size != w * h // 2:
                    continue                      # the raw BGRA tail levels
                raw = data[off:off + size]
                mine = decode_level(w, h, raw)
                theirs = bytes(oracle.decode_level(w, h, raw))
                blocks += size // 16
                if bytes(mine) != theirs:
                    bad += 1
                    print(f"MISMATCH {f.name} {w}x{h}", file=sys.stderr)
        print(f"{len(files) - skipped} compressed textures ({skipped} raw), "
              f"{blocks} blocks, {bad} levels differ",
              file=sys.stderr)
        return 1 if bad else 0

    data = files[0].read_bytes()
    off, size, w, h = refdec.levels(data)[0]
    bgra = decode_level(w, h, data[off:off + size])
    if args.out:
        args.out.write_bytes(refdec.to_png(w, h, bgra, 4))
    print(f"{files[0].name}: {w}x{h}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
