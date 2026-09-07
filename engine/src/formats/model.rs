//! `.mod` — the Omen renderer's models, geometry and animation.
//!
//! A 224-byte header, twelve `u16` counts at 0x08, then **thirteen section
//! offsets at 0x20** with 0xFFFFFFFF meaning the section is absent. The
//! counts are not in section order — the mapping below is `tools/mod2obj.py`'s
//! and is the one the corpus supports.
//!
//! ```text
//! 0  node table       136-byte records, counts[5] of them
//! 1  animation table  32 bytes, counts[0]
//! 2  channels          8 bytes, counts[1]
//! 3  targets           4 bytes, counts[2]
//! 4  keys              8 bytes, counts[3]
//! 5  value pool       16 bytes, typed by the channel's kind
//! 6  strip groups     32 bytes, counts[6]
//! 7  vertices         32 bytes, counts[7]
//! 8  resources        21 bytes, counts[8]
//! ```
//!
//! **There is no index list.** Each group names a run of consecutive
//! vertices and that run is a *triangle strip*, which is why the winding has
//! to be flipped on every odd triangle.
//!
//! Two things about this format are easy to get wrong and both are silent:
//!
//! - **Only animated models are in node-local space.** A static model — 1061
//!   of the 2207 — is already in world coordinates, and adding the node
//!   translations double-counts: `l3_maze.mod` comes out at 1.94x its true
//!   size and stops matching the plane distances in `l3_maze.bsp`.
//! - **Quaternions are stored (w, x, y, z).** Reading them as (x, y, z, w)
//!   finds no identity quaternion anywhere in the file, which is the check
//!   that settles it: an unrotated node stores (1, -0, -0, -0).
//!
//! Posing is **rigid**, not skinned: one node per vertex, no weights, so a
//! vertex needs only its own node's quaternion and offset.
//!
//! `../../tools/mod2obj.py` is the reference and `tools/modcheck.py` holds
//! the two to each other over all 2207 models.

pub const TYPE_MOD: u32 = 2002;
const HEADER_SIZE: usize = 224;
const NODE_STRIDE: usize = 136;
const GROUP_STRIDE: usize = 32;
const VERTEX_STRIDE: usize = 32;
const REF_NAME: usize = 16;
/// `char name[16] + char ext[5]`, e.g. "kurt" + ".tex".
const REF_STRIDE: usize = 21;
const ANIM_STRIDE: usize = 32;
const CHANNEL_STRIDE: usize = 8;
const TARGET_STRIDE: usize = 4;
const KEY_STRIDE: usize = 8;
const VALUE_STRIDE: usize = 16;
const ABSENT: u32 = 0xFFFF_FFFF;

/// Section-3 target kinds. 32..36 are scalars driving a sound — volume, min
/// and max distance — and are carried through untouched.
pub const KIND_TRANSLATION: u8 = 1;
pub const KIND_ROTATION: u8 = 2;

/// **A model can carry the camera a cutscene is filmed from**, and kind 0x68
/// is the switch that says so: it is the one the applier at 0x478800 turns
/// into a write of `camera + 100, +0xb4`, the field 0x46a880 tests before it
/// takes the camera's whole transform from a node of another gob instead of
/// following the player.
///
/// 102 of the 2207 models have one, every one of them a cutscene, a minigame
/// or an inventory screen, and in every one the node it names draws nothing
/// ([`NO_RESOURCE`]). The **names cannot be the rule** — `L0A_CAMERA` and
/// `ML1B_CAMERA` say so but `BOX01`, `SPHERE04` and `TIMMY` are cameras too —
/// and the binary holds no such string to match against.
///
/// The settings sit beside it as kinds 0x5a..=0x60 and 0x66, of which only
/// [`KIND_FOV`] is read here: 0x5c is **degrees**, 3.25 to 90 over the corpus
/// and 33, 30 and 25 the three commonest.
pub const KIND_CAMERA: u8 = 0x68;
pub const KIND_FOV: u8 = 0x5c;

/// **A node can be shown and hidden by its own animation.** 0x478800's cases
/// 0x0e and 0x0f are the two: 0x0e writes bit 2 of the runtime slot's flags
/// at +0x96 — the same bit `omGobGMSetSltVisible` (0x462a20) writes and the
/// draw at 0x464630 tests before it emits a strip — and 0x0f writes the
/// slot's alpha at +0x64, clamped to [0, 1].
///
/// This is how the comic book in the intro movie shows one panel at a time:
/// `L0a_AnimGOB` carries both on every one of its twenty panel nodes.
pub const KIND_VISIBLE: u8 = 0x0e;
pub const KIND_ALPHA: u8 = 0x0f;

