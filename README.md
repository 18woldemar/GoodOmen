# GoodOmen

An open reimplementation of the **MDK2** engine (BioWare, 2000), running on
the original assets you own. This is not a decompilation: *behaviour and data
formats* are recovered from the original, and the engine is written from
scratch. The model is OpenMW and devilutionX.

> **Game assets are not distributed here and never will be.**
> Running anything requires a legal copy of MDK2 (GOG, disc, or the 1C release).

## The engine

`engine/` is the engine itself, in Rust. Drop the binary into your MDK2
directory and run it.

    cargo build --release --manifest-path engine/Cargo.toml
    cp engine/target/release/goodomen "/path/to/MDK 2/"
    cd "/path/to/MDK 2" && ./goodomen --window --play 1 5 --walk

It reads the game out of its own directory — `data/*.zip` first and then
`override/`, because `override/` is a shipped patch — and today it:

- **reads every container**: 4751 files, every CRC32 matching, with its own
  PKWARE DCL Implode decompressor;
- **decodes every texture**: 761, all 6701 mip levels, through the six-layout
  block codec, **byte-identical to `tools/texdec.py`**;
- **reads every model**: 2207, geometry and animation, agreeing with
  `tools/mod2obj.py` on eleven numbers each;
- **reads every collision tree**: 692, answering *inside* identically to
  `tools/bsp.py` on points placed exactly on the planes;
- **compiles every shipped script** on a Lua 5.1 it carries itself, with the
  `$if` pragma pass, the Lua 3 prelude and the two rewrites;
- **starts every level at every checkpoint**: 129 boots, 2093 resources named
  and none missing, 677 rooms;
- **runs one**: the body walks with collision, the driver ticks, and
  `OnEnterRoom`, `OnTimer`, `OnUpdate` and **`OnCollision`** fire in the order
  the scripts expect — **11728 of 11958 handler calls run to the end**, which
  is `tools/boot.py`'s figure to the digit. Collisions come off the real
  collision world: a BSP tree names its object because **every one of the 605
  collision resources is named by exactly one object**, and `target == nil`
  is how the scripts spell the end of one, so both edges are events. The player's own object moves with
  the body, so proximity reaches the scripts: walking about level 6 is enough
  to make `l6_kermit` start an animation on its own;
- **draws it**: OpenGL 3.3 core, textured, animated in the vertex shader, lit
  by the level's own `OBJ_STATICLIGHT` objects, and culled by the authored
  room visibility — a median of 15.7% of a level's triangles. **Animation 0
  never moves** — in all 1146 animated models every one of its channels holds
  a single key — so a level only comes alive when `omAnimPlay` chooses
  another, which the driver now does. **The player is on screen**: the level
  names its own player object through `mdkSetPlayModeGobs`, and `OBJ_KURT`
  wears `kurt.mod`, and the engine drives his locomotion the way the
  original's `mdkWalkerAnimUpdate` does — **every one of `kurt.mod`'s 61
  animation ids is a constant the binary names `ANIM_*`**, and 6292 of the
  corpus's 6311 are. Characters and pickups are placed from their type too:
  the convention holds for 55 of the 149 `OBJ_*` types, which is what puts
  them in the world at all;
- **and you can hear it**: OpenAL Soft, opened at run time rather than linked,
  with the level's `OBJ_AMBIENTSOUND` objects looping where they stand. The
  payload's near and far distances drive DirectSound3D's own model, and the
  rooms reverberate through EFX with **the 26 EAX 2.0 environments** — level 1
  opens in a `PARKINGLOT` and level 7's hub is a `HANGAR`, which are the level
  tables' own numbers. The mixer is checked the way the renderer is: rendered
  through a **loopback device** so the mix comes back as samples, with nothing
  played at anyone. `1.00@5u 1.00@10u 0.50@20u 0.25@80u` against the clamped
  inverse law, and a tail of `0.000 PADDEDCELL against 0.214 ARENA` a quarter
  second after the sound stops. **All 76 ambient sounds the ten levels place
  decode**, and the **music streams** — 27 tracks, 1h 23m, decoded a block at
  a time into a queue rather than held in memory, and looping the way the
  `.mus` playlists say. The scripts make their own noise too: over all 129
  boots the handlers hang **32 sounds on objects and fire them 584 times**,
  every name resolving to a file.

