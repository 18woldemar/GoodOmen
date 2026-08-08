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

Animation, all of it validated across the corpus:

    1   animation table, 32 bytes: {u32 id; u32 -1; f32 length; f32 4.0;
        u32 0; u32 0; u32 first_channel; u32 channel_count}
    2   channels, 8 bytes: {u16 target; u16 0; u16 first_key; u16 key_count}
    3   targets, 4 bytes: {u8 kind; u8 ?; u8 ?; u8 node}
    4   keys, 8 bytes: {f32 time in [0,1]; u32 value}
    5   value pool, 16 bytes, meaning set by the channel's kind:
        kind 2 -> rotation, a unit quaternion (100.000% of 328782 keys)
        kind 1 -> translation, three floats (99.999% of 98456)
        kinds 32..36 -> scalars: sound volume, min and max distance

**There is no index list.** Each group in section 6 names a run of consecutive
vertices in section 7, and that run is a triangle strip. Sections 4 and 5 are
animation — (time, key) pairs and unit quaternions — not geometry.

Node record fields used here:

    +0x00 char[28]  name          +0x6c u16,u16  parent (0xFFFF at root), children
    +0x1c float[3]  bbox min      +0x70 u16,u16  first, count -> section 6
    +0x28 float[3]  bbox max      +0x78 u16,u16  count -> section 7
    +0x34 float[3]  translation from the parent
    +0x87 u8        index into the section 8 reference table; 0xFF = none

Vertices are in **node-local** space, and so is each node's bounding box: for
`kurt.mod` all 33 boxes equal the box of the node's own untransformed vertices
to within 1e-3. Without walking the hierarchy every part collapses onto the
origin and the model renders as a blob. Accumulating +0x34 down the parent
chain puts Kurt in a T-pose -- head at z=+0.215, thigh -0.265, shin -0.478,
foot -0.525, so **Z is up** -- and that T-pose is the bind pose, which is what
`posed()` returns. Rotations come from the animations, in `animate()`.

`animate()` is not the identity at t=0, and there is no reason it should be:
**animation 0 is an animation, not the bind pose.** Comparing the two looked
for a while like a bug worth 1.76 units; it is not one. Over 368 animated
models only 30 have `animate(anims[0], 0) == posed()`, and the biggest
divergences are cameras and movers -- `ML8x_camera.mod` differs by 1681 times
its own size, because its first animation begins wherever that shot begins.
Rendered side by side, `posed()` is a clean T-pose and `animate(anims[0], 0)`
is Kurt mid-stride.

Usage:
    python3 tools/mod2obj.py extracted/base/kurt.mod -o kurt.obj
    python3 tools/mod2obj.py extracted/base/kurt.mod --preview kurt.png
    python3 tools/mod2obj.py extracted/base --stats
