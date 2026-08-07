#!/usr/bin/env python3
"""
luarun.py -- actually run the game's shipped Lua, on a stock Lua 5.1.

M7 needs a script runtime. It does not need a new interpreter: the scripts are
Lua 3, and Lua 3 is close enough to 5.1 that a preprocessor, a compatibility
prelude and two rewrites carry all 53689 lines of it.

**The preprocessor.** Lua 3 had conditional compilation, gone by 4.0, and the
scripts use it -- 25 `$if`, 27 `$end`, 5 `$else`, 2 `$ifnot`, 1 `$debug`.
`$if nil` guards the cheat menu and `$if debug` the developer tools. A name is
true when passed to `--define`; `nil` never is. Skipped lines are blanked
rather than deleted so Lua's error messages still point at the real line.

**The prelude.** Lua 3's library is flat -- `strfind`, `tinsert`, `format`
rather than `string.find`, `table.insert`, `string.format` -- so the prelude
re-exports 5.1's under the old names and supplies what 5.1 dropped outright:
`foreach`, `foreachi`, `nextvar`, `getglobal`, `setglobal`, `dostring`,
`call`, and a `dofile` that takes a bare resource name the way the engine's
does.

**Two rewrites, both found by trying to compile.** It would be nice to say the
syntax is otherwise compatible, and a first reading of the corpus said so --
every `%` looked like a format string. It is not: `menu.lua` has
`mdkNewGame(%lev, %cp)`, Lua 3's **upvalue** syntax, which 5.1 closes over
lexically instead, so the sigil is simply dropped. And `break` was an ordinary
identifier until Lua 4 -- `mdk2.lua` says `local break` and assigns to it five
times -- so it is renamed where it is a variable rather than the statement.
Two constructs in 53689 lines, and a compile pass finds both in a second.

**The engine, stubbed.** Rather than list the 438 functions and 248 constants
`docs/lua-api.md` catalogues, the stub reads the naming convention the scripts
already keep: an ALL_CAPS global is a constant and stands for its own name;
any other unknown global is an engine function. One that makes or fetches
something -- Create, Make, New, Get -- returns a fresh table, because the
scripts immediately give it fields; anything else answers nil, so that
`if mdkIsSomething()` stays false rather than quietly turning true.
`--answer NAME=VALUE` overrides one, and `--set NAME=VALUE` supplies a global
the engine would have set, such as `checkpoint`.

**What that gets.** All 31 scripts compile, and **all 31 run to the end** with
the right boot sequence -- `--boot mdk2` for most, and `boss.lua` wants
`mdk2 -> level12 -> zizzyroom`, which is the Zizzy fight loading its arena.
`level1.lua` alone makes **1334 engine calls across 22 functions**, 409 of
them `mdkRegisterObject`: the whole scene graph, registered by running the
real script rather than by parsing it.

Which is the check that matters. `tools/scene.py` reads those calls out of the
text; running the same files through a real Lua and comparing every field of
every object must agree, and it does -- **54 scene graphs, 5633 objects, no
disagreement.**

Usage:
    python3 tools/luarun.py extracted/base/l1.lua
    python3 tools/luarun.py extracted/scripts --compile     # parse them all
    python3 tools/luarun.py extracted/base --crosscheck     # against scene.py
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

LUA = "lua5.1"
PRAGMA = re.compile(r"^\s*\$(debug|nodebug|if|ifnot|else|end)\b\s*(\S*)")

# Lua 3 had these at the top level; 5.1 moved or dropped them.
PRELUDE = r"""
strfind, strlen, strsub = string.find, string.len, string.sub
strlower, strupper, strrep = string.lower, string.upper, string.rep
format, gsub, ascii = string.format, string.gsub, string.byte
abs, ceil, floor, sqrt = math.abs, math.ceil, math.floor, math.sqrt
sin, cos, tan, asin, acos = math.sin, math.cos, math.tan, math.asin, math.acos
atan, atan2, exp, log, log10 = math.atan, math.atan2, math.exp, math.log, math.log10
deg, rad, max, min = math.deg, math.rad, math.max, math.min
random, randomseed = math.random, math.randomseed
mod = math.fmod
tinsert, tremove, sort = table.insert, table.remove, table.sort
getn = function(t) return table.getn and table.getn(t) or #t end
date, clock = os.date, os.clock
dostring = function(s) return assert(loadstring(s))() end
call = function(f, a) return f(unpack(a)) end
getglobal = function(n) return _G[n] end
setglobal = function(n, v) _G[n] = v end
rawgetglobal, rawsetglobal = getglobal, setglobal
function foreach(t, f)
  for k, v in pairs(t) do local r = f(k, v); if r ~= nil then return r end end
