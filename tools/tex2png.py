#!/usr/bin/env python3
"""
tex2png.py -- parse .tex textures; convert to PNG what can be decoded.

**The 4-bit-per-pixel codec is NOT decoded yet.** The container around it is,
and this tool validates that container across the whole corpus. Only the
uncompressed textures (the two font atlases) and the raw tail levels convert
to PNG today. See docs/journal.md for the codec hypotheses already ruled out.

The .tex format (Omen renderer; the binary's debug paths name
E:\\mdk2\\Omen\\omTexture.c):

    0x00  u32  2001            resource type tag
    0x04  u32  21              always 21 = 16+4+1, the pixel count in the tail
    0x08  u32  width
    0x0c  u32  height
    0x10  u32  3 | 4           channels: RGB / RGBA
    0x14  u32  1
    0x18  u32  0
    0x1c  u32  0
    0x20  u32  0 | 1           has alpha (exactly when channels == 4)
    0x24  u32  32 | 0          32 -> compressed TEXC chunk; 0 -> raw BGRA
    0x28  f32                  ? compression quality (Textures.txt, shipped
                               alongside, reports RMS/PSNR per texture)
    0x2c  u32  'TEXC'          chunk tag, stored LE so the bytes read "CXET"
    0x30  u32  0
    0x34  u32  width
    0x38  u32  height
    0x3c  u32[9]               absolute file offsets of the mip levels,
                               smallest level first, zero-padded. A texture
                               with more than 9 levels drops the largest one
                               from the table; it always starts at 0x68.
    0x60  u32  0, 0
    0x68        level 0 (the largest)

Levels run from the full size down to 1x1. Levels of 8x8 and above are coded
at exactly 4 bits per pixel; the 4x4, 2x2 and 1x1 levels are stored as raw
BGRA, which is why the file ends with 16+4+1 = 21 pixels = 84 bytes.

The 4 bpp coding is a multi-mode block codec: 8x4-pixel blocks, 16 bytes each,
with the top three bits of the block's fourth dword selecting one of four
decoders (fcn.0045d660 / 0045d8c0 / 0045da90 / 0045dc70 in mdk2Main.exe).
Those four modes are not decoded yet -- see docs/journal.md.

That gives an exact size identity, asserted below, which holds for all 761
textures in the corpus:

    filesize == 104 + sum(w_i*h_i/2 for levels >= 8x8) + 84

Two facts worth keeping:

  * channel order is **BGRA**. The engine byte-swaps before handing the buffer
    to glTexImage2D as GL_RGBA: for 4-channel textures it exchanges bytes 0
    and 2 in place, for 3-channel ones it writes (src[2], src[1], src[0]).
  * the levels are plain box-filtered mips, not a residual pyramid: for
    398/398 compressed textures, 2x2 == box(4x4) and 1x1 == box(2x2) exactly.
    So each level is a standalone image and can be decoded on its own.

Usage:
    python3 tools/tex2png.py extracted/base --validate    # structure check
    python3 tools/tex2png.py extracted/base -o png/       # what is decodable
"""

from __future__ import annotations

import argparse
import io
import struct
import sys
from pathlib import Path

TYPE_TEX = 2001
TAG_TEXC = 0x54455843  # 'TEXC'
DATA_OFFSET = 0x68  # level 0 of a compressed texture
RAW_OFFSET = 0x2c   # pixels of an uncompressed texture (fonts)
MIN_DXT_DIM = 8     # at and above this a level is 4 bpp; below it is raw RGBA
TAIL_PIXELS = 21    # 4x4 + 2x2 + 1x1
TAIL_BYTES = TAIL_PIXELS * 4


class TexError(ValueError):
    pass


class CodecNotDecoded(TexError):
    """The 4 bpp level codec is still unknown -- see docs/journal.md."""


def levels(width: int, height: int) -> list[tuple[int, int]]:
    """Mip chain from full size down to 8x8, i.e. the 4 bpp levels."""
    out = []
    w, h = width, height
    while w >= MIN_DXT_DIM and h >= MIN_DXT_DIM:
        out.append((w, h))
        w //= 2
        h //= 2
    return out


def expected_size(width: int, height: int) -> int:
    return (DATA_OFFSET
            + sum(w * h // 2 for w, h in levels(width, height))
            + TAIL_BYTES)


def parse(data: bytes) -> dict:
    """Validate the container and describe it. Raises TexError if it is not
    a .tex, or if the size identity above does not hold."""
    if len(data) < RAW_OFFSET:
        raise TexError("file shorter than the header")
    kind, tail_px, width, height, channels = struct.unpack_from("<5I", data, 0)
    if kind != TYPE_TEX:
        raise TexError(f"type tag {kind}, expected {TYPE_TEX}")

    # field 0x24: 32 -> a compressed TEXC chunk follows, 0 -> raw RGBA (fonts)
    if struct.unpack_from("<I", data, 0x24)[0] == 0:
        want = RAW_OFFSET + width * height * 4
        if len(data) != want:
            raise TexError(f"{width}x{height} raw: {len(data)} != {want}")
        return {"width": width, "height": height, "channels": channels,
                "compressed": False, "pixels": data[RAW_OFFSET:]}

    if struct.unpack_from("<I", data, 0x2c)[0] != TAG_TEXC:
        raise TexError("no TEXC chunk")
    want = expected_size(width, height)
    if len(data) != want:
        raise TexError(f"{width}x{height}: size {len(data)} != {want}")
    if tail_px != TAIL_PIXELS:
        raise TexError(f"tail pixel count {tail_px}, expected {TAIL_PIXELS}")
    return {"width": width, "height": height, "channels": channels,
            "compressed": True,
            "offsets": struct.unpack_from("<9I", data, 0x3c),
            # the largest level we can actually read today
            "thumb": data[-TAIL_BYTES:-20]}


def to_png(data: bytes) -> bytes:
    from PIL import Image
    info = parse(data)
    if info["compressed"]:
        raise CodecNotDecoded(
            "4 bpp level codec not decoded yet (see docs/journal.md)")
    img = Image.frombytes("RGBA", (info["width"], info["height"]),
                          info["pixels"])
    img = img.convert("RGBA" if info["channels"] == 4 else "RGB")
    buf = io.BytesIO()
    img.save(buf, "PNG")
    return buf.getvalue()


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path, help="a .tex file or a directory of them")
    ap.add_argument("-o", "--out", type=Path)
    ap.add_argument("--validate", action="store_true",
                    help="only check that the container parses; write nothing")
    args = ap.parse_args(argv)

    files = sorted(args.src.glob("*.tex")) if args.src.is_dir() else [args.src]
    if not files:
        ap.error(f"no .tex files in {args.src}")
    if not args.validate:
        if not args.out:
            ap.error("need -o OUT or --validate")
        args.out.mkdir(parents=True, exist_ok=True)

    written = skipped = 0
    bad = []
    for f in files:
        data = f.read_bytes()
        try:
            if args.validate:
                parse(data)
            else:
                (args.out / (f.stem + ".png")).write_bytes(to_png(data))
            written += 1
        except CodecNotDecoded:
            skipped += 1
        except (TexError, OSError) as e:
            bad.append((f.name, str(e)))
    for name, err in bad:
        print(f"ERROR {name}: {err}", file=sys.stderr)
    if args.validate:
        print(f"{written}/{len(files)} containers parsed", file=sys.stderr)
    else:
        print(f"{written} written, {skipped} skipped (codec not decoded), "
              f"{len(bad)} failed", file=sys.stderr)
    return 1 if bad else 0


if __name__ == "__main__":
    raise SystemExit(main())
