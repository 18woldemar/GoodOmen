#!/usr/bin/env python3
"""
luaapi.py -- catalogue the engine functions the shipped Lua calls.

The game's logic is 53688 lines of plain-text Lua in `scripts/`, and the
engine is whatever those scripts call and do not define themselves. That
difference is the specification: every name in it is a C function the
reimplementation owes, and the call sites say what it takes and roughly what
it does.

The scripts are **Lua 3.x, not 5.x**. `scripts/mdk2.lua` opens with

    $if debug
    $debug
    $end

which are Lua 3's conditional-compilation pragmas; they were gone by 4.0.
That fixes the standard library too -- `strfind` and `tinsert` rather than
`string.find` and `table.insert` -- and this tool subtracts the 3.2 library
by name, so what is left over really is BioWare's own.

The prefixes are the source layers the debug paths already showed:

    ch*   Chitin, the platform: chSndSwitchMusic, chGetDeltaT, chFogColor
    om*   Omen, the renderer and scene: omAnimPlay, omGobEnterStasis
    mdk*  the game itself: mdkGetPlayerGob, mdkCreateObjectLua
    lua*  the script-side helpers the engine installs
    COM_* and OBJ_* are constants rather than functions and are counted apart.

Arity is what the call sites show, so a range means the function is variadic
or has defaults. `chSetVideoMode` reads 9-12, which agrees with the signature
`override/options.lua` spells out in a comment -- an independent check that
the counting is right.

The result is **438 functions**: 288 Mdk2, 70 Chitin, 55 Omen, and 25 that
carry no prefix. Those 25 are the residue and are named as such: some are
genuinely the engine's (`vvSetInt` and the rest of the variable viewer), and
some are script helpers whose definition is not in the shipped files at all
(`QuatYDir` and `VectorAdd`, called only from `debug.lua`). Callback
parameters are already excluded -- a name that appears in any function's
parameter list is not counted, which is what `f(...)` inside `level8.lua` is.

Usage:
    python3 tools/luaapi.py extracted/scripts
    python3 tools/luaapi.py extracted/scripts --markdown > docs/lua-api.md
"""

from __future__ import annotations

import argparse
import re
import sys
from collections import defaultdict
from pathlib import Path

CALL = re.compile(r"\b([A-Za-z_]\w*)\s*\(")
# `function f(`, `f = function(`, `a.b.c = function(`, and `[key] = function`
DEF = re.compile(r"function\s+([\w.:]+)\s*\(|([\w.]+)\s*=\s*function\s*\(")
# a parameter can hold a callback and then be called by name; those are not
# engine functions, and `local x` names are the same case
PARAM = re.compile(r"function[\w.: ]*\(([^)]*)\)|\blocal\s+([\w, ]+)")
CONST = re.compile(r"\b((?:COM|OBJ|DAM|SND|MSG|EVT)_[A-Z0-9_]+)\b")

# Lua 3.2's standard library, which is flat -- no `string.` or `table.`
STDLIB = {
    "assert", "call", "collectgarbage", "copytagmethods", "dofile", "dostring",
    "error", "foreach", "foreachi", "foreachvar", "getglobal", "gettagmethod",
    "newtag", "next", "nextvar", "print", "rawcall", "rawget", "rawgetglobal",
    "rawset", "rawsetglobal", "setglobal", "settag", "settagmethod", "tag",
    "tonumber", "tostring", "type",
    "abs", "acos", "asin", "atan", "atan2", "ceil", "cos", "deg", "exp",
    "floor", "log", "log10", "max", "min", "mod", "rad", "random",
    "randomseed", "sin", "sqrt", "tan",
    "ascii", "date", "format", "gsub", "strfind", "strlen", "strlower",
    "strrep", "strsub", "strupper",
    "appendto", "clock", "getn", "read", "readfrom", "remove", "rename",
    "seek", "sort", "tinsert", "tmpname", "tremove", "write", "writeto",
    "exit", "getenv", "execute",
    # Lua keywords that the call regex would otherwise pick up
    "if", "elseif", "while", "return", "and", "or", "not", "for", "until",
    "function", "then", "do", "end", "in", "repeat", "local", "else",
}


def _args(text: str, open_paren: int) -> int | None:
    """Count the arguments of the call whose '(' is at open_paren."""
    depth, quote, n, empty = 0, "", 1, True
    for i in range(open_paren, len(text)):
        ch = text[i]
        if quote:
            quote = "" if ch == quote else quote
            continue
        if ch in "'\"":
            quote = ch
        elif ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
            if depth == 0:
                return 0 if empty else n
        elif ch == "," and depth == 1:
            n += 1
        elif not ch.isspace() and depth == 1:
            empty = False
        if i - open_paren > 4000:      # a call this long is a parse failure
            return None
    return None


def scan(paths: list[Path]) -> tuple[dict, set, dict]:
    calls: dict[str, dict] = defaultdict(
        lambda: {"count": 0, "arity": set(), "files": set()})
    defined: set[str] = set()
    consts: dict[str, int] = defaultdict(int)
    for p in paths:
        text = p.read_text(errors="replace")
        for m in DEF.finditer(text):
            name = m.group(1) or m.group(2)
            defined.add(name.split(".")[-1].split(":")[-1])
        for m in PARAM.finditer(text):
            for word in (m.group(1) or m.group(2) or "").split(","):
                word = word.strip()
                if word.isidentifier():
                    defined.add(word)
        for m in CONST.finditer(text):
            consts[m.group(1)] += 1
        for m in CALL.finditer(text):
            name = m.group(1)
            entry = calls[name]
            entry["count"] += 1
            entry["files"].add(p.name)
            n = _args(text, m.end() - 1)
            if n is not None:
                entry["arity"].add(n)
    return calls, defined, consts


