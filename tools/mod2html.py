#!/usr/bin/env python3
"""
mod2html.py -- pack a .mod model into a single self-contained WebGL viewer.

Writes one HTML file with the geometry and every texture embedded as data
URIs, and a hand-written WebGL renderer -- no libraries, no CDN, no server.
It opens straight from the filesystem, which matters here: assets never leave
the machine, and there is nothing to install.

Camera: drag to look, WASD to fly, R/F for up and down, shift to move faster.

Geometry comes from tools/mod2obj.py, so the same rules apply -- triangle
strips over consecutive vertices, node-local positions summed down the parent
chain, one texture per node named by the byte at +0x87.

With `--scene` it packs a whole level instead of one model: every object the
scene graph places, geometry merged into one buffer. See tools/scene.py for
the graph itself, and `place()` below for how an object's model is put where
the object stands.

With `--walk` as well it embeds the level's `.bsp` collision trees and the
camera can stop being a camera: **G** switches between flying and walking,
space jumps. The controller is in the page, roughly forty lines of it, and
what it does and does not do is written down there.

Usage:
    python3 tools/mod2html.py extracted/base/ml7z_castle.mod -o castle.html
    python3 tools/mod2html.py extracted/base/kurt.mod -o kurt.html --png png/
    python3 tools/mod2html.py --scene extracted/base/l1.lua -o l1.html \\
            --resources extracted --png png/
"""

from __future__ import annotations

import argparse
import base64
import json
import re
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import scene as sg  # noqa: E402
from mod2obj import Model  # noqa: E402

