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

Usage:
    python3 tools/mod2html.py extracted/base/ml7z_castle.mod -o castle.html
    python3 tools/mod2html.py extracted/base/kurt.mod -o kurt.html --png png/
"""

from __future__ import annotations

import argparse
import base64
import json
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

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
<div id=hud><b>__TITLE__</b><br>__STATS__<br>drag to look &middot; WASD fly &middot;
R/F up-down &middot; shift faster</div>
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
  precision mediump float; uniform sampler2D uTex;
  varying vec2 vUV; varying float vZ;
  void main(){
    vec4 c = texture2D(uTex, vec2(vUV.x, 1.0 - vUV.y));
    if (c.a < 0.35) discard;
    float fog = clamp(vZ / 4000.0, 0.0, 0.75);
    gl_FragColor = vec4(mix(c.rgb, vec3(0.05,0.06,0.07), fog), 1.0);
  }`));
gl.linkProgram(prog); gl.useProgram(prog);

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

let yaw = 0, pitch = 0, pos = MESH.eye.slice(), speed = MESH.span / 12;
const keys = {};
addEventListener('keydown', e => keys[e.code] = true);
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
  const step = (d, s) => { for (let i=0;i<3;i++) pos[i] += d[i]*s; };
  if (keys.KeyW) step(fwd, v); if (keys.KeyS) step(fwd, -v);
  if (keys.KeyD) step(right, v); if (keys.KeyA) step(right, -v);
  if (keys.KeyR) pos[2] += v;  if (keys.KeyF) pos[2] -= v;

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

  for (const d of MESH.draws){
    gl.bindTexture(gl.TEXTURE_2D, textures[d.tex] || blank);
    gl.drawArrays(gl.TRIANGLES, d.first, d.count);
  }
  requestAnimationFrame(frame);
}
frame();
</script>
"""


def build(model: Model, png_dir: Path | None) -> tuple[dict, dict]:
    verts, tris = model.posed()
    by_tex: dict[str | None, list[int]] = {}
    for a, b, c in tris:
        by_tex.setdefault(verts[a][2], []).extend((a, b, c))

    data = bytearray()
    draws = []
    for tex, idx in by_tex.items():
        first = len(data) // 20
        for i in idx:
            pos, uv, _ = verts[i]
            data += struct.pack("<5f", *pos, *uv)
        draws.append({"tex": tex or "", "first": first, "count": len(idx)})

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
    ap.add_argument("src", type=Path)
    ap.add_argument("-o", "--out", type=Path, required=True)
    ap.add_argument("--png", type=Path, default=Path("png"),
                    help="directory of converted textures")
    args = ap.parse_args(argv)

    m = Model(args.src.read_bytes())
    mesh, textures = build(m, args.png if args.png.is_dir() else None)
    stats = (f"{len(m.nodes)} nodes &middot; "
             f"{sum(d['count'] for d in mesh['draws']) // 3} triangles &middot; "
             f"{len(textures)} textures")
    page = (PAGE.replace("__TITLE__", args.src.stem)
                .replace("__STATS__", stats)
                .replace("__MESH__", json.dumps(mesh))
                .replace("__TEX__", json.dumps(textures)))
    args.out.write_text(page)
    print(f"{args.out}: {stats.replace('&middot;', '|')}, "
          f"{len(page)/2**20:.1f} MiB", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
