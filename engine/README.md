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

The containers (PKWARE DCL Implode, every CRC32 checked), the textures and
the models.

**Textures**: all 761, all 6701 mip levels, through the game's own
six-layout block codec. `--tex` prints one CRC32 per texture and
`../tools/texcheck.sh` diffs those against `../tools/texdec.py`, which is
byte-exact against the original routine under emulation.

**Models**: all 2207, geometry and animation — strip groups, the node
hierarchy, quaternion channels with slerp. `--mod` prints eleven numbers per
model and `../tools/modcheck.py` compares them against `../tools/mod2obj.py`:
six counts exactly and five sums within tolerance, because the arithmetic
crosses a slerp and two implementations of `acos` need not agree in the last
bit.

**Collision trees**: all 692, validated and answering *inside* identically to
`../tools/bsp.py` on points placed exactly on the planes, which is where the
one thing that can be quietly wrong in that format would show.

**Sounds**: all 998 — 992 `WAVCV1.0` wrappers over **Interplay ACM** and six
that really are RIFF — plus the 27 bare music streams. The ACM decoder is
ours; `--wav` prints one CRC32 per decoded sound and `../tools/acmcheck.py`
diffs those against `ffmpeg -f acm`. That reference is worth naming: it is the
only one in this project that nobody here wrote, so it cannot agree with us by
sharing our mistakes. **1019 streams, byte for byte.**

**Scripts**: all 89 shipped `.lua`, which are Lua **3** run on a Lua 5.1 the
engine carries itself (`mlua`, vendored — nothing to install). Three things
make that work and there are only three: the `$if` pragma pass, a prelude for
Lua 3's flat library, and two rewrites (`%upvalue`, and `break` used as an
identifier). `--lua` compiles them all and `../tools/luarun.py --engine`
requires the same answers.

## What it draws

    ./goodomen --window --play 1 1       # level 1 started at checkpoint 1
    ./goodomen --window --play 1 5 --walk  # ...on foot, running, with collision
    ./goodomen --run 1 5 45              # the same 45 seconds headless, checked
    ./goodomen --window --level l1.lua   # the scene graph alone, no rooms
    ./goodomen --level l1.lua            # the same frame offscreen, and its pixels checked
    ./goodomen --window                  # a window, and a triangle in it
    ./goodomen --triangle                # the same triangle offscreen, three pixels checked

A level is the scene graph **run**, its models and textures uploaded, and
every object placed — 409 objects and 74658 triangles for level 1. There is
no lighting yet and no room culling.

Add `--walk --from x,y,z` to put a body on the ground instead of flying. W A
S D to move, the mouse to look, shift to run, space to jump. It is the same
controller `--demo` replays the game's own recorded input through, which is
how it is checked: 1347 frames, 131 units, and never once inside the world.

`--triangle` is the renderer's own check: it opens a hidden window, draws
into a framebuffer object and reads three pixels back — the centre, a corner,
and the green vertex — each of which fails for a different reason. On a
machine with no display it says so and skips, because a check that cannot run
must not pass.

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

Cross-compiling for Windows needs the target, a linker and CMake:

    rustup target add x86_64-pc-windows-gnu
    sudo pacman -S mingw-w64-gcc cmake    # or your distribution's equivalent
    cargo build --release --target x86_64-pc-windows-gnu

4.3 MiB, no runtime to install, 47 seconds the first time and 2 after — the
difference is SDL2, which is **built from source and linked in** on Windows
because there is no system copy there and an `SDL2.dll` beside the binary
would break the whole point. On Linux SDL2 comes from the system instead:
every desktop has it, and the version there is years ahead of the one
`sdl2-sys` bundles.

## Stack, and why

Rust, SDL2, OpenGL 3.3 core written to the subset GLES 3.0 also has (through
`glow`), OpenAL Soft with EFX, and Lua 5.1 through `mlua`.

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
