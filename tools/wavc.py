#!/usr/bin/env python3
"""
wavc.py -- the sounds. They are not `.wav` files, whatever the extension says.

992 of the 998 files named `*.wav` are not RIFF at all. They begin `WAVCV1.0`
and carry a 28-byte header of their own:

    0x00  char[8]  "WAVCV1.0"
    0x08  u32      decompressed size, in bytes
    0x0c  u32      compressed size -- and 28 + this is always the file length
    0x10  u32      28, the header size
    0x14  u16      channels, always 1
    0x16  u16      bits per sample, always 16
    0x18  u16      sample rate: 22050 for 653 of them, 11025 for 337, 44100
                   for two
    0x1a  u16      0x77ED, constant

and what follows is an **Interplay ACM** stream, which its own magic at offset
28 confirms: `97 28 03 01`. That is the codec Interplay used across the
Infinity Engine, and this is the same `Chitin/` platform the debug paths in
the executable name -- BioWare carried its sound layer over from Baldur's Gate
into MDK2 and never renamed the files.

Which means nothing here needs writing. `ffmpeg` has decoded `interplayacm`
for years; this tool reads the header, hands the payload over, and trims the
result to the length the header states.

The six genuine RIFF files are footstep sounds -- `kurt_walkl.wav`,
`cone_walkr.wav` and four others -- short enough that compressing them was not
worth it.

**The music is the same codec without the wrapper.** `Music/` holds 27 bare
`.acm` streams, one per track, stereo at 22050 Hz and 84 MiB in total, each
beside a `.mus` playlist -- the Infinity Engine's playlist format again,
unchanged:

    Track01
    1
    A   Track01 A

a name, a segment count, and one line per segment giving its tag, its
directory and the tag to loop back to. Every one of the 27 is a single
segment looping on itself. This tool reads a bare `.acm` as happily as a
wrapped one; it just has no header to trim against, so the whole decode is
kept.

Checked across all 992: every one has the magic, every one has the ACM magic
at offset 28, every one satisfies `28 + compressed == filesize`, and **all 992
decode to exactly the number of bytes their header states**. Compression runs
from 2.1:1 to 14.7:1, averaging 3.9.

Usage:
    python3 tools/wavc.py extracted --validate
    python3 tools/wavc.py extracted/local/dr_bath1.wav -o dr_bath1.wav
    python3 tools/wavc.py extracted/sounds -o wav/
"""

from __future__ import annotations

import argparse
import shutil
import struct
import subprocess
import sys
from pathlib import Path

MAGIC = b"WAVCV1.0"
HEADER = 28
ACM_MAGIC = b"\x97\x28\x03\x01"
CONSTANT = 0x77ED
RIFF = b"RIFF"


class WavcError(ValueError):
    pass


def parse(data: bytes) -> dict:
    if data[:4] == ACM_MAGIC:            # music: a bare stream, no wrapper
        # ACM's own count is samples across all channels, not frames: for
        # track01 it is 12802458, which at 22050 Hz stereo is 4:50 -- what
        # ffprobe reports -- and twice that if you read it as frames
        samples, channels = struct.unpack_from("<IH", data, 4)
        rate = struct.unpack_from("<H", data, 10)[0]
        return {"decoded": samples * 2, "compressed": len(data),
                "channels": channels, "bits": 16, "rate": rate,
                "payload": data, "bare": True}
    if data[:8] != MAGIC:
        raise WavcError("not a WAVCV1.0 file")
    decoded, compressed, header = struct.unpack_from("<3I", data, 8)
    channels, bits = struct.unpack_from("<2H", data, 0x14)
    rate, constant = struct.unpack_from("<2H", data, 0x18)
    if header != HEADER:
        raise WavcError(f"header size {header}, expected {HEADER}")
    if header + compressed != len(data):
        raise WavcError(f"{header} + {compressed} != {len(data)}")
    if constant != CONSTANT:
        raise WavcError(f"constant {constant:#x}, expected {CONSTANT:#x}")
    if data[HEADER:HEADER + 4] != ACM_MAGIC:
        raise WavcError("no Interplay ACM magic at the payload")
    return {"decoded": decoded, "compressed": compressed, "channels": channels,
            "bits": bits, "rate": rate, "payload": data[HEADER:],
            "bare": False}


# The last ACM block is not padded out by the encoder, and 11 of the 992
# streams end so close to a block boundary that the decoder runs out of input
# before it can flush -- it stops short, by up to 3746 bytes on `reset.wav`.
# Sixty-four zero bytes are enough for every one of them, and the header says
# where the real audio ends, so the overshoot is trimmed off afterwards.
FLUSH = 64


def to_wav(data: bytes, ffmpeg: str = "ffmpeg") -> bytes:
    """-> a RIFF WAV, trimmed to the length the header states."""
    info = parse(data)
    p = subprocess.run([ffmpeg, "-v", "error", "-f", "acm", "-i", "pipe:0",
                        "-f", "wav", "pipe:1"],
                       input=info["payload"] + b"\0" * FLUSH,
                       capture_output=True)
    out = bytearray(p.stdout)
    body = _data_chunk(out)
    if len(out) - body < info["decoded"]:
        raise WavcError(f"decoded {max(0, len(out) - body)} of "
                        f"{info['decoded']} bytes: "
                        + p.stderr.decode(errors="replace").strip()[:120])
    del out[body + info["decoded"]:]
    struct.pack_into("<I", out, 4, len(out) - 8)             # RIFF size
    struct.pack_into("<I", out, body - 4, info["decoded"])   # data chunk size
    return bytes(out)