"""

from __future__ import annotations

import argparse
import math
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

KIND_TRANSLATION = 1      # section-3 target kinds, see animations()
KIND_ROTATION = 2


class ModError(ValueError):
    pass


def _slerp(a, b, u: float):
    """Spherical interpolation between two unit quaternions, shortest arc.

    Falls back to a normalised lerp when the two are nearly parallel, where
    the sine goes to zero and the spherical form loses all its precision.
    """
    dot = sum(a[c] * b[c] for c in range(4))
    if dot < 0.0:                      # -q is the same rotation; take the
        b = tuple(-c for c in b)       # short way round
        dot = -dot
    if dot > 0.9995:
        out = tuple(a[c] + (b[c] - a[c]) * u for c in range(4))
    else:
        theta = math.acos(max(-1.0, min(1.0, dot)))
        s = math.sin(theta)
        wa, wb = math.sin((1 - u) * theta) / s, math.sin(u * theta) / s
        out = tuple(a[c] * wa + b[c] * wb for c in range(4))
    length = math.sqrt(sum(c * c for c in out)) or 1.0
    return tuple(c / length for c in out)


def selftest() -> None:
    """`_slerp`, which is the only arithmetic here that can be wrong quietly."""
    q0 = (1.0, 0.0, 0.0, 0.0)                       # identity
    q1 = (0.0, 1.0, 0.0, 0.0)                       # half turn about x
    assert _slerp(q0, q1, 0.0) == q0
    half = _slerp(q0, q1, 0.5)
    r = math.sqrt(0.5)
    assert max(abs(half[c] - (r, r, 0, 0)[c]) for c in range(4)) < 1e-9, half
    # -q is the same rotation, so the arc must not go the long way round
    other = _slerp(q0, tuple(-c for c in q1), 0.5)
    assert max(abs(abs(half[c]) - abs(other[c])) for c in range(4)) < 1e-9
    # near-parallel falls back to a normalised lerp, and stays unit
    near = _slerp(q0, (0.99999, 0.00447, 0.0, 0.0), 0.5)
    assert abs(math.sqrt(sum(c * c for c in near)) - 1.0) < 1e-12, near
    print("mod2obj.py: self-test passed")


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

        Only *animated* models store their vertices in node-local space and
        need the node translations summed down the parent chain. A static
        model -- one with no animation table, which is 1061 of the 2207 -- has
        its vertices in world space already, and adding the translation
        double-counts: `l3_maze.mod` comes out at 1.94x its true size, and its
        coordinates then no longer match the plane distances in `l3_maze.bsp`.
        """
        animated = self._sec(1) is not None
        world = (self.world_offsets() if animated
                 else [(0.0, 0.0, 0.0)] * len(self.nodes))
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

    def animations(self) -> list[dict]:
        o = self._sec(1)
        if o is None:
            return []
        oc, ot, ok = self._sec(2), self._sec(3), self._sec(4)
        out = []
        for i in range(self.counts[0]):
            rec = struct.unpack_from("<8I", self.data, o + i * 32)
            length = struct.unpack_from("<f", self.data, o + i * 32 + 8)[0]
            channels = []
            for j in range(rec[7]):
                target, _z, first, count = struct.unpack_from(
                    "<4H", self.data, oc + (rec[6] + j) * 8)
                if target >= self.counts[2]:
                    continue
                kind = self.data[ot + target * 4]
                node = self.data[ot + target * 4 + 3]
                keys = [struct.unpack_from("<fI", self.data, ok + (first + k) * 8)
                        for k in range(count) if first + k < self.counts[3]]
                channels.append({"kind": kind, "node": node, "keys": keys})
            out.append({"id": rec[0], "length": length, "channels": channels})
        return out

    def _value(self, index: int) -> tuple[float, float, float, float]:
        return struct.unpack_from("<4f", self.data, self._sec(5) + index * 16)

    def sample(self, channel: dict, t: float):
        """Interpolated sample at t in [0, 1]. Rotations slerp, the rest lerp.

        Interpolation is not optional here. Over 400 models the median channel
        carries **6.8 keys per second** -- one key every 150 ms, or better than
        four frames apart at the 30 fps the recorded demo runs at -- so
        stepping between keys is visibly coarse rather than merely imprecise.
        86673 of the 117144 channels do hold a single key and are constant.

        13 channels of the 117144, all in explosions, have times that go
        backwards; the scan below simply takes the first bracketing pair and
        does not care.
        """
        keys = [k for k in channel["keys"] if k[0] == k[0]]   # drop NaN times
        if not keys:
            keys = channel["keys"]
            return self._value(keys[0][1]) if keys else None
        if len(keys) == 1 or t <= keys[0][0]:
            return self._value(keys[0][1])
        if t >= keys[-1][0]:
            return self._value(keys[-1][1])
        for i in range(len(keys) - 1):
            t0, t1 = keys[i][0], keys[i + 1][0]
            if t0 <= t <= t1:
                break
        else:
            return self._value(keys[-1][1])
        a = self._value(keys[i][1])
        b = self._value(keys[i + 1][1])
        u = 0.0 if t1 == t0 else (t - t0) / (t1 - t0)
        if channel["kind"] == KIND_ROTATION:
            return _slerp(a, b, u)
        return tuple(a[c] + (b[c] - a[c]) * u for c in range(4))

    def node_world(self, anim: dict, t: float) -> list:
        """Each node's world transform at t in [0,1]. -> [(quat, offset)].

        Split out of `animate()` so a renderer can pose the model itself:
        MDK2 models are **rigid hierarchies**, one node per vertex and no
        skinning weights, so a vertex only needs its own node's quaternion
        and offset. That is two vec4 of uniform per node, which is what
        `tools/mod2html.py` ships to the browser instead of re-posing the
        geometry on the CPU every frame.
        """
        return self._node_world(anim, t)

    def _node_world(self, anim: dict, t: float) -> list:
        n = len(self.nodes)
        trans = [list(node["translation"]) for node in self.nodes]
        quat = [(1.0, 0.0, 0.0, 0.0)] * n   # (w, x, y, z)
        for ch in anim["channels"]:
            if ch["node"] >= n:
                continue
            v = self.sample(ch, t)
            if v is None:
                continue
            if ch["kind"] == KIND_TRANSLATION:
                trans[ch["node"]] = list(v[:3])
            elif ch["kind"] == KIND_ROTATION:
                quat[ch["node"]] = v

        def rot(q, p):
            # component order is (w, x, y, z): the rest pose stores
            # (1, -0, -0, -0) for an unrotated bone, and (0.7071, -0.7071, 0, 0)
            # for a quarter turn about X. Reading it as (x, y, z, w) gives no
            # identity quaternion anywhere in the file.
            w, x, y, z = q
            return (
                p[0]*(1-2*(y*y+z*z)) + p[1]*2*(x*y-z*w) + p[2]*2*(x*z+y*w),
                p[0]*2*(x*y+z*w) + p[1]*(1-2*(x*x+z*z)) + p[2]*2*(y*z-x*w),
                p[0]*2*(x*z-y*w) + p[1]*2*(y*z+x*w) + p[2]*(1-2*(x*x+y*y)))

        world: list = [None] * n

        def walk(i):
            if world[i] is None:
                p = self.nodes[i]["parent"]
                if p is None:
                    world[i] = (quat[i], tuple(trans[i]))
                else:
                    pq, pt = walk(p)
                    r = rot(pq, trans[i])
                    qw, qx, qy, qz = pq
                    w, x, y, z = quat[i]
                    world[i] = ((qw*w - qx*x - qy*y - qz*z,
                                 qw*x + qx*w + qy*z - qz*y,
                                 qw*y - qx*z + qy*w + qz*x,
                                 qw*z + qx*y - qy*x + qz*w),
                                tuple(pt[c] + r[c] for c in range(3)))
            return world[i]

        return [walk(i) for i in range(n)]

    def animate(self, anim: dict, t: float) -> tuple[list, list]:
        """-> (vertices, triangles) with the animation applied at t in [0,1]."""
        world = self._node_world(anim, t)

        def rot(q, p):
            w, x, y, z = q
            return (
                p[0]*(1-2*(y*y+z*z)) + p[1]*2*(x*y-z*w) + p[2]*2*(x*z+y*w),
                p[0]*2*(x*y+z*w) + p[1]*(1-2*(x*x+z*z)) + p[2]*2*(y*z-x*w),
                p[0]*2*(x*z-y*w) + p[1]*2*(y*z+x*w) + p[2]*(1-2*(x*x+y*y)))

        verts, tris = [], []
        for ni, node in enumerate(self.nodes):
            q, off = world[ni]
            tex = self.node_texture(node)
            for g in range(node["group_first"],
                           node["group_first"] + node["group_count"]):
                first, count = self.groups[g]
                base = len(verts)
                for k in range(count):
                    pos, uv = self.vertices[first + k][:2]
                    r = rot(q, pos)
                    verts.append((tuple(r[c] + off[c] for c in range(3)),
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
            texture: Path | None = None, frame=None) -> None:
    """Software render, purely to eyeball that the geometry works.

    With a texture it rasterises with a z-buffer and affine UV interpolation;
    without one it falls back to flat shading and the painter's algorithm.
    Texture rows are top-down in the PNG but bottom-up in the model, so v is
    flipped on sampling -- the same convention noted in tools/tex2png.py.
    """
    if texture is not None:
        return _preview_textured(m, path, texture, size, yaw, frame)
    import math
    from PIL import Image, ImageDraw

    posed, tris = (m.animate(*frame) if frame else m.posed())
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
                      size: int, yaw: float, frame=None) -> None:
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

    posed, tris = (m.animate(*frame) if frame else m.posed())
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
              frames: int = 24, size: int = 320, anim: int | None = None) -> None:
    """Write an animated GIF: a turntable, or an animation played in place."""
    import io
    import math
    from PIL import Image

    shots = []
    clip = m.animations()[anim] if anim is not None else None
    for i in range(frames):
        buf = io.BytesIO()
        yaw = 0.7 if clip else 2 * math.pi * i / frames
        preview(m, buf, size=size, yaw=yaw, texture=texture,
                frame=(clip, i / frames) if clip else None)
        buf.seek(0)
        shots.append(Image.open(buf).convert("P", palette=Image.ADAPTIVE))
    shots[0].save(path, save_all=True, append_images=shots[1:],
                  duration=80, loop=0, optimize=True)