**Linux and Windows are both tested**, and the Windows build is
cross-compiled and run under Wine inside a real installation as part of the
checks. macOS is not claimed: there is no Apple hardware here, and an
untested platform is not a supported one.

**Every format the game ships is now read by the engine itself**, the last
being the **Interplay ACM** audio payload. The decoder is ours, written from
the routine `mdk2Main.exe` carries, and it is held to `ffmpeg -f acm` — a
decoder nobody here wrote — over all 992 wrapped sounds and all 27 music
tracks: **1019 streams, byte for byte, no exceptions**.

## The tools, which stay the reference

Where the engine and a tool can both do a thing, **they must agree**, and
`tools/check.py` is what says they do.


| | |
|---|---|
| `tools/unpack.py` | Unpacks the `data/*.zip` containers. **4751/4751 files, every CRC32 matches.** |
| `tools/tex2png.py` | Converts `.tex` textures to PNG. **761/761**, with no emulator and no game executable. |
| `tools/texdec.py` | The `.tex` block codec, reimplemented from scratch. **Byte-identical to the original on all 4205514 blocks.** |
| `tools/mod2obj.py` | Reads `.mod` models. **2207/2207, 857321 triangles.** Exports OBJ, renders textured previews and turntables, and plays animation with slerped rotations. |
| `tools/mod2html.py` | Packs a model — or, with `--scene`, a whole level and everything standing in it — into a single self-contained WebGL viewer. With `--walk` the collision trees come too and you can walk the level instead of flying it, spawning at the game's own checkpoints. No libraries, no server. |
| `tools/bsp.py` | Reads and validates the `.bsp` collision trees; point-in-solid and drop queries. **692/692 validate.** |
| `tools/scene.py` | Reads the level scene graphs — every object's type, position, parent and resource. **54 files, 5633 objects, 0 complaints.** |
| `tools/wavc.py` | The sounds and the music. **992 of the 998 `.wav` files are not RIFF** — they are `WAVCV1.0` over Interplay ACM, the Baldur's Gate codec — and `Music/` is 27 bare ACM streams, 1h 23m of it, with Infinity Engine `.mus` playlists. |
| `tools/walksim.py` | The viewer's controller, in Python, over every level. **2557 spawn points: 2556 stay standing, 2 ever inside geometry.** |
| `tools/spawn.py` | The checkpoints a level starts you at, pulled by running its script, and whether each one stands in open space. **129 checkpoints, 128 clear.** |
| `tools/luarun.py` | Runs the shipped Lua on a stock `lua5.1`. **All 31 scripts run to the end**, and the scene graphs they register match `scene.py` exactly — 5633 objects, no disagreement. |
| `tools/luaapi.py` | Catalogues the engine functions the shipped Lua calls — **438 of them**, which is the specification the engine has to meet. See [`docs/lua-api.md`](docs/lua-api.md). |
| `tools/strfile.py` | Reads `.str` — every line of text and the `.wav` that speaks it. **686 entries, 348 voiced, byte-exact**, and the same for all five shipped languages. |
| `tools/stars.py` | Reads `.sta`, the sky. **3141 real stars**, and the shipped southern declinations are wrong; `--fix` corrects them. |
| `tools/omn.py` | Reads `.omn`, the recorded attract-mode demo: 1348 frames of controller input at 30 fps. |
| `tools/rooms.py` | The room graph: the authored visibility, the streaming sections, the reverb preset and the music track per room. **823 rooms, 677 of them live.** |
| `tools/boot.py` | Starts a level the way the game does — `mdk2.lua:level(n, cp, sec)`, whose call graph BioWare left in a comment above it. **All ten levels start at all 129 checkpoints**, and the 2093 resources they demand all exist. |
| `tools/luaconst.py` | The engine's Lua surface, out of the binary: **507 constants with their values** and **461 registered functions with their addresses**. `OBJ_ROOM` is 803. |
| `tools/modcheck.py` | Holds the engine's `.mod` reader to `mod2obj.py`: eleven numbers per model, **2207/2207 agree**. |
| `tools/rand.py` | Runs the game's own random generator under emulation. It is **MT19937, the 1998 edition**, and the engine's matches it for 2000 numbers from the seed every level start uses. |
| `tools/acmcheck.py` | Holds the engine's Interplay ACM decoder to `ffmpeg -f acm`, the one reference here that nobody in this project wrote: **1019 streams, byte for byte**. |
| `tools/texcheck.sh` | Holds the engine's texture codec to `texdec.py`: one CRC32 per texture over every mip level, **761/761 identical**. |
| `tools/winbuild.sh` | Cross-builds the Windows binary, drops it into a real installation and runs it there under Wine. |
| `tools/refdec.py` | Reference decoder: runs the original's block codec under emulation. A research oracle, kept only to check `texdec.py` against. |
| `tools/exe_recon.py` | PE reconnaissance: toolchain, imports, RTTI, source paths. |
| `tools/inventory.py` | Catalogues an installation: sizes, SHA-1, entropy. |
| `tools/diffsets.py` | Diffs two editions — a free map of the localisable data. |