def _data_chunk(wav: bytes) -> int:
    """Offset of the `data` chunk's payload. ffmpeg writes a LIST chunk in
    between, so this is 78 rather than the 44 a minimal WAV would have, and
    the size it writes there is 0xFFFFFFFF because it is streaming."""
    if wav[:4] != b"RIFF" or wav[8:12] != b"WAVE":
        raise WavcError("ffmpeg did not produce a RIFF WAVE")
    i = 12
    while i + 8 <= len(wav):
        cid = wav[i:i + 4]
        size = struct.unpack_from("<I", wav, i + 4)[0]
        if cid == b"data":
            return i + 8
        i += 8 + size + (size & 1)
    raise WavcError("no data chunk")


def segment_file(root: Path, name: str, tag: str) -> Path | None:
    """Where a playlist entry's audio actually is, or None.

    `Music/Track01.mus` says `A   Track01 A`, and the stream is
    `Music/Track01/track01a.acm` -- the directory and the tag concatenated,
    under a directory of the same name. The case on disk is not consistent
    (`track01a.acm` beside `Track18a.acm`), so the lookup has to be
    case-insensitive.
    """
    want = (name + tag + ".acm").lower()
    folder = root / name
    if not folder.is_dir():
        for d in root.iterdir():
            if d.is_dir() and d.name.lower() == name.lower():
                folder = d
                break
        else:
            return None
    for f in folder.iterdir():
        if f.name.lower() == want:
            return f
    return None


def playlist(path: Path) -> dict:
    """Read a `.mus`: a name, a segment count, then one line per segment."""
    lines = [l.strip() for l in path.read_text(errors="replace").splitlines()
             if l.strip()]
    if len(lines) < 2:
        raise WavcError(f"{path.name}: too short to be a playlist")
    name, count = lines[0], int(lines[1])
    segments = []
    for line in lines[2:2 + count]:
        parts = line.split()
        segments.append({"tag": parts[0],
                         "directory": parts[1] if len(parts) > 1 else name,
                         "loops_to": parts[2] if len(parts) > 2 else None})
    if len(segments) != count:
        raise WavcError(f"{path.name}: {len(segments)} segments, said {count}")
    return {"name": name, "segments": segments}


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path, help="a file or a directory")
    ap.add_argument("-o", "--out", type=Path,
                    help="a .wav, or a directory to fill")
    ap.add_argument("--validate", action="store_true",
                    help="check every header; decode nothing")
    ap.add_argument("--playlists", action="store_true",
                    help="read the .mus playlists beside the music")
    ap.add_argument("--ffmpeg", default="ffmpeg")
    args = ap.parse_args(argv)

    if args.playlists:
        mus = sorted(args.src.rglob("*.mus"))
        total = found = 0
        for m in mus:
            pl = playlist(m)
            total += len(pl["segments"])
            where = [segment_file(m.parent, s["directory"], s["tag"])
                     for s in pl["segments"]]
            found += sum(1 for w in where if w)
            print("  " + f"{pl['name']:12s} " + ", ".join(
                f"{s['tag']} -> {w.name if w else 'MISSING'}"
                f", loops to {s['loops_to']}"
                for s, w in zip(pl["segments"], where)))
        # parsing a playlist is not reading it: follow the names to the files
        print(f"{total} segments in {len(mus)} playlists, "
              f"{found} resolve to a stream", file=sys.stderr)
        return 0 if found == total else 1

    if args.src.is_dir():
        files = (sorted(args.src.rglob("*.wav"))
                 + sorted(args.src.rglob("*.acm")))
    else:
        files = [args.src]
    if not files:
        ap.error(f"no .wav or .acm under {args.src}")

    if args.validate:
        ok = riff = 0
        bad = []
        for f in files:
            data = f.read_bytes()
            if data[:4] == RIFF:
                riff += 1
                continue
            try:
                parse(data)
                ok += 1
            except WavcError as e:
                bad.append(f"{f.name}: {e}")
        for line in bad[:5]:
            print(f"  {line}", file=sys.stderr)
        print(f"{ok} ACM streams valid, {riff} plain RIFF, {len(bad)} bad",
              file=sys.stderr)
        return 1 if bad else 0

    if not args.out:
        ap.error("need -o OUT or --validate")
    if not shutil.which(args.ffmpeg):
        ap.error(f"{args.ffmpeg} not found; it decodes the ACM payload")
    if args.out.suffix.lower() == ".wav":
        args.out.write_bytes(to_wav(files[0].read_bytes(), args.ffmpeg))
        print(f"{args.out}", file=sys.stderr)
        return 0

    args.out.mkdir(parents=True, exist_ok=True)
    done = copied = failed = 0
    for f in files:
        data = f.read_bytes()
        try:
            if data[:4] == RIFF:
                (args.out / f.name).write_bytes(data)
                copied += 1
            else:
                (args.out / f.name).write_bytes(to_wav(data, args.ffmpeg))
                done += 1
        except WavcError as e:
            print(f"  {f.name}: {e}", file=sys.stderr)
            failed += 1
    print(f"{done} decoded, {copied} already RIFF, {failed} failed",
          file=sys.stderr)
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