/// What a camera node with no [`KIND_FOV`] channel would be filmed at. Ours,
/// and unreachable in the shipped data — **all 102 name one** — so it is a
/// fallback for a file this engine has not seen, not a reading of anything.
/// 33 is the value the corpus uses most.
pub const DEFAULT_FOV_DEGREES: f64 = 33.0;

/// A node draws nothing when its resource byte is this.
pub const NO_RESOURCE: u8 = 0xFF;

/// MSVC's debug fill for heap nobody wrote, read as a float: -431602080.
/// Six models store it as a node's translation, and animation 0 is what puts
/// them somewhere. See [`Model::node_world`].
pub const UNWRITTEN: f32 = f32::from_bits(0xCDCD_CDCD);

#[derive(Debug, PartialEq)]
pub enum Error {
    NotAModel(u32),
    Truncated,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotAModel(t) => write!(f, "resource type {t}, not a model (2002)"),
            Error::Truncated => write!(f, "the file ends inside a record"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    pub bbox_min: [f32; 3],
    pub bbox_max: [f32; 3],
    /// `None` at the root, where the file stores 0xFFFF.
    pub parent: Option<u16>,
    pub children: u16,
    pub group_first: u16,
    pub group_count: u16,
    /// From the parent, and accumulated down the chain for an animated model.
    pub translation: [f32; 3],
    /// Index into [`Model::refs`] — the *whole* table, sounds included, not
    /// a texture table. [`NO_RESOURCE`] means the node draws nothing.
    pub resource: u8,
    /// Whether the node is drawn at all before any animation says otherwise.
    /// **Bit 0 of the byte at 0x83**: the slot builder at 0x460a20 shifts it
    /// into flag bit 2, which the draw tests. 3824 of the corpus's 28712
    /// nodes start hidden and 655 of those carry geometry, so this is real
    /// and small — the comic's panels are most of it.
    pub visible: bool,
    /// **Self-illumination**, the float at 0x58 (slot +0x60). 0x464ab6
    /// branches on it: at or below zero the node is drawn lit, with a
    /// `glMaterialfv` diffuse and per-vertex normals; above zero its colour
    /// is multiplied by it, `GL_LIGHTING` is switched **off**, and the strip
    /// is drawn flat with no normals at all. 4370 of the 28712 nodes carry
    /// 1.0 and 23630 carry zero — which is why the intro movie's comic book
    /// is at full texture brightness and the level around it is not.
    pub glow: f32,
    /// The node's own opacity, the float at 0x5c (slot +0x64) — the same
    /// field animation kind [`KIND_ALPHA`] writes. 1.0 on 27755 of the
    /// 28712, and `ML1a_camera.mod` settles the reading by example: its three
    /// TV-static frames are identical but for this, 1.0 on the first and 0.0
    /// on the other two.
    pub alpha: f32,
}

/// A run of consecutive vertices, which is a triangle strip.
#[derive(Clone, Copy, Debug)]
pub struct Group {
    pub first: u32,
    pub count: u32,
}

/// A vertex record is 32 bytes and only 20 of them are understood: the
/// position and the UV. The remaining 12 are **not a normal**, and three
/// readings were tested and refuted over the corpus: as three floats only
/// 722 of 19507 are unit vectors, as three signed bytes over 127 only 68 of
/// 500, and the trailing 0..255 dword matches the owning node's index for 6
/// of `kurt.mod`'s 1741 vertices. A renderer that wants normals has to
/// compute them.
#[derive(Clone, Copy, Debug)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
}

#[derive(Clone, Debug)]
pub struct Channel {
    pub kind: u8,
    pub node: u8,
    /// `(time in [0,1], index into the value pool)`.
    pub keys: Vec<(f32, u32)>,
}

/// The rate used when the record's own is not believable. It is the
/// **median of the 5457 plausible ones**, 0.6818, a loop of 1.467 seconds —
/// measured, not chosen.
pub const MEDIAN_RATE: f32 = 0.6818;

