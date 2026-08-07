# Reconnaissance through Wine

Wine is an instrumented implementation of Win32. Everything that would need a
proxy DLL on Windows is an environment variable here. That is the main reason
to run this project on Linux.

## Setting up a prefix

The game is 32-bit, so the prefix is too:

```bash
export MDK2_GOG="$HOME/wine/mdk2-gog"
WINEARCH=win32 WINEPREFIX="$MDK2_GOG" wineboot -i
```

One prefix per edition — otherwise the edition diffs are meaningless. On
CachyOS multilib is enabled by default, so nothing else is needed.

## 1. Which files the game opens, and in what order

The cheapest and most informative trace. It answers "what actually loads when
a level starts" before any format analysis.

```bash
WINEPREFIX="$MDK2_GOG" WINEDEBUG=+file wine MDK2.exe 2> trace-file.log
```

The order of opens is the dependency graph: container → index → resources.
Whatever opens first is almost always the table of contents.

Worth cross-checking against `+relay` (a full trace of Win32 calls), but that
is an order of magnitude larger — enable it only for a short scenario:

```bash
WINEDEBUG=+relay wine MDK2.exe 2> trace-relay.log   # tens of MB per second
```

Narrow it down through `HKCU\Software\Wine\Debug`, keys `RelayInclude` /
`RelayExclude`.

## 2. What actually reaches the GPU

Wine translates Direct3D 7 to OpenGL (wined3d), which means the GL stream can
be captured with `apitrace` and inspected — giving the **real vertex and index
buffers** of the game, with coordinates, UVs, normals and triangle order.

```bash
apitrace trace --api gl -- wine MDK2.exe
qapitrace MDK2.exe.trace     # GUI: frame by frame, with buffer contents
```

This is the strongest test available for M3–M4: whatever our `.mod` parser
produces must match what the original sent to GL. A mismatch immediately
localises the error — vertex layout, index order, or transform matrix.

Detail level of the D3D layer:

```bash
WINEDEBUG=+d3d,+d3d_draw,+d3d_shader,+ddraw wine MDK2.exe 2> trace-d3d.log
```

Alternative route if wined3d breaks something: dgVoodoo2 (D3D7 → D3D11) on top
of DXVK (D3D11 → Vulkan), captured with RenderDoc. Harder to set up, but it
gives proper frame captures.

Note that `mdk2Main.exe` imports no graphics API directly — the renderer is
reached through `IFC22.dll` or loaded dynamically — so a trace is currently
the fastest way to find out which API is actually in use.

## 3. Sound and input

```bash
WINEDEBUG=+dsound wine MDK2.exe 2> trace-snd.log     # buffer formats, rates
WINEDEBUG=+dinput wine MDK2.exe 2> trace-input.log
```

The sound buffer parameters give the audio resource format almost directly —
sample rate, bit depth, channels — without parsing a single header.

## 4. Debugger

```bash
WINEPREFIX="$MDK2_GOG" winedbg MDK2.exe
WINEPREFIX="$MDK2_GOG" winedbg --gdb MDK2.exe   # then it is ordinary gdb
```

The standard move: breakpoint on `ReadFile`, look at which buffer is filled
and with how many bytes, then follow where that buffer goes. That is how the
parser function for a format is found, and Ghidra analysis starts from there.

## 5. Memory dumps

When a format is compressed on disk but expanded in memory, it is easier to
take the unpacked representation:

```bash
# find the pid of the running game
pgrep -f MDK2.exe
# then winedbg, or read /proc/PID/mem using the maps from /proc/PID/maps
```

## Packages

```bash
sudo pacman -S wine winetricks apitrace renderdoc rizin rizin-cutter \
               python-pillow python-numpy python-pefile
yay -S ghidra imhex python-kaitaistruct
```

AUR names drift over time — if a package is missing, search by the tool name.
Ghidra needs a JDK, which Arch pulls in as a dependency.

## Order of work for M0

1. `+file` trace of startup and the first level load → the resource list.
2. `exe_recon.py` on the GOG executable → APIs, toolchain, RTTI, extensions.
3. `inventory.py` across all three installations.
4. `diffsets.py` GOG vs 1C → where the strings live.
5. Write the results up where the code will live: which formats exist, which one to
   take first.
