#!/usr/bin/env python3
r"""
luaconst.py -- the engine's Lua constants, with their values, out of the binary.

`docs/lua-api.md` catalogues the 248 ALL_CAPS constants the *scripts* use, but
not what they are worth, and the values are nowhere in the scripts:
`mdkRegisterObject(name, OBJ_ROOM, ...)` and
`omAnimPlay(gob, ANIM_X, ANIMFLAG_NOTRANS + ANIMFLAG_INTERRUPT)` pass them
straight back to the engine. `tools/luarun.py` works around that by letting a
constant stand for its own name, which is enough to *run* a script and not
enough to be one.

They are all in `mdk2Main.exe`, registered one call at a time:

    push 0x40891800            ; the high dword of a double
    push 0                     ; and its low dword
    push str.OBJ_ROOM
    push 0
    call 0x4448e0              ; register_constant(0, name, double)

cdecl, so the call sees `(0, name, value)` and the two dwords land in the
right order for a little-endian double. That is the whole pattern, and
scanning `.text` for it finds **507 constants** -- twice what the scripts use,
because the binary defines the whole surface and the scripts touch part of it.

Nothing here is a guess that has to be checked later: a name is only accepted
when the pushed pointer really addresses a NUL-terminated identifier inside
the image, and the value is read as an IEEE 754 double, which is what Lua
numbers are.

Usage:
    python3 tools/luaconst.py --markdown > docs/lua-constants.md
    python3 tools/luaconst.py --rust > engine/src/game/constants.rs
    python3 tools/luaconst.py --coverage extracted/scripts extracted/base
"""

from __future__ import annotations

import argparse
import os
import re
import struct
import sys
from pathlib import Path

NAME = re.compile(rb"[A-Za-z_][A-Za-z0-9_]*")
PUSH_IMM8 = 0x6A
PUSH_IMM32 = 0x68
CALL_REL32 = 0xE8


class Image:
    """Just enough PE to turn a virtual address into a file offset."""

    def __init__(self, data: bytes) -> None:
        self.data = data
        pe = struct.unpack_from("<I", data, 0x3C)[0]
        count = struct.unpack_from("<H", data, pe + 6)[0]
        opt = struct.unpack_from("<H", data, pe + 20)[0]
        self.base = struct.unpack_from("<I", data, pe + 24 + 28)[0]
        self.sections = []
        for i in range(count):
            o = pe + 24 + opt + i * 40
            name = data[o:o + 8].rstrip(b"\0").decode("latin1")
            vsize, va, rsize, raw = struct.unpack_from("<4I", data, o + 8)
            self.sections.append((name, va, vsize, raw, rsize))

    def offset(self, va: int) -> int | None:
        for _n, sva, vsize, raw, rsize in self.sections:
            if self.base + sva <= va < self.base + sva + min(vsize, rsize):
                return raw + (va - self.base - sva)
        return None

    def section(self, name: str):
        for s in self.sections:
            if s[0] == name:
                return s
        raise KeyError(name)

    def identifier(self, va: int) -> str | None:
        """The C string at `va`, if it is one and is a plain identifier."""
        off = self.offset(va)
        if off is None:
            return None
        end = self.data.find(b"\0", off, off + 64)
        if end < 0:
            return None
        s = self.data[off:end]
        return s.decode("latin1") if s and NAME.fullmatch(s) else None


def _push(data: bytes, o: int) -> tuple[int, int] | None:
    """A `push imm8` or `push imm32` at `o`. -> (value, offset after it)."""
    if data[o] == PUSH_IMM8:
        return struct.unpack_from("<b", data, o + 1)[0] & 0xFFFFFFFF, o + 2
    if data[o] == PUSH_IMM32:
        return struct.unpack_from("<I", data, o + 1)[0], o + 5
    return None


def constants(image: Image) -> dict[str, float]:
    """Every (name, value) the binary registers. Keyed by name, in the order
    the code registers them."""
    data = image.data
    _n, sva, _vs, raw, rsize = image.section(".text")
    out: dict[int, dict[str, float]] = {}
    o, end = raw, raw + rsize
    while o < end - 24:
        if data[o] not in (PUSH_IMM8, PUSH_IMM32):
            o += 1
            continue
        p, args = o, []
        for _ in range(4):
            got = _push(data, p)
            if not got:
                break
            args.append(got[0])
            p = got[1]
        # (high, low, name, 0) pushed right to left, then the call
        if len(args) == 4 and args[3] == 0 and data[p] == CALL_REL32:
            name = image.identifier(args[2])
            if name:
                value = struct.unpack("<d", struct.pack("<II", args[1], args[0]))[0]
                target = image.base + sva + (p + 5 - raw) + \
                    struct.unpack_from("<i", data, p + 1)[0]
                out.setdefault(target, {})[name] = value
        o += 1
    if not out:
        raise ValueError("no constant registrations found")
    # one call site does all of them; anything else is a coincidence of bytes
    site = max(out, key=lambda k: len(out[k]))
    return out[site]


