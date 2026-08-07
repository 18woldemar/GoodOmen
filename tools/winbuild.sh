#!/bin/sh
# Cross-build the engine for Windows and run the result under Wine against a
# real installation. "It builds" is not "it works", and this is the whole
# difference: the binary is dropped into the game directory and started with
# no arguments, which is the way it is meant to be used.
#
# Skips itself when the linker or Wine is missing. Needs MDK2_GOG.
set -e
cd "$(dirname "$0")/.."

command -v x86_64-w64-mingw32-gcc >/dev/null || {
  echo "skip: no x86_64-w64-mingw32-gcc (pacman -S mingw-w64-gcc)"; exit 0; }
WINE=$(command -v wine || ls "$HOME"/Downloads/wine-*/bin/wine 2>/dev/null | head -1)
[ -n "$WINE" ] || { echo "skip: no wine"; exit 0; }
[ -d "$MDK2_GOG" ] || { echo "skip: MDK2_GOG is not set to an installation"; exit 0; }

cargo build --release --quiet --target x86_64-pc-windows-gnu \
      --manifest-path engine/Cargo.toml
EXE=engine/target/x86_64-pc-windows-gnu/release/goodomen.exe

# into the game directory and out again: the binary has to find the game
# beside itself, which is the one thing running it from elsewhere cannot test
cp "$EXE" "$MDK2_GOG/goodomen-check.exe"
trap 'rm -f "$MDK2_GOG/goodomen-check.exe"' EXIT
OUT=$(cd "$MDK2_GOG" && WINEDEBUG=-all "$WINE" goodomen-check.exe 2>/dev/null)

echo "$OUT" | grep -q "4751/4751 files read" || {
  echo "the Windows build did not read the containers:"; echo "$OUT"; exit 1; }

# and the renderer, which on Windows is a statically linked SDL2 rather than
# the system one -- a different build of a different library, so it is worth
# asking the same question of it
TRI=$(cd "$MDK2_GOG" && WINEDEBUG=-all "$WINE" goodomen-check.exe --triangle 2>/dev/null)
case "$TRI" in
  *"triangle drawn offscreen"*|skip:*) ;;
  *) echo "the Windows build did not draw the triangle:"; echo "$TRI"; exit 1 ;;
esac

# and a whole level, which is the renderer, the Lua and every format at once
LVL=$(cd "$MDK2_GOG" && WINEDEBUG=-all "$WINE" goodomen-check.exe --level l1.lua 2>/dev/null)
case "$LVL" in
  *"of the frame in"*|skip:*) ;;
  *) echo "the Windows build did not draw a level:"; echo "$LVL"; exit 1 ;;
esac

echo "$OUT" | tail -1
echo "$TRI" | tail -1
echo "$LVL" | tail -1
