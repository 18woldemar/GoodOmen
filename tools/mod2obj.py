#!/usr/bin/env python3
"""
mod2obj.py -- read .mod models and export OBJ, or render a preview PNG.

The .mod format (Omen; the binary's debug paths name E:\\mdk2\\Omen\\omHModel.c).
Header, 224 bytes:

    0x00  u32      2002        resource type tag
    0x04  u32      81          constant, as 21 is for .tex
    0x08  u16[12]              counts, two per dword
    0x20  u32[13]              section offsets; 0xFFFFFFFF = section absent
    0x54..0xdf                 other fields, not offsets

Sections that matter for geometry:

    0   node table, 136-byte records, counts[5] of them
    6   strip groups, 32 bytes: {u32 first; u32 count; float bbox[6]}
    7   vertices, 32 bytes: {float pos[3]; float uv[2]; ...}
    8   resource references, 21 bytes: char name[16] + char ext[5]
        (".tex" for the skin, ".wav" for sounds the model triggers)

The texture of a node is named by the single byte at +0x87, which indexes the
whole reference table -- sounds included, not just the textures. `kurt.mod`
stores 10 and its refs[10] is "kurt.tex"; `max.mod` stores 12 for refs[12]
"Max2.tex"; `ml7z_castle.mod` uses all of 0..13 across its 370 nodes. 0xFF
means the node draws nothing.

**There is no index list.** Each group in section 6 names a run of consecutive
vertices in section 7, and that run is a triangle strip. Sections 4 and 5 are
animation — (time, key) pairs and unit quaternions — not geometry.

Node record fields used here:

    +0x00 char[28]  name          +0x6c u16,u16  parent (0xFFFF at root), children
    +0x1c float[3]  bbox min      +0x70 u16,u16  first, count -> section 6
    +0x28 float[3]  bbox max      +0x78 u16,u16  count -> section 7
    +0x34 float[3]  translation from the parent
    +0x87 u8        index into the section 8 reference table; 0xFF = none

Vertices are in **node-local** space. Without walking the hierarchy every part
collapses onto the origin and the model renders as a blob. Accumulating +0x34
down the parent chain puts Kurt in a recognisable T-pose: head at z=+0.215,
thigh -0.265, shin -0.478, foot -0.525, so **Z is up**. Bone *rotations* are
not applied yet -- limbs modelled along their local axis still lie flat.

Usage:
    python3 tools/mod2obj.py extracted/base/kurt.mod -o kurt.obj
    python3 tools/mod2obj.py extracted/base/kurt.mod --preview kurt.png
    python3 tools/mod2obj.py extracted/base --stats
"""

from __future__ import annotations

import argparse
import struct
import sys
from pathlib import Path

TYPE_MOD = 2002
HEADER_SIZE = 224
NODE_STRIDE = 136
GROUP_STRIDE = 32
VERTEX_STRIDE = 32
REF_NAME = 16
REF_STRIDE = 21   # char name[16] + char ext[5], e.g. "kurt" + ".tex"
ABSENT = 0xFFFFFFFF


class ModError(ValueError):
    pass


def _cstr(b: bytes) -> str:
    return b.split(b"\0")[0].decode("latin1")