end
function foreachi(t, f)
  for i, v in ipairs(t) do local r = f(i, v); if r ~= nil then return r end end
end
nextvar = function(n) return next(_G, n) end
settag, newtag, tag = function() end, function() return 0 end, function() return 0 end
settagmethod, gettagmethod = function() end, function() end
-- the scene graphs name their parent objects as bare globals, and the engine
-- is what defines them; an unset one has to read as nil, not raise
scene, points = "scene", {}
-- the engine's dofile takes a bare resource name and finds the file; ours
-- looks in the directory prepare() filled
local _dofile = dofile
function dofile(n)
  if LUADIR and not string.find(n, "%.") then
    n = LUADIR .. "/" .. string.lower(n) .. ".lua"
  end
  local f = io.open(n)
  if not f then return nil end             -- a resource that is not there
  f:close()
  return _dofile(n)
end
"""


class LuaError(RuntimeError):
    pass


def _code_spans(text: str):
    """Yield (start, end) of the parts of `text` that are code, not string
    or comment. Enough of a scanner for these scripts: quotes, long
    brackets, line comments."""
    i, n, start = 0, len(text), 0
    while i < n:
        c = text[i]
        if c in "'\"":
            yield (start, i)
            q, i = c, i + 1
            while i < n and text[i] != q:
                i += 2 if text[i] == "\\" else 1
            i += 1
            start = i
        elif text.startswith("--", i):
            yield (start, i)
            i = text.find("\n", i)
            i = n if i < 0 else i
            start = i
        elif text.startswith("[[", i):
            yield (start, i)
            i = text.find("]]", i)
            i = n if i < 0 else i + 2
            start = i
        else:
            i += 1
    yield (start, n)


# Lua 3 syntax that 4.0 removed and 5.1 cannot parse. Both appear once each in
# the shipped scripts, and both are found by trying to compile them.
UPVALUE = re.compile(r"%([A-Za-z_]\w*)")
# `break` only became a reserved word in Lua 4; mdk2.lua uses it as a variable
BREAK_VAR = re.compile(r"(?<![\w.])(local\s+|not\s+|and\s+|or\s+|[(,=]\s*)"
                       r"break\b|(?<![\w.])break(?=\s*[=,)])")


def modernise(text: str) -> tuple[str, int, int]:
    """Rewrite the two Lua 3 constructs 5.1 rejects. -> (text, upvals, breaks).

    `%name` was Lua 3's upvalue reference; 5.1 closes over lexically, so the
    sigil just goes. `break` was an ordinary name until Lua 4, and mdk2.lua
    declares one -- `local break` -- so it is renamed where it is being used
    as a variable rather than as the statement.
    """
    out, upvals, breaks = [], 0, 0
    last = 0
    for a, b in _code_spans(text):
        out.append(text[last:a])
        span = text[a:b]
        span, k = UPVALUE.subn(r"\1", span)
        upvals += k
        span, j = BREAK_VAR.subn(
            lambda m: (m.group(1) + "break_") if m.group(1) else "break_", span)
        breaks += j
        out.append(span)
        last = b
    out.append(text[last:])
    return "".join(out), upvals, breaks


def preprocess(text: str, defines: set[str] | None = None) -> str:
    """The pragma pass and the two rewrites, which is what a script needs."""
    return modernise(pragmas(text, defines))[0]


def pragmas(text: str, defines: set[str] | None = None) -> str:
    """Resolve Lua 3's `$if` / `$ifnot` / `$else` / `$end` pragmas.

    Lines are blanked rather than removed so that error messages from Lua
    still point at the line the script really has.
    """
    defines = defines or set()
    out, stack = [], []          # stack of "is this branch being kept"
    for line in text.splitlines():
        m = PRAGMA.match(line)
        if m:
            word, arg = m.group(1), m.group(2)
            if word == "if":
                stack.append(arg in defines)
            elif word == "ifnot":
                stack.append(arg not in defines)
            elif word == "else":
                if not stack:
                    raise LuaError("$else without $if")
                stack[-1] = not stack[-1]
            elif word == "end":
                if not stack:
                    raise LuaError("$end without $if")
                stack.pop()
            out.append("")                       # $debug and $nodebug: drop
            continue
        out.append(line if all(stack) else "")
    if stack:
        raise LuaError(f"{len(stack)} unterminated $if")
    return "\n".join(out)


STUBS = r"""
-- Everything the engine provides and Lua does not. Rather than list the 438
-- functions and 248 constants, read the naming convention the scripts already
-- follow: an ALL_CAPS global is one of the engine's constants and stands for
-- itself, and any other unknown global is one of its functions.
CALLS = {}
ANSWERS = ANSWERS or {}
-- The engine's ALL_CAPS constants are numbers: the scripts add them together
-- as bit flags -- `omAnimPlay(gob, ANIM_X, ANIMFLAG_NOTRANS+ANIMFLAG_INTERRUPT)`
-- -- and the values are nowhere in the scripts. Here a constant stands for
-- its own name, so `+` on two strings joins them instead. The sum only ever
-- goes back into a stubbed engine function, which does not read it. Numeric
-- strings still add as numbers: 5.1 tries coercion before the metamethod.
debug.setmetatable("", {__index = string,
                        __add = function(a, b) return a .. "+" .. b end})
