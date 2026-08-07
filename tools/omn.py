#!/usr/bin/env python3
"""
omn.py -- read `.omn`: a recorded demo, the attract-mode playback.

One file ships, `base/demo1_5.omn`, and it is the last of the asset types to
be identified. It is a flat stream of 8-byte records and nothing else --
23584 = 2948 x 8, no header, though the type tag 1005 that would sit at offset
0 in a `.tex` or `.mod` happens to be the first record's command id.

    record, 8 bytes:  { u32 command; f32 value }

A command of **0xFFFFFFFF ends a frame**, and then `value` is that frame's
delta time. There are 1348 of them. Drop the first, which is 8.59 s and is the
level load, and the remaining 1347 total 44.98 s: a mean of 0.03339 s, and
**1328 of them are within 1% of 1/30**. The demo was recorded at 30 fps, and
plays back against `chGetDeltaT` rather than against a frame counter.

The other 1600 records are input. The ids are the engine's `COM_*` constants,
which the shipped Lua uses through `omMakeCommand` and `omBindCommand` but
never assigns, and they fall into two kinds by their values alone:

    13, 57, 200, 203, 205, 208, 1000, 1001   always exactly 1.0 -- buttons
    1004, 1005, 1006, 1007                   0.02 to 1.15 -- analogue axes

So this is a controller trace, not a position trace: replaying it needs the
same physics, which makes it the strictest end-to-end test this project could
have. If GoodOmen ever plays this file back and the camera ends where the
original's does, the engine is right.

Usage:
    python3 tools/omn.py extracted/base/demo1_5.omn
    python3 tools/omn.py extracted/base/demo1_5.omn --frames 20
"""

from __future__ import annotations

import argparse
import struct
import sys
from collections import Counter
from pathlib import Path

RECORD = 8
END_OF_FRAME = 0xFFFFFFFF


class OmnError(ValueError):
    pass


def parse(data: bytes) -> list[dict]:
    """-> one dict per frame: {"dt": seconds, "input": [(command, value)]}."""
    if len(data) % RECORD:
        raise OmnError(f"{len(data)} bytes is not a multiple of {RECORD}")
    frames, pending = [], []
    for i in range(len(data) // RECORD):
        command, value = struct.unpack_from("<If", data, i * RECORD)
        if command == END_OF_FRAME:
            frames.append({"dt": value, "input": pending})
            pending = []
        else:
            pending.append((command, value))
    if pending:
        raise OmnError(f"{len(pending)} input records after the last frame")
    return frames


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path)
    ap.add_argument("--frames", type=int, default=0,
                    help="print this many frames of input")
    args = ap.parse_args(argv)

    frames = parse(args.src.read_bytes())
    for f in frames[:args.frames]:
        pressed = " ".join(f"{c}={v:g}" for c, v in f["input"])
        print(f"dt {f['dt']:.5f}  {pressed}")

    body = [f["dt"] for f in frames[1:]]
    commands = Counter(c for f in frames for c, _v in f["input"])
    buttons = {c for c in commands
               if all(v == 1.0 for f in frames for cc, v in f["input"]
                      if cc == c)}
    print(f"{len(frames)} frames, {sum(commands.values())} input records, "
          f"{sum(body):.2f} s after a {frames[0]['dt']:.2f} s load, "
          f"{len(body) / sum(body):.2f} fps", file=sys.stderr)
    print(f"buttons {sorted(buttons)}, "
          f"axes {sorted(set(commands) - buttons)}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