#[derive(Clone, Debug)]
pub struct Animation {
    pub id: u32,
    /// The float at +8, read as a **signed playback rate** so that a loop
    /// lasts `1 / |rate|`. The sign is the argument for that reading: 201 of
    /// the 6311 records are negative, a negative *length* is meaningless,
    /// and `omAnimSetSpeed(door, ANIM_OPEN, -1)` is how `elevators.lua`
    /// shuts a door it opened.
    ///
    /// **It does not always hold.** Over the corpus 5457 of 6311 are in a
    /// plausible range and **854 are not**: 186 exactly zero, 141 above 1e6,
    /// 521 below 1e-3. Level 1's animated models are unusually full of them,
    /// which is how this was noticed at all — every one of them posed to a
    /// standstill. Use [`Animation::loop_rate`], which says when the field
    /// can be believed.
    pub rate: f32,
    /// **How it ends**, the dword at +0x10. 0x4611b0 is the whole of it: when
    /// the normalised clock runs off either end it raises the instance's
    /// just-looped flag and fires `OnAnimLoop`, and then switches on this --
    /// **0 wraps, 2 reverses the rate (a ping-pong), anything else clamps**
    /// and the animation stops where it stopped.
    ///
    /// Over the 6311 records it is 0 in 2404, 3 in 2266, 1 in 865 and 2 in
    /// 41, and it lines up with what should loop: `ANIM_WALK` and
    /// `ANIM_WALKBACK` are **0 in every model that has them**, `ANIM_DIE` and
    /// `ANIM_THROW` are 3 in most. The copy from here into the instance was
    /// not traced; the correlation and the switch are the argument.
    pub ends: u32,
    pub channels: Vec<Channel>,
}

impl Animation {
    /// The rate to play this at: the record's own where it is believable, and
    /// the corpus median where it is not.
    /// Whether it comes round again. See [`Animation::ends`].
    pub fn repeats(&self) -> bool {
        self.ends == 0 || self.ends == 2
    }

    pub fn loop_rate(&self) -> f32 {
        let r = self.rate.abs();
        if (1e-3..=1e6).contains(&r) {
            r
        } else {
            MEDIAN_RATE
        }
    }
}

#[derive(Clone)]
pub struct Model {
    data: Vec<u8>,
    pub counts: [u16; 12],
    pub offsets: [u32; 13],
    pub nodes: Vec<Node>,
    pub groups: Vec<Group>,
    pub vertices: Vec<Vertex>,
    pub refs: Vec<String>,
    pub animations: Vec<Animation>,
}

