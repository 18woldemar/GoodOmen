//! Models and textures on the GPU, and a level drawn out of them.
//!
//! Three rules from the data, and each one is silent if broken:
//!
//! - **Rows are bottom-up.** `.tex` stores them the way `glTexImage2D` wants
//!   them, because the original hands the buffer straight over. Nothing is
//!   flipped here; `tools/tex2png.py` flips because PNG counts the other way.
//! - **Channel order is BGRA**, and it is swapped on the way up rather than
//!   uploaded as `GL_BGRA`, which GLES 3.0 does not have. The original
//!   byte-swaps in place for the same reason.
//! - **Only animated models are in node-local space.** A static one — 1061 of
//!   2207 — is already in world coordinates and must **not** be moved to its
//!   object's position: `l3_maze.mod` comes out at 1.94x its true size and
//!   stops matching the plane distances in `l3_maze.bsp`.

use super::camera::Mat4;
use super::program;
use crate::formats::model::{Model, NO_RESOURCE};
use crate::formats::tex::Texture;
use crate::game::install::Install;
use glow::HasContext;
use std::collections::HashMap;

/// `#version 330 core`, body in the GLES 3.0 subset.
///
/// **Posing is rigid**, not skinned: one node per vertex, no weights. So a
/// vertex needs only its own node's quaternion and offset — two `vec4` of
/// uniform per node — and the whole model poses here rather than on the CPU.
/// That is what lets a level animate at all: half of every level moves.
const VERTEX: &str = r#"#version 330 core
layout (location = 0) in vec3 position;
layout (location = 1) in vec2 uv;
layout (location = 2) in float node;
layout (location = 3) in vec3 normal;
uniform mat4 view_projection;
uniform mat4 model;
uniform vec4 node_rotation[64];
uniform vec4 node_offset[64];
out vec2 vary_uv;
out float vary_depth;
out vec3 vary_world;
out vec3 vary_normal;

// (w, x, y, z), the order the models store one in
vec3 turn(vec4 q, vec3 p) {
    vec3 v = q.yzw;
    return p + 2.0 * cross(v, cross(v, p) + q.x * p);
}

void main() {
    int i = int(node);
    vec3 posed = turn(node_rotation[i], position) + node_offset[i].xyz;
    vec4 world = model * vec4(posed, 1.0);
    vary_world = world.xyz;
    // the model matrix is a rotation and a translation, so the rotation
    // alone carries the normal -- no inverse transpose needed
    vary_normal = mat3(model) * turn(node_rotation[i], normal);
    vary_uv = uv;
    gl_Position = view_projection * world;
    vary_depth = gl_Position.w;
}
"#;

/// How many nodes fit in the uniform block. A model with more stays in its
/// bind pose, which is what `tools/mod2html.py` does for the same reason.
pub const MAX_NODES: usize = 64;

/// No lighting yet: the levels ship static lights as objects and nothing
/// reads them. A little distance shading instead, so that shape is visible
/// at all rather than a flat silhouette.
/// The levels ship their lighting as objects — `OBJ_STATICLIGHT`, 2080 of
/// them over the corpus — and this is what reads it. The **falloff is ours**,
/// not the original's: nothing in the data says what curve it used, so this
/// takes the light's own radius as the reach and squares a linear ramp
/// inside it, which is bounded and looks right. The colour, radius and
/// intensity are all the object's, out of its payload.
const FRAGMENT: &str = r#"#version 330 core
in vec2 vary_uv;
in float vary_depth;
in vec3 vary_world;
in vec3 vary_normal;
uniform sampler2D albedo;
uniform float alpha_test;
uniform vec4 fog;          // start, end, enabled, unused
uniform vec3 fog_colour;
uniform int light_count;
uniform vec3 light_position[16];
uniform vec3 light_colour[16];
uniform float light_radius[16];
out vec4 fragment;
void main() {
    vec4 texel = texture(albedo, vary_uv);
    if (texel.a < alpha_test) discard;
    vec3 n = normalize(vary_normal);
    // MDK2's static lights are small and local -- the median radius is 15
    // units in arenas a hundred across -- so they *add* to a base level
    // rather than being the whole of the lighting. Without one the levels
    // read almost black, which is a rendering choice made here and not
    // something the data says.
    vec3 lit = vec3(0.75);
    for (int i = 0; i < light_count; i++) {
        vec3 to = light_position[i] - vary_world;
        float d = length(to);
        float reach = max(1.0 - d / light_radius[i], 0.0);
        lit += light_colour[i] * reach * reach * max(dot(n, to / max(d, 0.001)), 0.0);
    }
    vec3 colour = texel.rgb * min(lit, vec3(1.6));
    // MDK2's fog is fixed-function OpenGL and the mode is read, not guessed:
    // `glFogi(GL_FOG_MODE, GL_LINEAR)` at 0x454492, start and end through
    // `chFogStartEnd`, the colour through `chFogColor`. GL 3.3 core has no
    // fixed-function fog, so the same arithmetic is done here.
    float f = clamp((fog.y - vary_depth) / max(fog.y - fog.x, 0.001), 0.0, 1.0);
    fragment = vec4(mix(fog_colour, colour, mix(1.0, f, fog.z)), 1.0);
}
"#;