class Model:
    def __init__(self, data: bytes) -> None:
        if len(data) < HEADER_SIZE:
            raise ModError("file shorter than the header")
        tag = struct.unpack_from("<I", data, 0)[0]
        if tag != TYPE_MOD:
            raise ModError(f"type tag {tag}, expected {TYPE_MOD}")
        self.data = data
        self.counts = struct.unpack_from("<12H", data, 8)
        self.offsets = struct.unpack_from("<13I", data, 0x20)

        self.nodes = [
            self._node(i) for i in range(self.counts[5])
        ] if self.offsets[0] != ABSENT else []
        self.groups = self._groups()
        self.vertices = self._vertices()
        self.refs = self._refs()

    def _sec(self, i: int) -> int | None:
        o = self.offsets[i]
        return None if o == ABSENT else o

    def _node(self, i: int) -> dict:
        o = self._sec(0) + i * NODE_STRIDE
        d = self.data
        parent, children = struct.unpack_from("<2H", d, o + 0x6c)
        gfirst, gcount = struct.unpack_from("<2H", d, o + 0x70)
        return {
            "name": _cstr(d[o:o + 28]),
            "bbox_min": struct.unpack_from("<3f", d, o + 0x1c),
            "bbox_max": struct.unpack_from("<3f", d, o + 0x28),
            "parent": None if parent == 0xFFFF else parent,
            "children": children,
            "group_first": gfirst,
            "group_count": gcount,
            "translation": struct.unpack_from("<3f", d, o + 0x34),
            "ref": d[o + 0x87],
        }

    def _groups(self) -> list[tuple[int, int]]:
        o = self._sec(6)
        if o is None:
            return []
        return [struct.unpack_from("<2I", self.data, o + i * GROUP_STRIDE)
                for i in range(self.counts[6])]

    def _vertices(self) -> list[tuple[tuple[float, float, float],
                                      tuple[float, float]]]:
        o = self._sec(7)
        if o is None:
            return []
        out = []
        for i in range(self.counts[7]):
            b = o + i * VERTEX_STRIDE
            out.append((struct.unpack_from("<3f", self.data, b),
                        struct.unpack_from("<2f", self.data, b + 12)))
        return out

    def _refs(self) -> list[str]:
        o = self._sec(8)
        if o is None:
            return []
        end = len(self.data)
        out = []
        for i in range(self.counts[8]):
            b = o + i * REF_STRIDE
            if b + REF_STRIDE > end:
                break
            chunk = self.data[b:b + REF_STRIDE]
            # the name field is fixed width and its tail is uninitialised
            # heap (0xCD, MSVC's debug filler), so stop at the first NUL
            out.append(_cstr(chunk[:REF_NAME]) + _cstr(chunk[REF_NAME:]))
        return out

    def world_offsets(self) -> list[tuple[float, float, float]]:
        """Node translation accumulated down the parent chain."""
        out: list[tuple[float, float, float] | None] = [None] * len(self.nodes)

        def walk(i: int):
            if out[i] is None:
                p = self.nodes[i]["parent"]
                base = (0.0, 0.0, 0.0) if p is None else walk(p)
                t = self.nodes[i]["translation"]
                out[i] = tuple(base[k] + t[k] for k in range(3))
            return out[i]

        for i in range(len(self.nodes)):
            walk(i)
        return out

    def node_texture(self, node: dict) -> str | None:
        i = node["ref"]
        if i == 0xFF or i >= len(self.refs):
            return None
        name = self.refs[i]
        return name if name.lower().endswith(".tex") else None

    def posed(self) -> tuple[list, list]:
        """-> (vertices in model space, triangles). Bind pose, no rotations.

        Each vertex is (position, uv, texture name or None).
        """
        world = self.world_offsets()
        verts, tris = [], []
        for ni, n in enumerate(self.nodes):
            w = world[ni]
            tex = self.node_texture(n)
            for g in range(n["group_first"],
                           n["group_first"] + n["group_count"]):
                first, count = self.groups[g]
                base = len(verts)
                for k in range(count):
                    pos, uv = self.vertices[first + k]
                    verts.append((tuple(pos[c] + w[c] for c in range(3)),
                                  uv, tex))
                for k in range(count - 2):
                    a, b, c = base + k, base + k + 1, base + k + 2
                    if k & 1:
                        b, c = c, b
                    tris.append((a, b, c))
        return verts, tris

    def triangles(self) -> list[tuple[int, int, int]]:
        """Each group is a triangle strip over consecutive vertices."""
        tris = []
        for first, count in self.groups:
            for k in range(count - 2):
                a, b, c = first + k, first + k + 1, first + k + 2
                if k & 1:
                    b, c = c, b          # keep the winding consistent
                tris.append((a, b, c))
        return tris

    def textures(self) -> list[str]:
        return [r for r in self.refs if r.lower().endswith(".tex")]


def write_obj(m: Model, path: Path) -> None:
    verts, tris = m.posed()
    lines = [f"# GoodOmen: {len(verts)} vertices, {len(tris)} triangles, "
             f"{len(m.nodes)} nodes"]
    for tex in m.textures():
        lines.append(f"# texture {tex}")
    for pos, _, _ in verts:
        lines.append("v %.6f %.6f %.6f" % pos)
    for _, uv, _ in verts:
        lines.append("vt %.6f %.6f" % uv)
    for a, b, c in tris:
        lines.append(f"f {a+1}/{a+1} {b+1}/{b+1} {c+1}/{c+1}")
    path.write_text("\n".join(lines) + "\n")


