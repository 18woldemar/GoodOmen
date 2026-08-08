#!/usr/bin/env python3
"""
exe_recon.py -- reconnaissance of a PE binary.

It answers the first-session questions:
  1. Which graphics API? (DDRAW/D3DIM700 vs OPENGL32 vs GLIDE)
  2. Which compiler built it? (the Rich header beats any guessing)
  3. Is MSVC RTTI present? If so, the REAL engine class names fall out of the
     binary. With no SDK to work from, that is the single most valuable find.
  4. Did source paths survive in the asserts? (the shape of BioWare's tree)
  5. Which file extensions does the engine mention? (the list of formats
     still to be decoded)

Usage:
    python3 exe_recon.py MDK2.exe
    python3 exe_recon.py MDK2.exe --json recon.json --min-str 6
"""

from __future__ import annotations

import argparse
import json
import math
import re
import sys
from collections import Counter
from pathlib import Path

try:
    import pefile
except ImportError:
    sys.exit("pefile required:  pip install pefile  (or pacman -S python-pefile)")

# Imports that give away the graphics and sound stack at a glance.
API_HINTS = {
    "ddraw": "DirectDraw (2D surfaces / legacy)",
    "d3dim": "Direct3D Immediate Mode (retained D3D <= 7)",
    "d3drm": "Direct3D Retained Mode",
    "d3d8": "Direct3D 8",
    "d3d9": "Direct3D 9",
    "opengl32": "OpenGL",
    "glide": "3dfx Glide",
    "dsound": "DirectSound",
    "dinput": "DirectInput",
    "dplayx": "DirectPlay (networking)",
    "winmm": "MCI / MIDI / timers",
    "msvcrt": "MSVC runtime",
    "msvcp": "MSVC STL",
    "binkw32": "Bink Video",
    "smackw32": "Smacker Video",
    "mss32": "Miles Sound System",
}

# Landmarks for the build ids in the Rich header. The list is incomplete --
# check external tables for detail -- but the magnitude pins the compiler era.
BUILD_HINTS = {
    8168: "MSVC 6.0 (12.00.8168, SP5)",
    8804: "MSVC 6.0 (12.00.8804, SP6)",
    8078: "MSVC 6.0 (early SP)",
    9782: "MSVC 7.0 / .NET 2002",
    3077: "MSVC 5.0",
}

RE_RTTI = re.compile(rb"\.\?A[VUW][\w@?$]{1,200}@@")
RE_SRCPATH = re.compile(rb"[A-Za-z]:\\\\?[\w\\ .\-+]{3,140}\.(?:cpp|c|h|hpp|inl|asm)",
                        re.IGNORECASE)
RE_EXTREF = re.compile(rb"(?<![\w.])[\w\-*]{1,40}\.([a-z0-9]{2,4})(?![\w.])")


def entropy(data: bytes) -> float:
    if not data:
        return 0.0
    n = len(data)
    return -sum((c / n) * math.log2(c / n) for c in Counter(data).values())


def ascii_strings(data: bytes, minlen: int) -> list[bytes]:
    return re.findall(rb"[\x20-\x7e]{%d,}" % minlen, data)


def demangle_rtti(sym: bytes) -> str:
    """'.?AVCFoo@Bar@@' -> 'Bar::CFoo' (crude, but readable)."""
    s = sym.decode("ascii", "replace")
    kind = {"V": "class", "U": "struct", "W": "enum"}.get(s[3], "?")
    body = s[4:]
    body = body[:-2] if body.endswith("@@") else body
    parts = [p for p in body.split("@") if p]
    return f"{kind} " + "::".join(reversed(parts))


def report_rich(pe) -> dict:
    out: dict = {"present": False, "entries": []}
    try:
        rich = pe.parse_rich_header()
    except Exception:
        rich = None
    if not rich:
        return out
    out["present"] = True
    values = rich.get("values", [])
    # values is a flat list: [comp_id, count, comp_id, count, ...]
    for i in range(0, len(values) - 1, 2):
        comp_id, count = values[i], values[i + 1]
        prod_id, build = comp_id >> 16, comp_id & 0xFFFF
        out["entries"].append({
            "prod_id": prod_id,
            "build": build,
            "count": count,
            "hint": BUILD_HINTS.get(build, ""),
        })
    return out


