//! The shipped scripts: Lua **3**, run on a stock Lua 5.1.
//!
//! The game does not need a new interpreter. Lua 3 is close enough to 5.1
//! that a preprocessor, a compatibility prelude and two rewrites carry all
//! 53689 lines of it — which `../../tools/luarun.py` established by running
//! them, and this is that finding as engine code.
//!
//! **The preprocessor.** Lua 3 had conditional compilation, gone by 4.0, and
//! the scripts use it: 25 `$if`, 27 `$end`, 5 `$else`, 2 `$ifnot`, 1
//! `$debug`. `$if nil` guards the cheat menu and `$if debug` the developer
//! tools. Skipped lines are **blanked rather than deleted**, so Lua's error
//! messages still point at the line the file really has.
//!
//! **Two rewrites, and there are only two.** `%name` was Lua 3's *upvalue*
//! reference — `menu.lua` has `mdkNewGame(%lev, %cp)` — and 5.1 closes over
//! lexically, so the sigil is dropped. And `break` was an ordinary
//! identifier until Lua 4: `mdk2.lua` says `local break` and assigns to it
//! five times, so it is renamed where it is a variable rather than the
//! statement. Both are found by trying to compile, and both must be applied
//! **only to code** — the corpus is full of `%s` inside strings and `%` in
//! comments.
//!
//! **The prelude.** Lua 3's library is flat: `strfind`, `tinsert`, `format`
//! rather than `string.find`, `table.insert`, `string.format`. The prelude
//! re-exports 5.1's under the old names and supplies what 5.1 dropped.
//!
//! **The constants.** The 507 ALL_CAPS globals the original defines are
//! installed too, with the values it gives them — see
//! [`crate::game::constants`]. Of the 271 the scripts name, 268 are among
//! them; the three that are not (`COM_KBLEFT`, `COM_KBRIGHT`, `COM_KJUMP`)
//! appear only in `debug.lua`, which `mdk2.lua` loads inside `$if features`
//! and so never loads at all.

use mlua::Lua;

