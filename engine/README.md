# goodomen

The engine. Build it, put it in your MDK2 directory, run it.

    cargo build --release
    cp target/release/goodomen "/path/to/MDK 2/"
    cd "/path/to/MDK 2" && ./goodomen

It reads the game out of its **own directory** — `data/*.zip` first and then
`override/`, in that order, because `override/` is a shipped patch. Nothing
to install, nothing to configure. Pass a path to read a game somewhere else:

    ./goodomen "/path/to/MDK 2"

## What it reads so far

The containers (PKWARE DCL Implode, every CRC32 checked) and the textures:
all 761 of them, all 6701 mip levels, decoded through the game's own six-layout
block codec. `--tex` prints one CRC32 per texture and `../tools/texcheck.sh`
diffs those against `../tools/texdec.py`, which is byte-exact against the
original routine under emulation.

## Platforms

**Linux and Windows are the targets**, and both are tested rather than
claimed. `tools/winbuild.sh` cross-builds the Windows binary, drops it into a
real installation and starts it with **no arguments** — the way it is meant
to be used — under Wine, and requires the same 4751 files and the same
checksums the Linux build gets. "It builds" is never mistaken for "it works",
and finding the game beside itself is the one thing running from elsewhere
cannot test.

**macOS is not claimed.** There is no Apple hardware to test on here, and an
untested platform is not a supported one. The graphics choice happens to
leave the door open — see below — and that is all that is being said about
it.

**Android is not attempted.** Perhaps once the engine is finished.

Cross-compiling for Windows needs the target and a linker:

    rustup target add x86_64-pc-windows-gnu
    sudo pacman -S mingw-w64-gcc          # or your distribution's equivalent
    cargo build --release --target x86_64-pc-windows-gnu

466 KiB, no runtime to install, 2 seconds to build.

## Stack, and why

Rust, SDL2, OpenGL 3.3 core written to the subset GLES 3.0 also has, OpenAL
Soft with EFX, and Lua 5.1 through `mlua`.

GL 3.3 core is simply the right level for Linux and Windows: old enough that
every driver has it, new enough for the shader-side node posing the models
need. Writing it in the subset GLES 3.0 also has costs nothing today and is
what would carry macOS (frozen at GL 4.1, deprecated since 2018, still
working — it is how OpenMW and devilutionX ship there) and Android (GLES
only) if either is ever wanted. That is a door left open, not a promise.

The Lua version is not a guess: `../tools/luarun.py` runs all 31 of the
game's scripts on a stock 5.1 with a preprocessor for the `$if` pragmas, a
Lua 3 prelude and two rewritten lines.

Rust because the dominant activity here is reading other people's bytes at
guessed offsets, and being wrong is the normal state of the work. A wrong
offset raises here; in C it reads whatever is next door and hands back a
plausible, wrong picture.

## Shape

Objects live in an **arena indexed by id**, not a pointer graph, because that
is what the game itself does — `mdkRegisterObject` files an object under a
name, scripts hold handles, and `mdkGetPlayerGob()` returns one. No
`Rc<RefCell<_>>` in game logic.

## Rules

Nothing here is decompiled. The formats are described in `../docs/`, checked
against the whole corpus by the Python tools in `../tools/`, and implemented
fresh. **The tools stay and remain the reference**: where both can do a
thing they must agree.

    cargo test
