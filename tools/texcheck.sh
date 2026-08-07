#!/bin/sh
# Hold the engine's texture codec to the Python one that defined it.
#
# `tools/texdec.py` is the reference: it was written from scratch and checked
# block for block against the original routine under emulation. This diffs one
# CRC32 per texture -- over *every* mip level, not just the largest -- so a
# disagreement names the texture it is in rather than a count.
#
# Python decodes 4.2 million blocks in about a minute, which is why this is
# not in the fast set. Skips itself when the game or its extraction is
# missing. Needs MDK2_GOG and extracted/.
set -e
cd "$(dirname "$0")/.."

[ -d "$MDK2_GOG" ] || { echo "skip: MDK2_GOG is not set to an installation"; exit 0; }
[ -d extracted/base ] || { echo "skip: no extracted/ -- run tools/unpack.py"; exit 0; }

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

python3 tools/texdec.py extracted --digest > "$TMP/python" 2>"$TMP/python.log"
cargo run --quiet --release --manifest-path engine/Cargo.toml -- \
      "$MDK2_GOG" --tex > "$TMP/engine" 2>"$TMP/engine.log"

if ! diff -q "$TMP/python" "$TMP/engine" >/dev/null; then
  echo "the engine and texdec.py disagree:"
  diff "$TMP/python" "$TMP/engine" | head -20
  exit 1
fi
echo "$(wc -l < "$TMP/python") textures identical -- $(cat "$TMP/python.log"), $(cat "$TMP/engine.log")"