def functions(image: Image) -> dict[str, int]:
    """Every Lua function the binary registers, and where its body is.

    The same shape as the constants, one push shorter:

        push 0x43ac80                   ; the C function
        push str.mdkRegisterObject
        push 0
        call 0x444890                   ; register_function(0, name, fn)

    cdecl again, so the callee sees `(0, name, function)`. A pointer is only
    accepted when it addresses `.text`, which is what keeps this from
    matching a constant registration or a coincidence of bytes.
    """
    data = image.data
    _n, sva, _vs, raw, rsize = image.section(".text")
    lo, hi = image.base + sva, image.base + sva + rsize
    out: dict[int, dict[str, int]] = {}
    o, end = raw, raw + rsize
    while o < end - 20:
        if data[o] != PUSH_IMM32:
            o += 1
            continue
        p, args = o, []
        for _ in range(3):
            got = _push(data, p)
            if not got:
                break
            args.append(got[0])
            p = got[1]
        if len(args) == 3 and args[2] == 0 and data[p] == CALL_REL32 \
                and lo <= args[0] < hi:
            name = image.identifier(args[1])
            if name:
                target = image.base + sva + (p + 5 - raw) + \
                    struct.unpack_from("<i", data, p + 1)[0]
                out.setdefault(target, {})[name] = args[0]
        o += 1
    if not out:
        raise ValueError("no function registrations found")
    site = max(out, key=lambda k: len(out[k]))
    return out[site]


def _pretty(value: float) -> str:
    return str(int(value)) if value == int(value) else repr(value)


def markdown(table: dict[str, float]) -> str:
    lines = ["# The engine's Lua constants",
             "",
             "Extracted from `mdk2Main.exe` by `tools/luaconst.py`, which "
             "reads the",
             "registration calls themselves rather than inferring anything: "
             "each is",
             "`push high; push low; push name; push 0; call` and the value is "
             "the",
             "double those two dwords make.",
             "",
             f"{len(table)} constants.",
             "",
             "| Name | Value |",
             "|---|---|"]
    lines += [f"| `{n}` | {_pretty(v)} |" for n, v in sorted(table.items())]
    return "\n".join(lines) + "\n"


def rust_functions(table: dict[str, int]) -> str:
    body = "\n".join(f'    ("{n}", {v:#x}),' for n, v in sorted(table.items()))
    return f'''//! Every Lua function the original registers, and the address of its body.
//!
//! **Generated** by `tools/luaconst.py --functions` from `mdk2Main.exe` -- do
//! not edit by hand. Each is registered as `push fn; push name; push 0;
//! call`, so `(0, name, function)`, and the pointer is only accepted when it
//! addresses `.text`.
//!
//! The addresses are here because they are the evidence and because they are
//! where to look when a function has to be understood. **Nothing is
//! disassembled into this engine**; the list is a surface and a work list.
//!
//! {len(table)} of them.

/// `(name, the address of the original's body)`, sorted by name.
pub const FUNCTIONS: [(&str, u32); {len(table)}] = [
{body}
];

#[cfg(test)]
mod tests {{
    use super::*;

    /// Two read straight off the disassembly at 0x436154.
    #[test]
    fn the_addresses_are_the_ones_the_binary_registers() {{
        let find = |want: &str| FUNCTIONS.iter().find(|(n, _)| *n == want).map(|(_, a)| *a);
        assert_eq!(find("mdkRegisterObject"), Some(0x43ac80));
        assert_eq!(find("mdkCreateObjectLua"), Some(0x43b040));
        assert!(FUNCTIONS.windows(2).all(|w| w[0].0 < w[1].0), "sorted, unique");
    }}
}}
'''