def source_map(args) -> int:
    """Where each `.c` lives, from the asserts that name it.

    Every assert in this binary pushes the address of its own source path
    before calling the handler, so the *push sites* of a path bracket the
    file that contains them. Finding them needs no disassembler: `push imm32`
    is one byte, 0x68, followed by the address, and a false positive would
    have to be four bytes of data that happen to spell a path address inside
    executable code.

    The result is a map of the engine. It is the cheapest structural thing in
    the binary and it was sitting there for months: `mdkBullet.c`,
    `mdkWalker.c`, `mdkKurt.c`, `mdkInventory.c`, `omCollision.c` and the rest,
    each with the address range to read when that subsystem is the question.
    """
    pe = pefile.PE(str(args.exe), fast_load=True)
    base = pe.OPTIONAL_HEADER.ImageBase
    paths = {}
    for s in pe.sections:
        va, data = s.VirtualAddress + base, s.get_data()
        for m in re.finditer(rb"[A-Za-z]:\\[^\x00]{3,60}\.(c|cpp)\x00", data):
            paths[va + m.start()] = m.group(0)[:-1].decode("latin1")

    text = next(s for s in pe.sections if s.Name.startswith(b".text"))
    code, code_va = text.get_data(), text.VirtualAddress + base
    sites: dict[str, list[int]] = {}
    for i in range(len(code) - 5):
        if code[i] != 0x68:
            continue
        v = int.from_bytes(code[i + 1:i + 5], "little")
        if v in paths:
            sites.setdefault(paths[v], []).append(code_va + i)

    rows = sorted(sites.items(), key=lambda kv: min(kv[1]))
    for name, at in rows:
        print(f"0x{min(at):08x}..0x{max(at):08x}  {len(at):3d}  "
              f"{name.rsplit(chr(92), 1)[-1]}")
    print(f"{len(paths)} source paths in the binary, {len(rows)} of them "
          f"asserted from code", file=sys.stderr)
    if args.expect_files is not None:
        return 0 if len(rows) == args.expect_files else 1
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("exe", type=Path)
    ap.add_argument("--json", type=Path, help="save the full result")
    ap.add_argument("--min-str", type=int, default=5)
    ap.add_argument("--max-show", type=int, default=40)
    ap.add_argument("--files", action="store_true",
                    help="map each surviving source path to the address range "
                         "of the code that asserts with it")
    ap.add_argument("--expect-files", type=int, metavar="N",
                    help="succeed only if exactly N paths are mapped")
    args = ap.parse_args()

    if args.files:
        return source_map(args)

    raw = args.exe.read_bytes()
    pe = pefile.PE(data=raw, fast_load=False)
    result: dict = {"file": str(args.exe), "size": len(raw)}

    oh, fh = pe.OPTIONAL_HEADER, pe.FILE_HEADER
    print(f"=== {args.exe.name}  ({len(raw) / 2**20:.2f} MiB) ===\n")
    print(f"machine          0x{fh.Machine:04x} "
          f"({'x86-32' if fh.Machine == 0x14c else 'other'})")
    print(f"linker           {oh.MajorLinkerVersion}.{oh.MinorLinkerVersion:02d}"
          f"   {'-> MSVC 6 era' if oh.MajorLinkerVersion == 6 else ''}")
    print(f"image base       0x{oh.ImageBase:08x}")
    print(f"entry point      0x{oh.AddressOfEntryPoint:08x}")
    print(f"timestamp        0x{fh.TimeDateStamp:08x}")
    result["header"] = {
        "machine": fh.Machine,
        "linker": f"{oh.MajorLinkerVersion}.{oh.MinorLinkerVersion}",
        "image_base": oh.ImageBase,
        "entry_point": oh.AddressOfEntryPoint,
        "timestamp": fh.TimeDateStamp,
    }

    # --- sections -----------------------------------------------------
    print("\n--- sections ---")
    print(f"{'name':<10} {'vaddr':>10} {'vsize':>10} {'rawsize':>10}  ent")
    sections = []
    for s in pe.sections:
        name = s.Name.rstrip(b"\x00").decode("ascii", "replace")
        data = s.get_data()
        e = entropy(data[: 64 * 1024])
        flag = "  <-- PACKED?" if e > 7.5 and s.SizeOfRawData > 4096 else ""
        print(f"{name:<10} 0x{s.VirtualAddress:08x} {s.Misc_VirtualSize:>10} "
              f"{s.SizeOfRawData:>10}  {e:.2f}{flag}")
        sections.append({"name": name, "vaddr": s.VirtualAddress,
                         "vsize": s.Misc_VirtualSize,
                         "rawsize": s.SizeOfRawData, "entropy": round(e, 3)})
    result["sections"] = sections

    # --- Rich header --------------------------------------------------
    rich = report_rich(pe)
    result["rich"] = rich
    print("\n--- Rich header (toolchain) ---")
    if rich["present"]:
        for e in sorted(rich["entries"], key=lambda x: -x["count"]):
            hint = f"  {e['hint']}" if e["hint"] else ""
            print(f"  prodID {e['prod_id']:>4}  build {e['build']:>6}  "
                  f"objects {e['count']:>5}{hint}")
        print("\n  This is an exact compiler signature. Critical for decompilation,")
        print("  and useful context for a reimplementation (ABI, alignment).")
    else:
        print("  absent (not MSVC, or stripped by a packer)")

    # --- imports ------------------------------------------------------
    print("\n--- imports ---")
    imports = {}
    if hasattr(pe, "DIRECTORY_ENTRY_IMPORT"):
        for entry in pe.DIRECTORY_ENTRY_IMPORT:
            dll = entry.dll.decode("ascii", "replace")
            names = [imp.name.decode("ascii", "replace") if imp.name
                     else f"#{imp.ordinal}" for imp in entry.imports]
            imports[dll] = names
            low = dll.lower()
            hint = next((v for k, v in API_HINTS.items() if k in low), "")
            mark = f"   <== {hint}" if hint else ""
            print(f"  {dll:<20} {len(names):>4} sym.{mark}")
    result["imports"] = imports

    # --- strings: RTTI, paths, extensions ------------------------------
    rtti = sorted({demangle_rtti(m) for m in RE_RTTI.findall(raw)})
    result["rtti"] = rtti
    print(f"\n--- MSVC RTTI: {len(rtti)} type names found ---")
    if rtti:
        print("  THIS IS THE JACKPOT: the real engine class names.")
        for name in rtti[: args.max_show]:
            print(f"    {name}")
        if len(rtti) > args.max_show:
            print(f"    ... {len(rtti) - args.max_show} more; full list via --json")
    else:
        print("  none (RTTI disabled at build time) -- recover names from behaviour")

    srcpaths = sorted({m.decode("ascii", "replace") for m in RE_SRCPATH.findall(raw)})
    result["source_paths"] = srcpaths
    print(f"\n--- source paths: {len(srcpaths)} ---")
    for p in srcpaths[: args.max_show]:
        print(f"    {p}")
    if len(srcpaths) > args.max_show:
        print(f"    ... {len(srcpaths) - args.max_show} more")

    strings = ascii_strings(raw, args.min_str)
    ext_counter = Counter()
    for s in strings:
        for m in RE_EXTREF.finditer(s):
            ext_counter[m.group(1).decode()] += 1
    result["referenced_extensions"] = ext_counter.most_common(60)
    print("\n--- extensions mentioned in strings (candidate formats) ---")
    noise = {"dll", "exe", "com", "sys", "ini", "txt", "tmp", "log",
             "text", "data", "rdata", "rsrc", "reloc", "bss", "idata"}
    for ext, cnt in ext_counter.most_common(40):
        tag = "" if ext in noise else "   <-- decode this"
        print(f"    .{ext:<6} {cnt:>5}{tag}")

    if args.json:
        result["strings_sample"] = [s.decode("ascii", "replace")
                                    for s in strings[:5000]]
        args.json.write_text(json.dumps(result, indent=2, ensure_ascii=False))
        print(f"\nfull result -> {args.json}")

    print("\nnext step: check the extension list above against inventory.py --summary;")
    print("extensions present in the strings but not on disk live inside containers.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