PAGE = """<!doctype html><meta charset=utf-8><title>__TITLE__</title>
<style>
html,body{margin:0;height:100%;background:#0d0f12;overflow:hidden}
canvas{display:block;width:100%;height:100%;cursor:grab}
canvas:active{cursor:grabbing}
#hud{position:fixed;left:12px;top:12px;color:#9aa;font:12px/1.5 system-ui,sans-serif;
     background:#0d0f12cc;padding:8px 11px;border-radius:7px;pointer-events:none}
b{color:#dde}
</style>
<canvas id=c></canvas>
<div id=hud><b>__TITLE__</b><br>__STATS__<br>drag to look &middot; WASD &middot;
R/F up-down &middot; shift faster<span id=walkhelp></span><br>
mode: <b id=mode>flying</b><span id=roomline></span></div>
<script>
const MESH = __MESH__, TEX = __TEX__;
const gl = document.getElementById('c').getContext('webgl');
const sh = (t, s) => { const o = gl.createShader(t); gl.shaderSource(o, s);
  gl.compileShader(o);
  if (!gl.getShaderParameter(o, gl.COMPILE_STATUS)) throw gl.getShaderInfoLog(o);
  return o; };
const prog = gl.createProgram();
gl.attachShader(prog, sh(gl.VERTEX_SHADER, `
  attribute vec3 aPos; attribute vec2 aUV;
  uniform mat4 uMVP; varying vec2 vUV; varying float vZ;
  void main(){ gl_Position = uMVP * vec4(aPos,1.0); vUV = aUV; vZ = gl_Position.w; }`));
gl.attachShader(prog, sh(gl.FRAGMENT_SHADER, `
  precision mediump float; uniform sampler2D uTex; uniform float uCut;
  varying vec2 vUV; varying float vZ;
  void main(){
    vec4 c = texture2D(uTex, vec2(vUV.x, 1.0 - vUV.y));
    if (c.a < uCut) discard;
    float fog = clamp(vZ / 4000.0, 0.0, 0.75);
    gl_FragColor = vec4(mix(c.rgb, vec3(0.05,0.06,0.07), fog), c.a);
  }`));
gl.linkProgram(prog); gl.useProgram(prog);
const uCut = gl.getUniformLocation(prog, 'uCut');

const bytes = s => Uint8Array.from(atob(s), c => c.charCodeAt(0));
const verts = new Float32Array(bytes(MESH.data).buffer);
const buf = gl.createBuffer();
gl.bindBuffer(gl.ARRAY_BUFFER, buf);
gl.bufferData(gl.ARRAY_BUFFER, verts, gl.STATIC_DRAW);
const aPos = gl.getAttribLocation(prog, 'aPos'), aUV = gl.getAttribLocation(prog, 'aUV');
gl.enableVertexAttribArray(aPos); gl.vertexAttribPointer(aPos, 3, gl.FLOAT, false, 20, 0);
gl.enableVertexAttribArray(aUV);  gl.vertexAttribPointer(aUV, 2, gl.FLOAT, false, 20, 12);

const blank = gl.createTexture();
gl.bindTexture(gl.TEXTURE_2D, blank);
gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, 1, 1, 0, gl.RGBA, gl.UNSIGNED_BYTE,
              new Uint8Array([160,160,170,255]));
const textures = {};
for (const [name, uri] of Object.entries(TEX)) {
  const t = gl.createTexture(); textures[name] = t;
  const img = new Image();
  img.onload = () => {
    gl.bindTexture(gl.TEXTURE_2D, t);
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, false);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, img);
    gl.generateMipmap(gl.TEXTURE_2D);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR_MIPMAP_LINEAR);
  };
  img.src = uri;
}

// --- collision, when the page was built with --walk ---------------------
// COLL.data is every tree's nodes back to back, 24 bytes each:
// {float normal[3]; float dist; u32 front; u32 back}, 0xFFFFFFFF for a leaf.
// COLL.trees[i] = {first, count, box} -- box is the world AABB, only there
// to skip trees the point is nowhere near.
const COLL = __COLL__;
// the level's own restart points, from Level.scenegraph.checkpoints
const SPAWNS = __SPAWNS__;

// --- the level's own visibility, when the page was built with a room graph
// ROOMS[name] = {box: [6], vis: [room names]} -- straight out of
// Level.scenegraph, see tools/rooms.py. Standing in a room, the engine draws
// that room and the rooms it lists, and nothing else; level 1 room 1 sees
// five of the level's fifty-nine. Geometry in no room at all is always
// drawn, which is what the engine does with a gob it never put in one.
const ROOMS = __ROOMS__;
let culling = !!ROOMS, shown = null, hereName = '';
function visibleRooms(p){
  if (!ROOMS) return null;
  let here = [];
  for (const name in ROOMS){
    const b = ROOMS[name].box;
    if (b && p[0] >= b[0] && p[1] >= b[1] && p[2] >= b[2] &&
             p[0] <= b[3] && p[1] <= b[4] && p[2] <= b[5]) here.push(name);
  }
  hereName = here.join(' + ');
  if (!here.length) return null;          // outside every room: draw it all
  const set = new Set(['']);              // '' is the geometry in no room
  for (const name of here)
    for (const v of ROOMS[name].vis) set.add(v);
  return set;
}
let CF = null, CU = null;
if (COLL) {
  const buf = bytes(COLL.data).buffer;
  CF = new Float32Array(buf); CU = new Uint32Array(buf);
}
// The tree is authored in a mirrored frame, so the query point is negated,
// and a point is inside solid geometry when the descent reaches a leaf
// through the *front* child. Same rule as tools/bsp.py.
function solid(x, y, z){
  if (!COLL) return false;
  const X = -x, Y = -y, Z = -z;
  for (const t of COLL.trees){
    const b = t.box;
    if (x < b[0] || y < b[1] || z < b[2] || x > b[3] || y > b[4] || z > b[5])
      continue;
    let i = t.first;
    for(;;){
      const o = i * 6;
      const side = CF[o]*X + CF[o+1]*Y + CF[o+2]*Z - CF[o+3];
      const c = side >= 0 ? CU[o+4] : CU[o+5];
      if (c === 0xFFFFFFFF){ if (side >= 0) return true; break; }
      i = t.first + c;
    }
  }
  return false;
}
// The body is a vertical segment, not a capsule -- the cheapest thing that
// cannot walk through a wall or stand inside a floor. Only the part *above*
// step height blocks movement: anything lower is a kerb to walk up, and
// including the feet made a standing body think it was obstructed and step
// two units into the air every frame.
// Sized from the game rather than guessed. Kurt's model is 1.86 units from
// sole to scalp and Max's 1.72, so a unit is about a metre and the eye sits
// at 1.7. The level agrees: over the 127 checkpoints the game spawns players
// at, the smallest headroom is 2.9 units, and a 4-unit body -- what this was
// before anyone measured -- did not fit five of them.
const EYE = 1.7, STEP = 0.6, GRAVITY = 20.0, JUMP = 7.0;
const WALK = 4.0, SPRINT = 9.0;               // units a second
function blocked(x, y, z){
  for (let h = STEP; h <= EYE; h += (EYE - STEP) / 2)
    if (solid(x, y, z - EYE + h)) return true;
  return false;
}
const footed = (x, y, z) => solid(x, y, z - EYE + 0.05);

let yaw = 0, pitch = 0, pos = MESH.eye.slice(), speed = MESH.span / 12;
let walking = false, vz = 0, onGround = false;
const keys = {};
addEventListener('keydown', e => {
  keys[e.code] = true;
  const n = e.code.match(/^Digit([1-9])$/);
  if (n && SPAWNS && SPAWNS[n[1] - 1]){
    const s = SPAWNS[n[1] - 1];
    pos = [s.position[0], s.position[1], s.position[2] + EYE];
    yaw = s.facing || 0; vz = 0; onGround = false;
    document.getElementById('mode').textContent =
      (walking ? 'walking' : 'flying') + ' \u00b7 ' + s.label;
  }
  if (e.code === 'KeyC' && ROOMS) culling = !culling;
  if (e.code === 'KeyG' && COLL){
    walking = !walking; vz = 0;
    document.getElementById('mode').textContent =
      walking ? 'walking' : 'flying';
  }
});
addEventListener('keyup', e => keys[e.code] = false);
let drag = false, lx = 0, ly = 0;
const c = gl.canvas;
c.addEventListener('mousedown', e => { drag = true; lx = e.clientX; ly = e.clientY; });
addEventListener('mouseup', () => drag = false);
addEventListener('mousemove', e => {
  if (!drag) return;
  yaw -= (e.clientX - lx) * 0.005; pitch -= (e.clientY - ly) * 0.005;
  pitch = Math.max(-1.55, Math.min(1.55, pitch));
  lx = e.clientX; ly = e.clientY;
});

function mul(a, b){ const o = new Float32Array(16);
  for (let i=0;i<4;i++) for (let j=0;j<4;j++){ let s=0;
    for (let k=0;k<4;k++) s += a[k*4+j]*b[i*4+k]; o[i*4+j]=s; } return o; }

function frame(){
  const w = c.clientWidth, h = c.clientHeight;
  if (c.width !== w || c.height !== h){ c.width = w; c.height = h; }
  gl.viewport(0,0,w,h);
  // no back-face culling: strip winding is not uniform across groups,
  // and with the depth test alone the result is right either way
  gl.enable(gl.DEPTH_TEST); gl.disable(gl.CULL_FACE);
  gl.clearColor(0.05,0.06,0.07,1); gl.clear(gl.COLOR_BUFFER_BIT|gl.DEPTH_BUFFER_BIT);

  // models are Z-up: forward is built in that frame, then flipped into GL's
  const cp = Math.cos(pitch), sp = Math.sin(pitch);
  const fwd = [Math.cos(yaw)*cp, Math.sin(yaw)*cp, sp];
  const right = [-Math.sin(yaw), Math.cos(yaw), 0];
  const v = speed * (keys.ShiftLeft || keys.ShiftRight ? 4 : 1);
  if (walking){
    const dt = 1/60;
    const run = (keys.ShiftLeft || keys.ShiftRight ? SPRINT : WALK) * dt;
    let dx = 0, dy = 0;
    if (keys.KeyW){ dx += fwd[0]; dy += fwd[1]; }
    if (keys.KeyS){ dx -= fwd[0]; dy -= fwd[1]; }
    if (keys.KeyD){ dx += right[0]; dy += right[1]; }
    if (keys.KeyA){ dx -= right[0]; dy -= right[1]; }
    const len = Math.hypot(dx, dy);
    if (len > 0){
      dx = dx / len * run; dy = dy / len * run;
      // in pieces of at most a quarter unit, or a fast run steps straight
      // over a thin wall; full step first, then each axis on its own, which
      // is what sliding along a wall amounts to
      const pieces = Math.max(1, Math.ceil(run / 0.25));
      for (let k = 0; k < pieces; k++){
        for (const [ax, ay] of [[dx, dy], [dx, 0], [0, dy]]){
          const nx = pos[0] + ax / pieces, ny = pos[1] + ay / pieces;
          if (!blocked(nx, ny, pos[2])){ pos[0] = nx; pos[1] = ny; break; }
        }
      }
    }
    if (onGround && keys.Space){ vz = JUMP; onGround = false; }
    vz -= GRAVITY * dt;
    // step the fall in pieces no larger than half a unit: at terminal speed
    // a whole frame's worth is longer than some floors are thick, and the
    // body would drop straight through them
    let nz = pos[2], left = vz * dt;
    while (Math.abs(left) > 1e-6 && !footed(pos[0], pos[1], nz)){
      const bit = Math.max(-0.5, Math.min(0.5, left));
      nz += bit; left -= bit;
    }
    if (footed(pos[0], pos[1], nz)){
      let lift = 0;                       // rise out of the surface landed on
      while (lift < EYE && footed(pos[0], pos[1], nz + lift)) lift += 0.05;
      nz += lift; onGround = true; vz = 0;
    } else if (onGround && vz < 0){
      let drop = 0;                       // stay glued over kerbs and stairs
      while (drop < STEP && !footed(pos[0], pos[1], nz - drop)) drop += 0.05;
      if (drop < STEP){ nz -= drop - 0.05; vz = 0; } else onGround = false;
    } else onGround = false;
    pos[2] = nz;
  } else {
    const step = (d, s) => { for (let i=0;i<3;i++) pos[i] += d[i]*s; };
    if (keys.KeyW) step(fwd, v); if (keys.KeyS) step(fwd, -v);
    if (keys.KeyD) step(right, v); if (keys.KeyA) step(right, -v);
    if (keys.KeyR) pos[2] += v;  if (keys.KeyF) pos[2] -= v;
  }

  // up = fwd x right, not right x fwd: the other order puts up at (0,0,-1)
  // when pitch is zero and renders the whole scene upside down
  const up = [fwd[1]*right[2]-fwd[2]*right[1], fwd[2]*right[0]-fwd[0]*right[2],
              fwd[0]*right[1]-fwd[1]*right[0]];
  const view = new Float32Array([
    right[0], up[0], -fwd[0], 0,
    right[1], up[1], -fwd[1], 0,
    right[2], up[2], -fwd[2], 0,
    -(right[0]*pos[0]+right[1]*pos[1]+right[2]*pos[2]),
    -(up[0]*pos[0]+up[1]*pos[1]+up[2]*pos[2]),
     (fwd[0]*pos[0]+fwd[1]*pos[1]+fwd[2]*pos[2]), 1]);
  const f = 1/Math.tan(0.9/2), a = w/h, near = MESH.span/2000, far = MESH.span*8;
  const proj = new Float32Array([f/a,0,0,0, 0,f,0,0,
    0,0,(far+near)/(near-far),-1, 0,0,2*far*near/(near-far),0]);
  gl.uniformMatrix4fv(gl.getUniformLocation(prog,'uMVP'), false, mul(proj, view));

  // The .tex header says which textures carry alpha, so the opaque ones go
  // down first with the depth buffer writing, and the translucent ones after
  // with it read-only -- glass and gradients then show what is behind them
  // instead of being punched out by an alpha test.
  shown = culling ? visibleRooms(pos) : null;
  if (ROOMS)
    document.getElementById('roomline').textContent =
      ' · room: ' + (hereName || 'none')
      + (culling ? ' · drawing ' + (shown ? shown.size - 1 : 'all') : '')
      + (culling ? '' : ' · culling off');
  const skip = d => shown && !shown.has(d.room);

  gl.disable(gl.BLEND); gl.depthMask(true);
  for (const d of MESH.draws)
    if (!d.blend && !skip(d)){
      gl.uniform1f(uCut, 0.35);
      gl.bindTexture(gl.TEXTURE_2D, textures[d.tex] || blank);
      gl.drawArrays(gl.TRIANGLES, d.first, d.count);
    }
  gl.enable(gl.BLEND); gl.depthMask(false);
  gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
  for (const d of MESH.draws)
    if (d.blend && !skip(d)){
      gl.uniform1f(uCut, 0.02);
      gl.bindTexture(gl.TEXTURE_2D, textures[d.tex] || blank);
      gl.drawArrays(gl.TRIANGLES, d.first, d.count);
    }
  gl.depthMask(true); gl.disable(gl.BLEND);
  requestAnimationFrame(frame);
}
if (COLL) document.getElementById('walkhelp').textContent =
  ' \u00b7 G to walk \u00b7 space to jump';
if (SPAWNS && SPAWNS.length)
  document.getElementById('walkhelp').textContent +=
    ' \u00b7 1-9 for checkpoints';
frame();
</script>
"""