-- registering an object defines a global of that name, which is how the
-- scene graphs name their parents and how the level scripts reach them
function mdkRegisterObject(name, kind, ...)
  CALLS[table.getn(CALLS) + 1] = "mdkRegisterObject"
  -- the type is kept because `mdkGetGobType` has to answer with it, and a
  -- getter that answers with a table is what "attempt to compare table with
  -- number" was, fifty-one times over
  _G[name] = {name = name, __kind = kind}
  return _G[name]
end
-- and so does creating one from a script: level5.lua says
--   mdkCreateObjectLua("doorwav", OBJ_NONE, mdkGetScene(), nil, nil)
--   doorwav.wav = omGobAddSound(doorwav, "jd_doors", 0)
-- so the name in the first argument is a global by the time the next line
-- runs. Three of the ten levels do not boot without this.
function mdkCreateObjectLua(name, ...)
  CALLS[table.getn(CALLS) + 1] = "mdkCreateObjectLua"
  _G[name] = {name = name}
  return _G[name]
end
setmetatable(_G, {__index = function(_, k)
  if string.find(k, "^[A-Z][A-Z0-9_]*$") then return k end
  if string.find(k, "^%l%l%a?%u") then          -- ch*, om*, mdk*, lua*
    -- a function that makes or fetches something hands back a handle, and
    -- the scripts immediately give it fields; anything else answers nil,
    -- so that `if mdkIsSomething()` stays false rather than silently true
    local handle = string.find(k, "Create") or string.find(k, "Make")
                or string.find(k, "New") or string.find(k, "Get")
    local f = function(...)
      CALLS[table.getn(CALLS) + 1] = k
      -- an answer may be a value or a function of the arguments: `chRand`
      -- has to be different every call, and `mdkGetGobType` has to look at
      -- what it was handed
      if ANSWERS[k] ~= nil then
        if type(ANSWERS[k]) == "function" then return ANSWERS[k](...) end
        return ANSWERS[k]
      end
      if handle then return {} end
      return nil
    end
    rawset(_G, k, f)
    return f
  end
  return nil
end})
"""


def stub_source(api: Path | None = None) -> str:
    """Bind every engine function and constant. `api` is unused, kept so the
    caller can point at docs/lua-api.md when it wants the count checked."""
    return STUBS


def scratch(prefix: str = "goodomen-lua-") -> Path:
    """A temporary directory that goes away when the process does.

    It did not, for a long time: every `--tree` run left 3.1 MiB of prepared
    scripts behind, and 4556 of them had accumulated -- 14 GiB, a full /tmp,
    and every tool on the machine failing at once. A temporary file that
    outlives its process is not temporary.
    """
    import atexit
    import shutil
    import tempfile
    tmp = Path(tempfile.mkdtemp(prefix=prefix))
    atexit.register(shutil.rmtree, tmp, ignore_errors=True)
    return tmp


def prepare(roots: list[Path], out: Path, defines: set[str]) -> int:
    """Write a modernised copy of every script into one flat directory.

    The engine's `dofile` takes a bare resource name -- `dofile('boss')`,
    `dofile('strings')` -- and finds it wherever it lives, so the runtime
    needs the same flat namespace. Doing it as files rather than by inlining
    keeps `dofile(s)` with a computed name working, which `mdk2.lua` uses.
    """
    out.mkdir(parents=True, exist_ok=True)
    n = 0
    for root in roots:
        for f in sorted(root.rglob("*.lua")):
            (out / f.name.lower()).write_text(
                preprocess(f.read_text(errors="replace"), defines))
            n += 1
    return n


DUMP = """
local function enc(v, out)
  local t = type(v)
  if t == "table" then
    local n, first = table.getn(v), true
    out[table.getn(out)+1] = "{"
    for k, x in pairs(v) do
      if not first then out[table.getn(out)+1] = "," end
      first = false
      out[table.getn(out)+1] = string.format("%q", tostring(k)) .. ":"
      enc(x, out)
    end
    out[table.getn(out)+1] = "}"
  elseif t == "number" then
    out[table.getn(out)+1] = string.format("%.9g", v)
  elseif t == "string" then
    out[table.getn(out)+1] = string.format("%q", v)
  elseif t == "boolean" then
    out[table.getn(out)+1] = tostring(v)
  else
    out[table.getn(out)+1] = "null"
  end