/// How many lights reach the shader at once — the nearest to the camera.
pub const MAX_LIGHTS: usize = 16;

/// One `OBJ_STATICLIGHT`, as its payload describes it.
///
/// The payload was unexplained until it was surveyed over all 2080: slot 0 is
/// a **packed `0xRRGGBB`** — every one of them a whole number in range, and
/// the commonest decode to a pale lilac, a deep blue, a warm yellow — slot 1
/// is the **radius** in units, slot 2 the **intensity**, and slot 3 a flag
/// field taking exactly four values.
#[derive(Clone, Copy, Debug)]
pub struct Light {
    pub position: [f32; 3],
    pub colour: [f32; 3],
    pub radius: f32,
    pub intensity: f32,
}

impl Light {
    /// From the four payload numbers and a position.
    pub fn from_payload(position: [f64; 3], payload: [f64; 4]) -> Light {
        let packed = payload[0] as u32;
        Light {
            position: [position[0] as f32, position[1] as f32, position[2] as f32],
            colour: [
                ((packed >> 16) & 255) as f32 / 255.0,
                ((packed >> 8) & 255) as f32 / 255.0,
                (packed & 255) as f32 / 255.0,
            ],
            radius: payload[1].max(1.0) as f32,
            intensity: payload[2].max(0.0) as f32,
        }
    }
}

pub struct GpuTexture {
    pub texture: glow::Texture,
    /// From the `u32` at 0x10 of the `.tex`: 4 means the surface carries
    /// alpha and is alpha-tested, 3 means it is opaque. Exact over all 755.
    pub alpha: bool,
}

/// One run of vertices drawn with one texture.
struct Part {
    texture: Option<String>,
    /// The node these triangles belong to. Parts break on a node change as
    /// well as a texture change, because the scripts hide **nodes**:
    /// `omGobGMSetSltVisible(gob, omGobGMGetSltIndexByName(gob, "EL_CENTER"), 0)`.
    node: u32,
    first: i32,
    count: i32,
}

pub struct GpuModel {
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    parts: Vec<Part>,
    pub triangles: usize,
    /// An animated model's vertices are node-local and have to be placed at
    /// the object; a static one's are already in the world.
    pub animated: bool,
    /// Kept so the node transforms can be sampled per frame. `None` when the
    /// model is not posed here — static, or more than [`MAX_NODES`] nodes.
    pub(crate) posed_here: Option<Model>,
}

#[derive(Default)]
pub struct Scene {
    models: HashMap<String, GpuModel>,
    textures: HashMap<String, GpuTexture>,
    /// White, for a node that names no texture.
    blank: Option<glow::Texture>,
    /// Every `OBJ_STATICLIGHT` the level placed.
    pub lights: Vec<Light>,
    /// Compiled once. It had been compiled and deleted **every frame**,
    /// which is two shader compiles and a link per frame for nothing.
    shader: Option<glow::Program>,
    /// `(model name, its transform)`, in the order the scene graph gave them.
    draws: Vec<(String, Mat4)>,
    /// The object each draw came from, so a moved object moves on screen.
    owners: Vec<Option<crate::game::world::Id>>,
    /// The animation each draw is playing, chosen by `omAnimPlay`. `None`
    /// means animation 0, which is the game's own default.
    playing: Vec<Option<f64>>,
    /// Node indices the scripts have hidden, per draw.
    hidden: Vec<Vec<u32>>,
    /// The game's own fog, from `chFogStartEnd`, `chFogColor` and
    /// `chFogEnable` — see [`Fog`].
    pub fog: Fog,
    /// Models refused because their vertices are not finite or not of a
    /// sane size — the uninitialised-root family.
    pub refused: usize,
    refused_names: std::collections::BTreeSet<String>,
    /// The room each draw belongs to, or `None` for an object in no room at
    /// all — which is **never culled**, as the original does not cull it.
    rooms: Vec<Option<usize>>,
    pub missing: usize,
}