def _rotate(q, p):
    """Rotate p by the quaternion q, which is (w, x, y, z) as in the .mod."""
    w, x, y, z = q
    tx = 2 * (y * p[2] - z * p[1])
    ty = 2 * (z * p[0] - x * p[2])
    tz = 2 * (x * p[1] - y * p[0])
    return (p[0] + w * tx + (y * tz - z * ty),
            p[1] + w * ty + (z * tx - x * tz),
            p[2] + w * tz + (x * ty - y * tx))


def place(obj: dict, verts: list, node_local: bool) -> list:
    """Put one object's vertices where the object stands.

    Some models are authored in world space and are already standing where
    their object is -- a level's rooms and its fixed scenery, `l1_r1.mod` for
    the object `l1_r1`. Others are prototypes authored around the origin and
    reused: `dr1.mod` is one door mesh placed at 21 different doorways. The
    first kind must not be moved and the second must be.

    Nothing in the *scene graph* says which is which -- `flag`, `group`, the
    parent and the model's root translation were all cross-tabulated and none
    of them separates the two. The model says it instead: **a model with an
    animation section is authored in node-local space and has to be placed; a
    static one is already in world space.** That is the same distinction
    `Model.posed()` already makes for the vertices themselves, and it holds:
    **all 126 models that more than one object uses are animated**, which is
    what a prototype has to be, and 1626 of the 1666 animated models sit
    around the origin while 913 of the 917 static ones sit at their object's
    position already.
    """
    if not node_local:
        return verts
    return [(tuple(_rotate(obj["rotation"], v[0])[c] + obj["position"][c]
                   for c in range(3)), v[1], v[2]) for v in verts]