## What the engine turned out to be

The original was built with MSVC 6.0 SP5. Debug paths left in the binary
reveal a three-layer structure:

- `Chitin/` — BioWare's engine platform (resources, sound, video,
  **zipfile.c**), the same lineage as the Infinity Engine;
- `Omen/` — renderer and scene: `omTexture.c`, `omHModel.c`, `omPolyhedron.c`,
  `omCollision.c`, `omAnimate.c`, `omLight.c`, `omBump.c`;
- `Mdk2/` — game logic: `mdkKurt.c`, `mdkDoctor.c`, `mdkHyde.c`, `mdkAI.c`,
  `mdkPhysics.c`, `mdkSceneGraph.c`;
- `Lua/` — scripting; the shipped scripts are **plain text**, not bytecode.

Hence the name: the engine is called Omen, so ours is a good one.

### Formats

| | |
|---|---|
| `data/*.zip` | ZIP, compression method **10 (PKWARE DCL Implode)**. Neither `zipfile`, `unzip`, `7z` nor `bsdtar` can read it — the decompressor here is our own. |
| `.tex` | A mip chain at exactly **4 bits per pixel** down to 8×8, then the 4×4/2×2/1×1 levels as raw BGRA (the 84-byte tail). The 4 bpp coding is a **multi-mode block codec**: 8×4-pixel blocks in 16 bytes, each two independent 4×4 sub-blocks, with **six layouts** chosen by the top four bits. Fully decoded — see [`tools/texdec.py`](tools/texdec.py). |
| `.mod` | Node hierarchy with per-node bounding boxes, **triangle strips over consecutive vertices — there is no index list**, 32-byte vertices (position + UV), animation as (time, key) pairs interpolated between keys — slerp for rotations — and a 21-byte resource table naming the model's texture and sounds. |
| `.bsp` | Not geometry: a flat array of 24-byte BSP nodes (unit plane + two child indices), no header at all. A point is solid when the descent reaches a leaf through the *front* child — **with the point negated**, since the tree is authored in a mirrored frame. **692/692 trees validate.** |
| `.lua` | Plain text, and **Lua 3.x** — the `$if` / `$end` pragmas, and two lines of syntax 5.x cannot parse (`%upvalue`, `break` as a name). `base/l*.lua` are the level scene graphs: 5633 `mdkRegisterObject` calls placing every object in the game. |
| `.str` | 686 entries of `{id, text, sound}`. Text is **UTF-16LE**, so one file per language is enough; `sound` names the `.wav` that speaks the line. |
| `.sta` | 3141 records of `{ra, dec, magnitude}` in radians — an actual star catalogue, Sirius first. The southern declinations are off by up to a degree, in the shipped game. |
| `.wav` | Mostly a lie: 992 of 998 are `WAVCV1.0`, a 28-byte header over an **Interplay ACM** stream — the same codec the Infinity Engine used. Mono 16-bit at 22050 or 11025 Hz. The payload is a bit stream: 14 bytes of header, then blocks of `rows × 2^level` values in which each **column** is coded by one of 32 routines named by a 5-bit code, and the block is put through an inverse transform of halving strides. **Every stream the game ships is level 7, rows 16.** Decoded from scratch in [`engine/src/formats/acm.rs`](engine/src/formats/acm.rs). |
| `.acm`, `.mus` | The music: 27 bare Interplay ACM streams, stereo 22050 Hz, and the Infinity Engine's playlist format beside them — a name, a segment count, and where each segment loops back to. |
| `.omn` | A recorded demo: `{command, value}` pairs, 0xFFFFFFFF ending each frame and carrying its delta time. 30 fps, controller input rather than positions. |

