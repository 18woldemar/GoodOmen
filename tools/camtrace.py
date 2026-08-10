#!/usr/bin/env python3
"""
camtrace.py -- the original's own camera, frame by frame, out of a GL trace.

The project's hardest gap has been that nothing could be *disproved*. The
recorded demos carry inputs and no positions, so a walk that looks right and
a walk that is right score the same. This closes it: the original engine is
made to replay `demo1_5.omn` while every GL call is captured, and the camera
comes out of the capture as a table of frame -> eye position and facing, to
hold our own replay against.

**Why a GL trace works at all.** `mdk2Main.exe` does not import a graphics
library -- it resolves `gl*` by name at run time, which is why the whole
OpenGL name table sits in its strings. So the engine is native fixed-function
OpenGL, and the camera is not inferred from anything: it is `glLoadMatrixf`
on `GL_MODELVIEW`.

**Capturing it.** Wine 11 drives OpenGL through **EGL**, not GLX, so apitrace
needs `--api egl`; with `--api gl` the run produces no trace file at all and
no error. Under the wow64 build a 32-bit game runs in a 64-bit host process,
so the ordinary 64-bit apitrace wrappers apply. X authority has to be passed
explicitly when the run is not started from the session's own shell.

    apitrace trace --api egl --output mdk2.trace -- wine mdk2Main.exe
    apitrace dump --arg-names=no --grep='glLoadMatrixf|eglSwapBuffers' \
        mdk2.trace > mats.txt
    python3 tools/camtrace.py mats.txt

To make the game replay the demo rather than sit in its menu, put a
`start.lua` in the installation's `override/` directory -- the same loose-file
path the 1.003 patch ships `level1.lua` through, so it shadows `scripts.zip`:

    CreateKurt()
    playdemo = 1
    level(1, 5)

`level()` in `mdk2.lua` already does the rest: `playdemo == 1` makes it call
`mdkPlayDemo("demo1_5")`, the name being `demo<level>_<checkpoint>`. Delete
the file afterwards and the installation is untouched.

**Which matrix is the camera.** A frame loads dozens of modelviews, one per
drawn thing, each of them `view * model`. The world's own geometry is drawn
with `model` identity, so its modelview *is* the view matrix -- but picking
the most frequently repeated matrix fails: every fifteenth frame the tally is
won by something else and the camera jumps hundreds of units. What holds is
**continuity**: from one frame to the next the camera moves a fraction of a
unit, and no other matrix in the frame is anywhere near where the camera was.
So the first frame is seeded by the tally and every frame after it takes the
non-identity matrix whose eye is nearest the previous frame's. Identity is
excluded outright because the HUD loads it.

Two checks say this is the camera, and neither is available to the extraction:

  - **The span is the demo.** The capture holds **1349** consecutive world
    frames where `omn.py` reads **1348** in `demo1_5.omn`. The original
    renders one frame per recorded frame, so the trace aligns with the demo
    one to one and no resampling is needed.
  - **The speed is Kurt's.** The demo runs at **29.94 fps**, not 60, and at
    that rate the recovered path's 95th-percentile speed is **17.4 units a
    second** against the **15** read out of the speed table at 0x42b678.
    A chase camera on the outside of a turn covers a little more ground than
    the man it follows, which is the direction the difference falls in.
    *An earlier note here put the figure at 16.6/s by assuming 60 fps; it was
    wrong by the ratio of the two rates and is corrected here, not deleted.*

**The eye.** A view matrix maps world to eye, so the camera's world position
is `-R^T t`, with GL's column-major layout putting `R`'s columns at 0,4,8 /
1,5,9 / 2,6,10 and `t` at 12,13,14. The facing is `R`'s third row negated --
GL looks down -Z in eye space.

Usage:
    python3 tools/camtrace.py mats.txt              # frame x y z fx fy fz
    python3 tools/camtrace.py mats.txt --summary    # spans and speeds
    python3 tools/camtrace.py --selftest
"""

from __future__ import annotations

import argparse
import math
import re
import sys
from collections import Counter

MATRIX = re.compile(r"glLoadMatrixf\(\{([^}]*)\}\)")
IDENTITY = (1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1)

# A frame drawing fewer modelviews than this is a menu or a loading screen,
# not the world: the menu frames of a demo capture load about 22 and the
# gameplay frames 47 to 126.
WORLD = 40


def is_identity(m: tuple[float, ...]) -> bool:
    return all(abs(a - b) < 1e-3 for a, b in zip(m, IDENTITY))


def eye(m: tuple[float, ...]) -> tuple[float, float, float]:
    """Where the camera is in world space: -R^T t."""
    r = ((m[0], m[4], m[8]), (m[1], m[5], m[9]), (m[2], m[6], m[10]))
    t = m[12:15]
    return tuple(-sum(r[k][c] * t[k] for k in range(3)) for c in range(3))


def facing(m: tuple[float, ...]) -> tuple[float, float, float]:
    """Where it looks: GL's eye space looks down -Z, so -R's third row.

    Column-major, so row 2 is m[2], m[6], m[10] -- not the third *column*
    m[8..10], which is the mistake that reads as a camera facing sideways.
    """
    return (-m[2], -m[6], -m[10])