def selftest() -> None:
    """`place()` on the two cases it exists to tell apart."""
    unit = [((0.0, 0.0, 0.0), (0, 0), None), ((1.0, 1.0, 1.0), (0, 0), None)]
    far = [((10.0, 0.0, 0.0), (0, 0), None), ((11.0, 1.0, 1.0), (0, 0), None)]
    quarter_turn = (0.70710678, 0.0, 0.0, 0.70710678)   # 90 degrees about z

    at_origin = {"position": [10.0, 0.0, 0.0], "rotation": (1.0, 0, 0, 0)}
    assert place(at_origin, unit, True)[0][0] == (10.0, 0.0, 0.0), "prototype"

    world = {"position": [10.5, 0.5, 0.5], "rotation": quarter_turn}
    assert place(world, far, False) is far, "static, already in place"

    turned = place({"position": [0.0, 0.0, 0.0],
                    "rotation": quarter_turn}, far, True)[0][0]
    assert max(abs(turned[c] - (0.0, 10.0, 0.0)[c]) for c in range(3)) < 1e-6, \
        turned
    print("mod2html.py: self-test passed")


def _rooms(graph_path: Path, resources: Path) -> dict | None:
    """The level's visibility graph, when this scene is one of the ten levels.

    Only `base/lN.lua` has a `scripts/levelN.lua` beside it to read it from;
    a movie set or a menu scene has none, and the page then draws everything
    the way it always did.
    """
    m = re.fullmatch(r"l(\d+)", graph_path.stem)
    if not m:
        return None
    try:
        import rooms as rm
        table, _, _ = rm.load(int(m.group(1)), resources, rm._override(resources))
    except Exception:
        return None
    return {name: {"box": (r["box"][0] + r["box"][1]) if r["box"] else None,
                   "vis": rm.visible_from(table, name)}
            for name, r in table.items() if r["live"]}


