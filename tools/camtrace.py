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

**The camera is not the player, and the offset is exact.** On a pure turn --
the demo's frames 14 to 30, where the only input is the turn axis -- the body
does not move and the camera swings three units around it. So a camera track
compared against a body track is comparing two different things. The camera
*looks at* the player, so `player = eye + 4 * facing` recovers him, and the
4 is not fitted: at the first frame that lands on **(271, -69)**, which is
level 1 checkpoint 5 to the unit.

**Measuring the turn (`--demo`).** With the player recovered, the camera's own
heading is his heading, and the demo supplies the axis that produced it. So
integrating the axis through a candidate turn law and comparing the angle
against the capture measures the law over 1348 samples at once, without
touching a position -- which matters, because position integrates every other
error too and diverges within forty frames whatever the turn does.

The answer over `demo1_5` is **a flat 0.306 radians per unit of recorded
axis**: 8.3 degrees RMS across 45 seconds and 0.4 degrees of error at the end,
against 633 degrees RMS for the flat 1.0 the replay had been using. Two
findings come with it:

  - **The flat law beats `turn_from_axis`.** Feeding the recorded axis through
    the shaping curve read out of the binary gives 10.3 degrees RMS at best,
    and the extra parameter buys nothing. The recorded axis reaches **1.15**
    in magnitude, which a normalised stick cannot, so `chRecordInput` stores
    the value *after* the input layer shaped it. Applying the curve on a demo
    path would apply it twice.
  - **The turn was never the reason the replay wandered.** With the measured
    turn the body meets a wall on **338** frames instead of 66 and finishes
    **inside geometry on 28**, because it is still a point: no width, no
    sweep. A turn three times too fast had been swinging it clear.

Usage:
    python3 tools/camtrace.py mats.txt              # frame x y z fx fy fz
    python3 tools/camtrace.py mats.txt --player     # the body, not the camera
    python3 tools/camtrace.py mats.txt --summary    # spans and speeds
    python3 tools/camtrace.py mats.txt --demo demo1_5.omn    # the turn law
    python3 tools/camtrace.py --selftest