def engine(calls: dict, defined: set) -> dict:
    """The calls that nothing in the scripts defines and the library lacks."""
    return {k: v for k, v in calls.items()
            if k not in defined and k not in STDLIB}


def layer(name: str) -> str:
    for prefix, title in (("ch", "Chitin"), ("om", "Omen"),
                          ("mdk", "Mdk2"), ("lua", "script helpers")):
        if name.startswith(prefix):
            return title
    return "unclassified"


def markdown(api: dict, consts: dict, nfiles: int, nlines: int) -> str:
    out = [
        "# The engine's Lua API",
        "",
        "Generated by `tools/luaapi.py`; do not edit by hand.",
        "",
        f"{nfiles} script files, {nlines} lines. Every function here is called "
        "by the shipped scripts and defined nowhere in them, so the engine "
        "provides it. Arity is what the call sites show; a range means "
        "optional arguments.",
        "",
    ]
    by_layer: dict[str, list] = defaultdict(list)
    for name, v in api.items():
        by_layer[layer(name)].append((name, v))
    for title in ("Chitin", "Omen", "Mdk2", "script helpers", "unclassified"):
        rows = sorted(by_layer.get(title, []), key=lambda kv: -kv[1]["count"])
        if not rows:
            continue
        out += [f"## {title} — {len(rows)} functions", "",
                "| function | calls | arity | files |", "|---|---|---|---|"]
        for name, v in rows:
            a = sorted(v["arity"])
            span = "?" if not a else (str(a[0]) if len(a) == 1
                                      else f"{a[0]}–{a[-1]}")
            out.append(f"| `{name}` | {v['count']} | {span} | {len(v['files'])} |")
        out.append("")
    out += [f"## Constants — {len(consts)}", "",
            "Defined by the engine, never assigned in Lua.", "",
            "| constant | uses |", "|---|---|"]
    for name, n in sorted(consts.items(), key=lambda kv: -kv[1])[:40]:
        out.append(f"| `{name}` | {n} |")
    if len(consts) > 40:
        out.append(f"\n...and {len(consts) - 40} more.")
    return "\n".join(out) + "\n"


SAMPLE = """
function Helper(gob, cb)
    cb(gob)
    omAnimPlay(gob, "walk", 1)
    mdkGobSetPosition(gob, {x=1, y=2})
    chFogEnable()
    if strfind(gob, "x") then omGobSetTimer(gob, 1, 2) end
end
Other = function() omGobSetTimer(1, 2) end
"""


def selftest() -> None:
    """Argument counting, and the three things that must not be counted."""
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        p = Path(d) / "s.lua"
        p.write_text(SAMPLE)
        calls, defined, _consts = scan([p])
    api = engine(calls, defined)
    assert api["omAnimPlay"]["arity"] == {3}, api["omAnimPlay"]
    # a table argument is one argument, not two
    assert api["mdkGobSetPosition"]["arity"] == {2}, api["mdkGobSetPosition"]
    assert api["chFogEnable"]["arity"] == {0}, api["chFogEnable"]
    assert api["omGobSetTimer"]["arity"] == {2, 3}, api["omGobSetTimer"]
    assert "cb" not in api, "a callback parameter is not the engine"
    assert "Helper" not in api and "Other" not in api, "defined in Lua"
    assert "strfind" not in api and "if" not in api, "library and keywords"
    print("luaapi.py: self-test passed")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path, nargs="?",
                    help="a directory of .lua scripts")
    ap.add_argument("--selftest", action="store_true",
                    help="check the scanner against a built-in sample")
    ap.add_argument("--markdown", action="store_true",
                    help="write the catalogue as a document")
    args = ap.parse_args(argv)
    if args.selftest:
        selftest()
        return 0
    if args.src is None:
        ap.error("a directory of .lua scripts is required")

    paths = sorted(args.src.rglob("*.lua"))
    if not paths:
        ap.error(f"no .lua under {args.src}")
    calls, defined, consts = scan(paths)
    api = engine(calls, defined)
    lines = sum(len(p.read_text(errors="replace").splitlines()) for p in paths)

    if args.markdown:
        sys.stdout.write(markdown(api, consts, len(paths), lines))
        print(f"{len(api)} engine functions, {len(defined)} defined in Lua, "
              f"{len(consts)} constants", file=sys.stderr)
        return 0

    for name, v in sorted(api.items(), key=lambda kv: -kv[1]["count"])[:40]:
        a = sorted(v["arity"])
        print(f"{v['count']:5d}  {name:28s} {layer(name):15s} "
              f"arity {a[0] if a else '?'}"
              f"{'' if len(a) < 2 else '-' + str(a[-1])}")
    print(f"{len(api)} engine functions, {len(defined)} defined in Lua, "
          f"{len(consts)} constants, over {len(paths)} files and {lines} lines",
          file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
