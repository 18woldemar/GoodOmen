#!/usr/bin/env python3
"""
inventory.py -- catalogue the files of an installed game.

For each file it records: relative path, size, SHA-1, the first 16 bytes
(hex + ascii), and the Shannon entropy of the first 64 KB.

Entropy sorts the files into classes:
    < 4.0   text, tables, indices, sparse structures -> START HERE
    4.0-7.0 mixed binary: geometry, containers, palettes
    > 7.5   compressed or encrypted -- not analysable until unpacked

Usage:
    python3 inventory.py "/home/user/.wine/drive_c/Program Files/MDK2" -o gog.json
    python3 inventory.py <dir> -o out.json --summary
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import sys
from collections import Counter, defaultdict
from pathlib import Path

SAMPLE_BYTES = 64 * 1024
MAGIC_BYTES = 16


def shannon_entropy(data: bytes) -> float:
    """Entropy in bits per byte, 0.0 .. 8.0."""
    if not data:
        return 0.0
    counts = Counter(data)
    n = len(data)
    return -sum((c / n) * math.log2(c / n) for c in counts.values())


def printable(data: bytes) -> str:
    return "".join(chr(b) if 32 <= b < 127 else "." for b in data)


def scan_file(path: Path, root: Path, full_hash_limit: int) -> dict:
    size = path.stat().st_size
    sha1 = hashlib.sha1()
    head = b""
    sample = b""
    read = 0

    with path.open("rb") as fh:
        while True:
            chunk = fh.read(1 << 20)
            if not chunk:
                break
            if not head:
                head = chunk[:MAGIC_BYTES]
            if read < SAMPLE_BYTES:
                sample += chunk[: SAMPLE_BYTES - read]
            read += len(chunk)
            # Hash the whole file unless it is huge; otherwise just a prefix.
            if full_hash_limit == 0 or size <= full_hash_limit:
                sha1.update(chunk)
            elif read <= full_hash_limit:
                sha1.update(chunk)

    return {
        "path": str(path.relative_to(root)).replace(os.sep, "/"),
        "size": size,
        "sha1": sha1.hexdigest(),
        "sha1_partial": full_hash_limit != 0 and size > full_hash_limit,
        "magic_hex": head.hex(),
        "magic_ascii": printable(head),
        "entropy": round(shannon_entropy(sample), 3),
        "ext": path.suffix.lower(),
    }


def walk(root: Path, full_hash_limit: int) -> list[dict]:
    out = []
    for dirpath, _dirnames, filenames in os.walk(root):
        for name in sorted(filenames):
            p = Path(dirpath) / name
            if not p.is_file() or p.is_symlink():
                continue
            try:
                out.append(scan_file(p, root, full_hash_limit))
            except OSError as exc:
                print(f"[skip] {p}: {exc}", file=sys.stderr)
    out.sort(key=lambda r: r["path"].lower())
    return out


def entropy_class(e: float) -> str:
    if e < 4.0:
        return "low(text/index)"
    if e < 7.0:
        return "mid(structured)"
    if e < 7.5:
        return "high(dense)"
    return "max(compressed?)"


def print_summary(records: list[dict]) -> None:
    by_ext: dict[str, list[dict]] = defaultdict(list)
    for r in records:
        by_ext[r["ext"] or "<none>"].append(r)

    total = sum(r["size"] for r in records)
    print(f"\nfiles: {len(records)}   total: {total / 2**20:.1f} MiB\n")

    print(f"{'ext':<10} {'n':>5} {'MiB':>9}  {'ent':>5}  typical signature")
    print("-" * 78)
    for ext, rs in sorted(by_ext.items(), key=lambda kv: -sum(r["size"] for r in kv[1])):
        mib = sum(r["size"] for r in rs) / 2**20
        avg_e = sum(r["entropy"] for r in rs) / len(rs)
        magics = Counter(r["magic_hex"][:8] for r in rs)
        top, cnt = magics.most_common(1)[0]
        share = f"{cnt}/{len(rs)}"
        ascii_hint = printable(bytes.fromhex(top))
        print(f"{ext:<10} {len(rs):>5} {mib:>9.2f}  {avg_e:>5.2f}  {top} '{ascii_hint}' ({share})")

    print("\nbest candidates to decode first (low entropy, not text by extension):")
    cands = [r for r in records if r["entropy"] < 5.0 and r["size"] > 256]
    cands.sort(key=lambda r: r["entropy"])
    for r in cands[:25]:
        print(f"  {r['entropy']:>5.2f} {entropy_class(r['entropy']):<18} "
              f"{r['size']:>10}  {r['magic_ascii']}  {r['path']}")
    if not cands:
        print("  (none -- everything is either tiny or dense)")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("root", type=Path, help="root of the installed game")
    ap.add_argument("-o", "--out", type=Path, required=True, help="output JSON")
    ap.add_argument("--summary", action="store_true", help="print a summary")
    ap.add_argument("--full-hash-limit", type=int, default=0,
                    metavar="BYTES",
                    help="hash files up to N bytes in full (0 = all in full)")
    args = ap.parse_args()

    if not args.root.is_dir():
        print(f"not a directory: {args.root}", file=sys.stderr)
        return 1

    records = walk(args.root, args.full_hash_limit)
    payload = {"root": str(args.root), "count": len(records), "files": records}
    args.out.write_text(json.dumps(payload, indent=2, ensure_ascii=False))
    print(f"wrote {len(records)} records -> {args.out}")

    if args.summary:
        print_summary(records)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
