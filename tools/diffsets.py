#!/usr/bin/env python3
"""
diffsets.py -- compare two installations of the game (e.g. GOG vs 1C).

The idea: files that are identical in the English and Russian editions hold
nothing but geometry and logic. Files that differ hold LOCALISABLE DATA --
strings, fonts, voice lines. That is a free map of where the string tables sit
inside the formats, obtained without touching a disassembler.

On top of that, for differing files of equal size it computes a map of the
differing byte ranges: if the differences are confined to one block, that block
is almost certainly a string section at a fixed offset.

Usage:
    python3 diffsets.py gog.json ru1c.json --report diff.md
    python3 diffsets.py gog.json ru1c.json --byteranges \
        --root-a "$WINEPREFIX_GOG/drive_c/games/MDK2" \
        --root-b "$WINEPREFIX_1C/drive_c/games/MDK2"
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def load(p: Path) -> tuple[str, dict[str, dict]]:
    data = json.loads(p.read_text())
    return data.get("root", "?"), {r["path"].lower(): r for r in data["files"]}


def diff_ranges(a: Path, b: Path, granularity: int = 4096) -> list[tuple[int, int]]:
    """Blocks in which the files differ. Only meaningful for equal sizes."""
    ranges: list[tuple[int, int]] = []
    with a.open("rb") as fa, b.open("rb") as fb:
        offset = 0
        start: int | None = None
        while True:
            ba = fa.read(granularity)
            bb = fb.read(granularity)
            if not ba and not bb:
                break
            if ba != bb:
                if start is None:
                    start = offset
            else:
                if start is not None:
                    ranges.append((start, offset))
                    start = None
            offset += max(len(ba), len(bb))
        if start is not None:
            ranges.append((start, offset))
    return ranges


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("inv_a", type=Path, help="inventory JSON of edition A")
    ap.add_argument("inv_b", type=Path, help="inventory JSON of edition B")
    ap.add_argument("--report", type=Path, help="write a markdown report")
    ap.add_argument("--byteranges", action="store_true",
                    help="compute the difference map (needs --root-a/--root-b)")
    ap.add_argument("--root-a", type=Path)
    ap.add_argument("--root-b", type=Path)
    ap.add_argument("--granularity", type=int, default=4096)
    args = ap.parse_args()

    root_a, a = load(args.inv_a)
    root_b, b = load(args.inv_b)

    keys_a, keys_b = set(a), set(b)
    common = keys_a & keys_b

    identical = sorted(k for k in common if a[k]["sha1"] == b[k]["sha1"])
    differing = sorted(k for k in common if a[k]["sha1"] != b[k]["sha1"])
    only_a = sorted(keys_a - keys_b)
    only_b = sorted(keys_b - keys_a)

    same_size = [k for k in differing if a[k]["size"] == b[k]["size"]]

    lines: list[str] = []
    w = lines.append
    w("# Edition diff\n")
    w(f"- A: `{root_a}` ({len(a)} files)")
    w(f"- B: `{root_b}` ({len(b)} files)\n")
    w(f"| category | n |")
    w(f"|---|---|")
    w(f"| identical | {len(identical)} |")
    w(f"| differing | {len(differing)} |")
    w(f"| of those, same size | {len(same_size)} |")
    w(f"| only in A | {len(only_a)} |")
    w(f"| only in B | {len(only_b)} |")

    w("\n## Differing, same size\n")
    w("The first candidates to look at: the localisable data sits at fixed "
      "offsets, so the format has a rigid layout.\n")
    for k in same_size[:200]:
        w(f"- `{a[k]['path']}` ({a[k]['size']} b, ent {a[k]['entropy']})")

    w("\n## Differing in size\n")
    for k in [x for x in differing if x not in set(same_size)][:200]:
        w(f"- `{a[k]['path']}` A={a[k]['size']} B={b[k]['size']}")

    w("\n## Only in A\n")
    for k in only_a[:200]:
        w(f"- `{a[k]['path']}`")
    w("\n## Only in B\n")
    for k in only_b[:200]:
        w(f"- `{b[k]['path']}`")

    if args.byteranges:
        if not (args.root_a and args.root_b):
            print("--byteranges requires --root-a and --root-b")
            return 1
        w("\n## Map of differing blocks\n")
        w(f"Granularity {args.granularity} b. A compact set of ranges means a "
          "string section at a fixed offset.\n")
        for k in same_size[:60]:
            pa = args.root_a / a[k]["path"]
            pb = args.root_b / b[k]["path"]
            if not (pa.is_file() and pb.is_file()):
                continue
            rs = diff_ranges(pa, pb, args.granularity)
            total = sum(e - s for s, e in rs)
            pct = 100.0 * total / max(1, a[k]["size"])
            head = ", ".join(f"0x{s:x}-0x{e:x}" for s, e in rs[:8])
            more = f" (+{len(rs) - 8})" if len(rs) > 8 else ""
            w(f"- `{a[k]['path']}` -- {len(rs)} block(s), {pct:.1f}% of file: {head}{more}")

    report = "\n".join(lines)
    if args.report:
        args.report.write_text(report)
        print(f"report -> {args.report}")
    else:
        print(report)

    print(f"\nidentical={len(identical)} differing={len(differing)} "
          f"(same size={len(same_size)}) only_A={len(only_a)} only_B={len(only_b)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