# The **animation key channel**. A channel whose target kind is 23 carries no
# geometry at all: its values are key codes, and 0x478ad8 hands each one to
# 0x42bf80 as the animation passes it. What that does with the code is a
# four-way split, and it is the whole system:
#
#     code >= 100   create an object of that `OBJ_*` type at the node
#     30..99        ScreenFlash(code - 29)
#     20..29        Earthquake(code - 19)
#     1..19         fire OnCustomKey(gob, slot, code) on the object
#
# The first line is where an enemy's shot comes from. `hans.mod` animation 56
# carries **421** at t = 0.513 and 421 is `hansshot`; `hoser.mod` 56 carries
# **428**, `hosershot`. Nothing in the enemy's definition names a projectile
# because the *animation* does.
#
# The value is not the key entry's second word -- that is an index into the
# model's value pool -- but the **raw bits** of the first float of the entry it
# points at, which is how the engine reads it (`mov eax, [edx]`).
KEY_KIND = 23


def keys(args) -> int:
    """Every animation key in a directory of models, split by what it does."""
    import collections
    files = sorted(args.src.glob("*.mod")) if args.src.is_dir() else [args.src]
    codes: collections.Counter = collections.Counter()
    carriers = []
    for f in files:
        try:
            model = Model(f.read_bytes())
        except Exception:
            continue
        found = []
        for anim in model.animations():
            for ch in anim["channels"]:
                if ch["kind"] != KEY_KIND:
                    continue
                for when, index in ch["keys"]:
                    raw = struct.unpack("<I", struct.pack(
                        "<f", model._value(index)[0]))[0]
                    codes[raw] += 1
                    found.append((anim["id"], round(when, 3), raw))
        if found:
            carriers.append((f.name, found))

    def total(lo, hi):
        return sum(v for k, v in codes.items() if lo <= k < hi)

    for name, found in carriers[:12]:
        spawns = sorted({c for _, _, c in found if c >= 100})
        print(f"  {name:22s} {len(found):3d} keys"
              + (f"  spawns {spawns[:6]}" if spawns else ""))
    made = {k: v for k, v in codes.items() if k >= 100}
    print(f"{len(files)} models, {len(carriers)} carry key channels, "
          f"{sum(codes.values())} keys: {total(1, 20)} OnCustomKey, "
          f"{total(20, 30)} earthquakes, {total(30, 100)} screen flashes, "
          f"{sum(made.values())} objects of {len(made)} types",
          file=sys.stderr)
    if args.expect_keys is not None:
        return 0 if sum(codes.values()) == args.expect_keys else 1
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path, nargs="?")
    ap.add_argument("--selftest", action="store_true",
                    help="check the quaternion interpolation")
    ap.add_argument("-o", "--out", type=Path, help="write an OBJ here")
    ap.add_argument("--preview", type=Path, help="write a preview PNG here")
    ap.add_argument("--texture", type=Path,
                    help="a PNG, or a directory of PNGs named after the "
                         ".tex references, to map onto the preview")
    ap.add_argument("--turntable", type=Path,
                    help="write an animated GIF spinning around Z")
    ap.add_argument("--frames", type=int, default=24)
    ap.add_argument("--anim", type=int,
                    help="animation index; with --turntable, plays it in "
                         "place instead of spinning the model")
    ap.add_argument("--stats", action="store_true",
                    help="parse a whole directory and report")
    ap.add_argument("--keys", action="store_true",
                    help="the animation key channels: what the models fire, "
                         "shake and spawn as an animation plays")
    ap.add_argument("--expect-keys", type=int, metavar="N",
                    help="succeed only if exactly N keys are found")
    args = ap.parse_args(argv)
    if args.selftest:
        selftest()
        return 0
    if args.src is None:
        ap.error("a .mod file or a directory is required")

    if args.keys:
        return keys(args)

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
          f"{len(m.animations())} animations, textures {m.textures()}",
          file=sys.stderr)
    if args.out:
        write_obj(m, args.out)
    if args.preview:
        preview(m, args.preview, texture=args.texture)
    if args.turntable:
        turntable(m, args.turntable, texture=args.texture,
                  frames=args.frames, anim=args.anim)
    if not args.out and not args.preview and not args.turntable:
        for n in m.nodes[:40]:
            print(f"  {n['name']:<20} parent={n['parent']} "
                  f"groups {n['group_first']}+{n['group_count']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