#[derive(Debug)]
pub enum Error {
    Pragma(String),
    Lua(mlua::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Pragma(m) => write!(f, "{m}"),
            Error::Lua(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<mlua::Error> for Error {
    fn from(e: mlua::Error) -> Error {
        Error::Lua(e)
    }
}

/// Lua 3 had these at the top level; 5.1 moved or dropped them.
pub const PRELUDE: &str = r#"
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
"#;

/// Where in `text` the code is, as opposed to a string or a comment.
///
/// Enough of a scanner for these scripts — quotes, long brackets, line
/// comments — and it is what keeps the two rewrites off `"a %s"` and off
/// `-- a %comment`.
fn code_spans(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let n = b.len();
    let (mut i, mut start) = (0usize, 0usize);
    let mut out = Vec::new();
    while i < n {
        if b[i] == b'\'' || b[i] == b'"' {
            out.push((start, i));
            let quote = b[i];
            i += 1;
            while i < n && b[i] != quote {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i = (i + 1).min(n);
            start = i;
        } else if b[i..].starts_with(b"--") {
            out.push((start, i));
            i = text[i..].find('\n').map_or(n, |k| i + k);
            start = i;
        } else if b[i..].starts_with(b"[[") {
            out.push((start, i));
            i = text[i..].find("]]").map_or(n, |k| i + k + 2);
            start = i;
        } else {
            i += 1;
        }
    }
    out.push((start, n));
    out
}

/// Is this byte part of a Lua name?
fn word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Rewrite the two Lua 3 constructs 5.1 rejects. `-> (text, upvalues, breaks)`.
pub fn modernise(text: &str) -> (String, usize, usize) {
    let (mut upvalues, mut breaks) = (0usize, 0usize);
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    for (a, b) in code_spans(text) {
        out.push_str(&text[last..a]);
        let span = &text[a..b];
        let bytes = span.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            // `%name` -> `name`: Lua 3's upvalue sigil, dropped
            if bytes[i] == b'%'
                && i + 1 < bytes.len()
                && (bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'_')
            {
                upvalues += 1;
                i += 1;
                continue;
            }
            // `break` as a *variable*: preceded by an assignment or a
            // declaration, or followed by one. The statement `do break end`
            // has neither and is left alone.
            if bytes[i] == b'b'
                && span[i..].starts_with("break")
                && (i == 0 || !word(bytes[i - 1]))
                && !word(*bytes.get(i + 5).unwrap_or(&b' '))
            {
                let before = span[..i].trim_end();
                let after = span[i + 5..].trim_start();
                let declared = before.ends_with("local")
                    || before.ends_with("not")
                    || before.ends_with("and")
                    || before.ends_with("or")
                    || before.ends_with('(')
                    || before.ends_with(',')
                    || before.ends_with('=');
                let used = after.starts_with('=') && !after.starts_with("==")
                    || after.starts_with(',')
                    || after.starts_with(')');
                if declared || used {
                    breaks += 1;
                    out.push_str("break_");
                    i += 5;
                    continue;
                }
            }
            let c = span[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
        last = b;
    }
    out.push_str(&text[last..]);
    (out, upvalues, breaks)
}

/// Resolve Lua 3's `$if` / `$ifnot` / `$else` / `$end` pragmas, then apply
/// the two rewrites. A name is true only when it is in `defines`; `nil`
/// never is.
pub fn preprocess(text: &str, defines: &[&str]) -> Result<String, Error> {
    Ok(modernise(&pragmas(text, defines)?).0)
}

/// The pragma pass alone, before the rewrites — so that what gets counted as
/// rewritten is what actually gets compiled, and not a line inside a `$if
/// nil` the compiler never sees.
pub fn pragmas(text: &str, defines: &[&str]) -> Result<String, Error> {
    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<bool> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix('$') {
            let mut parts = rest.split_whitespace();
            let word = parts.next().unwrap_or("");
            let arg = parts.next().unwrap_or("");
            match word {
                "if" => stack.push(defines.contains(&arg)),
                "ifnot" => stack.push(!defines.contains(&arg)),
                "else" => {
                    let last = stack
                        .last_mut()
                        .ok_or_else(|| Error::Pragma("$else without $if".into()))?;
                    *last = !*last;
                }
                "end" => {
                    stack
                        .pop()
                        .ok_or_else(|| Error::Pragma("$end without $if".into()))?;
                }
                // $debug and $nodebug carry no branch; they are simply dropped
                "debug" | "nodebug" => {}
                other => return Err(Error::Pragma(format!("unknown pragma ${other}"))),
            }
            out.push(String::new());
            continue;
        }
        // blanked, not removed, so the line numbers Lua reports stay true
        out.push(if stack.iter().all(|&k| k) {
            line.to_string()
        } else {
            String::new()
        });
    }
    if !stack.is_empty() {
        return Err(Error::Pragma(format!("{} unterminated $if", stack.len())));
    }
    Ok(out.join("\n"))
}

/// A Lua 5.1 state with the Lua 3 prelude already in it.
pub struct Scripts {
    pub lua: Lua,
}

impl Scripts {
    pub fn new() -> Result<Scripts, Error> {
        let lua = Lua::new();
        lua.load(PRELUDE).set_name("prelude").exec()?;
        // the ALL_CAPS globals are the engine's, and they are numbers: the
        // scripts add them as bit flags. Their values are read out of the
        // original by `tools/luaconst.py`.
        crate::game::constants::install(&lua)?;
        Ok(Scripts { lua })
    }

    /// Preprocess and run.
    pub fn run(&self, name: &str, source: &str) -> Result<(), Error> {
        let text = preprocess(source, &[])?;
        self.lua.load(&text).set_name(name).exec()?;
        Ok(())
    }

    /// Preprocess and compile, without running. `name` is only for the error
    /// messages, and it matters that it is right — a syntax error in Lua 3
    /// source is reported against the line of the *blanked* text, which is
    /// the line of the original.
    pub fn compile(&self, name: &str, source: &str) -> Result<(usize, usize), Error> {
        let (text, upvalues, breaks) = modernise(&pragmas(source, &[])?);
        self.lua.load(&text).set_name(name).into_function()?;
        Ok((upvalues, breaks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same sample `tools/luarun.py`'s self-test uses, so the two are
    /// answering the same questions.
    const SAMPLE: &str = r#"
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
"#;

    #[test]
    fn the_pragmas_resolve_and_the_line_numbers_do_not_move() {
        let off = preprocess(SAMPLE, &[]).unwrap();
        assert!(!off.contains("kept_by_define"));
        assert!(off.contains("dropped_by_define"));

        let on = preprocess(SAMPLE, &["debug"]).unwrap();
        assert!(on.contains("kept_by_define"));
        assert!(!on.contains("dropped_by_define"));
        assert!(!on.contains("never"), "$if nil is never true");
        assert_eq!(
            on.lines().count(),
            SAMPLE.lines().count(),
            "line numbers must not move"
        );
    }

    #[test]
    fn the_two_lua_3_constructs_are_rewritten_and_nothing_else_is() {
        let on = preprocess(SAMPLE, &["debug"]).unwrap();
        assert!(on.contains("local break_ = 0"), "{on}");
        assert!(
            on.contains("while 1 do break end"),
            "the statement stays a statement"
        );
        assert!(on.contains("mdkNewGame(lev, cp)"), "{on}");
        assert!(
            on.contains("\"a %s and a %d, both untouched\""),
            "strings are left alone"
        );
        assert!(on.contains("%comment"), "comments are left alone");
    }

    #[test]
    fn an_unterminated_pragma_is_an_error() {
        assert!(matches!(preprocess("$if debug\nx = 1\n", &[]), Err(Error::Pragma(_))));
        assert!(matches!(preprocess("$end\n", &[]), Err(Error::Pragma(_))));
    }

    /// The prelude has to load, and the flat Lua 3 names have to be there:
    /// every shipped script uses them.
    #[test]
    fn the_prelude_supplies_the_lua_3_library() {
        let scripts = Scripts::new().unwrap();
        let found: bool = scripts
            .lua
            .load("return strfind('abc', 'b') == 2 and getn({1,2,3}) == 3 and mod(7,4) == 3")
            .eval()
            .unwrap();
        assert!(found);
    }

    #[test]
    fn a_lua_3_script_compiles_after_preprocessing() {
        let scripts = Scripts::new().unwrap();
        let (upvalues, breaks) = scripts.compile("sample", SAMPLE).unwrap();
        assert_eq!((upvalues, breaks), (2, 1));
        assert!(scripts.compile("bad", "x = = 1").is_err());
    }
}
