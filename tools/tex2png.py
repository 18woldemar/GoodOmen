#!/usr/bin/env python3
"""
tex2png.py -- convert .tex textures to PNG.

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
    0x24  u32  32 | 0          32 -> compressed TEXC chunk; 0 -> raw RGBA
    0x28  f32                  ? compression quality (Textures.txt, shipped
                               alongside, reports RMS/PSNR per texture)
    0x2c  u32  'TEXC'         chunk tag, stored LE so the bytes read "CXET"
    0x30  u32  0
    0x34  u32  width
    0x38  u32  height
    0x3c  u32[9]              bytes from the start of level i to end of file;
                              consecutive differences are the level sizes
    0x60  u32  0, 0
    0x68       payload

The payload is a mip chain in S3TC/DXT1 (8 bytes per 4x4 block, i.e. 4 bits
per pixel), **largest level first**, down to and including 8x8. The trailing
84 bytes hold the 4x4, 2x2 and 1x1 levels as raw RGBA (16+4+1 = 21 pixels of
4 bytes): those are too small to express as DXT1 blocks.

That yields an exact size identity, which this parser asserts and which holds
across the whole corpus:

    filesize == 104 + sum(w_i*h_i/2 for levels >= 8x8) + 84

DXT1 block decoding is delegated to Pillow: a level is wrapped in a 128-byte
DDS header. There is no hand-written block decoder here, and none is needed.

Usage:
    python3 tools/tex2png.py extracted/base -o png/
    python3 tools/tex2png.py extracted/base/angel.tex -o png/
    python3 tools/tex2png.py extracted/base --validate     # parse only
"""

from __future__ import annotations

import argparse
import io
import struct
import sys
from pathlib import Path

TYPE_TEX = 2001
TAG_TEXC = 0x54455843  # 'TEXC'
DATA_OFFSET = 0x68  # start of the mip chain in compressed files
RAW_OFFSET = 0x2c   # start of the pixels in uncompressed files (fonts)
MIN_DXT_DIM = 8     # below 8x8 the levels are stored as raw RGBA
TAIL_PIXELS = 21    # 4x4 + 2x2 + 1x1
TAIL_BYTES = TAIL_PIXELS * 4


class TexError(ValueError):
    pass


def _dds(width: int, height: int, blocks: bytes) -> bytes:
    """Minimal DDS wrapper around a single DXT1 level, for Pillow."""
    hdr = bytearray(128)
    hdr[0:4] = b"DDS "
    struct.pack_into(
        "<7I", hdr, 4,
        124,           # dwSize
        0x000A1007,    # CAPS|HEIGHT|WIDTH|PIXELFORMAT|LINEARSIZE|MIPMAPCOUNT
        height, width,
        len(blocks),   # dwPitchOrLinearSize
        0,             # dwDepth
        1,             # dwMipMapCount
    )
    struct.pack_into("<2I4s5I", hdr, 76,
                     32,          # ddspf.dwSize
                     0x4,         # DDPF_FOURCC
                     b"DXT1",
                     0, 0, 0, 0, 0)
    struct.pack_into("<I", hdr, 108, 0x1000)  # DDSCAPS_TEXTURE
    return bytes(hdr) + blocks


def levels(width: int, height: int) -> list[tuple[int, int]]:
    """Mip chain from full size down to 8x8."""
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


def parse(data: bytes) -> tuple[int, int, int, bool, bytes]:
    """-> (width, height, channels, is_compressed, top level payload)."""
    if len(data) < RAW_OFFSET:
        raise TexError("file shorter than the header")
    kind, _, width, height, channels = struct.unpack_from("<5I", data, 0)
    if kind != TYPE_TEX:
        raise TexError(f"type tag {kind}, expected {TYPE_TEX}")

    # field 0x24: 32 -> a compressed TEXC chunk follows, 0 -> raw RGBA (fonts)
    if struct.unpack_from("<I", data, 0x24)[0] == 0:
        want = RAW_OFFSET + width * height * 4
        if len(data) != want:
            raise TexError(f"{width}x{height} raw: {len(data)} != {want}")
        return width, height, channels, False, data[RAW_OFFSET:]

    if struct.unpack_from("<I", data, 0x2c)[0] != TAG_TEXC:
        raise TexError("no TEXC chunk")
    want = expected_size(width, height)
    if len(data) != want:
        raise TexError(f"{width}x{height}: size {len(data)} != {want}")
    top = width * height // 2
    return width, height, channels, True, data[DATA_OFFSET:DATA_OFFSET + top]


def to_png(data: bytes) -> bytes:
    from PIL import Image
    width, height, channels, compressed, payload = parse(data)
    if compressed:
        img = Image.open(io.BytesIO(_dds(width, height, payload)))
    else:
        img = Image.frombytes("RGBA", (width, height), payload)
    img = img.convert("RGBA" if channels == 4 else "RGB")
    buf = io.BytesIO()
    img.save(buf, "PNG")
    return buf.getvalue()


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path, help="a .tex file or a directory of them")
    ap.add_argument("-o", "--out", type=Path)
    ap.add_argument("--validate", action="store_true",
                    help="only check that the files parse; write nothing")
    args = ap.parse_args(argv)

    files = sorted(args.src.glob("*.tex")) if args.src.is_dir() else [args.src]
    if not files:
        ap.error(f"no .tex files in {args.src}")
    if not args.validate:
        if not args.out:
            ap.error("need -o OUT or --validate")
        args.out.mkdir(parents=True, exist_ok=True)

    bad = []
    for f in files:
        data = f.read_bytes()
        try:
            if args.validate:
                parse(data)
            else:
                (args.out / (f.stem + ".png")).write_bytes(to_png(data))
        except (TexError, OSError) as e:
            bad.append((f.name, str(e)))
    for name, err in bad:
        print(f"ERROR {name}: {err}", file=sys.stderr)
    print(f"{len(files) - len(bad)}/{len(files)} textures parsed",
          file=sys.stderr)
    return 1 if bad else 0


if __name__ == "__main__":
    raise SystemExit(main())