fn u16le(b: &[u8], at: usize) -> Result<u16, Error> {
    let s = b.get(at..at + 2).ok_or(Error::Truncated)?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

fn u32le(b: &[u8], at: usize) -> Result<u32, Error> {
    let s = b.get(at..at + 4).ok_or(Error::Truncated)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn f32le(b: &[u8], at: usize) -> Result<f32, Error> {
    Ok(f32::from_bits(u32le(b, at)?))
}

fn vec3(b: &[u8], at: usize) -> Result<[f32; 3], Error> {
    Ok([f32le(b, at)?, f32le(b, at + 4)?, f32le(b, at + 8)?])
}

/// The name fields are fixed width and their tails are uninitialised heap
/// (0xCD, MSVC's debug filler), so a string stops at the first NUL.
fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    b[..end].iter().map(|&c| c as char).collect()
}

impl Model {
    pub fn parse(data: &[u8]) -> Result<Model, Error> {
        if data.len() < HEADER_SIZE {
            return Err(Error::Truncated);
        }
        let tag = u32le(data, 0)?;
        if tag != TYPE_MOD {
            return Err(Error::NotAModel(tag));
        }
        let mut counts = [0u16; 12];
        for (i, c) in counts.iter_mut().enumerate() {
            *c = u16le(data, 8 + 2 * i)?;
        }
        let mut offsets = [0u32; 13];
        for (i, o) in offsets.iter_mut().enumerate() {
            *o = u32le(data, 0x20 + 4 * i)?;
        }

        let section = |i: usize| -> Option<usize> {
            (offsets[i] != ABSENT).then_some(offsets[i] as usize)
        };

        let mut nodes = Vec::new();
        if let Some(base) = section(0) {
            for i in 0..counts[5] as usize {
                let o = base + i * NODE_STRIDE;
                let parent = u16le(data, o + 0x6c)?;
                nodes.push(Node {
                    name: cstr(data.get(o..o + 28).ok_or(Error::Truncated)?),
                    bbox_min: vec3(data, o + 0x1c)?,
                    bbox_max: vec3(data, o + 0x28)?,
                    parent: (parent != 0xFFFF).then_some(parent),
                    children: u16le(data, o + 0x6e)?,
                    group_first: u16le(data, o + 0x70)?,
                    group_count: u16le(data, o + 0x72)?,
                    translation: vec3(data, o + 0x34)?,
                    resource: *data.get(o + 0x87).ok_or(Error::Truncated)?,
                    visible: data.get(o + 0x83).is_none_or(|b| b & 1 != 0),
                    glow: f32le(data, o + 0x58).unwrap_or(0.0),
                    alpha: f32le(data, o + 0x5c).unwrap_or(1.0),
                });
            }
        }

        let mut groups = Vec::new();
        if let Some(base) = section(6) {
            for i in 0..counts[6] as usize {
                let o = base + i * GROUP_STRIDE;
                groups.push(Group {
                    first: u32le(data, o)?,
                    count: u32le(data, o + 4)?,
                });
            }
        }

        let mut vertices = Vec::new();
        if let Some(base) = section(7) {
            for i in 0..counts[7] as usize {
                let o = base + i * VERTEX_STRIDE;
                vertices.push(Vertex {
                    pos: vec3(data, o)?,
                    uv: [f32le(data, o + 12)?, f32le(data, o + 16)?],
                });
            }
        }

        let mut refs = Vec::new();
        if let Some(base) = section(8) {
            for i in 0..counts[8] as usize {
                let o = base + i * REF_STRIDE;
                let Some(chunk) = data.get(o..o + REF_STRIDE) else {
                    break; // the table is the last thing in the file
                };
                refs.push(cstr(&chunk[..REF_NAME]) + &cstr(&chunk[REF_NAME..]));
            }
        }

        let mut animations = Vec::new();
        if let (Some(base), Some(oc), Some(ot), Some(ok)) =
            (section(1), section(2), section(3), section(4))
        {
            for i in 0..counts[0] as usize {
                let o = base + i * ANIM_STRIDE;
                let first_channel = u32le(data, o + 24)? as usize;
                let channel_count = u32le(data, o + 28)? as usize;
                let mut channels = Vec::with_capacity(channel_count);
                for j in 0..channel_count {
                    let c = oc + (first_channel + j) * CHANNEL_STRIDE;
                    let target = u16le(data, c)? as usize;
                    if target >= counts[2] as usize {
                        continue;
                    }
                    let first = u16le(data, c + 4)? as usize;
                    let count = u16le(data, c + 6)? as usize;
                    let t = ot + target * TARGET_STRIDE;
                    let mut keys = Vec::with_capacity(count);
                    for k in 0..count {
                        if first + k >= counts[3] as usize {
                            break;
                        }
                        let kb = ok + (first + k) * KEY_STRIDE;
                        keys.push((f32le(data, kb)?, u32le(data, kb + 4)?));
                    }
                    channels.push(Channel {
                        kind: *data.get(t).ok_or(Error::Truncated)?,
                        node: *data.get(t + 3).ok_or(Error::Truncated)?,
                        keys,
                    });
                }
                animations.push(Animation {
                    id: u32le(data, o)?,
                    rate: f32le(data, o + 8)?,
                    ends: u32le(data, o + 16)?,
                    channels,
                });
            }
        }

        Ok(Model {
            data: data.to_vec(),
            counts,
            offsets,
            nodes,
            groups,
            vertices,
            refs,
            animations,
        })
    }

    /// A model with an animation table stores its vertices in **node-local**
    /// space; one without is already in world space.
    pub fn animated(&self) -> bool {
        self.offsets[1] != ABSENT
    }

    /// The `.tex` a node draws with, if it draws.
    pub fn node_texture(&self, node: &Node) -> Option<&str> {
        let name = self.refs.get(node.resource as usize)?;
        name.to_ascii_lowercase().ends_with(".tex").then_some(&name[..])
    }

    /// One entry of the value pool: a quaternion for a rotation channel,
    /// three floats and a spare for a translation, a scalar for the rest.
    pub fn value(&self, index: u32) -> [f64; 4] {
        let base = self.offsets[5] as usize + index as usize * VALUE_STRIDE;
        let mut out = [0.0f64; 4];
        for (c, slot) in out.iter_mut().enumerate() {
            *slot = f32le(&self.data, base + 4 * c).unwrap_or(0.0) as f64;
        }
        out
    }

    /// The node a cutscene is filmed from, and its field of view in radians.
    ///
    /// See [`KIND_CAMERA`]. The settings live on animation 0 and the motion
    /// on the `ANIM_ACTION*` the script plays, so both tables are searched:
    /// `L0a_AnimGOB.mod` names the node and its 21 degrees in animation 0 and
    /// moves it in animation 77.
    pub fn camera(&self) -> Option<(usize, f64)> {
        let channels = || self.animations.iter().flat_map(|a| &a.channels);
        let node = channels().find(|c| c.kind == KIND_CAMERA)?.node as usize;
        let fov = channels()
            .find(|c| c.kind == KIND_FOV && c.node as usize == node)
            .and_then(|c| c.keys.first())
            .map(|k| self.value(k.1)[0])
            .unwrap_or(DEFAULT_FOV_DEGREES);
        Some((node, fov.to_radians()))
    }

    /// Which nodes are drawn at `t`: each node's own bit, and the
    /// animation's [`KIND_VISIBLE`] channels over it.
    ///
    /// **A visibility key is an integer, not a float.** 0x478800 takes the
    /// low bit of the value's first byte, and the pool holds a plain 0 or 1
    /// there — read as a float that is a denormal, so anything that samples
    /// it as one gets a number that is not zero and not one either.
    pub fn visible_at(&self, anim: &Animation, t: f64) -> Vec<bool> {
        let mut out: Vec<bool> = self.nodes.iter().map(|n| n.visible).collect();
        for (source, at) in self.over(anim, t) {
            for c in &source.channels {
                if c.kind != KIND_VISIBLE || c.node as usize >= out.len() {
                    continue;
                }
                // a step, not a ramp: the value in force is the last key at
                // or before `t`, and the first key if `t` is before them all
                let key =
                    c.keys.iter().take_while(|k| k.0 as f64 <= at).last().or(c.keys.first());
                if let Some(k) = key {
                    out[c.node as usize] = self.raw_value(k.1) & 1 != 0;
                }
            }
        }
        out
    }

    /// Animation 0 and then the one asked for — **state persists across an
    /// animation change**, because in the original it lives on the model
    /// instance and `omAnimPlay` only overwrites the channels the new
    /// animation has. Animation 0 is what ran at load, so it is the baseline
    /// every other animation starts from. Without it the intro movie's comic
    /// book goes invisible the moment its first shot begins: animation 0
    /// turns the cover's opacity to 1 and animation 77 does not mention it.
    fn over<'a>(&'a self, anim: &'a Animation, t: f64) -> Vec<(&'a Animation, f64)> {
        match self.animations.first() {
            Some(setup) if !std::ptr::eq(setup, anim) => vec![(setup, 0.0), (anim, t)],
            _ => vec![(anim, t)],
        }
    }

    /// Each node's opacity at `t` — its own, and the animation's
    /// [`KIND_ALPHA`] channels over it, interpolated and clamped the way
    /// 0x478800 clamps them.
    pub fn alpha_at(&self, anim: &Animation, t: f64) -> Vec<f32> {
        let mut out: Vec<f32> = self.nodes.iter().map(|n| n.alpha).collect();
        for (source, when) in self.over(anim, t) {
            for c in &source.channels {
                if c.kind != KIND_ALPHA || c.node as usize >= out.len() || c.keys.is_empty() {
                    continue;
                }
                let at = |k: &(f32, u32)| self.value(k.1)[0] as f32;
                let v = match c.keys.iter().position(|k| k.0 as f64 > when) {
                    None => at(c.keys.last().unwrap()),
                    Some(0) => at(&c.keys[0]),
                    Some(i) => {
                        let (a, b) = (&c.keys[i - 1], &c.keys[i]);
                        let span = (b.0 - a.0).max(f32::EPSILON);
                        at(a) + (at(b) - at(a)) * ((when as f32 - a.0) / span)
                    }
                };
                out[c.node as usize] = v.clamp(0.0, 1.0);
            }
        }
        out
    }

    /// One entry of the value pool as it is stored, for a channel whose
    /// values are not floats — see [`Model::visible_at`].
    fn raw_value(&self, index: u32) -> u32 {
        let base = self.offsets[5] as usize + index as usize * VALUE_STRIDE;
        u32le(&self.data, base).unwrap_or(0)
    }

    /// Node translations accumulated down the parent chain.
    fn world_offsets(&self) -> Vec<[f64; 3]> {
        let mut out = vec![None; self.nodes.len()];
        for i in 0..self.nodes.len() {
            self.accumulate(i, &mut out);
        }
        out.into_iter().map(|o| o.unwrap_or([0.0; 3])).collect()
    }

    fn accumulate(&self, i: usize, out: &mut Vec<Option<[f64; 3]>>) -> [f64; 3] {
        if let Some(v) = out[i] {
            return v;
        }
        // guard against a parent cycle, which a corrupt file could hold and
        // which would otherwise be an infinite recursion
        out[i] = Some([0.0; 3]);
        let base = match self.nodes[i].parent {
            Some(p) if (p as usize) < self.nodes.len() => self.accumulate(p as usize, out),
            _ => [0.0; 3],
        };
        let t = self.nodes[i].translation;
        let v = [
            base[0] + t[0] as f64,
            base[1] + t[1] as f64,
            base[2] + t[2] as f64,
        ];
        out[i] = Some(v);
        v
    }

    /// The bind pose: geometry with the hierarchy applied but no rotations.
    ///
    /// **Animation 0 is an animation, not the bind pose** — over 368 animated
    /// models only 30 agree at t = 0, and `ML8x_camera.mod` differs by 1681
    /// times its own size because its first animation begins wherever that
    /// shot begins.
    pub fn posed(&self) -> Mesh {
        let zero = vec![[0.0f64; 3]; self.nodes.len()];
        let world = if self.animated() { self.world_offsets() } else { zero };
        self.build(|ni, p| {
            let w = world[ni];
            [p[0] as f64 + w[0], p[1] as f64 + w[1], p[2] as f64 + w[2]]
        })
    }

    /// The geometry **as stored**, node-local and unposed, for a renderer
    /// that will apply each node's transform itself.
    pub fn local(&self) -> Mesh {
        self.build(|_, p| [p[0] as f64, p[1] as f64, p[2] as f64])
    }

    /// The same geometry with an animation applied at `t` in [0, 1].
    pub fn animate(&self, anim: &Animation, t: f64) -> Mesh {
        let world = self.node_world(anim, t);
        self.build(|ni, p| {
            let (q, off) = world[ni];
            let r = rotate(q, [p[0] as f64, p[1] as f64, p[2] as f64]);
            [r[0] + off[0], r[1] + off[1], r[2] + off[2]]
        })
    }

    fn build(&self, place: impl Fn(usize, [f32; 3]) -> [f64; 3]) -> Mesh {
        let mut mesh = Mesh::default();
        for (ni, node) in self.nodes.iter().enumerate() {
            for g in node.group_first..node.group_first.saturating_add(node.group_count) {
                let Some(group) = self.groups.get(g as usize) else {
                    continue;
                };
                let base = mesh.positions.len() as u32;
                for k in 0..group.count {
                    let Some(v) = self.vertices.get((group.first + k) as usize) else {
                        break;
                    };
                    mesh.positions.push(place(ni, v.pos));
                    mesh.uvs.push(v.uv);
                    mesh.resource.push(node.resource);
                    mesh.node.push(ni as u32);
                }
                // a strip, so every odd triangle has its winding flipped
                let made = mesh.positions.len() as u32 - base;
                for k in 0..made.saturating_sub(2) {
                    let (a, b, c) = (base + k, base + k + 1, base + k + 2);
                    mesh.triangles
                        .push(if k & 1 == 0 { [a, b, c] } else { [a, c, b] });
                }
            }
        }
        mesh
    }

    /// Each node's world transform at `t` in [0, 1] — `(quaternion, offset)`.
    ///
    /// This is all a renderer needs: the models are rigid hierarchies, so a
    /// vertex takes its own node's transform and nothing else. Two `vec4` of
    /// uniform per node.
    pub fn node_world(&self, anim: &Animation, t: f64) -> Vec<([f64; 4], [f64; 3])> {
        let n = self.nodes.len();
        let mut trans: Vec<[f64; 3]> = self
            .nodes
            .iter()
            .map(|node| {
                [
                    node.translation[0] as f64,
                    node.translation[1] as f64,
                    node.translation[2] as f64,
                ]
            })
            .collect();
        let mut quat = vec![[1.0f64, 0.0, 0.0, 0.0]; n];

        // **The pose persists across an animation change**, because in the
        // original it lives on the model *instance* and `omAnimPlay` only
        // overwrites the channels the new animation carries. Animation 0 is
        // what ran at load, so it is the baseline every other one starts
        // from, and it is applied first.
        //
        // The intro movie is what says so twice over. `L0A_COMICDUMMY` is one
        // of the six nodes in the corpus whose stored translation is
        // [`UNWRITTEN`] and only animation 0 places it, so without this the
        // whole comic book stands at -4.3e8; and `L0A_COVRFLIP01`'s cover is
        // lifted off the backdrop by 0.095 in animation 0 and by nothing in
        // animation 77, so without this the MDK2 cover is coplanar with the
        // wall behind it and loses the depth test.
        for (source, at) in self.over(anim, t) {
            for ch in &source.channels {
                let node = ch.node as usize;
                if node >= n {
                    continue;
                }
                let Some(v) = self.sample(ch, at) else { continue };
                match ch.kind {
                    KIND_TRANSLATION => trans[node] = [v[0], v[1], v[2]],
                    KIND_ROTATION => quat[node] = v,
                    _ => {} // 32..36 drive a sound, not a transform
                }
            }
        }

        let mut world: Vec<Option<([f64; 4], [f64; 3])>> = vec![None; n];
        for i in 0..n {
            self.compose(i, &quat, &trans, &mut world);
        }
        world
            .into_iter()
            .map(|w| w.unwrap_or(([1.0, 0.0, 0.0, 0.0], [0.0; 3])))
            .collect()
    }

    fn compose(
        &self,
        i: usize,
        quat: &[[f64; 4]],
        trans: &[[f64; 3]],
        world: &mut Vec<Option<([f64; 4], [f64; 3])>>,
    ) -> ([f64; 4], [f64; 3]) {
        if let Some(w) = world[i] {
            return w;
        }
        world[i] = Some((quat[i], trans[i])); // cycle guard, as in accumulate
        let w = match self.nodes[i].parent {
            Some(p) if (p as usize) < self.nodes.len() => {
                let (pq, pt) = self.compose(p as usize, quat, trans, world);
                let r = rotate(pq, trans[i]);
                (
                    multiply(pq, quat[i]),
                    [pt[0] + r[0], pt[1] + r[1], pt[2] + r[2]],
                )
            }
            _ => (quat[i], trans[i]),
        };
        world[i] = Some(w);
        w
    }

    /// A channel's value at `t` in [0, 1]. Rotations slerp, the rest lerp.
    ///
    /// Interpolation is not optional: over 400 models the median channel
    /// carries **6.8 keys a second**, better than four frames apart at the 30
    /// fps the recorded demo runs at, so stepping is visibly coarse rather
    /// than merely imprecise. 86673 of the 117144 channels do hold a single
    /// key and are constant. Thirteen, all in explosions, have times that run
    /// backwards; taking the first bracketing pair simply does not care.
    pub fn sample(&self, channel: &Channel, t: f64) -> Option<[f64; 4]> {
        let keys: Vec<(f32, u32)> = channel
            .keys
            .iter()
            .copied()
            .filter(|k| !k.0.is_nan())
            .collect();
        let keys = if keys.is_empty() { &channel.keys[..] } else { &keys[..] };
        let first = keys.first()?;
        if keys.len() == 1 || t <= first.0 as f64 {
            return Some(self.value(first.1));
        }
        let last = keys[keys.len() - 1];
        if t >= last.0 as f64 {
            return Some(self.value(last.1));
        }
        let Some(i) = (0..keys.len() - 1)
            .find(|&i| keys[i].0 as f64 <= t && t <= keys[i + 1].0 as f64)
        else {
            return Some(self.value(last.1));
        };
        let (t0, t1) = (keys[i].0 as f64, keys[i + 1].0 as f64);
        let a = self.value(keys[i].1);
        let b = self.value(keys[i + 1].1);
        let u = if t1 == t0 { 0.0 } else { (t - t0) / (t1 - t0) };
        Some(if channel.kind == KIND_ROTATION {
            slerp(a, b, u)
        } else {
            [
                a[0] + (b[0] - a[0]) * u,
                a[1] + (b[1] - a[1]) * u,
                a[2] + (b[2] - a[2]) * u,
                a[3] + (b[3] - a[3]) * u,
            ]
        })
    }
}