The engine's Lua surface came out of the binary the same way: the **461
functions** and **507 constants** it registers are each `push value; push
name; push 0; call`, read out of the instruction stream rather than inferred.
`OBJ_ROOM` is 803. See [`docs/lua-constants.md`](docs/lua-constants.md).

The seven rules the project is held to are in [`docs/rules.md`](docs/rules.md)
— assets are never committed, no decompiled code goes into the engine, and a
format counts as solved only at 100%.

Every format above is described where the code that reads it lives — the
tool's module docstring, and the engine's matching module documentation.
Reconnaissance through Wine — file traces, apitrace on the GL stream — is in
[`docs/wine-recon.md`](docs/wine-recon.md).

## Milestones

- [x] **M0** All file types, imports, toolchain and RTTI status known
- [x] **M1** Unpacker extracts every resource from the containers
- [x] **M2** Every texture converts to PNG without artefacts, and the codec is our own
- [x] **M3** Kurt's model spins in a viewer, textured
- [x] **M4** Level geometry loads, free camera flies through it
- [x] **M5** Skeletal animation plays back, interpolated
- [x] **M6** The character runs around a level, collision works
- [x] **M7** Scripts, triggers, enemies — the shipped Lua runs, the scene graphs load
- [ ] **M8** The first level can be played end to end
- [ ] **M9** The engine itself — started, and it starts, runs and draws a level

## Checking it

```bash
python3 tools/check.py            # 45 checks, about a minute
python3 tools/check.py --quick    # the 7 that need no game files
python3 tools/check.py --slow     # plus the texture codec and the RNG,
                                  # both against the emulated original
```

Eighteen of those hold the **Rust engine against the Python that defines
it** — the same texture bytes, the same model numbers, the same collision
answers, the same scene graphs, the same boot — and two hold its audio
decoder against `ffmpeg`, which is not our code at all. Anything whose inputs
are missing is reported as skipped, never as passed.

## Running the tools

```bash
python3 -m venv .venv && . .venv/bin/activate
pip install pefile pillow            # or: pacman -S python-pefile python-pillow

export MDK2_GOG="/path/to/GOG Games/MDK 2"
python3 tools/unpack.py  "$MDK2_GOG/data/base.zip" -o extracted/base
python3 tools/tex2png.py extracted/base -o png/

# a walkable page per level: drag to look, WASD, G to walk, 1-9 for checkpoints
python3 tools/mod2html.py --scene extracted/base/l1.lua -o l1.html \
        --resources extracted --png png/ --walk
```

## Legal

No original code is copied and no assets are redistributed. The tools only
operate on files you already have. MDK2 is a trademark of its respective
owners; this project is not affiliated with them.

Licence: GPL-3.0-or-later.