def rust(table: dict[str, float]) -> str:
    body = "\n".join(f'    ("{n}", {v!r}),' for n, v in sorted(table.items()))
    return f'''//! The engine's Lua constants and their values.
//!
//! **Generated** by `tools/luaconst.py` from `mdk2Main.exe` -- do not edit by
//! hand, and regenerate rather than patch. Each one is registered by the
//! original as `push high; push low; push name; push 0; call`, so these are
//! read out of the instruction stream rather than inferred, and a Lua number
//! is a double, which is what they are stored as.
//!
//! {len(table)} of them, against the 248 the shipped scripts actually use:
//! the binary defines the whole surface and the scripts touch part of it.

/// Every constant the original defines for Lua, sorted by name.
pub const CONSTANTS: [(&str, f64); {len(table)}] = [
{body}
];

/// Install them all as globals.
pub fn install(lua: &mlua::Lua) -> mlua::Result<()> {{
    let globals = lua.globals();
    for (name, value) in CONSTANTS {{
        globals.set(name, value)?;
    }}
    Ok(())
}}

#[cfg(test)]
mod tests {{
    use super::*;

    /// Three read straight off the disassembly listing in
    /// `../../../tools/luaconst.py`. If the generator ever drifts, these are
    /// what says so.
    #[test]
    fn the_values_are_the_ones_the_binary_registers() {{
        let find = |want: &str| CONSTANTS.iter().find(|(n, _)| *n == want).map(|(_, v)| *v);
        assert_eq!(find("OBJ_ROOM"), Some(803.0));
        assert_eq!(find("OBJ_AMBIENTSOUND"), Some(1101.0));
        assert_eq!(find("CUSTKEY_SHOOT"), Some(3.0));
        assert!(CONSTANTS.windows(2).all(|w| w[0].0 < w[1].0), "sorted, unique");
    }}
}}
'''


def coverage(table: dict[str, float], roots: list[Path],
             expect_undefined: int | None) -> int:
    """Every constant the scripts use, and whether the binary defines it.

    `tools/luaapi.py` already knows which names are the engine's constants
    rather than the scripts' own globals, so this asks it rather than
    guessing with a second regular expression.
    """
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import luaapi
    files = [f for root in roots for f in sorted(root.rglob("*.lua"))]
    _calls, _defined, used = luaapi.scan(files)
    undefined = sorted(n for n in used if n not in table)
    print(f"{len(used)} constants used across {len(files)} scripts, "
          f"{len(used) - len(undefined)} defined by the binary",
          file=sys.stderr)
    if undefined:
        print(f"undefined: {', '.join(undefined)}", file=sys.stderr)
    if expect_undefined is not None and len(undefined) != expect_undefined:
        print(f"{len(undefined)} undefined, expected {expect_undefined}",
              file=sys.stderr)
        return 1
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("--exe", type=Path,
                    default=Path(os.environ.get("MDK2_GOG", ".")) / "mdk2Main.exe")
    ap.add_argument("--markdown", action="store_true")
    ap.add_argument("--rust", action="store_true")
    ap.add_argument("--functions", action="store_true",
                    help="the registered functions and their addresses "
                         "instead of the constants")
    ap.add_argument("--coverage", type=Path, nargs="+", metavar="DIR")
    ap.add_argument("--expect", type=int, help="fail unless this many are found")
    ap.add_argument("--expect-undefined", type=int, metavar="N",
                    help="with --coverage: fail unless exactly N of the "
                         "constants the scripts name are missing from the "
                         "binary")
    args = ap.parse_args(argv)

    if not args.exe.is_file():
        print(f"{args.exe} is not there -- set MDK2_GOG or pass --exe",
              file=sys.stderr)
        return 1
    image = Image(args.exe.read_bytes())
    if args.functions:
        found = functions(image)
        if args.expect is not None and len(found) != args.expect:
            print(f"{len(found)} functions, expected {args.expect}",
                  file=sys.stderr)
            return 1
        if args.rust:
            sys.stdout.write(rust_functions(found))
        else:
            for n, a in sorted(found.items()):
                print(f"{n} {a:#x}")
        print(f"{len(found)} functions", file=sys.stderr)
        return 0
    table = constants(image)

    if args.expect is not None and len(table) != args.expect:
        print(f"{len(table)} constants, expected {args.expect}", file=sys.stderr)
        return 1
    if args.markdown:
        sys.stdout.write(markdown(table))
    elif args.rust:
        sys.stdout.write(rust(table))
    elif args.coverage:
        return coverage(table, args.coverage, args.expect_undefined)
    else:
        for n, v in sorted(table.items()):
            print(f"{n} {_pretty(v)}")
    print(f"{len(table)} constants", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
