#!/usr/bin/env python3
"""Pick the player out of peek.so's log, and say what he did.

tools/peek.c logs every candidate address the scan matched, many times a
second, with no idea which of them is the player.  This picks him out and
turns the log into a path that can be held against a replay of the same
demo -- the thing camtrace.py could only reach through the camera, and
only as a filtered, lagged copy.

The player is the candidate that *travels*.  A scan for the spawn position
matches the checkpoint table (static, never moves), a few stack frames
(garbage that jumps), and the live object (a smooth path).  Distinguishing
them is one number: the longest single step.  A real body at 15 units a
second sampled at 200 Hz moves under a tenth of a unit a step; a stack
slot reused for something else jumps by hundreds.

The log is sampled by wall clock, not by frame -- see the note in peek.c
about why there is no frame hook -- and under Wine's software EGL the game
runs well under its own 29.94 Hz, so wall-clock frames come out duplicated.
The path is therefore cut on *change*: one entry per time the engine moved
the player, which is one entry a tick.  A tick the player spent turning on
the spot leaves no entry, so this indexes moves, not frames -- enough for
distance and shape, not enough to line up frame n with frame n.  Lining
them up is what the frame hook in peek.c would buy.
"""
import argparse
import math
import sys
from pathlib import Path

FPS = 29.94          # the demo's rate, measured in camtrace.py
STILL = 1e-4         # a step smaller than this is the object not moving
JUMP = 5.0           # a step larger than this is not a body, it is reuse


def read(path):
    """Return {address: [(t, x, y, z), ...]} from one peek.so log."""
    tracks = {}
    for line in Path(path).read_text().splitlines():
        if line.startswith("#") or not line:
            continue
        f = line.split()
        if len(f) < 5:
            continue
        try:
            t, xyz = float(f[0]), tuple(float(v) for v in f[2:5])
        except ValueError:
            continue                      # nan and inf print as words
        if any(math.isnan(v) or math.isinf(v) for v in xyz):
            continue
        tracks.setdefault(f[1], []).append((t,) + xyz)
    return tracks


def steps(track):
    """Distances between consecutive samples, ignoring the still ones."""
    out = []
    for (_, x0, y0, z0), (_, x1, y1, z1) in zip(track, track[1:]):
        d = math.dist((x0, y0, z0), (x1, y1, z1))
        if d > STILL:
            out.append(d)
    return out


def travelled(track):
    return sum(steps(track))


def player(tracks):
    """The candidate that moves like a body: it travels, and never jumps.

    Returns (address, track) or (None, None).  Ties go to the longest path,
    because a body walking a demo out-travels anything that merely twitches.
    """
    best, best_dist = None, 0.0
    for addr, track in tracks.items():
        s = steps(track)
        if not s or max(s) > JUMP:
            continue
        d = sum(s)
        if d > best_dist:
            best, best_dist = addr, d
    return (best, tracks[best]) if best else (None, None)


def ticks(track):
    """One entry per position update: the engine's own step, not the clock.

    Kurt walking comes out at a median step of 0.4997, which is 15.0 units
    a second at the demo's 29.94 -- the speed table read straight off the
    running game.
    """
    out = []
    for s in track:
        if not out or s[1:] != out[-1]:
            out.append(s[1:])
    return out


def resample(track, fps=FPS, t0=None):
    """One position a frame, by holding the last sample before each frame.

    Held, not interpolated: the game's own position is a step function
    updated once a tick, and interpolating would invent motion between
    ticks that the engine never had.
    """
    if not track:
        return []
    start = track[0][0] if t0 is None else t0
    out, i = [], 0
    n = int((track[-1][0] - start) * fps)
    for k in range(n + 1):
        t = start + k / fps
        while i + 1 < len(track) and track[i + 1][0] <= t:
            i += 1
        out.append(track[i][1:])
    return out


def selftest():
    # A body, a static record and a reused stack slot, side by side.
    body = [(i / 200, 100 + i * 0.05, 5.0, 0.0) for i in range(400)]
    static = [(i / 200, 271.0, -69.0, -163.0) for i in range(400)]
    stack = [(i / 200, 12.0 + (i % 3) * 900.0, 0.0, 0.0) for i in range(400)]
    tracks = {"aaa": body, "bbb": static, "ccc": stack}
    addr, track = player(tracks)
    assert addr == "aaa", addr
    assert abs(travelled(track) - 399 * 0.05) < 1e-6, travelled(track)

    # A static record must never win, even when it is the only candidate.
    assert player({"bbb": static}) == (None, None)

    # Resampling holds, so a 200 Hz track of a body walking 0.05 a sample
    # comes back at 29.94 Hz having covered the same ground, give or take
    # one frame's worth at each end.
    r = resample(body, fps=29.94)
    assert len(r) == int(399 / 200 * 29.94) + 1, len(r)
    walked = sum(math.dist(a, b) for a, b in zip(r, r[1:]))
    assert abs(walked - 399 * 0.05) < 0.5, walked

    # Two samples at the same instant must not divide by zero.
    assert resample([(0.0, 1.0, 2.0, 3.0)]) == [(1.0, 2.0, 3.0)]

    # Cutting on change keeps every move and drops every repeat, however
    # many times the sampler caught the same position.
    held = [(0.0, 1.0, 0.0, 0.0), (0.1, 1.0, 0.0, 0.0), (0.2, 2.0, 0.0, 0.0),
            (0.3, 2.0, 0.0, 0.0), (0.4, 3.0, 0.0, 0.0)]
    assert ticks(held) == [(1.0, 0.0, 0.0), (2.0, 0.0, 0.0), (3.0, 0.0, 0.0)]
    assert ticks([]) == []
    print("peek: self-test passed")


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("log", nargs="?", type=Path, help="a peek.so log")
    ap.add_argument("--wall", type=float, metavar="FPS", nargs="?",
                    const=FPS, help="index by wall clock at this rate "
                    f"(default {FPS}) instead of by position update")
    ap.add_argument("--path", type=Path, metavar="FILE",
                    help="write the player's path, one 'x y z' a frame, in "
                         "the same form walksim.py --track writes")
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()
    if args.selftest:
        return selftest()
    if not args.log:
        ap.error("a log, or --selftest")

    tracks = read(args.log)
    addr, track = player(tracks)
    print(f"{len(tracks)} candidates in {args.log.name}")
    for a in sorted(tracks, key=lambda a: -travelled(tracks[a])):
        s = steps(tracks[a])
        print(f"  {a:>10}  {len(tracks[a]):6d} samples  travelled "
              f"{travelled(tracks[a]):9.2f}  biggest step "
              f"{(max(s) if s else 0):8.2f}"
              f"{'   <- the player' if a == addr else ''}")
    if not addr:
        sys.exit("no candidate moves like a body")

    frames = resample(track, args.wall) if args.wall else ticks(track)
    first, last = frames[0], frames[-1]
    print(f"\nthe player over {len(frames)} "
          f"{'frames at %g fps' % args.wall if args.wall else 'moves'}:")
    print(f"  from ({first[0]:.2f}, {first[1]:.2f}, {first[2]:.2f})"
          f" to ({last[0]:.2f}, {last[1]:.2f}, {last[2]:.2f})")
    print(f"  travelled {travelled(track):.1f}, "
          f"drifted {math.dist(first, last):.1f}")
    if args.path:
        args.path.write_text("".join(f"{p[0]:.4f} {p[1]:.4f} {p[2]:.4f}\n"
                                     for p in frames))
        print(f"  wrote {args.path}")


if __name__ == "__main__":
    main()