def object_rooms(graph: dict) -> dict:
    """Which room each object belongs to. -> {object name: room name or None}.

    A room is an `OBJ_ROOM` object and everything under it in the parent
    chain is in it, whether it hangs off the room directly (290 of level 1's
    409 objects) or off a group that does. 5159 of the ten levels' 5237
    objects resolve; the 78 that do not hang off `scene` itself -- scenery,
    lights, spawners -- and are never culled, which is what the engine does
    with a gob that is in no room.
    """
    by = {o["name"]: o for o in graph["objects"]}
    out = {}
    for o in graph["objects"]:
        seen, cur = set(), o
        while cur is not None and cur["name"] not in seen:
            seen.add(cur["name"])
            if cur["type"] == "OBJ_ROOM":
                break
            cur = by.get(cur["parent"])
        out[o["name"]] = cur["name"] if cur and cur["type"] == "OBJ_ROOM" else None
    return out


def scene_geometry(path: Path, resources: Path) -> tuple[list, list, int]:
    """Merge every model a scene graph places. -> (vertices, triangles, count)

    Each triangle carries the name of the room its object is in, so that
    `build()` can group the draw calls by room and the page can use the
    level's authored visibility -- see tools/rooms.py.
    """
    graph = sg.parse(path.read_text(errors="replace"))
    where = object_rooms(graph)
    cache: dict[Path, Model] = {}
    verts: list = []
    tris: list = []
    placed = 0
    for o in graph["objects"]:
        if o["resource"] is None:
            continue
        found = sg.resolve(o, graph, resources)
        if not isinstance(found, Path) or found.suffix != ".mod":
            continue
        model = cache.get(found)
        if model is None:
            model = cache[found] = Model(found.read_bytes())
        v, t = model.posed()
        if not v:
            continue
        base = len(verts)
        room = where.get(o["name"]) or ""
        verts += place(o, v, model._sec(1) is not None)
        tris += [(a + base, b + base, c + base, room) for a, b, c in t]
        placed += 1
    return verts, tris, placed


