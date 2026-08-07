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
uniform mat4 view_projection;
uniform mat4 model;
uniform vec4 node_rotation[64];
uniform vec4 node_offset[64];
out vec2 vary_uv;
out float vary_depth;

// (w, x, y, z), the order the models store one in
vec3 turn(vec4 q, vec3 p) {
    vec3 v = q.yzw;
    return p + 2.0 * cross(v, cross(v, p) + q.x * p);
}

void main() {
    int i = int(node);
    vec3 posed = turn(node_rotation[i], position) + node_offset[i].xyz;
    vec4 world = model * vec4(posed, 1.0);
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
const FRAGMENT: &str = r#"#version 330 core
in vec2 vary_uv;
in float vary_depth;
uniform sampler2D albedo;
uniform float alpha_test;
uniform float fade_distance;
out vec4 fragment;
void main() {
    vec4 texel = texture(albedo, vary_uv);
    if (texel.a < alpha_test) discard;
    float fade = clamp(1.0 - vary_depth / fade_distance, 0.45, 1.0);
    fragment = vec4(texel.rgb * fade, 1.0);
}
"#;

pub struct GpuTexture {
    pub texture: glow::Texture,
    /// From the `u32` at 0x10 of the `.tex`: 4 means the surface carries
    /// alpha and is alpha-tested, 3 means it is opaque. Exact over all 755.
    pub alpha: bool,
}

/// One run of vertices drawn with one texture.
struct Part {
    texture: Option<String>,
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
    posed_here: Option<Model>,
}

#[derive(Default)]
pub struct Scene {
    models: HashMap<String, GpuModel>,
    textures: HashMap<String, GpuTexture>,
    /// White, for a node that names no texture.
    blank: Option<glow::Texture>,
    /// `(model name, its transform)`, in the order the scene graph gave them.
    draws: Vec<(String, Mat4)>,
    /// The room each draw belongs to, or `None` for an object in no room at
    /// all — which is **never culled**, as the original does not cull it.
    rooms: Vec<Option<usize>>,
    pub missing: usize,
}

/// Every node's transform for animation 0 at `clock`, packed for the shader.
///
/// The animation record's float at +8 is a **signed playback rate**, so a
/// loop lasts `1 / |rate|` — median about 1.5 s — and the sign is the
/// argument, not a length: 99 of 5165 records are negative and
/// `omAnimSetSpeed(door, ANIM_OPEN, -1)` is how `elevators.lua` shuts a door
/// it opened.
fn node_pose(model: &Model, clock: f64) -> (Vec<f32>, Vec<f32>) {
    let mut rotation = vec![0.0f32; MAX_NODES * 4];
    let mut offset = vec![0.0f32; MAX_NODES * 4];
    for i in 0..MAX_NODES {
        rotation[i * 4] = 1.0;
    }
    let Some(anim) = model.animations.first() else { return (rotation, offset) };
    let rate = anim.rate.abs() as f64;
    let t = if rate > 0.0 { (clock * rate).fract() } else { 0.0 };
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
        let mut data: Vec<f32> = Vec::with_capacity(mesh.triangles.len() * 3 * 6);
        let mut parts: Vec<Part> = Vec::new();

        for tri in &mesh.triangles {
            // a node has one resource, so a run of triangles shares one
            let slot = mesh.resource[tri[0] as usize];
            let texture = (slot != NO_RESOURCE)
                .then(|| model.refs.get(slot as usize))
                .flatten()
                .filter(|n| n.to_ascii_lowercase().ends_with(".tex"))
                .map(|n| n.to_ascii_lowercase());
            match parts.last_mut() {
                Some(p) if p.texture == texture => p.count += 3,
                _ => parts.push(Part {
                    texture,
                    first: (data.len() / 5) as i32,
                    count: 3,
                }),
            }
            for &v in tri {
                let p = mesh.positions[v as usize];
                let uv = mesh.uvs[v as usize];
                let node = if here { mesh.node[v as usize] as f32 } else { 0.0 };
                data.extend_from_slice(&[
                    p[0] as f32, p[1] as f32, p[2] as f32, uv[0], uv[1], node,
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
        let stride = 6 * std::mem::size_of::<f32>() as i32;
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, stride, 0);
        gl.enable_vertex_attrib_array(1);
        gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, stride, 12);
        gl.enable_vertex_attrib_array(2);
        gl.vertex_attrib_pointer_f32(2, 1, glow::FLOAT, false, stride, 20);

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
    pub fn place(&mut self, name: &str, transform: Mat4, room: Option<usize>) {
        self.draws.push((name.to_ascii_lowercase(), transform));
        self.rooms.push(room);
    }

    pub fn draw_count(&self) -> usize {
        self.draws.len()
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
        fade: f32,
        visible: Option<&std::collections::BTreeSet<usize>>,
        clock: f64,
    ) -> Result<usize, String> {
        let shader = program(gl, VERTEX, FRAGMENT)?;
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
        let rotation_at = gl.get_uniform_location(shader, "node_rotation");
        let offset_at = gl.get_uniform_location(shader, "node_offset");
        // node 0 as the identity is what an unposed model rides on
        let identity: Vec<f32> = std::iter::repeat([1.0f32, 0.0, 0.0, 0.0])
            .take(MAX_NODES)
            .flatten()
            .collect();
        let zero = vec![0.0f32; MAX_NODES * 4];
        let alpha_at = gl.get_uniform_location(shader, "alpha_test");
        gl.uniform_1_f32(
            gl.get_uniform_location(shader, "fade_distance").as_ref(),
            fade.max(1.0),
        );
        gl.active_texture(glow::TEXTURE0);
        gl.uniform_1_i32(gl.get_uniform_location(shader, "albedo").as_ref(), 0);

        let mut drawn = 0usize;
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
            match &model.posed_here {
                Some(source) => {
                    let (rotation, offset) = node_pose(source, clock);
                    gl.uniform_4_f32_slice(rotation_at.as_ref(), &rotation);
                    gl.uniform_4_f32_slice(offset_at.as_ref(), &offset);
                }
                None => {
                    gl.uniform_4_f32_slice(rotation_at.as_ref(), &identity);
                    gl.uniform_4_f32_slice(offset_at.as_ref(), &zero);
                }
            }
            gl.bind_vertex_array(Some(model.vao));
            for part in &model.parts {
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
        gl.delete_program(shader);
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
    }
}