/// Geometry ready to hand to a renderer: strips already expanded to triangles.
#[derive(Default)]
pub struct Mesh {
    pub positions: Vec<[f64; 3]>,
    pub uvs: Vec<[f32; 2]>,
    /// Per vertex, the node's index into [`Model::refs`].
    pub resource: Vec<u8>,
    /// Per vertex, the node it belongs to. Posing is rigid — one node per
    /// vertex, no weights — so this is all a shader needs to move it.
    pub node: Vec<u32>,
    pub triangles: Vec<[u32; 3]>,
}

/// Rotate a point by a quaternion in **(w, x, y, z)** order.
pub fn rotate(q: [f64; 4], p: [f64; 3]) -> [f64; 3] {
    let [w, x, y, z] = q;
    [
        p[0] * (1.0 - 2.0 * (y * y + z * z))
            + p[1] * 2.0 * (x * y - z * w)
            + p[2] * 2.0 * (x * z + y * w),
        p[0] * 2.0 * (x * y + z * w) + p[1] * (1.0 - 2.0 * (x * x + z * z))
            + p[2] * 2.0 * (y * z - x * w),
        p[0] * 2.0 * (x * z - y * w)
            + p[1] * 2.0 * (y * z + x * w)
            + p[2] * (1.0 - 2.0 * (x * x + y * y)),
    ]
}