end
function DUMP(path)
  local v = _G
  for part in string.gfind(path, "[^.]+") do
    if type(v) ~= "table" then v = nil break end
    v = v[part]
  end
  local out = {}
  enc(v, out)
  io.write("---\\n" .. table.concat(out) .. "\\n")
end
"""

TRACE = """
local chunk = assert(loadstring(SOURCE, "=script"))
local ok, err = pcall(chunk)
io.write("---\\n")
local seen, order = {}, {}
for i = 1, table.getn(CALLS) do
  local n = CALLS[i]
  if not seen[n] then seen[n] = 0; order[table.getn(order)+1] = n end
  seen[n] = seen[n] + 1
end
for i = 1, table.getn(order) do
  io.write(order[i] .. "\\t" .. seen[order[i]] .. "\\n")
end
if not ok then io.write("!\\t" .. tostring(err) .. "\\n") end
"""


def run(source: str, extra: str = "", trace: bool = False,
        timeout: float = 300) -> str:
    """Run one preprocessed script under the prelude. -> its stdout."""
    if trace:
        if "]=====]" in source:
            raise LuaError("script contains the long-bracket delimiter")
        extra += "\nSOURCE = [=====[\n" + source + "\n]=====]\n" + TRACE
        source = ""
    program = PRELUDE + "\n" + extra + "\n" + source
    try:
        p = subprocess.run([LUA, "-"], input=program, capture_output=True,
                           text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        raise LuaError(f"did not finish in {timeout:g}s")
    if p.returncode:
        err = [l for l in (p.stderr or "").splitlines()
               if l.strip() and not l.lstrip().startswith(("stack traceback",
                                                           "[C]:", "stdin:"))]
        raise LuaError(err[0].strip() if err else (p.stderr or "?").strip())
    return p.stdout


# Printed by the recorder below, one line per object, so Python can compare it
# with what tools/scene.py parsed out of the same file.
RECORDER = r"""
local n = 0
function mdkRegisterObject(name, type, sc, parent, group, x, y, z,
                           qw, qx, qy, qz, res, a, b, c, d, lo, hi, flag)
  n = n + 1
  _G[name] = name
  io.write(format("%s\t%s\t%s\t%.6g\t%.6g\t%.6g\t%.6g\t%.6g\t%.6g\t%.6g\t%s\n",
                  name, type or "?", parent or "", x, y, z, qw, qx, qy, qz,
                  res or ""))