/// The game's fog, which is fixed-function OpenGL in the original.
///
/// `chFogEnable` is `glEnable(GL_FOG)` (0x4542b0), `chFogStartEnd` is two
/// `glFogf` calls with `GL_FOG_START` and `GL_FOG_END` (0x4542f0), and
/// `chFogColor` is `glFogfv(GL_FOG_COLOR, ...)` (0x454376). **The mode is
/// `GL_LINEAR`**, set once at 0x454492 and never changed — so the falloff is
/// `(end - z) / (end - start)`, clamped, and not exponential.
///
/// The defaults here are ours and are only what a level that sets nothing
/// gets: 56 of the scripts' fog calls are `chFogEnable()` and every level
/// that enables it also sets a colour.
#[derive(Clone, Copy, Debug)]
pub struct Fog {
    pub near: f32,
    pub far: f32,
    pub colour: [f32; 3],
    pub on: bool,
}

impl Default for Fog {
    fn default() -> Fog {
        Fog { near: 50.0, far: 600.0, colour: [0.05, 0.06, 0.09], on: false }
    }
}

/// The triangle's own normal, right-handed over the winding the strips give.
fn face_normal(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> [f32; 3] {
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len == 0.0 {
        [0.0, 0.0, 1.0]
    } else {
        [(n[0] / len) as f32, (n[1] / len) as f32, (n[2] / len) as f32]
    }
}

/// Every node's transform for animation 0 at `clock`, packed for the shader.
///
/// The animation record's float at +8 is a **signed playback rate**, so a
/// loop lasts `1 / |rate|` — median about 1.5 s — and the sign is the
/// argument, not a length: 99 of 5165 records are negative and
/// `omAnimSetSpeed(door, ANIM_OPEN, -1)` is how `elevators.lua` shuts a door
/// it opened.
fn node_pose(model: &Model, chosen: Option<f64>, clock: f64) -> (Vec<f32>, Vec<f32>) {
    let mut rotation = vec![0.0f32; MAX_NODES * 4];
    let mut offset = vec![0.0f32; MAX_NODES * 4];
    for i in 0..MAX_NODES {
        rotation[i * 4] = 1.0;
    }
    // the animation `omAnimPlay` named, by its id, or animation 0 -- which
    // is the game's own default and is an animation, not a bind pose
    let anim = chosen
        .and_then(|id| model.animations.iter().find(|a| a.id as f64 == id))
        .or_else(|| model.animations.first());
    let Some(anim) = anim else { return (rotation, offset) };
    let t = (clock * anim.loop_rate() as f64).fract();
    for (i, (q, o)) in model.node_world(anim, t).into_iter().enumerate().take(MAX_NODES) {
        for c in 0..4 {
            rotation[i * 4 + c] = q[c] as f32;
        }
        for c in 0..3 {
            offset[i * 4 + c] = o[c] as f32;
        }
    }
    (rotation, offset)
}

/// Turn a decoded BGRA level into the RGBA GL expects.
fn to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut out = bgra.to_vec();
    for p in out.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    out
}