def scene_collision(path: Path, resources: Path) -> dict | None:
    """Every `.bsp` the scene's objects name, packed into one buffer.

    A tree comes with the model, so an object collides against
    `<resource>.bsp` when one exists: for level 1 that is 81 of 265 objects,
    the rooms and the fixed scenery, 85796 nodes in all. **None of the 81
    belongs to an animated model**, so nothing here needs transforming --
    static geometry is already in world space, and so is its tree.
    """
    graph = sg.parse(path.read_text(errors="replace"))
    blob = bytearray()
    trees = []
    seen: dict[Path, dict] = {}
    for o in graph["objects"]:
        if o["resource"] is None:
            continue
        found = sg._find(resources, o["resource"] + ".bsp")
        if found is None or found in seen:
            continue
        data = found.read_bytes()
        model = sg._find(resources, o["resource"] + ".mod")
        box = _model_box(model) if model else None
        if box is None:
            continue
        seen[found] = {
            "first": len(blob) // 24,
            "count": len(data) // 24,
            "box": [round(c, 3) for c in box],
        }
        blob += data
        trees.append(seen[found])
    if not trees:
        return None
    return {"data": base64.b64encode(bytes(blob)).decode(), "trees": trees}


def _spawns(graph: Path, resources: Path) -> list[dict] | None:
    """The checkpoints of whichever level script names this scene graph."""
    import spawn as sp
    for n in range(0, 14):
        script = resources / "scripts" / f"level{n}.lua"
        if not script.is_file():
            continue
        try:
            named, points = sp.checkpoints(resources, n)
        except Exception:
            continue
        if named and named.lower() == graph.stem.lower():
            return [{"label": f"{n}-{c['index']} {c['label']}",
                     "position": c["position"],
                     "facing": c["facing"]} for c in points]
    return None


def _model_box(path: Path) -> list[float] | None:
    """The model's world bounds, padded, used only to skip distant trees."""
    verts, _tris = Model(path.read_bytes()).posed()
    if not verts:
        return None
    pad = 1.0
    return [min(v[0][c] for v in verts) - pad for c in range(3)] + \
           [max(v[0][c] for v in verts) + pad for c in range(3)]


def _blended(name: str, resources: Path | None) -> bool:
    """Does this texture need blending rather than an alpha test?

    The `.tex` header says so outright, at offset 0x10: 3 for RGB and 4 for
    RGBA. It is exact over the whole set -- all 517 textures that say 3 are
    fully opaque, and all 238 that say 4 carry alpha, 190 of them partial
    rather than cutout. No guessing at the pixels required.
    """
    if not resources or not name:
        return False
    found = sg._find(resources, Path(name).stem + ".tex")
    if found is None:
        return False
    with found.open("rb") as fh:
        head = fh.read(0x14)
    return len(head) >= 0x14 and struct.unpack_from("<I", head, 0x10)[0] == 4