/// Quaternion product, parent then child, both (w, x, y, z).
pub fn multiply(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    let ([aw, ax, ay, az], [bw, bx, by, bz]) = (a, b);
    [
        aw * bw - ax * bx - ay * by - az * bz,
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
    ]
}

/// Spherical interpolation, shortest arc, falling back to a normalised lerp
/// where the sine goes to zero and the spherical form loses its precision.
pub fn slerp(a: [f64; 4], mut b: [f64; 4], u: f64) -> [f64; 4] {
    let mut dot: f64 = (0..4).map(|c| a[c] * b[c]).sum();
    if dot < 0.0 {
        // -q is the same rotation; take the short way round
        b = [-b[0], -b[1], -b[2], -b[3]];
        dot = -dot;
    }
    let mut out = [0.0f64; 4];
    if dot > 0.9995 {
        for c in 0..4 {
            out[c] = a[c] + (b[c] - a[c]) * u;
        }
    } else {
        let theta = dot.clamp(-1.0, 1.0).acos();
        let s = theta.sin();
        let (wa, wb) = (((1.0 - u) * theta).sin() / s, (u * theta).sin() / s);
        for c in 0..4 {
            out[c] = a[c] * wa + b[c] * wb;
        }
    }
    let len = out.iter().map(|c| c * c).sum::<f64>().sqrt();
    let len = if len == 0.0 { 1.0 } else { len };
    out.map(|c| c / len)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The only arithmetic here that can be wrong quietly.
    #[test]
    fn slerp_takes_the_short_way_round_and_stays_unit() {
        let q0 = [1.0, 0.0, 0.0, 0.0]; // identity
        let q1 = [0.0, 1.0, 0.0, 0.0]; // half turn about x
        assert_eq!(slerp(q0, q1, 0.0), q0);
        let half = slerp(q0, q1, 0.5);
        let r = 0.5f64.sqrt();
        for (got, want) in half.iter().zip([r, r, 0.0, 0.0]) {
            assert!((got - want).abs() < 1e-9, "{half:?}");
        }
        // -q is the same rotation, so the arc must not go the long way round
        let other = slerp(q0, [0.0, -1.0, 0.0, 0.0], 0.5);
        for c in 0..4 {
            assert!((half[c].abs() - other[c].abs()).abs() < 1e-9);
        }
        // near-parallel falls back to a normalised lerp and stays unit
        let near = slerp(q0, [0.99999, 0.00447, 0.0, 0.0], 0.5);
        assert!((near.iter().map(|c| c * c).sum::<f64>().sqrt() - 1.0).abs() < 1e-12);
    }

    /// (w, x, y, z), not (x, y, z, w): a quarter turn about X takes +Y to +Z.
    #[test]
    fn rotation_reads_the_quaternion_w_first() {
        let r = 0.5f64.sqrt();
        let p = rotate([r, r, 0.0, 0.0], [0.0, 1.0, 0.0]);
        assert!(p[0].abs() < 1e-12 && p[1].abs() < 1e-12 && (p[2] - 1.0).abs() < 1e-12);
        // and composing with the identity leaves it alone
        assert_eq!(multiply([1.0, 0.0, 0.0, 0.0], [r, r, 0.0, 0.0]), [r, r, 0.0, 0.0]);
    }

    #[test]
    fn refuses_what_is_not_a_model() {
        let mut data = vec![0u8; HEADER_SIZE];
        data[..4].copy_from_slice(&2001u32.to_le_bytes()); // a texture
        assert!(matches!(Model::parse(&data), Err(Error::NotAModel(2001))));
        assert!(matches!(Model::parse(&[]), Err(Error::Truncated)));
    }

    /// A strip of four vertices is two triangles, and the second is wound the
    /// other way. Getting that wrong shows up as every other face missing
    /// under back-face culling.
    #[test]
    fn a_strip_flips_the_winding_on_odd_triangles() {
        let mut data = vec![0u8; HEADER_SIZE];
        data[..4].copy_from_slice(&TYPE_MOD.to_le_bytes());
        // counts[5] nodes, counts[6] groups, counts[7] vertices
        data[8 + 2 * 5..8 + 2 * 5 + 2].copy_from_slice(&1u16.to_le_bytes());
        data[8 + 2 * 6..8 + 2 * 6 + 2].copy_from_slice(&1u16.to_le_bytes());
        data[8 + 2 * 7..8 + 2 * 7 + 2].copy_from_slice(&4u16.to_le_bytes());
        let mut offsets = [ABSENT; 13];
        offsets[0] = HEADER_SIZE as u32;
        offsets[6] = offsets[0] + NODE_STRIDE as u32;
        offsets[7] = offsets[6] + GROUP_STRIDE as u32;
        for (i, o) in offsets.iter().enumerate() {
            data[0x20 + 4 * i..0x20 + 4 * i + 4].copy_from_slice(&o.to_le_bytes());
        }
        data.resize(HEADER_SIZE + NODE_STRIDE + GROUP_STRIDE + 4 * VERTEX_STRIDE, 0);
        let node = HEADER_SIZE;
        data[node + 0x6c..node + 0x6e].copy_from_slice(&0xFFFFu16.to_le_bytes()); // root
        data[node + 0x72..node + 0x74].copy_from_slice(&1u16.to_le_bytes()); // one group
        data[node + 0x87] = NO_RESOURCE;
        let group = HEADER_SIZE + NODE_STRIDE;
        data[group + 4..group + 8].copy_from_slice(&4u32.to_le_bytes()); // four vertices

        let m = Model::parse(&data).unwrap();
        let mesh = m.posed();
        assert_eq!(mesh.positions.len(), 4);
        assert_eq!(mesh.triangles, vec![[0, 1, 2], [1, 3, 2]]);
        assert!(!m.animated());
    }
}