def preview(m: Model, path, size: int = 512, yaw: float = 0.6,
            texture: Path | None = None) -> None:
    """Software render, purely to eyeball that the geometry works.

    With a texture it rasterises with a z-buffer and affine UV interpolation;
    without one it falls back to flat shading and the painter's algorithm.
    Texture rows are top-down in the PNG but bottom-up in the model, so v is
    flipped on sampling -- the same convention noted in tools/tex2png.py.
    """
    if texture is not None:
        return _preview_textured(m, path, texture, size, yaw)
    import math
    from PIL import Image, ImageDraw

    posed, tris = m.posed()
    verts = [p for p, _, _ in posed]
    if not verts:
        raise ModError("model has no vertices")
    xs = [v[0] for v in verts]
    ys = [v[1] for v in verts]
    zs = [v[2] for v in verts]
    cx, cy, cz = ((min(a) + max(a)) / 2 for a in (xs, ys, zs))
    span = max(max(a) - min(a) for a in (xs, ys, zs)) or 1.0
    scale = size * 0.42 / span
    cos, sin = math.cos(yaw), math.sin(yaw)

    def project(v):
        # models are Z-up; yaw spins around Z, screen y grows downward
        x, y, z = v[0] - cx, v[1] - cy, v[2] - cz
        xr, yr = x * cos - y * sin, x * sin + y * cos
        return (size / 2 + xr * scale, size / 2 - z * scale, yr)

    img = Image.new("RGB", (size, size), (24, 26, 30))
    draw = ImageDraw.Draw(img)
    faces = []
    for a, b, c in tris:
        pa, pb, pc = project(verts[a]), project(verts[b]), project(verts[c])
        faces.append(((pa[2] + pb[2] + pc[2]) / 3, pa, pb, pc))
    faces.sort(key=lambda f: f[0])          # painter's algorithm, back to front
    for depth, pa, pb, pc in faces:
        ux, uy = pb[0] - pa[0], pb[1] - pa[1]
        vx, vy = pc[0] - pa[0], pc[1] - pa[1]
        area = ux * vy - uy * vx
        if abs(area) < 0.01:
            continue
        shade = 90 + int(120 * min(1.0, abs(area) ** 0.25 / 6))
        tone = (shade, int(shade * 0.93), int(shade * 0.82))
        draw.polygon([(pa[0], pa[1]), (pb[0], pb[1]), (pc[0], pc[1])],
                     fill=tone, outline=(40, 42, 48))
    img.save(path, format="PNG")