end
"""


def digest(files: list[Path], override: Path | None,
           gamedir: str | None) -> int:
    """`name upvalues breaks` a line -- what the two Lua 3 rewrites did to
    each script -- and, with --engine, the same from the engine, compared.

    The `override/` copy of a script wins, because that is what the engine's
    own loader does and `override/level1.lua` differs by sixty lines. Without
    that rule the two sides read different files and agree by accident.
    """
    import subprocess
    patch = {}
    if override and override.is_dir():
        patch = {f.name.lower(): f for f in override.rglob("*.lua")}
    chosen: dict[str, Path] = {}
    for f in files:
        chosen.setdefault(f.name.lower(), f)
    chosen.update(patch)

    mine = {}
    for name in sorted(chosen):
        text = chosen[name].read_text(errors="replace")
        _, upvals, breaks = modernise(pragmas(text))
        mine[name] = (upvals, breaks)

    if not gamedir:
        for name, (u, b) in mine.items():
            print(f"{name} {u} {b}")
        print(f"{len(mine)} scripts", file=sys.stderr)
        return 0

    root = Path(__file__).resolve().parent.parent
    out = subprocess.run(
        ["cargo", "run", "--quiet", "--release", "--manifest-path",
         str(root / "engine/Cargo.toml"), "--", gamedir, "--lua"],
        capture_output=True, text=True, check=True).stdout
    theirs = {}
    for line in out.splitlines():
        f = line.split()
        if len(f) == 3:
            theirs[f[0]] = (int(f[1]), int(f[2]))
    bad = [f"{n}: {mine[n]} against the engine's {theirs.get(n)}"
           for n in sorted(mine) if theirs.get(n) != mine[n]]
    bad += [f"{n}: the engine compiled it and this did not"
            for n in sorted(set(theirs) - set(mine))]
    for line in bad[:20]:
        print(f"MISMATCH {line}", file=sys.stderr)
    print(f"{len(mine)} scripts, {len(bad)} disagree", file=sys.stderr)
    return 1 if bad else 0


def _obj_key(o: dict) -> tuple:
    return (o["name"], o["type"], o["parent"] or "",
            *(float(f"{v:.6g}") for v in o["position"]),
            *(float(f"{v:.6g}") for v in o["rotation"]),
            o["resource"] or "")


def crosscheck(path: Path, api: Path) -> tuple[int, list[str]]:
    """Run a scene graph for real and compare with tools/scene.py."""
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import scene as sg

    text = path.read_text(errors="replace")
    out = run(preprocess(text), stub_source(api) + RECORDER)
    ran = []
    for line in out.splitlines():
        f = line.split("\t")
        ran.append((f[0], f[1], f[2], *(float(x) for x in f[3:10]), f[10]))
    parsed = [_obj_key(o) for o in sg.parse(text)["objects"]]
    bad = [f"{a} != {b}" for a, b in zip(ran, parsed) if a != b]
    if len(ran) != len(parsed):
        bad.append(f"{len(ran)} objects run, {len(parsed)} parsed")
    return len(ran), bad


SAMPLE = """
$if debug
  kept_by_define = 1
$else
  dropped_by_define = 1
