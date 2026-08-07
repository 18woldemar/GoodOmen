#!/usr/bin/env python3
"""
acmcheck.py -- hold the engine's Interplay ACM decoder to `ffmpeg`'s.

Every other format in this project is checked against a Python reader written
here, which makes the check only as good as the reading. ACM does not need
that: `ffmpeg -f acm` is a decoder nobody here wrote, from a codebase that has
carried it for years, so it is ground truth in a way our own code can never be.

The engine prints one line per sound (`goodomen --wav`):

    name  decoded-bytes channels rate levels rows crc32

and this decodes the same sound with ffmpeg and compares the CRC32 of the PCM,
byte for byte. Anything less than exact equality is a wrong decoder: a codec
that is 99.9% right is one that clicks.

`--music` checks the 27 bare streams under `Music/` instead, which are the
only stereo ones and by far the longest -- 84 MiB against 85 MiB for all 992
sounds put together -- so they exercise the block loop far harder.

Usage:
    python3 tools/acmcheck.py extracted --run "$MDK2_GOG"
    python3 tools/acmcheck.py extracted engine-output.txt
    python3 tools/acmcheck.py --music "$MDK2_GOG/Music"
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import zlib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import wavc  # noqa: E402


def reference(payload: bytes, ffmpeg: str) -> bytes:
    """-> the PCM ffmpeg makes of an ACM stream.

    The trailing zeros are `wavc.FLUSH`, for the eleven streams that stop
    inside their last block; the engine gets the same effect for free by
    reading zeros past the end of the buffer.
    """
    p = subprocess.run(
        [ffmpeg, "-v", "error", "-f", "acm", "-i", "pipe:0",
         "-f", "s16le", "-c:a", "pcm_s16le", "pipe:1"],
        input=payload + b"\0" * wavc.FLUSH, capture_output=True)
    if p.returncode != 0 and not p.stdout:
        raise RuntimeError(p.stderr.decode(errors="replace").strip()[:200])
    return p.stdout


def check(files: list[Path], theirs: dict | None, ffmpeg: str,
          engine: list[str] | None = None) -> int:
    bad = []
    for f in files:
        info = wavc.parse(f.read_bytes())
        pcm = reference(info["payload"], ffmpeg)[:info["decoded"]]
        if len(pcm) != info["decoded"]:
            bad.append(f"{f.name}: ffmpeg gave {len(pcm)} of "
                       f"{info['decoded']} bytes")
            continue
        want = f"{zlib.crc32(pcm) & 0xffffffff:08x}"
        if engine:
            # the music has no line in `--wav`; ask the engine for the samples
            got = subprocess.run(engine + ["--pcm", str(f)],
                                 capture_output=True, check=True).stdout
            got = f"{zlib.crc32(got) & 0xffffffff:08x}"
        elif theirs is None:
            print(f"{f.name.lower()} {info['decoded']} {want}")
            continue
        else:
            got = theirs.get(f.name.lower())
        if got is None:
            bad.append(f"{f.name}: the engine did not read it")
        elif got != want:
            bad.append(f"{f.name}: crc32 {got}, ffmpeg says {want}")
    for line in bad[:20]:
        print(f"MISMATCH {line}", file=sys.stderr)
    print(f"{len(files)} streams, {len(bad)} disagree with ffmpeg",
          file=sys.stderr)
    return 1 if bad else 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path, help="the extracted resources, or a "
                                           "Music directory with --music")
    ap.add_argument("engine", type=Path, nargs="?",
                    help="a file of `goodomen --wav` output")
    ap.add_argument("--run", metavar="GAMEDIR",
                    help="run the engine over this installation instead")
    ap.add_argument("--music", action="store_true",
                    help="the 27 bare .acm streams, not the wrapped sounds")
    ap.add_argument("--ffmpeg", default="ffmpeg")
    ap.add_argument("--limit", type=int, default=0)
    args = ap.parse_args(argv)

    files = sorted(args.src.rglob("*.acm" if args.music else "*.wav"),
                   key=lambda p: p.name.lower())
    if not args.music:
        # the six genuine RIFF footsteps are not ACM at all
        files = [f for f in files if f.read_bytes()[:4] != wavc.RIFF]
    if args.limit:
        files = files[:args.limit]
    if not files:
        print(f"no streams under {args.src}", file=sys.stderr)
        return 1

    run = ["cargo", "run", "--quiet", "--release", "--manifest-path",
           str(Path(__file__).resolve().parent.parent / "engine/Cargo.toml"), "--"]
    if args.music:
        return check(files, None, args.ffmpeg, engine=run)

    theirs = None
    if args.run or args.engine:
        if args.run:
            out = subprocess.run(run + [args.run, "--wav"],
                                 capture_output=True, text=True,
                                 check=True).stdout
        else:
            out = args.engine.read_text()
        theirs = {}
        for line in out.splitlines():
            f = line.split()
            if len(f) == 7:
                theirs[f[0]] = f[6]

    return check(files, theirs, args.ffmpeg)


if __name__ == "__main__":
    raise SystemExit(main())
