#!/usr/bin/env python3
"""
diffsets.py -- сравнение двух установок игры (например GOG vs 1С).

Смысл: файлы, идентичные в английском и русском изданиях, содержат только
геометрию/логику. Отличающиеся файлы содержат ЛОКАЛИЗУЕМЫЕ ДАННЫЕ -- строки,
шрифты, озвучку. Это бесплатная карта того, где внутри форматов сидят строковые
таблицы, без всякого дизассемблера.

Дополнительно: для отличающихся файлов одинакового размера считает карту
различающихся байтовых диапазонов -- если различия локализованы в одном
блоке, это почти наверняка строковая секция с фиксированным офсетом.

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
    """Блоки, в которых файлы различаются. Только для файлов равного размера."""
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
    ap.add_argument("inv_a", type=Path, help="inventory JSON издания A")
    ap.add_argument("inv_b", type=Path, help="inventory JSON издания B")
    ap.add_argument("--report", type=Path, help="записать markdown-отчёт")
    ap.add_argument("--byteranges", action="store_true",
                    help="считать карту различий (нужны --root-a/--root-b)")
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
    w("# Дифф изданий\n")
    w(f"- A: `{root_a}` ({len(a)} файлов)")
    w(f"- B: `{root_b}` ({len(b)} файлов)\n")
    w(f"| категория | n |")
    w(f"|---|---|")
    w(f"| идентичны | {len(identical)} |")
    w(f"| отличаются | {len(differing)} |")
    w(f"| из них тот же размер | {len(same_size)} |")
    w(f"| только в A | {len(only_a)} |")
    w(f"| только в B | {len(only_b)} |")

    w("\n## Отличаются при одинаковом размере\n")
    w("Первоочередные кандидаты: локализуемые данные лежат по фиксированным "
      "офсетам, значит формат имеет жёсткую раскладку.\n")
    for k in same_size[:200]:
        w(f"- `{a[k]['path']}` ({a[k]['size']} b, ent {a[k]['entropy']})")

    w("\n## Отличаются размером\n")
    for k in [x for x in differing if x not in set(same_size)][:200]:
        w(f"- `{a[k]['path']}` A={a[k]['size']} B={b[k]['size']}")

    w("\n## Только в A\n")
    for k in only_a[:200]:
        w(f"- `{a[k]['path']}`")
    w("\n## Только в B\n")
    for k in only_b[:200]:
        w(f"- `{b[k]['path']}`")

    if args.byteranges:
        if not (args.root_a and args.root_b):
            print("--byteranges требует --root-a и --root-b")
            return 1
        w("\n## Карта различающихся блоков\n")
        w(f"Гранулярность {args.granularity} b. Компактный набор диапазонов = "
          "строковая секция с фиксированным офсетом.\n")
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
            w(f"- `{a[k]['path']}` -- {len(rs)} блок(ов), {pct:.1f}% файла: {head}{more}")

    report = "\n".join(lines)
    if args.report:
        args.report.write_text(report)
        print(f"отчёт -> {args.report}")
    else:
        print(report)

    print(f"\nидентичны={len(identical)} отличаются={len(differing)} "
          f"(тот же размер={len(same_size)}) только_A={len(only_a)} только_B={len(only_b)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