$end
$if nil
  never = 1
$end
local break = 0
while 1 do break end
f = function() return mdkNewGame(%lev, %cp) end
s = "a %s and a %d, both untouched"
-- and a %comment, likewise
"""


def selftest() -> None:
    """The preprocessor and the two rewrites, without needing the game."""
    off = preprocess(SAMPLE)
    assert "kept_by_define" not in off and "dropped_by_define" in off, off
    on = preprocess(SAMPLE, {"debug"})
    assert "kept_by_define" in on and "dropped_by_define" not in on, on
    assert "never" not in on, "$if nil is never true"
    assert len(on.splitlines()) == len(SAMPLE.splitlines()), \
        "line numbers must not move"
    assert "local break_ = 0" in on, on
    assert "while 1 do break end" in on, "the statement stays a statement"
    assert "mdkNewGame(lev, cp)" in on, on
    assert '"a %s and a %d, both untouched"' in on, "strings are left alone"
    assert "%comment" in on, "comments are left alone"
    print("luarun.py: self-test passed")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path, nargs="?",
                    help="a .lua file or a directory")
    ap.add_argument("--selftest", action="store_true",
                    help="check the preprocessor and the Lua 3 rewrites")
    ap.add_argument("--define", action="append", default=[],
                    help="a name the $if pragmas should treat as true")
    ap.add_argument("--api", type=Path, default=Path("docs/lua-api.md"))
    ap.add_argument("--compile", action="store_true",
                    help="only check that every script parses")
    ap.add_argument("--crosscheck", action="store_true",
                    help="run the scene graphs and compare with scene.py")
    ap.add_argument("--set", action="append", default=[], metavar="NAME=VALUE",
                    help="a global the engine would have set, e.g. "
                         "--set checkpoint=1")
    ap.add_argument("--boot", action="append", default=[], metavar="NAME",
                    help="script to dofile() first, e.g. --boot mdk2; needs "
                         "--tree")
    ap.add_argument("--answer", action="append", default=[],
                    metavar="NAME=VALUE",
                    help="what an engine function should return, e.g. "
                         "chGetLanguageIsEnglish=1")
    ap.add_argument("--dump", metavar="PATH",
                    help="after running, print this global as JSON, e.g. "
                         "--dump Level.scenegraph.checkpoints")
    ap.add_argument("--call", metavar="LUA",
                    help="run this once the script has loaded, and count "
                         "only what it does -- `--call 'Level.Init(1)'` is "
                         "the engine's own entry into a level")
    ap.add_argument("--trace", action="store_true",
                    help="report the engine calls made, and where it stopped")
    ap.add_argument("--tree", type=Path,
                    help="extraction root: makes dofile() work by preparing "
                         "every script in one flat directory first")
    ap.add_argument("--digest", action="store_true",
                    help="`name upvalues breaks` a line, which is what the "
                         "engine prints for --lua")
    ap.add_argument("--engine", metavar="GAMEDIR",
                    help="run the engine over this installation and require "
                         "it to rewrite exactly the same things")
    ap.add_argument("--override", type=Path,
                    help="the game's override/ directory, which the engine "
                         "reads in preference to the archives -- it is a "
                         "shipped patch, and level1.lua differs by 60 lines")
    args = ap.parse_args(argv)
    if args.selftest:
        selftest()
        return 0
    if args.src is None:
        ap.error("a .lua file or a directory is required")

    files = sorted(args.src.rglob("*.lua")) if args.src.is_dir() else [args.src]
    if not files:
        ap.error(f"no .lua under {args.src}")

    if args.digest or args.engine:
        return digest(files, args.override, args.engine)

    if args.crosscheck:
        files = [f for f in files if "mdkRegisterObject(" in
                 f.read_text(errors="replace")]
        total = complaints = 0
        for f in files:
            n, bad = crosscheck(f, args.api)
            total += n
            complaints += len(bad)
            for line in bad[:3]:
                print(f"  {f.name}: {line}", file=sys.stderr)
        print(f"{len(files)} scene graphs run under {LUA}, {total} objects, "
              f"{complaints} disagree with tools/scene.py", file=sys.stderr)
        return 1 if complaints else 0

    answers = "".join(f'ANSWERS["{a.split("=", 1)[0]}"] = '
                      f"{a.split('=', 1)[1]}\n" for a in args.answer)
    globals_ = "".join(f"{g.split('=', 1)[0]} = {g.split('=', 1)[1]}\n"
                       for g in args.set)
    stubs = ("ANSWERS = {}\n" + answers) + stub_source() + globals_
    if args.tree:
        tmp = scratch()
        roots = [d for d in (args.tree / "scripts", args.tree / "base",
                             args.override) if d and d.is_dir()]
        count = prepare(roots, tmp, set(args.define))
        stubs = f"LUADIR = [[{tmp}]]\n" + stubs
        print(f"{count} scripts prepared", file=sys.stderr)
        # the entry script has to come from the prepared tree too, or an
        # override of it is silently ignored
        files = [tmp / f.name.lower() if (tmp / f.name.lower()).is_file()
                 else f for f in files]

    ok = bad = 0
    for f in files:
        text = f.read_text(errors="replace")
        # a prepared file has already been through preprocess()
        source = (text if args.tree and f.parent == locals().get("tmp")
                  else preprocess(text, set(args.define)))
        try:
            if args.compile:
                run("", f"local f = assert(loadstring([==[\n{source}\n]==]))")
                ok += 1
            elif args.dump:
                boot = ""
                if args.boot:
                    boot = (f"dofile('{args.boot[0]}')\n"
                            "local _pre = PreInitLevel\n"
                            "function PreInitLevel()\n"
                            "  if Level and Level.file then"
                            " dofile(Level.file) end\n"
                            "  return _pre()\nend\n")
                    boot += "".join(f"dofile('{b}')\n"
                                    for b in args.boot[1:])
                out = run(boot + source + f"\nDUMP('{args.dump}')\n",
                          stubs + DUMP)
                sys.stdout.write(out.split("---\n", 1)[-1])
                ok += 1
            elif args.trace or args.call:
                # the engine loads the scene graph named by Level.file while
                # it is loading the level script -- the comment above
                # PreInitLevel in mdk2.lua says "called from the bowels of
                # the dofile" -- so the shim does it there too
                boot = ""
                if args.boot:
                    boot = (f"dofile('{args.boot[0]}')\n"
                            "local _pre = PreInitLevel\n"
                            "function PreInitLevel()\n"
                            "  if Level and Level.file then"
                            " dofile(Level.file) end\n"
                            "  return _pre()\nend\n")
                    boot += "".join(f"dofile('{b}')\n"
                                    for b in args.boot[1:])
                # `--call` counts only what the call itself does, so the
                # tally is cleared once the script has finished loading
                if args.call:
                    source += "\nCALLS = {}\n" + args.call + "\n"
                out = run(boot + source, stubs, trace=True)
                body = out.split("---\n", 1)[-1].splitlines()
                calls = [l for l in body if not l.startswith("!")]
                fail = [l for l in body if l.startswith("!")]
                total = sum(int(l.split("\t")[1]) for l in calls)
                print(f"{f.name}: {len(calls)} engine functions, "
                      f"{total} calls" +
                      (f", stopped at {fail[0][2:]}" if fail else ", ran to the end"))
                for l in (calls if args.call else calls[:12]):
                    n, c = l.split("\t")
                    print(f"    {c:>5}x  {n}")
                ok += not fail
                bad += bool(fail)
            else:
                sys.stdout.write(run(source, stubs))
                ok += 1
        except LuaError as e:
            bad += 1
            print(f"  {f.name}: {e}", file=sys.stderr)
    verb = "compile" if args.compile else "run"
    print(f"{ok}/{ok + bad} scripts {verb} under {LUA}", file=sys.stderr)
    return 1 if bad else 0


if __name__ == "__main__":
    raise SystemExit(main())
