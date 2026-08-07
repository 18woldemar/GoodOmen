# GoodOmen

An open reimplementation of the **MDK2** engine (BioWare, 2000), running on
the original assets you own. This is not a decompilation: *behaviour and data
formats* are recovered from the original, and the engine is written from
scratch. The model is OpenMW and devilutionX.

> **Game assets are not distributed here and never will be.**
> Running anything requires a legal copy of MDK2 (GOG, disc, or the 1C release).

## What works today

| | |
|---|---|
| `tools/unpack.py` | Unpacks the `data/*.zip` containers. **4751/4751 files, every CRC32 matches.** |
| `tools/tex2png.py` | Converts `.tex` textures to PNG. **761/761**, with no emulator and no game executable. |
| `tools/texdec.py` | The `.tex` block codec, reimplemented from scratch. **Byte-identical to the original on all 4205514 blocks.** |
| `tools/mod2obj.py` | Reads `.mod` models. **2207/2207, 857321 triangles.** Exports OBJ, renders textured previews and turntables, and plays animation with slerped rotations. |
| `tools/mod2html.py` | Packs a model — or, with `--scene`, a whole level and everything standing in it — into a single self-contained WebGL viewer. With `--walk` the collision trees come too and you can walk the level instead of flying it. No libraries, no server. |
| `tools/bsp.py` | Reads and validates the `.bsp` collision trees; point-in-solid and drop queries. **692/692 validate.** |
| `tools/scene.py` | Reads the level scene graphs — every object's type, position, parent and resource. **54 files, 5633 objects, 0 complaints.** |
| `tools/wavc.py` | The sounds and the music. **992 of the 998 `.wav` files are not RIFF** — they are `WAVCV1.0` over Interplay ACM, the Baldur's Gate codec — and `Music/` is 27 bare ACM streams, 2h 46m of it, with Infinity Engine `.mus` playlists. |
| `tools/walksim.py` | The viewer's controller, in Python, over every level. **2557 spawn points: 2556 stay standing, 2 ever inside geometry.** |
| `tools/spawn.py` | The checkpoints a level starts you at, pulled by running its script, and whether each one stands in open space. **127 checkpoints, 126 clear.** |
| `tools/luarun.py` | Runs the shipped Lua on a stock `lua5.1`. **All 31 scripts run to the end**, and the scene graphs they register match `scene.py` exactly — 5633 objects, no disagreement. |
| `tools/luaapi.py` | Catalogues the engine functions the shipped Lua calls — **438 of them**, which is the specification the engine has to meet. See [`docs/lua-api.md`](docs/lua-api.md). |
| `tools/strfile.py` | Reads `.str` — every line of text and the `.wav` that speaks it. **686 entries, 348 voiced, byte-exact**, and the same for all five shipped languages. |
| `tools/stars.py` | Reads `.sta`, the sky. **3141 real stars**, and the shipped southern declinations are wrong; `--fix` corrects them. |
| `tools/omn.py` | Reads `.omn`, the recorded attract-mode demo: 1348 frames of controller input at 30 fps. |
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
| `.wav` | Mostly a lie: 992 of 998 are `WAVCV1.0`, a 28-byte header over an **Interplay ACM** stream — the same codec the Infinity Engine used. Mono 16-bit at 22050 or 11025 Hz. `ffmpeg` decodes the payload. |
| `.acm`, `.mus` | The music: 27 bare Interplay ACM streams, stereo 22050 Hz, and the Infinity Engine's playlist format beside them — a name, a segment count, and where each segment loops back to. |
| `.omn` | A recorded demo: `{command, value}` pairs, 0xFFFFFFFF ending each frame and carrying its delta time. 30 fps, controller input rather than positions. |

Details, and the hypotheses that turned out to be wrong, live in
[`docs/journal.md`](docs/journal.md). Reconnaissance through Wine — file
traces, apitrace on the GL stream — is in
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

## Checking it

```bash
python3 tools/check.py            # 18 checks, about 10 seconds
python3 tools/check.py --quick    # the 6 that need no game files
python3 tools/check.py --slow     # plus the texture codec, 4205514 blocks
```

Anything whose inputs are missing is reported as skipped, never as passed.

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