impl Scene {
    /// # Safety
    /// A GL context must be current on this thread.
    pub unsafe fn upload_texture(&mut self, gl: &glow::Context, name: &str, tex: &Texture) {
        let handle = match gl.create_texture() {
            Ok(t) => t,
            Err(_) => return,
        };
        gl.bind_texture(glow::TEXTURE_2D, Some(handle));
        for (level, image) in tex.levels.iter().enumerate() {
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                level as i32,
                glow::RGBA8 as i32,
                image.width as i32,
                image.height as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&to_rgba(&image.bgra))),
            );
        }
        // the chain is complete down to 1x1, so there is nothing to generate
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR_MIPMAP_LINEAR as i32,
        );
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAX_LEVEL,
            tex.levels.len() as i32 - 1,
        );
        self.textures.insert(
            name.to_ascii_lowercase(),
            GpuTexture { texture: handle, alpha: tex.channels == 4 },
        );
    }

    /// # Safety
    /// A GL context must be current on this thread.
    unsafe fn upload_model(&mut self, gl: &glow::Context, name: &str, model: &Model) {
        // A model that poses in the shader sends its vertices **node-local**
        // and lets the uniforms move them; anything else sends them already
        // posed and rides on node 0 being the identity.
        let here = model.animated()
            && model.nodes.len() <= MAX_NODES
            && !model.animations.is_empty();
        let mesh = if here { model.local() } else { model.posed() };

        // **A model whose vertices are not finite and not of a sane size is
        // refused**, because one bad vertex takes the whole draw call with
        // it.
        //
        // Eight models carry an uninitialised translation on a dummy root
        // that draws nothing: six NaN — `fishy`, `chuckles`, `flyer`,
        // `l1_r7cloudspin`, `l3_slide`, `l7_dshwang` — and two merely
        // absurd, `cloak` at 3.5e26 units across and `zizzy` at 2.2e24.
        // **None of them reaches this guard**, and that is the point worth
        // recording: all eight are animated, so the mesh above is
        // `local()`, which never accumulates a root translation. Only the
        // bind pose does, and only a *static* model takes it. The guard is
        // there for a static model with the same defect, which the corpus
        // does not happen to contain.
        const SANE: f64 = 1.0e4;
        let ok = mesh.positions.iter().all(|p| {
            p.iter().all(|c| c.is_finite() && c.abs() < SANE)
        });
        if !ok {
            self.refused += 1;
            return;
        }
        let mut data: Vec<f32> = Vec::with_capacity(mesh.triangles.len() * 3 * 9);
        let mut parts: Vec<Part> = Vec::new();

        for tri in &mesh.triangles {
            // a node has one resource, so a run of triangles shares one
            let slot = mesh.resource[tri[0] as usize];
            // **0xFF means the node draws nothing**, and it has to be taken
            // at its word: drawing it with a blank white texture instead put
            // an untextured lump in the middle of every room.
            if slot == NO_RESOURCE {
                continue;
            }
            let texture = (slot != NO_RESOURCE)
                .then(|| model.refs.get(slot as usize))
                .flatten()
                .filter(|n| n.to_ascii_lowercase().ends_with(".tex"))
                .map(|n| n.to_ascii_lowercase());
            let node = mesh.node[tri[0] as usize];
            match parts.last_mut() {
                Some(p) if p.texture == texture && p.node == node => p.count += 3,
                _ => parts.push(Part {
                    texture,
                    node,
                    first: (data.len() / 9) as i32,
                    count: 3,
                }),
            }
            // The vertex record carries no normal -- its 12 spare bytes are
            // not one, and three readings of them were refuted -- so the
            // triangle's own is used. Flat shading, which is what the
            // geometry of this era looks like anyway.
            let face = face_normal(
                mesh.positions[tri[0] as usize],
                mesh.positions[tri[1] as usize],
                mesh.positions[tri[2] as usize],
            );
            for &v in tri {
                let p = mesh.positions[v as usize];
                let uv = mesh.uvs[v as usize];
                let node = if here { mesh.node[v as usize] as f32 } else { 0.0 };
                data.extend_from_slice(&[
                    p[0] as f32, p[1] as f32, p[2] as f32,
                    uv[0], uv[1], node,
                    face[0], face[1], face[2],
                ]);
            }
        }

        let vao = gl.create_vertex_array().expect("vertex array");
        gl.bind_vertex_array(Some(vao));
        let vbo = gl.create_buffer().expect("buffer");
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_u8_slice(
            glow::ARRAY_BUFFER,
            std::slice::from_raw_parts(data.as_ptr() as *const u8, std::mem::size_of_val(&data[..])),
            glow::STATIC_DRAW,
        );
        let stride = 9 * std::mem::size_of::<f32>() as i32;
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, stride, 0);
        gl.enable_vertex_attrib_array(1);
        gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, stride, 12);
        gl.enable_vertex_attrib_array(2);
        gl.vertex_attrib_pointer_f32(2, 1, glow::FLOAT, false, stride, 20);
        gl.enable_vertex_attrib_array(3);
        gl.vertex_attrib_pointer_f32(3, 3, glow::FLOAT, false, stride, 24);

        self.models.insert(
            name.to_ascii_lowercase(),
            GpuModel {
                vao,
                vbo,
                parts,
                triangles: mesh.triangles.len(),
                animated: model.animated(),
                posed_here: here.then(|| model.clone()),
            },
        );
    }

    /// Load a model and everything it draws with, once.
    ///
    /// # Safety
    /// A GL context must be current on this thread.
    pub unsafe fn load(&mut self, gl: &glow::Context, install: &mut Install, name: &str) -> bool {
        let key = name.to_ascii_lowercase();
        if self.models.contains_key(&key) {
            return true;
        }
        if self.refused_names.contains(&key) {
            return false;
        }
        let Ok(bytes) = install.read(&format!("{key}.mod")) else {
            self.missing += 1;
            return false;
        };
        let Ok(model) = Model::parse(&bytes) else {
            self.missing += 1;
            return false;
        };
        for reference in &model.refs {
            let lower = reference.to_ascii_lowercase();
            if !lower.ends_with(".tex") || self.textures.contains_key(&lower) {
                continue;
            }
            if let Ok(bytes) = install.read(&lower) {
                if let Ok(tex) = Texture::parse(&bytes) {
                    self.upload_texture(gl, &lower, &tex);
                }
            }
        }
        self.upload_model(gl, &key, &model);
        if !self.models.contains_key(&key) {
            self.refused_names.insert(key);
            return false;
        }
        true
    }

    /// Is a loaded model animated, and so in node-local space?
    pub fn is_animated(&self, name: &str) -> bool {
        self.models
            .get(&name.to_ascii_lowercase())
            .map(|m| m.animated)
            .unwrap_or(false)
    }

    /// Put a loaded model into the draw list at a transform, in a room.
    pub fn place(
        &mut self,
        name: &str,
        transform: Mat4,
        room: Option<usize>,
        owner: Option<crate::game::world::Id>,
    ) {
        self.draws.push((name.to_ascii_lowercase(), transform));
        self.rooms.push(room);
        self.owners.push(owner);
        self.playing.push(None);
        self.hidden.push(Vec::new());
    }

    /// Follow the world: an object the scripts moved moves on screen, and one
    /// they told to play an animation plays that one.
    ///
    /// Only the models that are **posed here** take a transform from their
    /// object — a static model's vertices are already in world space, so
    /// moving it would move it twice.
    pub fn follow(
        &mut self,
        world: &crate::game::world::World,
        playing: &std::collections::BTreeMap<String, f64>,
        hidden: &std::collections::BTreeSet<(String, String)>,
    ) {
        for i in 0..self.draws.len() {
            let Some(id) = self.owners[i] else { continue };
            let Some(gob) = world.get(id) else { continue };
            self.playing[i] = playing.get(&gob.name).copied();
            // the slot names are the model's node names, resolved here
            // because this is where the model is
            self.hidden[i].clear();
            if hidden.iter().any(|(g, _)| *g == gob.name) {
                if let Some(model) = self.models.get(&self.draws[i].0) {
                    if let Some(source) = &model.posed_here {
                        for (n, node) in source.nodes.iter().enumerate() {
                            if hidden.contains(&(gob.name.clone(), node.name.clone())) {
                                self.hidden[i].push(n as u32);
                            }
                        }
                    }
                }
            }
            if self
                .models
                .get(&self.draws[i].0)
                .is_some_and(|m| m.posed_here.is_some())
            {
                self.draws[i].1 = Mat4::translation([
                    gob.position[0] as f32,
                    gob.position[1] as f32,
                    gob.position[2] as f32,
                ])
                .times(&Mat4::rotation([
                    gob.rotation[0] as f32,
                    gob.rotation[1] as f32,
                    gob.rotation[2] as f32,
                    gob.rotation[3] as f32,
                ]));
            }
        }
    }

    pub fn draw_count(&self) -> usize {
        self.draws.len()
    }

    /// How far the shader-posed models move between two clocks, on the CPU.
    /// If this is zero the uniforms are not the problem.
    pub fn pose_delta(&self, a: f64, b: f64) -> f64 {
        let mut total = 0.0;
        for (name, _) in &self.draws {
            let Some(m) = self.models.get(name) else { continue };
            let Some(source) = &m.posed_here else { continue };
            let (ra, oa) = node_pose(source, None, a);
            let (rb, ob) = node_pose(source, None, b);
            total += ra.iter().zip(&rb).map(|(x, y)| (x - y).abs() as f64).sum::<f64>();
            total += oa.iter().zip(&ob).map(|(x, y)| (x - y).abs() as f64).sum::<f64>();
        }
        total
    }

    /// How many draws pose in the shader — the ones that can move at all.
    pub fn posed_draws(&self) -> usize {
        self.draws
            .iter()
            .filter(|(n, _)| self.models.get(n).is_some_and(|m| m.posed_here.is_some()))
            .count()
    }

    pub fn triangle_count(&self) -> usize {
        self.draws
            .iter()
            .filter_map(|(n, _)| self.models.get(n))
            .map(|m| m.triangles)
            .sum()
    }

    /// The box every placed model's own bounding box fits in, which is what a
    /// camera with nothing better to go on can frame.
    pub fn bounds(&self) -> ([f32; 3], [f32; 3]) {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for (_, m) in &self.draws {
            let p = [m.0[12], m.0[13], m.0[14]];
            for c in 0..3 {
                lo[c] = lo[c].min(p[c]);
                hi[c] = hi[c].max(p[c]);
            }
        }
        if lo[0] > hi[0] {
            (([0.0; 3]), ([0.0; 3]))
        } else {
            (lo, hi)
        }
    }

    /// # Safety
    /// A GL context must be current on this thread.
    /// `fade` is the distance at which the debug shading bottoms out; it has
    /// to be scaled to the scene, since a level spans 1558 units and a single
    /// model two. `clock` is seconds since the level started: every animated
    /// model plays its **animation 0** at its own rate, and the record's
    /// float at +8 is a signed playback rate, so a loop lasts `1 / |rate|`.
    pub unsafe fn draw(
        &mut self,
        gl: &glow::Context,
        view_projection: &Mat4,
        fog: Fog,
        visible: Option<&std::collections::BTreeSet<usize>>,
        clock: f64,
        eye: [f32; 3],
    ) -> Result<usize, String> {
        let shader = match self.shader {
            Some(s) => s,
            None => {
                let s = program(gl, VERTEX, FRAGMENT)?;
                self.shader = Some(s);
                s
            }
        };
        if self.blank.is_none() {
            let white = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(white));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                1,
                1,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&[220u8, 220, 220, 255])),
            );
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            self.blank = Some(white);
        }

        gl.enable(glow::DEPTH_TEST);
        gl.enable(glow::CULL_FACE);
        gl.cull_face(glow::BACK);
        gl.use_program(Some(shader));
        let vp = gl.get_uniform_location(shader, "view_projection");
        gl.uniform_matrix_4_f32_slice(vp.as_ref(), false, &view_projection.0);
        let model_at = gl.get_uniform_location(shader, "model");
        // **`[0]`, not the bare name.** A uniform array's location is the
        // location of its first element, and a driver is entitled to return
        // nothing for the array's own name. When it does, the upload is a
        // silent no-op: every node keeps the zero quaternion, every vertex
        // stays node-local, and the model piles into a lump at its own
        // origin. That is exactly what Kurt looked like.
        let rotation_at = gl.get_uniform_location(shader, "node_rotation[0]");
        let offset_at = gl.get_uniform_location(shader, "node_offset[0]");
        if rotation_at.is_none() || offset_at.is_none() {
            return Err("the node transform uniforms are not where the shader says".into());
        }
        // node 0 as the identity is what an unposed model rides on
        let identity: Vec<f32> = std::iter::repeat([1.0f32, 0.0, 0.0, 0.0])
            .take(MAX_NODES)
            .flatten()
            .collect();
        let zero = vec![0.0f32; MAX_NODES * 4];
        let alpha_at = gl.get_uniform_location(shader, "alpha_test");
        gl.uniform_4_f32(
            gl.get_uniform_location(shader, "fog").as_ref(),
            fog.near,
            fog.far.max(fog.near + 1.0),
            if fog.on { 1.0 } else { 0.0 },
            0.0,
        );
        gl.uniform_3_f32(
            gl.get_uniform_location(shader, "fog_colour").as_ref(),
            fog.colour[0],
            fog.colour[1],
            fog.colour[2],
        );
        // the nearest lights to the camera, which is the cheap choice and
        // enough for a room: a level ships hundreds
        let mut near: Vec<&Light> = self.lights.iter().collect();
        near.sort_by(|a, b| {
            let d = |l: &Light| {
                (0..3).map(|c| (l.position[c] - eye[c]).powi(2)).sum::<f32>()
            };
            d(a).total_cmp(&d(b))
        });
        near.truncate(MAX_LIGHTS);
        let mut positions = Vec::with_capacity(MAX_LIGHTS * 3);
        let mut colours = Vec::with_capacity(MAX_LIGHTS * 3);
        let mut radii = Vec::with_capacity(MAX_LIGHTS);
        for l in &near {
            positions.extend_from_slice(&l.position);
            colours.extend(l.colour.iter().map(|c| c * l.intensity));
            radii.push(l.radius);
        }
        gl.uniform_1_i32(
            gl.get_uniform_location(shader, "light_count").as_ref(),
            near.len() as i32,
        );
        if !near.is_empty() {
            gl.uniform_3_f32_slice(
                gl.get_uniform_location(shader, "light_position").as_ref(), &positions);
            gl.uniform_3_f32_slice(
                gl.get_uniform_location(shader, "light_colour").as_ref(), &colours);
            gl.uniform_1_f32_slice(
                gl.get_uniform_location(shader, "light_radius").as_ref(), &radii);
        }

        gl.active_texture(glow::TEXTURE0);
        gl.uniform_1_i32(gl.get_uniform_location(shader, "albedo").as_ref(), 0);

        let mut drawn = 0usize;
        let mut unposed = false;
        for (i, (name, transform)) in self.draws.iter().enumerate() {
            // the authored cull list: a room draws the rooms it names, and
            // an object in no room is always drawn
            if let (Some(visible), Some(room)) = (visible, self.rooms[i]) {
                if !visible.contains(&room) {
                    continue;
                }
            }
            let Some(model) = self.models.get(name) else { continue };
            drawn += model.triangles;
            gl.uniform_matrix_4_f32_slice(model_at.as_ref(), false, &transform.0);
            // 512 floats of uniform per draw is worth not repeating: most
            // models are static and want the same identity every time
            match &model.posed_here {
                Some(source) => {
                    let (rotation, offset) = node_pose(source, self.playing[i], clock);
                    gl.uniform_4_f32_slice(rotation_at.as_ref(), &rotation);
                    gl.uniform_4_f32_slice(offset_at.as_ref(), &offset);
                    unposed = false;
                }
                None if !unposed => {
                    gl.uniform_4_f32_slice(rotation_at.as_ref(), &identity);
                    gl.uniform_4_f32_slice(offset_at.as_ref(), &zero);
                    unposed = true;
                }
                None => {}
            }
            gl.bind_vertex_array(Some(model.vao));
            for part in &model.parts {
                if self.hidden[i].contains(&part.node) {
                    continue;
                }
                let bound = part
                    .texture
                    .as_ref()
                    .and_then(|t| self.textures.get(t));
                gl.bind_texture(
                    glow::TEXTURE_2D,
                    Some(bound.map(|t| t.texture).unwrap_or(self.blank.unwrap())),
                );
                // an opaque surface must not discard, or every texel whose
                // alpha channel happens to be low vanishes
                gl.uniform_1_f32(
                    alpha_at.as_ref(),
                    if bound.map(|t| t.alpha).unwrap_or(false) { 0.5 } else { -1.0 },
                );
                gl.draw_arrays(glow::TRIANGLES, part.first, part.count);
            }
        }
        Ok(drawn)
    }

    /// # Safety
    /// A GL context must be current on this thread.
    pub unsafe fn delete(self, gl: &glow::Context) {
        for m in self.models.values() {
            gl.delete_buffer(m.vbo);
            gl.delete_vertex_array(m.vao);
        }
        for t in self.textures.values() {
            gl.delete_texture(t.texture);
        }
        if let Some(t) = self.blank {
            gl.delete_texture(t);
        }
        if let Some(s) = self.shader {
            gl.delete_program(s);
        }
    }
}
