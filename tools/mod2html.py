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

**The animated objects move.** Nearly half of a level is on something that
is supposed to be: level 1 places 142 animated objects against 110 static
ones. MDK2's models are rigid hierarchies -- one node per vertex, no skinning
weights -- so posing one is a quaternion and an offset per node, two vec4 of
uniform. `scene_movers()` samples each model's node table 30 times over a
loop and uploads the model's geometry once; the page draws it once per object
with that object's own placement. `--movers` checks that arithmetic against
`tools/mod2obj.py`'s, which shares no code with it, over every animated
object in a level.

Two ceilings, both deliberate: a model with more than 64 nodes does not fit
the uniform array WebGL 1 guarantees and stays in its bind pose (9 of level
1's models), and only animation 0 plays, on a loop.

**And the level's ambience.** `OBJ_AMBIENTSOUND` objects are placed in the
scene graph like anything else, so the page loops each one through WebAudio
with a distance falloff and the level has a sound again -- six sources and
three clips in level 1, 354 KiB of them. It needs `ffmpeg` at build time to
turn the WAVC into RIFF, and is skipped without it; browsers will not start
audio before a gesture, so the first click or key does.

For one of the ten levels it also embeds the **room graph** (`tools/rooms.py`)
and uses the level's own visibility: standing in a room, only that room and
the rooms it lists are drawn, which at the game's own spawn points is a median
11.3% of the level's triangles. **C** turns it off. The HUD names the room you
are in, and when you cross into a new one it prints the handlers the engine
would dispatch there -- `l1_r1.OnEnterRoom` and the rest -- which is the level
script's logic answering the player rather than a viewer's idea of one.

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
#events{margin:6px 0 0;color:#7c9;font:11px/1.5 ui-monospace,monospace;white-space:pre-wrap}
</style>
<canvas id=c></canvas>
<div id=hud><b>__TITLE__</b><br>__STATS__<br>drag to look &middot; WASD &middot;
R/F up-down &middot; shift faster<span id=walkhelp></span><br>
mode: <b id=mode>flying</b><span id=roomline></span><span id=sound></span>
<pre id=events></pre></div>
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

// --- the level's ambient sound, when ffmpeg was there to decode it ------
// SOUND.sources are OBJ_AMBIENTSOUND objects straight from the scene graph.
// Three of their four payload numbers are readable: a near distance, a far
// distance (near < far in all 80 in the corpus) and a volume. The fourth is
// not explained -- see scene_sounds(). Browsers will not start audio before
// a gesture, so the first click or key starts it.
const SOUND = __SOUND__;
let audio = null;
function startAudio(){
  if (audio || !SOUND) return;
  audio = new (window.AudioContext || window.webkitAudioContext)();
  for (const s of SOUND.sources){
    s.gainNode = audio.createGain();
    s.gainNode.gain.value = 0;
    s.gainNode.connect(audio.destination);
    fetch(SOUND.clips[s.clip]).then(r => r.arrayBuffer())
      .then(b => audio.decodeAudioData(b))
      .then(buf => {
        const src = audio.createBufferSource();
        src.buffer = buf; src.loop = true;
        src.connect(s.gainNode); src.start();
      }).catch(() => {});
  }
  document.getElementById('sound').textContent = ' \u00b7 sound on';
}
addEventListener('pointerdown', startAudio, {once: true});
addEventListener('keydown', startAudio, {once: true});
function mixAudio(p){
  if (!audio) return;
  for (const s of SOUND.sources){
    if (!s.gainNode) continue;
    const d = Math.hypot(p[0]-s.pos[0], p[1]-s.pos[1], p[2]-s.pos[2]);
    // full volume inside `near`, falling linearly to nothing at `far`
    let g = 0;
    if (d <= s.near) g = s.gain;
    else if (d < s.far) g = s.gain * (1 - (d - s.near) / (s.far - s.near));
    if (shown && !shown.has(s.room)) g = 0;
    s.gainNode.gain.value = g;
  }
}

// --- the animated objects, when the page was built with --scene ---------
// MOVE.data is one vertex buffer of every animated *model* -- six floats,
// position, uv, node index -- and MOVE.movers places those models in the
// world. MDK2 models are rigid hierarchies: one node per vertex and no
// skinning weights, so posing is a quaternion and an offset per node, which
// fits in two vec4 of uniform. The node tables are sampled 30 times over a
// loop by tools/mod2html.py; nothing here reads keyframes.
const MOVE = __MOVE__;
let mprog = null, mbuf = null, uM = {};
if (MOVE){
  mprog = gl.createProgram();
  gl.attachShader(mprog, sh(gl.VERTEX_SHADER, `
    attribute vec3 aPos; attribute vec2 aUV; attribute float aNode;
    uniform mat4 uMVP;
    uniform vec4 uNodeQ[64];       // (x, y, z, w) -- the file stores (w,x,y,z)
    uniform vec4 uNodeT[64];
    uniform vec4 uObjQ; uniform vec3 uObjP;
    varying vec2 vUV; varying float vZ;
    vec3 qrot(vec4 q, vec3 v){
      return v + 2.0 * cross(q.xyz, cross(q.xyz, v) + q.w * v);
    }
    void main(){
      int i = int(aNode + 0.5);
      vec3 p = qrot(uNodeQ[i], aPos) + uNodeT[i].xyz;
      p = qrot(uObjQ, p) + uObjP;
      gl_Position = uMVP * vec4(p, 1.0);
      vUV = aUV; vZ = gl_Position.w;
    }`));
  gl.attachShader(mprog, sh(gl.FRAGMENT_SHADER, `
    precision mediump float; uniform sampler2D uTex; uniform float uCut;
    varying vec2 vUV; varying float vZ;
    void main(){
      vec4 c = texture2D(uTex, vec2(vUV.x, 1.0 - vUV.y));
      if (c.a < uCut) discard;
      float fog = clamp(vZ / 4000.0, 0.0, 0.75);
      gl_FragColor = vec4(mix(c.rgb, vec3(0.05,0.06,0.07), fog), c.a);
    }`));
  gl.linkProgram(mprog);
  for (const n of ['uMVP','uNodeQ','uNodeT','uObjQ','uObjP','uCut'])
    uM[n] = gl.getUniformLocation(mprog, n);
  mbuf = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, mbuf);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(bytes(MOVE.data).buffer),
                gl.STATIC_DRAW);
  for (const m of MOVE.models){
    m.Q = []; m.T = [];
    for (const f of m.frames){
      const q = new Float32Array(64 * 4), t = new Float32Array(64 * 4);
      for (let i = 0; i < 64; i++){
        if (i < m.nodes){
          q[i*4] = f[i*8]; q[i*4+1] = f[i*8+1];
          q[i*4+2] = f[i*8+2]; q[i*4+3] = f[i*8+3];
          t[i*4] = f[i*8+4]; t[i*4+1] = f[i*8+5]; t[i*4+2] = f[i*8+6];
        } else { q[i*4+3] = 1.0; }
      }
      m.Q.push(q); m.T.push(t);
    }
    m.frames = null;                       // the flat copy is no longer needed
  }
  // the scene graph stores (w, x, y, z); the shader wants (x, y, z, w)
  for (const o of MOVE.movers)
    o.q = new Float32Array([o.quat[1], o.quat[2], o.quat[3], o.quat[0]]);
}
function drawMovers(mvp, now, blend){
  if (!MOVE) return;
  gl.useProgram(mprog);
  gl.bindBuffer(gl.ARRAY_BUFFER, mbuf);
  const aP = gl.getAttribLocation(mprog, 'aPos');
  const aU = gl.getAttribLocation(mprog, 'aUV');
  const aN = gl.getAttribLocation(mprog, 'aNode');
  gl.enableVertexAttribArray(aP); gl.vertexAttribPointer(aP, 3, gl.FLOAT, false, 24, 0);
  gl.enableVertexAttribArray(aU); gl.vertexAttribPointer(aU, 2, gl.FLOAT, false, 24, 12);
  gl.enableVertexAttribArray(aN); gl.vertexAttribPointer(aN, 1, gl.FLOAT, false, 24, 20);
  gl.uniformMatrix4fv(uM.uMVP, false, mvp);
  gl.uniform1f(uM.uCut, blend ? 0.02 : 0.35);
  for (const o of MOVE.movers){
    if (shown && !shown.has(o.room)) continue;
    const m = MOVE.models[o.model];
    const f = Math.floor(now / m.seconds * MOVE.frames) % MOVE.frames;
    gl.uniform4fv(uM.uNodeQ, m.Q[f]);
    gl.uniform4fv(uM.uNodeT, m.T[f]);
    gl.uniform4fv(uM.uObjQ, o.q);
    gl.uniform3fv(uM.uObjP, o.pos);
    for (const d of m.draws){
      if (!!d.blend !== blend) continue;
      gl.bindTexture(gl.TEXTURE_2D, textures[d.tex] || blank);
      gl.drawArrays(gl.TRIANGLES, d.first, d.count);
    }
  }
  gl.useProgram(prog);
  gl.bindBuffer(gl.ARRAY_BUFFER, buf);
  gl.enableVertexAttribArray(aPos); gl.vertexAttribPointer(aPos, 3, gl.FLOAT, false, 20, 0);
  gl.enableVertexAttribArray(aUV);  gl.vertexAttribPointer(aUV, 2, gl.FLOAT, false, 20, 12);
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
let lastRoom = null;
const log = [];
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
  const wasX = pos[0], wasY = pos[1];
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
    // rise out of the surface landed on, but stop at the ceiling: lifting
    // until the feet are clear is right in the open and wrong under an
    // overhang, where it drives the head into the slab
    if (footed(pos[0], pos[1], nz)){
      let lift = 0;
      while (lift < EYE && footed(pos[0], pos[1], nz + lift)
             && !blocked(pos[0], pos[1], nz + lift + 0.05)) lift += 0.05;
      nz += lift; onGround = true; vz = 0;
    } else if (onGround && vz < 0){
      let drop = 0;                       // stay glued over kerbs and stairs
      while (drop < STEP && !footed(pos[0], pos[1], nz - drop)) drop += 0.05;
      if (drop < STEP){ nz -= drop - 0.05; vz = 0; } else onGround = false;
    } else onGround = false;
    // the sideways move was checked at the height the body had before it
    // settled; if the finished position is inside the world, give it back
    if (blocked(pos[0], pos[1], nz) && !blocked(wasX, wasY, nz)){
      pos[0] = wasX; pos[1] = wasY;
    }
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
  const mvp = mul(proj, view);
  gl.uniformMatrix4fv(gl.getUniformLocation(prog,'uMVP'), false, mvp);
  const now = performance.now() / 1000;

  // The .tex header says which textures carry alpha, so the opaque ones go
  // down first with the depth buffer writing, and the translucent ones after
  // with it read-only -- glass and gradients then show what is behind them
  // instead of being punched out by an alpha test.
  shown = culling ? visibleRooms(pos) : null;
  if (ROOMS && hereName !== lastRoom){
    // what the engine would dispatch on crossing into this room: the level
    // script hangs handlers on the room's own object, and OnEnterRoom is the
    // commonest of them all -- 125 of them across the ten levels
    const on = hereName.split(' + ')
      .filter(r => ROOMS[r] && ROOMS[r].on.length)
      .map(r => r + '.' + ROOMS[r].on.join(', ' + r + '.'));
    if (on.length) log.unshift('→ ' + on.join(' · '));
    // the track the room asks for: mdkRoomSetMusic(room, 18) means
    // Music/Track18, see rooms.track()
    const mus = hereName.split(' + ')
      .map(r => ROOMS[r] && ROOMS[r].music).filter(Boolean);
    if (mus.length) log.unshift('♪ ' + mus.join(', '));
    lastRoom = hereName;
    document.getElementById('events').textContent = log.slice(0, 4).join('\n');
  }
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
  mixAudio(pos);
  drawMovers(mvp, now, false);
  gl.enable(gl.BLEND); gl.depthMask(false);
  gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
  for (const d of MESH.draws)
    if (d.blend && !skip(d)){
      gl.uniform1f(uCut, 0.02);
      gl.bindTexture(gl.TEXTURE_2D, textures[d.tex] || blank);
      gl.drawArrays(gl.TRIANGLES, d.first, d.count);
    }
  drawMovers(mvp, now, true);
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
        level = int(m.group(1))
        over = rm._override(resources)
        table, _, _ = rm.load(level, resources, over)
        hooks = rm.handlers(level, resources, over)
    except Exception:
        return None
    return {name: {"box": (r["box"][0] + r["box"][1]) if r["box"] else None,
                   "vis": rm.visible_from(table, name),
                   "on": hooks.get(name, []),
                   "music": rm.track(r["music"])}
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


def _moves(model: Model) -> bool:
    """Whether scene_movers() will pose this model, so the static pass skips
    it and nothing is drawn twice."""
    return (model._sec(1) is not None and bool(model.animations())
            and len(model.nodes) <= ANIM_MAX_NODES)


def scene_geometry(path: Path, resources: Path,
                   animate: bool = False) -> tuple[list, list, int]:
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
        if animate and _moves(model):
            continue              # scene_movers() poses this one in the page
        v, t = model.posed()
        if not v:
            continue
        base = len(verts)
        room = where.get(o["name"]) or ""
        verts += place(o, v, model._sec(1) is not None)
        tris += [(a + base, b + base, c + base, room) for a, b, c in t]
        placed += 1
    return verts, tris, placed


ANIM_FRAMES = 30            # samples of a loop; the demo runs at 30 fps
ANIM_MAX_NODES = 64         # 2 vec4 of uniform each, and WebGL1 promises 128


def _duration(anim: dict) -> float:
    """How long one loop of an animation lasts, in seconds.

    The animation record's float at +8 is a **signed playback rate**, in
    loops a second, so the duration is its reciprocal. Read as a *length* it
    would have to explain 99 negative values, and a negative length means
    nothing; a negative speed is exactly what the scripts ask for by name --
    `omAnimSetSpeed(door, ANIM_OPEN, -1)` is how `elevators.lua` shuts a door
    it opened. Over the corpus that puts the median loop at about a second and
    a half. Not confirmed against the running game, so it sets the viewer's
    playback speed and nothing that has to be exact.
    """
    rate = abs(anim.get("length") or 0.0)
    if not 1e-3 < rate < 1e3:
        return 2.0
    return 1.0 / rate


def scene_movers(path: Path, resources: Path) -> dict | None:
    """The animated objects, posed in the browser rather than baked flat.

    MDK2 models are rigid hierarchies -- each vertex belongs to exactly one
    node, no skinning weights -- so a vertex needs only its own node's
    quaternion and offset. Those are sampled here into a small table per
    model and per frame, and the page multiplies them out: the geometry is
    uploaded once per *model* and drawn once per *object*, with the node
    table and the object's own placement as uniforms.

    That matters for how much of a level moves. Level 1 places 151 animated
    objects against 101 static ones, and 35583 of its 74658 triangles are on
    something that is supposed to be moving.
    """
    graph = sg.parse(path.read_text(errors="replace"))
    where = object_rooms(graph)
    cache: dict[Path, Model] = {}
    models: dict[Path, int] = {}
    out_models: list = []
    movers: list = []
    data = bytearray()

    for o in graph["objects"]:
        if o["resource"] is None:
            continue
        found = sg.resolve(o, graph, resources)
        if not isinstance(found, Path) or found.suffix != ".mod":
            continue
        model = cache.get(found)
        if model is None:
            model = cache[found] = Model(found.read_bytes())
        anims = model.animations() if model._sec(1) is not None else []
        if not anims or len(model.nodes) > ANIM_MAX_NODES:
            continue                      # static, or too big for the uniforms
        if found not in models:
            packed = _pack_model(model, anims[0], data, resources)
            if packed is None:
                continue
            models[found] = len(out_models)
            out_models.append(packed)
        movers.append({
            "at": o["line"],           # the graph line: names are NOT unique
            "model": models[found],
            "pos": list(o["position"]),
            "quat": list(o["rotation"]),
            "room": where.get(o["name"]) or "",
        })
    if not movers:
        return None
    return {"data": base64.b64encode(bytes(data)).decode(),
            "models": out_models, "movers": movers, "frames": ANIM_FRAMES}


def _pack_model(model: Model, anim: dict, data: bytearray,
                resources: Path | None = None) -> dict | None:
    """Append one model's node-local vertices and sample its node table."""
    first_vertex = len(data) // 24         # 6 floats: pos, uv, node index
    draws: dict[str, list] = {}
    for ni, node in enumerate(model.nodes):
        tex = model.node_texture(node) or ""
        for g in range(node["group_first"],
                       node["group_first"] + node["group_count"]):
            gfirst, count = model.groups[g]
            for k in range(count - 2):
                # the strip, expanded, so one draw can hold many nodes
                idx = [gfirst + k, gfirst + k + 1, gfirst + k + 2]
                if k & 1:
                    idx[1], idx[2] = idx[2], idx[1]
                for i in idx:
                    pos, uv = model.vertices[i]
                    draws.setdefault(tex, []).append(
                        struct.pack("<6f", *pos, *uv, float(ni)))
    if not draws:
        return None
    packed = []
    for tex, rows in draws.items():
        packed.append({"tex": tex, "first": len(data) // 24,
                       "count": len(rows),
                       "blend": _blended(tex, resources)})
        for r in rows:
            data += r
    frames = []
    for f in range(ANIM_FRAMES):
        flat: list[float] = []
        for q, off in model.node_world(anim, f / ANIM_FRAMES):
            flat += [q[1], q[2], q[3], q[0], off[0], off[1], off[2], 0.0]
        # six decimals: a quaternion rounded harder than this turns into
        # a centimetre of error out at the end of a node 1000 units long
        frames.append([round(v, 6) for v in flat])
    return {"draws": packed, "nodes": len(model.nodes),
            "seconds": _duration(anim), "frames": frames,
            "first": first_vertex}


AMBIENT = "OBJ_AMBIENTSOUND"


def scene_sounds(path: Path, resources: Path,
                 ffmpeg: str = "ffmpeg") -> dict | None:
    """The level's ambient sounds, decoded and placed. -> {clips, sources}.

    The scene graph puts these down like any other object, and three of the
    four numbers in the payload slot are readable. Slots 0 and 1 are a near
    and a far distance -- **near < far in all 80 ambient objects in the
    corpus**, 1 to 40 units against 6 to 80 -- and slot 3 is the volume, 0.4
    to 1.0. The `.mod` animation channels say the same thing from a different
    file: a sound is animated through kind 32 (volume, 1.0), kind 33 (min
    distance, 5 to 50) and kind 34 (max distance, 500). Three parameters, not
    four.

    **Slot 2 is not explained.** It takes four values, 0.0, 0.1, 0.2 and 0.3,
    and a first reading of it as "the volume out at the far distance" fitted
    the data -- slot 3 is never below it, in all 80 -- but so would anything
    small, and the channel kinds say the engine's sound has no such
    parameter. It is left out rather than guessed at, and the falloff here
    runs from the volume at `near` to nothing at `far`.

    A level uses few of them -- six objects and three distinct sounds in
    level 1 -- so the clips fit in the page beside the textures.
    """
    import shutil
    if not shutil.which(ffmpeg):
        return None
    import wavc
    graph = sg.parse(path.read_text(errors="replace"))
    where = object_rooms(graph)
    clips: dict[str, str] = {}
    sources = []
    for o in graph["objects"]:
        if o["type"] != AMBIENT or o["resource"] is None:
            continue
        found = sg.resolve(o, graph, resources)
        if not isinstance(found, Path) or not found.is_file():
            continue
        name = found.stem
        if name not in clips:
            try:
                wav = wavc.to_wav(found.read_bytes(), ffmpeg)
            except Exception:
                continue
            clips[name] = ("data:audio/wav;base64,"
                           + base64.b64encode(wav).decode())
        near, far, _unexplained, volume = (list(o["payload"]) + [0] * 4)[:4]
        sources.append({"clip": name, "pos": list(o["position"]),
                        "near": near, "far": far, "gain": volume,
                        "room": where.get(o["name"]) or ""})
    if not sources:
        return None
    return {"clips": clips, "sources": sources}


def _qrot(q, v):
    """The shader's rotation, with q as (x, y, z, w) -- see the vertex shader."""
    x, y, z, w = q
    c1 = (y*v[2] - z*v[1] + w*v[0], z*v[0] - x*v[2] + w*v[1],
          x*v[1] - y*v[0] + w*v[2])
    c2 = (y*c1[2] - z*c1[1], z*c1[0] - x*c1[2], x*c1[1] - y*c1[0])
    return tuple(v[i] + 2.0 * c2[i] for i in range(3))


def check_movers(path: Path, resources: Path) -> tuple[int, float, int]:
    """Pose every mover the way the shader will, and compare with animate().

    -> (movers checked, worst difference, models too big for the uniforms).
    The two paths share no code: `animate()` walks the parent chain and
    transforms vertices on the CPU, while this multiplies out the sampled
    node table exactly as the vertex shader does. If they agree the page is
    drawing what `tools/mod2obj.py` says it should.
    """
    packed = scene_movers(path, resources)
    if not packed:
        return 0, 0.0, 0
    graph = sg.parse(path.read_text(errors="replace"))
    worst, checked, big = 0.0, 0, 0
    cache: dict[Path, Model] = {}
    for o in graph["objects"]:
        if o["resource"] is None:
            continue
        found = sg.resolve(o, graph, resources)
        if not isinstance(found, Path) or found.suffix != ".mod":
            continue
        model = cache.get(found)
        if model is None:
            model = cache[found] = Model(found.read_bytes())
        if model._sec(1) is None or not model.animations():
            continue
        if len(model.nodes) > ANIM_MAX_NODES:
            big += 1
            continue
        anim = model.animations()[0]
        mv = next((m for m in packed["movers"] if m["at"] == o["line"]), None)
        if mv is None:                     # a model with no drawable groups
            continue
        table = packed["models"][mv["model"]]["frames"][0]
        verts, tris = model.animate(anim, 0.0)
        placed = place(o, verts, True)
        oq = [mv["quat"][1], mv["quat"][2], mv["quat"][3], mv["quat"][0]]
        cpu = [placed[i][0] for a, b, c in tris for i in (a, b, c)]
        k = 0
        for ni, node in enumerate(model.nodes):
            q = table[ni * 8:ni * 8 + 4]
            t = table[ni * 8 + 4:ni * 8 + 7]
            for g in range(node["group_first"],
                           node["group_first"] + node["group_count"]):
                gf, count = model.groups[g]
                for j in range(count - 2):
                    idx = [gf + j, gf + j + 1, gf + j + 2]
                    if j & 1:
                        idx[1], idx[2] = idx[2], idx[1]
                    for i in idx:
                        p = _qrot(q, model.vertices[i][0])
                        p = _qrot(oq, tuple(p[c] + t[c] for c in range(3)))
                        p = tuple(p[c] + mv["pos"][c] for c in range(3))
                        worst = max(worst, max(abs(p[c] - cpu[k][c])
                                               for c in range(3)))
                        k += 1
        checked += 1
    return checked, worst, big


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
          resources: Path | None = None,
          also: list | None = None) -> tuple[dict, dict]:
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
        for d in draws + list(also or []):
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
    ap.add_argument("--movers", action="store_true",
                    help="with --scene, check the shader's posing against "
                         "mod2obj's, and print nothing else")
    args = ap.parse_args(argv)

    if args.selftest:
        selftest()
        return 0
    if args.movers:
        n, worst, big = check_movers(args.src, args.resources)
        print(f"{n} animated objects posed, worst difference {worst:.1e}, "
              f"{big} models over {ANIM_MAX_NODES} nodes", file=sys.stderr)
        # the two paths are the same arithmetic; what is left is the
        # rounding of the shipped node table, magnified by how far a vertex
        # sits from its node's origin
        return 0 if worst < 1e-2 else 1
    if args.src is None or args.out is None:
        ap.error("src and -o are required")

    coll, spawns, room_graph, movers, sounds = None, None, None, None, None
    if args.scene:
        movers = scene_movers(args.src, args.resources)
        sounds = scene_sounds(args.src, args.resources)
        verts, tris, placed = scene_geometry(args.src, args.resources,
                                             animate=movers is not None)
        what = f"{placed} objects"
        if movers:
            what += f" &middot; {len(movers['movers'])} animated"
        if sounds:
            what += f" &middot; {len(sounds['sources'])} ambient sounds"
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
                           args.resources if args.scene else None,
                           [d for m in movers["models"] for d in m["draws"]]
                           if movers else None)
    stats = (f"{what} &middot; "
             f"{sum(d['count'] for d in mesh['draws']) // 3} triangles &middot; "
             f"{len(textures)} textures")
    page = (PAGE.replace("__TITLE__", args.src.stem)
                .replace("__STATS__", stats)
                .replace("__MESH__", json.dumps(mesh))
                .replace("__TEX__", json.dumps(textures))
                .replace("__COLL__", json.dumps(coll))
                .replace("__SPAWNS__", json.dumps(spawns))
                .replace("__ROOMS__", json.dumps(room_graph))
                .replace("__MOVE__", json.dumps(movers))
                .replace("__SOUND__", json.dumps(sounds)))
    args.out.write_text(page)
    print(f"{args.out}: {stats.replace('&middot;', '|')}, "
          f"{len(page)/2**20:.1f} MiB", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