def build(verts: list, tris: list, png_dir: Path | None,
          resources: Path | None = None) -> tuple[dict, dict]:
    # grouped by room as well as by texture, so the page can drop the rooms
    # the level says are not visible from where you stand
    groups: dict[tuple, list[int]] = {}
    for t in tris:
        a, b, c = t[0], t[1], t[2]
        room = t[3] if len(t) > 3 else ""
        groups.setdefault((room, verts[a][2]), []).extend((a, b, c))

    data = bytearray()
    draws = []
    for (room, tex), idx in groups.items():
        first = len(data) // 20
        for i in idx:
            pos, uv, _ = verts[i]
            data += struct.pack("<5f", *pos, *uv)
        draws.append({"tex": tex or "", "first": first, "count": len(idx),
                      "room": room,
                      "blend": _blended(tex or "", resources)})

    pts = [v[0] for v in verts]
    lo = [min(p[c] for p in pts) for c in range(3)]
    hi = [max(p[c] for p in pts) for c in range(3)]
    span = max(hi[c] - lo[c] for c in range(3)) or 1.0
    mid = [(lo[c] + hi[c]) / 2 for c in range(3)]
    mesh = {
        "data": base64.b64encode(bytes(data)).decode(),
        "draws": draws,
        "span": span,
        # start outside the model looking in
        "eye": [mid[0] - span * 0.9, mid[1] - span * 0.9, mid[2] + span * 0.3],
    }

    textures: dict[str, str] = {}
    if png_dir:
        for d in draws:
            name = d["tex"]
            if not name or name in textures:
                continue
            f = png_dir / (Path(name).stem + ".png")
            if f.is_file():
                textures[name] = ("data:image/png;base64,"
                                  + base64.b64encode(f.read_bytes()).decode())
    return mesh, textures


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path, nargs="?",
                    help="a .mod, or a scene .lua with --scene")
    ap.add_argument("-o", "--out", type=Path)
    ap.add_argument("--png", type=Path, default=Path("png"),
                    help="directory of converted textures")
    ap.add_argument("--scene", action="store_true",
                    help="src is a level scene graph, pack the whole level")
    ap.add_argument("--resources", type=Path, default=Path("extracted"),
                    help="extraction root, to resolve the scene's resources")
    ap.add_argument("--walk", action="store_true",
                    help="with --scene, embed the .bsp trees so the camera "
                         "can walk the level instead of flying")
    ap.add_argument("--selftest", action="store_true", help="check place()")
    args = ap.parse_args(argv)

    if args.selftest:
        selftest()
        return 0
    if args.src is None or args.out is None:
        ap.error("src and -o are required")

    coll, spawns, room_graph = None, None, None
    if args.scene:
        verts, tris, placed = scene_geometry(args.src, args.resources)
        what = f"{placed} objects"
        room_graph = _rooms(args.src, args.resources)
        if room_graph:
            what += f" &middot; {len(room_graph)} rooms"
        if args.walk:
            spawns = _spawns(args.src, args.resources)
            coll = scene_collision(args.src, args.resources)
            if coll:
                what += (f" &middot; {len(coll['trees'])} collision trees, "
                         f"{sum(t['count'] for t in coll['trees'])} nodes")
    else:
        m = Model(args.src.read_bytes())
        verts, tris = m.posed()
        what = f"{len(m.nodes)} nodes"
    mesh, textures = build(verts, tris,
                           args.png if args.png.is_dir() else None,
                           args.resources if args.scene else None)
    stats = (f"{what} &middot; "
             f"{sum(d['count'] for d in mesh['draws']) // 3} triangles &middot; "
             f"{len(textures)} textures")
    page = (PAGE.replace("__TITLE__", args.src.stem)
                .replace("__STATS__", stats)
                .replace("__MESH__", json.dumps(mesh))
                .replace("__TEX__", json.dumps(textures))
                .replace("__COLL__", json.dumps(coll))
                .replace("__SPAWNS__", json.dumps(spawns))
                .replace("__ROOMS__", json.dumps(room_graph)))
    args.out.write_text(page)
    print(f"{args.out}: {stats.replace('&middot;', '|')}, "
          f"{len(page)/2**20:.1f} MiB", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