"""

from __future__ import annotations

import argparse
import math
import re
import sys
from pathlib import Path
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


# How far behind the player the camera sits. Not fitted: at the first frame
# of demo1_5 `eye + FOLLOW * facing` is (271, -69), which is level 1
# checkpoint 5 exactly.
# Measured, not assumed: over the demo's turn on the spot the eye sits
# 4.0000 back along its own look on every frame, to four places.
FOLLOW = 4.0

# The demo's two turn commands, right and left. omn.py lists them as axes.
TURN_RIGHT, TURN_LEFT = 1004, 1005


def player(eye_xyz, face, follow: float = FOLLOW):
    """Where the man the camera is looking at stands."""
    n = math.hypot(face[0], face[1]) or 1.0
    return (eye_xyz[0] + follow * face[0] / n,
            eye_xyz[1] + follow * face[1] / n, eye_xyz[2])


def orbit(rows: list, at, feet: float, first: int, last: int) -> dict:
    """How far back the camera sits, off a **turn on the spot**.

    The player holds one position while the camera swings round him, so every
    quantity here is a difference against a point that is known exactly and
    no alignment of any kind is needed.

    This replaces a measurement that matched the trace to the game's memory
    by nearest point -- and that was **circular**, because the matching used
    `player()`, which assumes the follow distance, and then reported it back.
    A camera built five units back came out as 4.15 through it. The self-test
    below is that bug, kept.

    `at` is where the player stands, `feet` the z of his feet, and `first`
    and `last` bracket the frames of the turn. -> the distance back along the
    camera's own look, the pivot above the feet, and the spread of each.
    """
    back, pivot, sideways = [], [], []
    for k in range(first, last + 1):
        _, eye, look = rows[k]
        pitch = math.atan2(look[2], math.hypot(look[0], look[1]))
        flat = math.hypot(eye[0] - at[0], eye[1] - at[1])
        d = flat / (math.cos(pitch) or 1e-9)
        back.append(d)
        pivot.append((eye[2] - feet) - d * -math.sin(pitch))
        # the eye must lie opposite the look, or it is not behind him at all
        n = math.hypot(look[0], look[1]) or 1.0
        sideways.append(((eye[0] - at[0]) * look[1] - (eye[1] - at[1]) * look[0]) / n)
    mid = lambda xs: sorted(xs)[len(xs) // 2]
    return {"frames": len(back), "back": mid(back), "pivot": mid(pivot),
            "back_spread": max(back) - min(back),
            "pivot_spread": max(pivot) - min(pivot),
            "sideways": mid(sideways)}


def replayed(rows: list, path: list, back: float = 4.0, pivot: float = 1.5168,
             eye_height: float = 1.7) -> list[tuple[int, float]]:
    """`[(frame, how far our replay is from the original)]`, frame by frame.

    The chase camera has no spring: the eye sits `(feet + pivot) - back *
    look` to a ten-thousandth of a unit, so **`eye + back * look` is the
    original's own player position** on every frame the trace caught. The
    trace is one world frame per demo tick -- its numbering has a single gap
    and that gap falls after world frame 1348, which is the demo's length.

    So this needs no alignment: `path[k]` against `rows[k]`, where `path` is
    what `walksim.py --track` wrote. Being able to say *which frame* the
    replay leaves the original, rather than how far apart they end up, is
    what the rigid camera buys.
    """
    out = []
    for k in range(min(len(rows), len(path))):
        _, e, look = rows[k]
        p = path[k]
        theirs = (e[0] + look[0] * back, e[1] + look[1] * back)
        out.append((k, math.dist(theirs, (p[0], p[1]))))
    return out


def turn_scale(rows: list, demo_frames: list) -> tuple[float, float, float]:
    """Radians of yaw per unit of recorded axis, and how well it fits.

    Returns (scale, RMS in radians, final error in radians).

    **The fit is on the accumulated angle, not on the per-frame change**, and
    that is not a detail. A chase camera is smoothed: it does not reach the
    player's heading on the frame he turns, it catches up over several. So a
    per-frame regression of angle change on axis is attenuated by the lag and
    reads **0.1488 rad per unit at 145 degrees RMS** -- less than half the
    truth, and confidently wrong. The lag cancels in the integral, because a
    camera that ends up behind the player has turned exactly as far as he has.
    Fitting the cumulative series gives 0.306 at 8.3 degrees.
    """
    n = min(len(rows), len(demo_frames))
    angle = [math.atan2(f[1], f[0]) for _, _, f in rows[:n]]
    for i in range(1, n):                                  # unwrap
        d = (angle[i] - angle[i - 1] + math.pi) % (2 * math.pi) - math.pi
        angle[i] = angle[i - 1] + d
    raw = []
    for f in demo_frames[:n]:
        held = {c: v for c, v in f["input"]}
        raw.append(held.get(TURN_RIGHT, 0.0) - held.get(TURN_LEFT, 0.0))
    # The accumulated axis, against which the accumulated angle is fitted.
    # yaw falls as the right axis rises, hence the sign on the prediction.
    total, swept = 0.0, [0.0]
    for r in raw[:n - 1]:
        total += r
        swept.append(total)
    num = sum(-c * (angle[i] - angle[0]) for i, c in enumerate(swept))
    den = sum(c * c for c in swept) or 1.0
    scale = num / den
    mine = [angle[0]]
    for r in raw[:n - 1]:
        mine.append(mine[-1] - scale * r)
    rms = (sum((a - b) ** 2 for a, b in zip(mine, angle)) / n) ** 0.5
    return scale, rms, mine[-1] - angle[-1]


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
    # A camera that lags the player must still yield the player's turn rate.
    # This is the case a per-frame fit gets badly wrong, so it is pinned:
    # the player turns 0.3 rad per unit of a square wave, the camera follows
    # a fifth of the way to him each frame, and the scale must come back 0.3.
    want, heading, lens = 0.3, 0.0, 0.0
    rows_, demo = [], []
    for i in range(400):
        axis = 1.0 if (i // 40) % 2 == 0 else -0.5
        heading -= want * axis
        lens += (heading - lens) * 0.2               # the smoothing
        rows_.append((i, (0.0, 0.0, 0.0),
                      (math.cos(lens), math.sin(lens), 0.0)))
        demo.append({"input": [(TURN_RIGHT, max(axis, 0.0)),
                               (TURN_LEFT, max(-axis, 0.0))]})
    scale, rms, _ = turn_scale(rows_, demo)
    assert abs(scale - want) < 0.01, scale
    # The residual is not small here and is not meant to be: this camera lags
    # far harder than the real one, so it sits tens of degrees behind through
    # every swing. What the integral recovers is the *rate*, and that is the
    # claim under test. The real capture's residual is 8 degrees.
    assert math.degrees(rms) < 60, math.degrees(rms)

    # A camera five back along a look pitched twelve degrees down, from a
    # pivot 1.2 above the feet, swinging round a player who stands still.
    # The measurement that matched by nearest point read this as 4.15,
    # because it used player() -- which assumes four -- to build the pairing.
    # Reading it off the orbit assumes nothing.
    at, feet, D, h = (10.0, -3.0), 5.0, 5.0, 1.2
    made = []
    for k in range(40):
        a = math.radians(k * 3.0)
        pitch_ = math.radians(-12.0)
        look = (math.cos(a) * math.cos(pitch_), math.sin(a) * math.cos(pitch_),
                math.sin(pitch_))
        # not `eye`: that is this module's own function, and shadowing it
        # here is the same slip `frames` caused once already
        e = (at[0] - look[0] * D, at[1] - look[1] * D, feet + h - look[2] * D)
        made.append((k, e, look))
    got = orbit(made, at, feet, 0, 39)
    assert abs(got["back"] - D) < 1e-6, got
    assert abs(got["pivot"] - h) < 1e-6, got
    assert got["back_spread"] < 1e-6 and got["pivot_spread"] < 1e-6, got
    assert abs(got["sideways"]) < 1e-9, got
    # and a camera that is not looking straight back at him is caught: turn
    # each look twenty degrees and the eye stops being opposite it. Pushing
    # the eye by a fixed vector instead would not do -- over a whole orbit
    # that is sideways one way as often as the other, and the median is zero.
    turned = []
    for k, e, l in made:
        a = math.radians(20.0)
        turned.append((k, e, (l[0] * math.cos(a) - l[1] * math.sin(a),
                              l[0] * math.sin(a) + l[1] * math.cos(a), l[2])))
    assert abs(orbit(turned, at, feet, 0, 39)["sideways"]) > 1.5


    # `replayed` on a camera built from a known walk: a body going due +y at
    # half a unit a frame, the eye four back along a level look. A replay
    # that matches it reads zero, and one that is a frame behind reads the
    # frame's own step.
    look = (0.0, 1.0, 0.0)
    walk = [(0.0, i * 0.5, 0.0) for i in range(40)]
    made = [(i, (p[0] - look[0] * 4.0, p[1] - look[1] * 4.0, p[2] - 1.7 + 1.5168), look)
            for i, p in enumerate(walk)]
    assert max(d for _, d in replayed(made, walk)) < 1e-9
    late = [walk[0]] + walk[:-1]
    apart = [d for _, d in replayed(made, late)][1:]
    assert all(abs(d - 0.5) < 1e-9 for d in apart), apart[:4]

    print("camtrace: self-tests pass")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[1])
    ap.add_argument("dump", nargs="?", help="apitrace dump, filtered")
    ap.add_argument("--summary", action="store_true")
    ap.add_argument("--player", action="store_true",
                    help="emit where the player stands, not the camera")
    ap.add_argument("--demo", type=Path, metavar="OMN",
                    help="measure the turn law against this recording")
    ap.add_argument("--expect-rms", type=float, metavar="DEG",
                    help="fail unless the fitted law tracks this closely")
    # A recorded demo is not 60 Hz: omn.py reads 29.94 out of demo1_5, and
    # assuming otherwise doubles every speed this prints.
    ap.add_argument("--fps", type=float, default=29.94)
    ap.add_argument("--orbit", metavar="X,Y,FEETZ,FIRST,LAST",
                    help="measure the chase camera off a turn on the spot: "
                         "where the player stands, the z of his feet, and the "
                         "frames the turn spans")
    ap.add_argument("--against", type=Path, metavar="TRACK",
                    help="a walksim.py --track file of the same demo: say "
                         "which frame the replay leaves the original")
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()

    if args.selftest or not args.dump:
        selftest()
        return 0

    rows = track(frames(open(args.dump, encoding="latin1").read()))
    if not rows:
        print("no world frames in this dump", file=sys.stderr)
        return 1

    if args.against:
        path = [tuple(float(v) for v in line.split())
                for line in args.against.read_text().splitlines() if line.strip()]
        apart = replayed(rows, path)
        left = next((k for k, d in apart if d > 0.01 and k > 5), None)
        print(f"{args.against.name}: {len(path)} replayed frames against "
              f"{len(rows)} captured")
        print(f"  exact to a hundredth of a unit through frame "
              f"{'all of them' if left is None else left - 1}")
        for a, b in ((5, 200), (200, 400), (400, 600), (600, 900), (900, len(apart))):
            seg = sorted(d for k, d in apart if a <= k < b)
            if seg:
                print(f"  frames {a:4d}..{b:4d}: median {seg[len(seg)//2]:8.4f}  "
                      f"p90 {seg[int(len(seg)*0.9)]:8.4f}  max {seg[-1]:8.4f}")
        return 0

    if args.orbit:
        at = [float(v) for v in args.orbit.split(",")]
        got = orbit(rows, (at[0], at[1]), at[2], int(at[3]), int(at[4]))
        print(f"{got['frames']} frames of a turn on the spot:")
        print(f"  {got['back']:.4f} back along the camera's own look "
              f"(spread {got['back_spread']:.4f})")
        print(f"  pivot {got['pivot']:.4f} above the feet "
              f"(spread {got['pivot_spread']:.4f})")
        print(f"  {got['sideways']:.4f} to one side of it")
        return 0

    if args.demo:
        sys.path.insert(0, str(Path(__file__).resolve().parent))
        import omn
        recorded = omn.parse(args.demo.read_bytes())[1:]  # 0 is the load
        scale, rms, end = turn_scale(rows, recorded)
        print(f"{args.demo.name}: {len(recorded)} frames against "
              f"{len(rows)} captured")
        print(f"  turn is a flat {scale:.4f} rad per unit of recorded axis")
        print(f"  tracks the capture to {math.degrees(rms):.1f} deg RMS, "
              f"{math.degrees(end):+.1f} deg at the end")
        if args.expect_rms is not None and math.degrees(rms) > args.expect_rms:
            print(f"FAIL: wanted {args.expect_rms} deg RMS or better")
            return 1
        return 0

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
        if args.player:
            p = player(e, f)
            print(f"{n} {p[0]:.4f} {p[1]:.4f} {p[2]:.4f} "
                  f"{f[0]:.4f} {f[1]:.4f} {f[2]:.4f}")
        else:
            print(f"{n} {e[0]:.4f} {e[1]:.4f} {e[2]:.4f} "
                  f"{f[0]:.4f} {f[1]:.4f} {f[2]:.4f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
