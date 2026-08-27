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

## 5. Reading the running game's own memory

The strongest measurement available, and the only one that reports game
state rather than its shadow. `tools/peek.c` does it; what follows is why
it is shaped the way it is, because each point below cost a run to learn.

**Not from outside.** Wine reparents every process it starts to init, so
with `kernel.yama.ptrace_scope=1` -- the default on this machine and on
most distributions -- nothing outside is an ancestor of the game, and both
`process_vm_readv` and `/proc/PID/mem` fail with `EPERM`. Launching the
game from the reader does not help: the reparenting happens anyway.
Loosening the sysctl would work and weakens the whole system to read one
process; do not.

**From inside, by `LD_PRELOAD`.** The library is loaded into every Wine
process including the game -- confirmed by `grep -c libpeek.so
/proc/PID/maps` on the process whose maps contain `mdk2Main.exe`. There is
no permission question about reading yourself.

**Configured by a file, never the environment.** Wine rebuilds the
environment for the Windows process it starts. Exported variables reach
the helper processes and never the game, which looks exactly like the
preload having failed. The config path is baked in at build time.

**Read through `/proc/self/mem`, never by dereferencing.** A writable
mapping in a Wine process is not necessarily a page you may touch:
wineserver's shared mappings and the reserved low ranges are in the list
too. A direct read kills the game, which also looks exactly like the
preload having failed. `pread` returns an error instead of a signal.

**Scan only the 32-bit window.** A 32-bit program keeps everything below
4 GB. Without the window the first match was a pair of floats up at
`0x7f4f...` in the 64-bit host's own heap, which never moved and was never
the player.

**Scan above the image, not through it.** The static data holds the
checkpoint table, which contains the spawn position permanently, so a scan
that includes it locks onto it before the level has even loaded. Setting
`lo=0x1000000` leaves the heap.

**No hook on the frame.** The Linux capture tools all hook
`glXSwapBuffers`/`eglSwapBuffers` to sample once a frame, and that would be
the right thing here too. It does not work through Wine: Wine reaches EGL
by `dlsym` on its own `libEGL` handle, which plain symbol interposition
does not intercept. Catching it means wrapping `dlsym` as well. A thread
polling far faster than the frame rate answers the same questions, as long
as the log is indexed by position update rather than by wall clock -- under
software EGL the game runs well below its own 29.94 Hz.

**The X authority file is named per login session.** A remembered name
gives a run with no window, no rendering and no error worth reading.
Discover it: `ls -t /run/user/$(id -u)/xauth_* | head -1`.

### What it found

```bash
tools/peek.sh /path/to/peek.conf      # builds tools/libpeek.so
# peek.conf: out=/tmp/x  find=271,-69  lo=0x1000000  hz=200  every=0.3
LD_PRELOAD=$PWD/tools/libpeek.so wine mdk2Main.exe
tools/peek.py /tmp/x.<pid> --path orig.txt
```

* `mdk2Main.exe` maps at **0x400000** in every run. It was linked by MSVC 6,
  which predates `/DYNAMICBASE`, and Wine randomises neither the exe nor
  the heap nor the stack.
* **0x4bbb44** is a table of checkpoints: `x, y, z, heading` as four floats
  in a 36-byte record, heading in radians. Level 1 checkpoint 5 reads
  `271, -69, -163, 3.1416`.
* The player's transform is a heap object -- `0x3d23fec` in one run --
  holding position followed by a unit quaternion. Its address is stable
  between identical runs but moves when the run differs, so it is found
  fresh each time rather than written down.
* Kurt's walking step is **0.4997 a tick**, which is 15.0 units a second at
  29.94 -- the speed table, read off the running game.

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
