#!/bin/bash
# Build libpeek.so.  The config path is baked in because Wine does not pass a
# Unix environment through to the Windows process it starts.
#   tools/peek.sh [conf-path]
set -e
HERE=$(dirname "$(readlink -f "$0")")
CONF=${1:-$HERE/peek.conf}
gcc -shared -fPIC -O2 -DPEEK_CONF="\"$CONF\"" \
    -o "$HERE/libpeek.so" "$HERE/peek.c" -lpthread -lm
echo "built $HERE/libpeek.so reading $CONF"
