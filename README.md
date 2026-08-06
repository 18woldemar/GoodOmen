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
| `tools/tex2png.py` | Parses the `.tex` container, 761/761. Converts the uncompressed textures; **the 4 bpp level codec is not decoded yet.** |
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
| `.tex` | Container solved: a mip chain at exactly **4 bits per pixel** down to 8×8, then the 4×4/2×2/1×1 levels as raw RGBA (the 84-byte tail). Channel order is RGBA and the levels are plain box-filtered mips. The 4 bpp codec itself is **unsolved** — DXT1 and two other models are refuted in the journal. Fonts are uncompressed and convert fine. |
| `.mod` | Models (2207 files), type tag 2002. Not yet decoded. |
| `.bsp` | Level geometry (692 files), no signature. Not yet decoded. |
| `.lua` | Plain text. |

Details, and the hypotheses that turned out to be wrong, live in
[`docs/journal.md`](docs/journal.md). Reconnaissance through Wine — file
traces, apitrace on the GL stream — is in
[`docs/wine-recon.md`](docs/wine-recon.md).

## Milestones

- [x] **M0** All file types, imports, toolchain and RTTI status known
- [x] **M1** Unpacker extracts every resource from the containers
- [ ] **M2** Every texture converts to PNG without artefacts — *container done, codec open*
- [ ] **M3** Kurt's model spins in a viewer, textured
- [ ] **M4** Level geometry loads, free camera flies through it
- [ ] **M5** Skeletal animation plays back
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