def frames(text: str) -> dict[int, list[tuple[float, ...]]]:
    """Every modelview loaded, grouped by the swap that ended its frame."""
    out: dict[int, list] = {}
    n = 0
    for line in text.splitlines():
        if "SwapBuffers" in line:
            n += 1
            continue
        m = MATRIX.search(line)
        if m:
            out.setdefault(n, []).append(
                tuple(float(x) for x in m.group(1).split(", ")))
    return out


def track(per_frame: dict[int, list]) -> list[tuple[int, tuple, tuple]]:
    """(frame, eye, facing) for every frame that drew the world."""
    out = []
    previous = None
    for n in sorted(per_frame):
        candidates = [m for m in per_frame[n] if not is_identity(m)]
        if len(per_frame[n]) < WORLD or not candidates:
            previous = None          # a menu frame breaks the chain
            continue
        if previous is None:
            tally = Counter(tuple(round(v, 4) for v in m) for m in candidates)
            wanted = tally.most_common(1)[0][0]
            best = next(m for m in candidates
                        if tuple(round(v, 4) for v in m) == wanted)
        else:
            best = min(candidates, key=lambda m: sum(
                (a - b) ** 2 for a, b in zip(eye(m), previous)))
        previous = eye(best)
        out.append((n, previous, facing(best)))
    return out


def spans(rows: list) -> list[tuple[int, int]]:
    """Contiguous runs of world frames, which is one replay each."""
    out = []
    start = last = None
    for n, _, _ in rows:
        if last is None or n != last + 1:
            if last is not None:
                out.append((start, last))
            start = n
        last = n
    if last is not None:
        out.append((start, last))
    return out


def selftest() -> None:
    """Build a view matrix from a known camera and read the camera back."""
    def view(px, py, pz, yaw):
        # right, up, back in world space for a camera at (px,py,pz) turned by
        # yaw about Z, written the way GL stores a column-major matrix.
        c, s = math.cos(yaw), math.sin(yaw)
        r, u, b = (c, s, 0.0), (0.0, 0.0, 1.0), (s, -c, 0.0)
        t = tuple(-sum(v[k] * p for k, p in enumerate((px, py, pz)))
                  for v in (r, u, b))
        return (r[0], u[0], b[0], 0.0, r[1], u[1], b[1], 0.0,
                r[2], u[2], b[2], 0.0, t[0], t[1], t[2], 1.0)

    m = view(10.0, -20.0, 3.0, 0.7)
    assert all(abs(a - b) < 1e-6 for a, b in zip(eye(m), (10.0, -20.0, 3.0)))
    # facing is -back, which for this build is (-sin yaw, cos yaw, 0)
    f = facing(m)
    assert abs(f[0] + math.sin(0.7)) < 1e-6 and abs(f[1] - math.cos(0.7)) < 1e-6

    # And the tracker follows the near matrix rather than the frequent one:
    # a frame where eleven copies of a far matrix outvote the camera.
    near, far = view(10.1, -20.0, 3.0, 0.7), view(400.0, 400.0, 0.0, 0.0)
    per = {1: [m] * 9 + [far] * 3, 2: [far] * 11 + [near]}
    per[1] += [IDENTITY] * 40          # pad past the world threshold
    per[2] += [IDENTITY] * 40
    got = track(per)
    assert len(got) == 2, got
    assert abs(got[1][1][0] - 10.1) < 1e-4, got[1]
    print("camtrace: self-tests pass")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[1])
    ap.add_argument("dump", nargs="?", help="apitrace dump, filtered")
    ap.add_argument("--summary", action="store_true")
    # A recorded demo is not 60 Hz: omn.py reads 29.94 out of demo1_5, and
    # assuming otherwise doubles every speed this prints.
    ap.add_argument("--fps", type=float, default=29.94)
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()

    if args.selftest or not args.dump:
        selftest()
        return 0

    rows = track(frames(open(args.dump, encoding="latin1").read()))
    if not rows:
        print("no world frames in this dump", file=sys.stderr)
        return 1

    if args.summary:
        by = {n: (e, f) for n, e, f in rows}
        for a, b in spans(rows):
            step = sorted(math.dist(by[n][0], by[n + 1][0])
                          for n in range(a, b))
            walk = sum(step)
            rise = [by[n][0][2] for n in range(a, b + 1)]
            p95 = step[int(len(step) * 0.95)] * args.fps
            print(f"frames {a}..{b}  ({b - a + 1})  travelled {walk:8.1f} "
                  f"(mean {walk / (b - a) * args.fps:6.2f}/s, p95 {p95:6.2f}/s)"
                  f"  height {min(rise):8.2f}..{max(rise):8.2f}")
        return 0

    for n, e, f in rows:
        print(f"{n} {e[0]:.4f} {e[1]:.4f} {e[2]:.4f} "
              f"{f[0]:.4f} {f[1]:.4f} {f[2]:.4f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