def _preview_textured(m: Model, path, texture: Path,
                      size: int, yaw: float) -> None:
    import math
    from PIL import Image

    # a single PNG, or a directory of them named after the .tex references
    cache: dict[str, tuple] = {}

    def sampler(name):
        if texture.is_dir():
            if name is None:
                return None
            key = Path(name).stem
            if key not in cache:
                f = texture / (key + ".png")
                if not f.is_file():
                    cache[key] = None
                else:
                    im = Image.open(f).convert("RGB")
                    cache[key] = (im.load(), *im.size)
            return cache[key]
        if "" not in cache:
            im = Image.open(texture).convert("RGB")
            cache[""] = (im.load(), *im.size)
        return cache[""]

    posed, tris = m.posed()
    verts = [p for p, _, _ in posed]
    uvs = [uv for _, uv, _ in posed]
    vtex = [tn for _, _, tn in posed]
    xs = [v[0] for v in verts]
    ys = [v[1] for v in verts]
    zs = [v[2] for v in verts]
    cx, cy, cz = ((min(a) + max(a)) / 2 for a in (xs, ys, zs))
    span = max(max(a) - min(a) for a in (xs, ys, zs)) or 1.0
    scale = size * 0.44 / span
    cos, sin = math.cos(yaw), math.sin(yaw)

    proj = []
    for x, y, z in verts:
        x, y, z = x - cx, y - cy, z - cz
        xr, yr = x * cos - y * sin, x * sin + y * cos
        proj.append((size / 2 + xr * scale, size / 2 - z * scale, yr))

    img = Image.new("RGB", (size, size), (24, 26, 30))
    out = img.load()
    zbuf = [1e30] * (size * size)

    for a, b, c in tris:
        pa, pb, pc = proj[a], proj[b], proj[c]
        area = ((pb[0] - pa[0]) * (pc[1] - pa[1])
                - (pb[1] - pa[1]) * (pc[0] - pa[0]))
        if abs(area) < 1e-6:
            continue
        x0 = max(0, int(min(pa[0], pb[0], pc[0])))
        x1 = min(size - 1, int(max(pa[0], pb[0], pc[0])) + 1)
        y0 = max(0, int(min(pa[1], pb[1], pc[1])))
        y1 = min(size - 1, int(max(pa[1], pb[1], pc[1])) + 1)
        sam = sampler(vtex[a])
        if sam is None:
            continue
        tpix, tw, th = sam
        ua, ub, uc = uvs[a], uvs[b], uvs[c]
        for py in range(y0, y1 + 1):
            for px in range(x0, x1 + 1):
                w0 = ((pb[0] - pa[0]) * (py + .5 - pa[1])
                      - (pb[1] - pa[1]) * (px + .5 - pa[0])) / area
                w1 = ((pc[0] - pb[0]) * (py + .5 - pb[1])
                      - (pc[1] - pb[1]) * (px + .5 - pb[0])) / area
                w2 = 1.0 - w0 - w1
                if w0 < 0 or w1 < 0 or w2 < 0:
                    continue
                depth = w1 * pa[2] + w2 * pb[2] + w0 * pc[2]
                idx = py * size + px
                if depth >= zbuf[idx]:
                    continue
                zbuf[idx] = depth
                u = w1 * ua[0] + w2 * ub[0] + w0 * uc[0]
                v = w1 * ua[1] + w2 * ub[1] + w0 * uc[1]
                sx = int(u * tw) % tw
                sy = int((1.0 - v) * th) % th
                out[px, py] = tpix[sx, sy]
    img.save(path, format="PNG")


def turntable(m: Model, path: Path, texture: Path | None = None,
              frames: int = 24, size: int = 320) -> None:
    """Spin the model around Z and write an animated GIF."""
    import io
    import math
    from PIL import Image

    shots = []
    for i in range(frames):
        buf = io.BytesIO()
        preview(m, buf, size=size, yaw=2 * math.pi * i / frames,
                texture=texture)
        buf.seek(0)
        shots.append(Image.open(buf).convert("P", palette=Image.ADAPTIVE))
    shots[0].save(path, save_all=True, append_images=shots[1:],
                  duration=80, loop=0, optimize=True)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path)
    ap.add_argument("-o", "--out", type=Path, help="write an OBJ here")
    ap.add_argument("--preview", type=Path, help="write a preview PNG here")
    ap.add_argument("--texture", type=Path,
                    help="a PNG, or a directory of PNGs named after the "
                         ".tex references, to map onto the preview")
    ap.add_argument("--turntable", type=Path,
                    help="write an animated GIF spinning around Z")
    ap.add_argument("--frames", type=int, default=24)
    ap.add_argument("--stats", action="store_true",
                    help="parse a whole directory and report")
    args = ap.parse_args(argv)

    if args.stats:
        files = sorted(args.src.glob("*.mod"))
        ok = tris = 0
        bad = []
        for f in files:
            try:
                m = Model(f.read_bytes())
                tris += len(m.triangles())
                ok += 1
            except (ModError, struct.error) as e:
                bad.append((f.name, str(e)))
        for n, e in bad[:10]:
            print(f"ERROR {n}: {e}", file=sys.stderr)
        print(f"{ok}/{len(files)} models parsed, {tris} triangles total",
              file=sys.stderr)
        return 1 if bad else 0

    m = Model(args.src.read_bytes())
    print(f"{args.src.name}: {len(m.nodes)} nodes, {len(m.vertices)} vertices, "
          f"{len(m.groups)} strips, {len(m.triangles())} triangles, "
          f"textures {m.textures()}", file=sys.stderr)
    if args.out:
        write_obj(m, args.out)
    if args.preview:
        preview(m, args.preview, texture=args.texture)
    if args.turntable:
        turntable(m, args.turntable, texture=args.texture,
                  frames=args.frames)
    if not args.out and not args.preview:
        for n in m.nodes[:40]:
            print(f"  {n['name']:<20} parent={n['parent']} "
                  f"groups {n['group_first']}+{n['group_count']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
