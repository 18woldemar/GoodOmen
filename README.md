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
| `tools/tex2png.py` | Converts `.tex` textures to PNG. **761/761.** |
| `tools/mod2obj.py` | Reads `.mod` models. **2207/2207, 857321 triangles.** Exports OBJ, renders textured previews and turntables. |
| `tools/mod2html.py` | Packs a model — or, with `--scene`, a whole level and everything standing in it — into a single self-contained WebGL viewer. Geometry and textures inlined, no libraries, no server. |
| `tools/bsp.py` | Reads and validates the `.bsp` collision trees; point-in-solid and drop queries. **692/692 validate.** |
| `tools/scene.py` | Reads the level scene graphs — every object's type, position, parent and resource. **54 files, 5633 objects, 0 complaints.** |
| `tools/refdec.py` | Reference decoder: runs the original's block codec under emulation. A research oracle, not engine code. |
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
| `.tex` | A mip chain at exactly **4 bits per pixel** down to 8×8, then the 4×4/2×2/1×1 levels as raw BGRA (the 84-byte tail). The 4 bpp coding is a **multi-mode block codec**: 8×4-pixel blocks in 16 bytes, with the top three bits of the block's fourth dword selecting one of four decoders. |
| `.mod` | Node hierarchy with per-node bounding boxes, **triangle strips over consecutive vertices — there is no index list**, 32-byte vertices (position + UV), animation as (time, key) pairs at 18 fps, and a 21-byte resource table naming the model's texture and sounds. |
| `.bsp` | Not geometry: a flat array of 24-byte BSP nodes (unit plane + two child indices), no header at all. A point is solid when the descent reaches a leaf through the *front* child — **with the point negated**, since the tree is authored in a mirrored frame. **692/692 trees validate.** |
| `.lua` | Plain text, and **Lua 3.x** — the scripts use the `$if` / `$end` pragmas that only Lua 3 had. `base/l*.lua` are the level scene graphs: 5633 `mdkRegisterObject` calls placing every object in the game. |

Details, and the hypotheses that turned out to be wrong, live in
[`docs/journal.md`](docs/journal.md). Reconnaissance through Wine — file
traces, apitrace on the GL stream — is in
[`docs/wine-recon.md`](docs/wine-recon.md).

## Milestones

- [x] **M0** All file types, imports, toolchain and RTTI status known
- [x] **M1** Unpacker extracts every resource from the containers
- [x] **M2** Every texture converts to PNG without artefacts
- [x] **M3** Kurt's model spins in a viewer, textured
- [x] **M4** Level geometry loads, free camera flies through it
- [x] **M5** Skeletal animation plays back
- [ ] **M6** The character runs around a level, collision works
- [ ] **M7** Scripts, triggers, enemies
- [ ] **M8** The first level can be played end to end

## Running the tools

```bash
python3 -m venv .venv && . .venv/bin/activate
pip install pefile pillow            # or: pacman -S python-pefile python-pillow

export MDK2_GOG="/path/to/GOG Games/MDK 2"
python3 tools/unpack.py  "$MDK2_GOG/data/base.zip" -o extracted/base
python3 tools/tex2png.py extracted/base -o png/
```

## Legal

No original code is copied and no assets are redistributed. The tools only
operate on files you already have. MDK2 is a trademark of its respective
owners; this project is not affiliated with them.

Licence: GPL-3.0-or-later.
