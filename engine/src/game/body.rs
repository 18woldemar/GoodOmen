//! The collision world and the body that walks it.
//!
//! A tree comes with a model, so an object collides against
//! `<resource>.bsp` when one exists — for level 1 that is 81 of 265 objects,
//! the rooms and the fixed scenery, 85796 nodes. **None of the 81 belongs to
//! an animated model**, so nothing here is transformed: static geometry is
//! already in world space and so is its tree.
//!
//! The body is sized from the game, not guessed. `kurt.mod` is 1.86 units
//! from sole to scalp and `max.mod` 1.72, so a unit is about a metre; the
//! smallest headroom over the 129 checkpoints is 2.9. Hence an eye at 1.7
//! and a step of 0.6; the gravity and the jump are measured, see
//! [`GRAVITY`], and the walk comes out of the game's own speed table.
//!
//! **Forward is the model's local +Y**, which is [`facing`], and getting that
//! wrong cost this controller a quarter turn against the game from the day it
//! was written. It was found by reading the original's own position and
//! orientation out of the running game (`tools/peek.c`): over `demo1_5` there
//! are 21 ticks where the yaw holds perfectly still and the player walks, and
//! on every one he moves at his yaw plus 90.00 degrees. Replaying the same
//! demo went from 219 units travelled to 338 against the original's 320, from
//! 323 frames against a wall to 34, and from 30 frames inside geometry to
//! none.
//!
//! It is still a **vertical segment** rather than a capsule, and that is the
//! known ceiling: `tools/walksim.py` measures thirteen transient clips in
//! 2557 runs, all of them a frame or ten of brushing through a tight spot.
//! (It was six before the frame turned; setting the slide fan to its single
//! straight-ahead candidate gives thirteen too, so that is the survey's
//! fixed-direction starts walking new ground, not the mover.) The player is
//! also still **`EYE` tall and no width at all**, where `mdkKurt.c` writes
//! **2.0 and 0.8** at 0x416863. Both wait on the same thing: `omMath3d.c:637`
//! in the Dreamcast build asserts on `radius`, `height`, `det`, `t1`, `t2`
//! and `normal` together, so the original solves a **swept cylinder**, and
//! that solver is what gives a width somewhere to be checked against.
//!
//! Two things here are about checking the *whole* frame rather than a piece
//! of it, and both were wedging bodies under overhangs:
//!
//! - the lift out of a surface **stops at the ceiling**. Rising until the
//!   feet are clear is right in the open and wrong in a gap 1.7 units tall,
//!   which is exactly the body's height.
//! - the sideways move is validated against the position the body *finishes*
//!   the frame in, not the one it started in.

use crate::formats::bsp::Bsp;
use crate::formats::model::Model;
use crate::formats::omn;
use crate::game::install::Install;
use crate::game::world::World;

/// **These are ours**, and the speeds no longer are: a playable character's
/// are [`crate::game::world::PLAYER_SPEED`], read out of the binary.
///
/// `kurt + 0x64` is resolved, and the earlier note here was one dereference
/// short. It is not a float — it is a **pointer to a 236-byte block that
/// omGob.c allocates** (0x46db30, at omGob.c:1982), and the turn rate is its
/// first float. The default is 50.0; Kurt's constructor overwrites it with
/// **45.0** at 0x416ad5, and so does the bullet camera at 0x41a270. The turn
/// itself is at 0x419d1e:
///
/// ```text
/// mouse:    yaw += axis(1) * block[0] * 1/60          ; 0x48f9f4
/// buttons:  yaw += dt * block[0] * (body[0xc0] ? 0.05 : 1/60)
/// ```
///
/// and the keyboard turns three times faster while `body + 0xc0` is set. The
/// HD build compiles the same two as `/ 60.0` and `* 3.0 / 60.0`, which is
/// how 0.05 was settled. The pitch accumulates into the same block at +0x3c
/// and is not wired. The mouse path **is** — see [`turn_from_axis`], which
/// carries the axis's own curve as well as this factor.
///
/// [`EYE`] is still ours, though Kurt's constructor writes **1.2** to
/// `mdkGob + 0x08` at 0x4168b1, beside his hitpoints — the field
/// `mdkAILineOfSight` raises a sight line by. It is not adopted here because
/// nothing has checked it against the other three characters.
pub const EYE: f64 = 1.7;
pub const STEP: f64 = 0.6;
/// **Measured**, both of them, off the two jumps `demo1_5` records.
///
/// The chase camera is rigidly four units back along its own look, so
/// `eye + 4 * look` out of a GL trace is the player's own position to a
/// ten-thousandth of a unit -- which makes the z of a jump readable frame by
/// frame. Fitting one gravity and one launch a jump over the fourteen rising
/// frames gives **29.80** and **15.983 / 16.844**, at an rms residual of
/// **0.0013 units**. The old 20.0 and 7.0 were ours, and they are 2.5 units
/// low by the ninth frame of a jump.
///
/// The two launches differ by 5% and that is not explained. A partial first
/// frame -- the press landing part way into a tick -- would do it, and so
/// would something in `mdkKurt.c` this has not read; 16.0 is taken as the
/// launch because it is the better-conditioned of the two fits and it is the
/// round one.
pub const GRAVITY: f64 = 29.8;
/// Test speeds, not the game's. Anything driving a *player* takes the table.
pub const WALK: f64 = 4.0;
pub const SPRINT: f64 = 9.0;
/// Measured with [`GRAVITY`], off the same two jumps.
pub const JUMP_SPEED: f64 = 16.0;

/// The fastest a landing costs nothing: `0x48f7e0`, the float Kurt's landing
/// handler compares his fall against. See [`fall_damage`].
pub const LAND_SAFE: f64 = 45.0;

/// What a landing costs, out of **`mdkKurt.c` at 0x418280** -- the handler
/// that plays `KurtFootsteps`, stops `KurtChute`, and then decides between
/// the events `GeneralLand` and `GeneralLandWithDamage`.
///
/// It reads the fall out of the motion block (`gob + 0x68`, field `0x54`),
/// negates it, and compares it with [`LAND_SAFE`]. `fcom` + `test ah, 0x41`
/// takes the quiet branch when the fall is **less than or equal**, so 45.0
/// itself is free. Over it:
///
/// ```text
/// 0x41838a  fsub  [0x48f7e0]   ; v - 45.0
/// 0x418393  fmul  [0x48f5b0]   ; * 0.2
/// 0x41839c  call  0x4814f7     ; floor
/// 0x4183a1  fmul  [0x48f5a8]   ; * 10.0
/// 0x4183a7  call  0x4814d0     ; to int
/// 0x4183b9  push  0x10         ; DAMAGE_FALLING
/// 0x4183be  call  0x40e660     ; the damage mdkDealDamage also calls
/// ```
///
/// `0x4814f7` is **floor and not rint**: it rounds with `frndint` after
/// setting the whole x87 control word to `0x173f` (`0x4b9590`), whose
/// rounding field is 01 -- toward minus infinity. Elsewhere in the binary the
/// same routine quantises a volume as `x * 20.004`, round, `* 0.05`, which is
/// only a quantiser if it rounds.
///
/// So the law is a staircase: **every 5 units a second of fall over 45 costs
/// 10 hitpoints**, and 95 is the whole 100 Kurt has. With [`GRAVITY`] that is
/// a free drop of 34 units, and death at 151.
pub fn fall_damage(speed: f64) -> i64 {
    if speed <= LAND_SAFE {
        return 0;
    }
    // 0.2 and 10.0 are the binary's own two constants, kept as the pair it
    // uses rather than folded into "5 units a step" -- see the listing above
    (((speed - LAND_SAFE) * 0.2).floor() as i64) * 10
}
/// The box round each tree is padded, so a query just outside still descends.
const PAD: f64 = 1.0;

struct Tree {
    bsp: Bsp,
    lo: [f64; 3],
    hi: [f64; 3],
    /// The object this tree belongs to. Deduping by resource is safe *and*
    /// unambiguous: over all ten level graphs, **every one of the 605
    /// collision resources is named by exactly one object**, so a tree
    /// identifies its gob and `OnCollision` has something to name.
    gob: String,
}

#[derive(Default)]
pub struct Collision {
    trees: Vec<Tree>,
    pub nodes: usize,
}

impl Collision {
    /// One plane, boxed generously — solid on the side the normal points
    /// away from. The only way to build a world for a test without an
    /// install to read one out of.
    #[cfg(test)]
    pub(crate) fn one_plane(normal: [f32; 3], dist: f32, gob: &str) -> Collision {
        let mut d = Vec::new();
        for v in [normal[0], normal[1], normal[2], dist] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        d.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        Collision {
            nodes: 1,
            trees: vec![Tree {
                bsp: crate::formats::bsp::Bsp::parse(&d).unwrap(),
                lo: [-100.0; 3],
                hi: [100.0; 3],
                gob: gob.into(),
            }],
        }
    }

    /// Every `.bsp` the objects of a run world name, once each.
    pub fn load(install: &mut Install, world: &World) -> Collision {
        let mut out = Collision::default();
        let mut seen = std::collections::HashSet::new();
        for (_, gob) in world.iter() {
            let owner = gob.name.clone();
            let Some(resource) = gob.resource.as_ref().map(|r| r.to_ascii_lowercase()) else {
                continue;
            };
            if !seen.insert(resource.clone()) {
                continue;
            }
            let Ok(tree) = install.read(&format!("{resource}.bsp")) else { continue };
            let Ok(bsp) = Bsp::parse(&tree) else { continue };
            // the box is the model's own bounds, and without the model there
            // is nothing to bound the descent with
            let Ok(bytes) = install.read(&format!("{resource}.mod")) else { continue };
            let Ok(model) = Model::parse(&bytes) else { continue };
            let mesh = model.posed();
            if mesh.positions.is_empty() {
                continue;
            }
            let mut lo = [f64::INFINITY; 3];
            let mut hi = [f64::NEG_INFINITY; 3];
            for p in &mesh.positions {
                for c in 0..3 {
                    lo[c] = lo[c].min(p[c]);
                    hi[c] = hi[c].max(p[c]);
                }
            }
            out.nodes += bsp.nodes.len();
            out.trees.push(Tree {
                bsp,
                lo: [lo[0] - PAD, lo[1] - PAD, lo[2] - PAD],
                hi: [hi[0] + PAD, hi[1] + PAD, hi[2] + PAD],
                gob: owner,
            });
        }
        out
    }

    pub fn len(&self) -> usize {
        self.trees.len()
    }

    pub fn is_empty(&self) -> bool {
        self.trees.is_empty()
    }

    pub fn solid(&self, p: [f64; 3]) -> bool {
        self.at(p).is_some()
    }

    /// **Which** tree holds this point, as an index. Same descent as
    /// [`Collision::solid`]; the index is what turns "the body hit
    /// something" into "the body hit *that object*".
    pub fn at(&self, p: [f64; 3]) -> Option<usize> {
        self.trees.iter().position(|t| {
            (0..3).all(|c| t.lo[c] <= p[c] && p[c] <= t.hi[c]) && t.bsp.contains(p)
        })
    }

    /// The object a tree belongs to.
    pub fn owner(&self, tree: usize) -> Option<&str> {
        self.trees.get(tree).map(|t| t.gob.as_str())
    }

    /// **Which** tree stops the body here, as an index — and
    /// [`Collision::blocked`] is this asked as a yes or no, so the two cannot
    /// disagree about whether there was a collision at all.
    ///
    /// The body is an upright cylinder and the test is [`Bsp::overlaps`],
    /// which is exact for the tree rather than a sampling of it. It replaced
    /// a cross of five points at three heights; the reason is in that
    /// method's own note.
    pub fn blocking(&self, p: [f64; 3], tall: f64, wide: f64) -> Option<usize> {
        let (centre, half, radius) = Collision::column(p, tall, wide);
        let reach = [radius, radius, half];
        self.trees.iter().position(|t| {
            (0..3).all(|c| centre[c] + reach[c] >= t.lo[c] && centre[c] - reach[c] <= t.hi[c])
                && t.bsp.overlaps(centre, radius, half)
        })
    }

    /// The cylinder a body of this height stands in: its centre, its
    /// half-height and its radius. It spans from the step height — below
    /// which is a kerb to walk over — to the crown.
    fn column(p: [f64; 3], tall: f64, wide: f64) -> ([f64; 3], f64, f64) {
        let half = (tall - STEP).max(1e-9) / 2.0;
        ([p[0], p[1], p[2] - half], half, (wide / 2.0).max(0.0))
    }

    /// The tree **under** the feet, which is what the body is standing on.
    ///
    /// Sampled below them rather than at them, which is where
    /// [`Collision::footed`] looks: `settle` lifts the body clear of the
    /// surface it landed on, so at the feet there is nothing left to find.
    /// The game's own floors are thick enough that either probe answers, and
    /// a floor that is one plane thick — which is what a test builds — only
    /// answers to this one.
    pub fn footing(&self, p: [f64; 3], tall: f64) -> Option<usize> {
        self.at([p[0], p[1], p[2] - tall - 0.05])
    }

    /// Is there anything solid between the two points?
    ///
    /// The first query in this engine that is a **line** rather than a point.
    /// Every collision test until now sampled positions, which cannot see a
    /// wall thinner than the sample spacing; [`Bsp::crosses`] splits at the
    /// planes instead, so this is exact for the trees.
    ///
    /// The box test in front of it is a conservative reject — a tree whose
    /// box does not overlap the segment's own box cannot be crossed — so it
    /// only ever saves work.
    pub fn sees(&self, a: [f64; 3], b: [f64; 3]) -> bool {
        let lo = [0, 1, 2].map(|c| a[c].min(b[c]));
        let hi = [0, 1, 2].map(|c| a[c].max(b[c]));
        !self.trees.iter().any(|t| {
            (0..3).all(|c| hi[c] >= t.lo[c] && lo[c] <= t.hi[c]) && t.bsp.crosses(a, b)
        })
    }

    /// **The bottom of the world**: below every collision tree there is, so a
    /// body under it can never land on anything again.
    ///
    /// Ours, and it is a measurement rather than a rule — nothing in the
    /// original stops a falling body, and `0x40ee00`, the move a walker's
    /// gait goes through, has no ground check at all. This exists so a run
    /// can *say* that a body left the world instead of reporting the
    /// hundreds of thousands of units it accrues on the way down.
    pub fn underworld(&self) -> f64 {
        self.trees.iter().map(|t| t.lo[2]).fold(f64::INFINITY, f64::min)
    }

    /// Only the body above step height stops it; below is a kerb to walk over.
    pub fn blocked(&self, p: [f64; 3], tall: f64, wide: f64) -> bool {
        self.blocking(p, tall, wide).is_some()
    }

    pub fn footed(&self, p: [f64; 3], tall: f64) -> bool {
        self.solid([p[0], p[1], p[2] - tall + 0.05])
    }
}

/// How a blocked move is retried: the move projected onto a direction turned
/// by each of these, `d * cos(a)` along `d` rotated by `a` — exactly the part
/// of the move that survives a wall whose tangent lies that way.
///
/// The three candidates this replaces — the whole move, then x alone, then y
/// alone — are that same projection for a wall whose normal is an axis, and
/// for nothing else. Against a wall at 45 degrees all three push into it and
/// the body stops dead. Measured on `demo1_5`: the axis slide refused 178
/// frames and a turned direction goes on 123 of them, needing 30 degrees or
/// more every time. The original, read out of its own memory, is never
/// refused a step at all.
///
/// ponytail: a fan, not the wall's own normal. `omMath3d.c:637` in the
/// Dreamcast build asserts on `radius`, `height`, `det`, `t1`, `t2` and
/// `normal` together, so the original solves a swept cylinder and has a
/// normal to project onto. This finds the tangent by trying.
const SLIDE: [f64; 11] = [0.0, 15.0, -15.0, 30.0, -30.0, 45.0, -45.0,
                          60.0, -60.0, 75.0, -75.0];

fn slide(dx: f64, dy: f64) -> impl Iterator<Item = (f64, f64)> {
    SLIDE.into_iter().map(move |a| {
        let (s, c) = a.to_radians().sin_cos();
        ((dx * c - dy * s) * c, (dx * s + dy * c) * c)
    })
}

pub struct Body {
    pub position: [f64; 3],
    pub yaw: f64,
    pub velocity_z: f64,
    pub on_ground: bool,
    /// Frames on which the sideways move met a wall.
    pub hits: usize,
    /// Frames that finished inside geometry. This one must stay zero.
    pub inside: usize,
    pub travelled: f64,
    /// **This body flies**: no gravity, no floor, and the vertical move is
    /// whatever its `velocity_z` says, refused only by geometry. Set for the
    /// four types in [`crate::game::world::FLIES`].
    pub flying: bool,
    /// The collision trees the body is against **this frame** — what it
    /// walked into and what it stands on. The driver diffs this between
    /// frames, and a name entering it is an `OnCollision` and a name leaving
    /// it is the same handler called with `nil`, which is how the scripts
    /// spell "the collision ended".
    pub touching: std::collections::BTreeSet<usize>,
    /// How tall this body is. The player's is `EYE`; a walker's is its type's
    /// own `def + 0x78`, so a bfb sixteen units tall does not fit where a
    /// hoser two units tall does.
    pub height: f64,
    /// And how wide: `def + 0x7c`, halved into a radius by the probe. The
    /// player's is **zero** on purpose — his walk is pinned frame for frame
    /// against `walksim.py` and against his own recorded demo, and widening
    /// him would move both.
    pub width: f64,
    /// How fast the body was falling when it landed **this frame**, and zero
    /// on every frame that did not land. [`fall_damage`] is what reads it.
    pub landed: f64,
}

impl Body {
    pub fn new(position: [f64; 3], yaw: f64) -> Body {
        Body::sized(position, yaw, EYE)
    }

    /// A body of a named height, which is what a walker gets.
    pub fn sized(position: [f64; 3], yaw: f64, height: f64) -> Body {
        Body::shaped(position, yaw, height, 0.0)
    }

    /// A body with a width as well, which is what a walker gets.
    pub fn shaped(position: [f64; 3], yaw: f64, height: f64, width: f64) -> Body {
        Body {
            height,
            width,
            position,
            yaw,
            velocity_z: 0.0,
            on_ground: false,
            hits: 0,
            inside: 0,
            travelled: 0.0,
            flying: false,
            touching: Default::default(),
            landed: 0.0,
        }
    }

    /// Rise out of the surface landed on, or stay glued over a kerb.
    fn settle(&mut self, world: &Collision, mut z: f64) -> f64 {
        let p = |b: &Body, z: f64| [b.position[0], b.position[1], z];
        if world.footed(p(self, z), self.height) {
            let mut lift = 0.0;
            while lift < self.height
                && world.footed(p(self, z + lift), self.height)
                && !world.blocked(p(self, z + lift + 0.05), self.height, self.width)
            {
                lift += 0.05;
            }
            if !self.on_ground {
                self.landed = -self.velocity_z;
            }
            self.on_ground = true;
            self.velocity_z = 0.0;
            return z + lift;
        }
        // A grounded body probes down for a step every frame, whatever its
        // vertical velocity: gravity no longer runs while it is grounded, so
        // the velocity is zero and the old `velocity_z < 0.0` guard would
        // never open.
        if self.on_ground {
            let mut drop = 0.0;
            while drop < STEP && !world.footed(p(self, z - drop), self.height) {
                drop += 0.05;
            }
            if drop < STEP {
                self.velocity_z = 0.0;
                z -= drop - 0.05;
                return z;
            }
        }
        self.on_ground = false;
        z
    }

    /// An outside push along z, in units a second squared, for `dt`. A
    /// blower is the only thing in the game that does this — see
    /// [`crate::game::api::blowers`], where the 40.0 comes from.
    ///
    /// It leaves the ground the way a jump does, and for the same reason:
    /// `on_ground` is our own latch, and [`Body::settle`] would spend the
    /// push and put the body straight back down without it.
    pub fn blow(&mut self, up: f64, dt: f64) {
        if up == 0.0 {
            return;
        }
        self.velocity_z += up * dt;
        if self.velocity_z > 0.0 {
            self.on_ground = false;
        }
    }

    /// One frame: a direction in the horizontal plane, a jump, and `dt`.
    pub fn step(&mut self, world: &Collision, direction: [f64; 2], jump: bool, speed: f64, dt: f64) {
        let was = [self.position[0], self.position[1]];
        let start = self.position;
        self.landed = 0.0;
        // what the body is against this frame, for `OnCollision`. Two honest
        // sources and no others: the thing it walked into, and the thing it
        // is standing on. A body never ends up *inside* geometry -- the
        // checks hold that at zero -- so sampling its own column would find
        // nothing at all.
        self.touching.clear();

        let length = (direction[0] * direction[0] + direction[1] * direction[1]).sqrt();
        if length > 0.0 {
            let run = speed * dt;
            let (dx, dy) = (direction[0] / length * run, direction[1] / length * run);
            let ahead = [self.position[0] + dx, self.position[1] + dy, self.position[2]];
            if world.blocked(ahead, self.height, self.width) {
                self.hits += 1;
                if let Some(t) = world.blocking(ahead, self.height, self.width) {
                    self.touching.insert(t);
                }
            }
            // A body that is **already** inside geometry may leave it. Without
            // this every candidate is refused and it stands there for ever,
            // which is not hypothetical: 39 of the game's own 625 waypoints
            // are inside a collision tree, and one grunt on level 7 spawns in
            // one and holds up every sequence queued behind it.
            let stuck = world.blocked(self.position, self.height, self.width);
            // in pieces, so a fast frame cannot step over a thin wall
            let pieces = (run / 0.25).ceil().max(1.0) as usize;
            for _ in 0..pieces {
                for (ax, ay) in slide(dx, dy) {
                    let candidate = [
                        self.position[0] + ax / pieces as f64,
                        self.position[1] + ay / pieces as f64,
                        self.position[2],
                    ];
                    if stuck || !world.blocked(candidate, self.height, self.width) {
                        self.position[0] = candidate[0];
                        self.position[1] = candidate[1];
                        break;
                    }
                }
            }
        }

        if self.on_ground && jump {
            self.velocity_z = JUMP_SPEED;
            self.on_ground = false;
        }
        // **A body that flies has no floor.** The four types whose
        // `def + 0x14` carries bit 2 -- see [`crate::game::world::FLIES`] --
        // never ask about the ground: their AI writes the mover's own z
        // velocity from `def + 0x30` and everything under them is scenery.
        // So the vertical half of a frame becomes one line, and the only
        // thing that refuses a climb is geometry.
        if self.flying {
            let want = self.position[2] + self.velocity_z * dt;
            if !world.blocked([self.position[0], self.position[1], want], self.height, self.width) {
                self.position[2] = want;
            }
            self.on_ground = false;
            if world.blocked(self.position, self.height, self.width) {
                self.inside += 1;
            }
            if let Some(t) = world.blocking(self.position, self.height, self.width) {
                self.touching.insert(t);
            }
            self.travelled += (0..2)
                .map(|c| (self.position[c] - was[c]).powi(2))
                .sum::<f64>()
                .sqrt();
            return;
        }
        // **Gravity only bites when there is nothing underfoot.** Integrating
        // it while the body is resting makes the body bob for ever, because
        // `footed` answers at a resolution of 0.05 and one frame's fall is
        // 0.022: the drop re-buries the feet, `settle` lifts by a whole 0.05,
        // and the pair repeat. Measured on the original: over `demo1_5` its
        // own z moves on 318 of 706 ticks and only **5** of those are
        // vertical alone, where ours moved on 1138 of 1348 with **601**
        // vertical alone -- 53.6 units of climbing that never happened
        // against the original's 18.3.
        if !self.on_ground {
            self.velocity_z -= GRAVITY * dt;
        }

        // the fall is walked in pieces too, or a frame passes through a floor
        let mut z = self.position[2];
        let mut left = self.velocity_z * dt;
        while left.abs() > 1e-6
            && !world.footed([self.position[0], self.position[1], z], self.height)
        {
            let bit = left.clamp(-0.5, 0.5);
            z += bit;
            left -= bit;
        }
        let z = self.settle(world, z);

        // give the sideways move back if the *finished* position is solid
        self.position[2] = z;
        if world.blocked(self.position, self.height, self.width) {
            let mut back = Body {
                position: [was[0], was[1], z],
                ..Body::shaped([0.0; 3], 0.0, self.height, self.width)
            };
            back.on_ground = self.on_ground;
            back.velocity_z = self.velocity_z;
            let settled = back.settle(world, z);
            if !world.blocked([was[0], was[1], settled], self.height, self.width) {
                self.position = [was[0], was[1], settled];
            }
        }
        if world.blocked(self.position, self.height, self.width) {
            self.inside += 1;
        }
        // and what it finishes the frame standing on
        if let Some(t) = world.footing(self.position, self.height) {
            self.touching.insert(t);
        }
        self.travelled += (0..3)
            .map(|c| (self.position[c] - start[c]).powi(2))
            .sum::<f64>()
            .sqrt();
    }

    /// Drive the body from a parsed `.omn`.
    ///
    /// `mouse` is radians per unit of the demo's axis value — a flat factor,
    /// and **not** what the original does. [`turn_from_axis`] is what the
    /// original does, and why it is not used here is in the body below.
    pub fn replay(&mut self, world: &Collision, frames: &[omn::Frame], mouse: f64, kind: f64) {
        let mut drive = Drive::default();
        for frame in frames {
            let dt = (frame.dt as f64).clamp(1e-4, 0.2);
            // **The turn is measured, and it is flat.** `tools/peek.c` reads
            // the player's own orientation quaternion out of the running
            // game, and two windows of `demo1_5` fix the factor to four
            // places without a fit: frame 0 holds `TURN_L = 0.63` and takes
            // the yaw from pi to 3.3307, which is 0.3002 a unit; frames 15
            // to 22 hold `TURN_R` summing to 3.05 and take it to 2.4155,
            // which is 0.3001 a unit. **0.300 is `sens * sens + 0.05` at the
            // shipped sensitivity of 0.5** — the gain out of
            // `mdkSetTurnSensitivity`, so this is a reading and not a
            // coincidence. The recorded value therefore already carries the
            // axis's compressive curve and the 45/60, which is why
            // [`turn_from_axis`] must not be applied to a recording.
            self.yaw -= mouse * frame.held(omn::TURN_RIGHT).unwrap_or(0.0) as f64;
            self.yaw += mouse * frame.held(omn::TURN_LEFT).unwrap_or(0.0) as f64;
            let ahead = frame.held(omn::FORWARD).is_some() as i32
                - frame.held(omn::BACKWARD).is_some() as i32;
            let side = frame.held(omn::RIGHT).is_some() as i32
                - frame.held(omn::LEFT).is_some() as i32;
            // **The frame the jump is pressed already counts as airborne.**
            // The original's speed goes 6.01, 8.02 on the two grounded
            // frames and then 8.51 on the frame the rise begins, which is
            // the air gain and not the ground one; taking `on_ground` alone
            // would spend one more frame at 60 and leave 0.05 a frame of
            // speed that never comes back.
            let jump = frame.held(omn::JUMP).is_some();
            drive.push_at(kind, ahead, side, dt, self.on_ground && !jump);
            let (d, speed) = drive.heading(self.yaw);
            self.step(world, d, jump, speed, dt);
        }
    }
}

/// Radians of yaw for one frame's worth of mouse, and **none of it is a gain
/// picked by us**. Three pieces, each read where it lives:
///
/// - the axis itself (0x46f110). Its value is the positive half's command
///   minus the negative half's — `MOUSEX+` and `MOUSEX-`, which is the pair
///   `mdk2.lua` declares and the pair the demo records. Then, if the axis's
///   `+0x20` is not 1.0, `sign(raw) * pow(|raw|, +0x20)` — a **compressive
///   curve**, and the turn axis's exponent is **0.8**. Then times the axis's
///   gain at `+0x1c`, and zeroed under a dead zone at `+0x18` which is 0.
/// - the gain, from `mdkSetTurnSensitivity` (0x43ab60 into 0x42d050):
///   **`n * n + 0.05`**, the 0.05 being the float at 0x48f3c4. It writes the
///   same gain to axes 0 and 1, giving axis 0 a linear response and axis 1
///   the 0.8 curve. `defaultopt.lua` calls it with **0.5**, so the shipped
///   gain is 0.30.
/// - Kurt (0x419d1e): `axis(1)` times the first float of the block at
///   `gob + 0x64` — 45.0 — times 1/60.
///
/// The curve is why no single "radians per unit of axis" ever fitted: a
/// compressive exponent boosts small movements relative to large ones, and a
/// linear factor cannot be both at once.
pub fn turn_from_axis(raw: f64, sensitivity: f64) -> f64 {
    const EXPONENT: f64 = 0.8; // the axis's +0x20, set at 0x42d079
    const FLOOR: f64 = 0.05; // 0x48f3c4
    const TURN_RATE: f64 = 45.0; // the block at gob + 0x64, written at 0x416ad5
    let shaped = raw.abs().powf(EXPONENT) * raw.signum();
    shaped * (sensitivity * sensitivity + FLOOR) * TURN_RATE / 60.0
}

/// The world directions a character faces and strafes toward at this yaw.
///
/// **Forward is the model's local +Y, not its +X.** That is measured, not
/// assumed: `tools/peek.c` reads the original's own position and orientation
/// quaternion out of the running game, and over `demo1_5` there are 21 ticks
/// where the yaw held perfectly still and the player walked. On every one of
/// them the direction he moved is his yaw **plus 90.00 degrees**, to two
/// decimal places. The quaternion is `(w, x, y, z)` with only `w` and `z`
/// ever moving -- the same order the `.mod` animation channels use -- so the
/// yaw itself is `2 * atan2(z, w)` and unambiguous.
///
/// Which way is right comes off the same capture: where a strafe mixes in,
/// the movement swings from yaw+90 **down** toward yaw, so the strafe axis is
/// `(cos, sin)` and not `(-cos, -sin)`.
///
/// This is only about a *character's* frame. Every `atan2(dy, dx)` bearing
/// elsewhere in the engine is compared against another of its own kind and
/// stays self-consistent; what they are not yet checked against is an
/// authored quaternion, and that check is the next thing this measurement
/// buys.
pub fn facing(yaw: f64) -> ([f64; 2], [f64; 2]) {
    let (s, c) = (yaw.sin(), yaw.cos());
    ([-s, c], [c, s])
}



/// The yaw that faces along this direction — the inverse of [`facing`].
///
/// Every place that turns a delta into an angle and then compares it against
/// a gob's own yaw, or writes it back as one, needs this and not
/// `atan2(dy, dx)`. Mixing the two is a quarter turn, and it is the same
/// quarter turn [`facing`] documents: a *bearing* is an angle in world x-y, a
/// *yaw* is a rotation of a model whose forward is +Y, and they differ by
/// ninety degrees. `mdkWalkerHeadToPoint` writes a yaw into `walker + 0x14`
/// and the anim update compares the gob's own against it, so the engine's
/// `heading` is a yaw too.
pub fn bearing(dx: f64, dy: f64) -> f64 {
    (-dx).atan2(dy)
}

/// How much slower the camera pitches than it turns: **0.4027**.
///
/// The demo records `MLOOKDOWN` and `MLOOKUP` beside its turn, and the GL
/// trace gives the camera's pitch exactly as `atan2(look.z, |look.xy|)`.
/// Regressing the pitch step on the axis over the 189 frames that carry one
/// gives **0.1208 radians a unit** at a residual of 0.82 degrees, against
/// the turn's 0.300 on the same recording -- so the vertical is 40% as
/// sensitive as the horizontal, and a mouse that drives both at one rate
/// pitches two and a half times too fast.
///
/// Two things about the pitch are **not** explained and are not modelled:
/// it holds when nothing asks it to move -- 0.18 degrees of drift over 105
/// quiet frames -- but it also steps on its own, once from 0.00 to -6.32
/// over four frames with no look input at all, and then holds there. And
/// the range it covers, -28.4 to +5.5 degrees, is where the player looked
/// rather than a clamp: the extremes are touched once or twice and never
/// held against the axis.
pub const LOOK_OVER_TURN: f64 = 0.4027;

/// A playable character's two speeds, smoothed toward what the table asks for.
///
/// The original keeps them at `kurt + 0x0c` and `kurt + 0x10` and steps each
/// through 0x40ef40 every frame; the table is read fresh each time, so an
/// input change moves the *target* and the body catches up at
/// [`crate::game::world::ACCELERATE`]. Holding both is what makes the
/// diagonals right: the table's own entries are pre-normalised, so this must
/// **not** normalise them again — only the direction is a unit vector.
#[cfg(test)]
mod turn_tests {
    /// The curve is the part that cannot be folded into a gain, so this pins
    /// its shape rather than a number: a compressive exponent makes a small
    /// movement worth proportionally more than a large one.
    #[test]
    fn the_turn_axis_compresses_before_it_scales() {
        use super::turn_from_axis;
        let (small, large) = (turn_from_axis(0.1, 0.5), turn_from_axis(1.0, 0.5));
        assert!(small / 0.1 > large / 1.0, "small movements are worth more per unit");
        assert!((turn_from_axis(-0.4, 0.5) + turn_from_axis(0.4, 0.5)).abs() < 1e-12);
        assert_eq!(turn_from_axis(0.0, 0.5), 0.0);
        // the shipped slider is 0.5, so the gain is 0.25 + 0.05, and a full
        // unit of axis turns by that times 45/60
        assert!((turn_from_axis(1.0, 0.5) - 0.3 * 0.75).abs() < 1e-12);
        assert!(turn_from_axis(1.0, 1.0) > turn_from_axis(1.0, 0.5), "and it is monotonic");
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Drive {
    pub forward: f64,
    pub strafe: f64,
}

impl Drive {
    /// `ahead` and `side` are -1, 0 or +1 — the intent, not a speed.
    pub fn push(&mut self, kind: f64, ahead: i32, side: i32, dt: f64) {
        self.push_at(kind, ahead, side, dt, true)
    }

    /// The same, told whether the body is standing on anything: off the
    /// ground the gain is [`crate::game::world::AIR`], measured across the
    /// launch of `demo1_5`'s first jump.
    pub fn push_at(&mut self, kind: f64, ahead: i32, side: i32, dt: f64, ground: bool) {
        use crate::game::world::{approach_at, player_speed, ACCELERATE, AIR};
        let (f, s, _) = player_speed(kind, ahead, side).unwrap_or_default();
        let gain = if ground { ACCELERATE } else { AIR };
        self.forward = approach_at(self.forward, f, dt, gain);
        self.strafe = approach_at(self.strafe, s, dt, gain);
    }

    /// `-> (a unit direction in the plane, how fast to go along it)`, both
    /// axes from [`facing`], which is where the measurement lives.
    pub fn heading(&self, yaw: f64) -> ([f64; 2], f64) {
        let (f, r) = facing(yaw);
        let v = [
            self.forward * f[0] + self.strafe * r[0],
            self.forward * f[1] + self.strafe * r[1],
        ];
        let n = (v[0] * v[0] + v[1] * v[1]).sqrt();
        if n < 1e-9 { ([0.0, 0.0], 0.0) } else { ([v[0] / n, v[1] / n], n) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One plane at z = 0 with solid below it, in the mirrored frame the
    /// trees are authored in, boxed generously.
    fn floor() -> Collision {
        Collision::one_plane([0.0, 0.0, 1.0], 0.0, "the floor")
    }

    /// The same one plane stood on end: solid everywhere below x = 0.
    fn wall() -> Collision {
        Collision::one_plane([1.0, 0.0, 0.0], 0.0, "the wall")
    }

    /// **A flying body has no floor.** Stood on one and told to climb, it
    /// leaves; told to fall, it goes straight through. Neither is true of the
    /// same body with the flag clear -- which is the point, because it is the
    /// same body.
    #[test]
    fn a_flying_body_leaves_the_ground() {
        let world = floor();
        let mut bird = Body::new([0.0, 0.0, EYE], 0.0);
        bird.flying = true;
        for _ in 0..30 {
            bird.velocity_z = 6.5; // `def + 0x30` for a birdbrain
            bird.step(&world, [0.0, 0.0], false, 0.0, 1.0 / 30.0);
        }
        assert!(bird.position[2] > EYE + 6.0, "it climbed: {:?}", bird.position);
        assert!(!bird.on_ground, "and it is not standing on anything");
        assert_eq!(bird.inside, 0);
        // **and it hovers**: the same body, ten up, asked for nothing. A
        // flier stays where it is and a walker falls the whole way.
        let mut hover = Body::new([0.0, 0.0, EYE + 10.0], 0.0);
        hover.flying = true;
        let mut falls = Body::new([0.0, 0.0, EYE + 10.0], 0.0);
        for _ in 0..90 {
            hover.velocity_z = 0.0;
            hover.step(&world, [0.0, 0.0], false, 0.0, 1.0 / 30.0);
            falls.step(&world, [0.0, 0.0], false, 0.0, 1.0 / 30.0);
        }
        assert_eq!(hover.position[2], EYE + 10.0, "it hangs there");
        assert!((falls.position[2] - EYE).abs() < 0.1, "and the walker is down");
        // the floor is still geometry, though: a flier driven into it stops
        let mut diving = Body::new([0.0, 0.0, EYE + 10.0], 0.0);
        diving.flying = true;
        for _ in 0..90 {
            diving.velocity_z = -6.5;
            diving.step(&world, [0.0, 0.0], false, 0.0, 1.0 / 30.0);
        }
        // it stops **on** the floor rather than through it, and a little
        // lower than a walker rests: a flier is refused by `blocked`, which
        // is the body's own box, where a walker is placed by `settle`, which
        // puts the head an EYE above the ground.
        assert!(
            diving.position[2] > 0.5 && diving.position[2] < falls.position[2],
            "on it, not through it: {:?}",
            diving.position
        );
    }

    /// **A wide body does not fit where a narrow one does.** `def + 0x7c` is
    /// the full width and omCollision halves it, so half of it is how far the
    /// probe reaches sideways: a grunt is 3.8 wide and stops 1.9 short of a
    /// wall its centre would have walked into.
    /// The measurement this whole frame rests on: walking is yaw + 90
    /// degrees, and a strafe swings the move back toward the yaw itself.
    #[test]
    fn forward_is_the_model_s_y_axis() {
        for deg in [0.0, 37.0, 145.28, 206.82, -90.0] {
            let yaw = (deg as f64).to_radians();
            let (f, r) = facing(yaw);
            let moved = f[1].atan2(f[0]).to_degrees();
            let want = (deg + 90.0 + 540.0) % 360.0 - 180.0;
            assert!(((moved - want + 540.0) % 360.0 - 180.0).abs() < 1e-9,
                    "yaw {deg} faces {moved}, wanted {want}");
            // right is a quarter turn back from forward, so a pure strafe
            // moves along the yaw and not against it
            assert!((r[0] - yaw.cos()).abs() < 1e-12 && (r[1] - yaw.sin()).abs() < 1e-12);
            assert!((f[0] * r[0] + f[1] * r[1]).abs() < 1e-12, "and they are square");
        }
    }

    /// `bearing` is `facing` run backwards, and that is the whole contract:
    /// an angle taken off a delta and an angle taken off a quaternion have to
    /// mean the same thing, or every comparison between them is a quarter
    /// turn wrong.
    #[test]
    fn bearing_is_facing_run_backwards() {
        for (dx, dy) in [(0.0, 1.0), (1.0, 0.0), (-1.0, 0.0), (0.0, -1.0),
                         (3.0, 4.0), (-2.5, 0.75)] {
            let yaw = bearing(dx, dy);
            let (f, _) = facing(yaw);
            let n = (dx * dx + dy * dy).sqrt();
            assert!((f[0] - dx / n).abs() < 1e-12 && (f[1] - dy / n).abs() < 1e-12,
                    "bearing({dx}, {dy}) = {yaw} faces {f:?}");
        }
        // due +y is a yaw of zero, which is the identity quaternion every
        // scene graph writes when it does not care which way a thing looks
        assert!(bearing(0.0, 1.0).abs() < 1e-12);
        assert!((bearing(1.0, 0.0) + std::f64::consts::FRAC_PI_2).abs() < 1e-12);
    }

    /// The jump against the original's own arc, read off `demo1_5` frame by
    /// frame. These five heights are the game's, not ours: the chase camera
    /// is rigidly four back along its look, so `eye + 4 * look` out of a GL
    /// trace is the player's position to a ten-thousandth of a unit.
    #[test]
    fn a_jump_rises_the_way_the_original_s_does() {
        const WANT: [f64; 5] = [0.500, 0.964, 1.400, 1.798, 2.165];
        let world = floor();
        let mut body = Body::new([0.0, 0.0, 3.0], 0.0);
        for _ in 0..200 {
            body.step(&world, [0.0, 0.0], false, 0.0, 1.0 / 30.0);
        }
        let ground = body.position[2];
        for (i, want) in WANT.iter().enumerate() {
            // the demo's own frame time, which is not quite 1/30
            body.step(&world, [0.0, 0.0], true, 0.0, 0.0333);
            let got = body.position[2] - ground;
            assert!((got - want).abs() < 0.01,
                    "frame {i}: rose {got:.3}, the original rose {want}");
        }
        assert!(!body.on_ground);
    }

    /// A blower lifts a body off the floor and settles it at the speed the
    /// blower names. 40 against a gravity of 29.8 wins by 10.2, and the push
    /// stops at the strength, so the body hovers there rather than climbing
    /// away.
    #[test]
    fn a_blower_lifts_a_body_and_holds_it_at_the_strength() {
        /// what `api::blowers` returns, and the strength of the fan
        const PUSH: f64 = 40.0;
        const STRENGTH: f64 = 10.0;
        let world = floor();
        let mut body = Body::new([0.0, 0.0, 3.0], 0.0);
        for _ in 0..100 {
            body.step(&world, [0.0, 0.0], false, 0.0, 1.0 / 30.0);
        }
        let ground = body.position[2];
        assert!(body.on_ground, "it starts on the floor");
        let mut top = 0.0f64;
        for _ in 0..300 {
            let up = if body.velocity_z < STRENGTH { PUSH } else { 0.0 };
            body.blow(up, 1.0 / 30.0);
            body.step(&world, [0.0, 0.0], false, 0.0, 1.0 / 30.0);
            top = top.max(body.velocity_z);
        }
        assert!(!body.on_ground, "the blower took it off the floor");
        assert!(body.position[2] - ground > 20.0,
                "it rose {:.1}", body.position[2] - ground);
        // the climb settles at the strength, give or take one frame of each
        assert!(top > STRENGTH && top < STRENGTH + PUSH / 30.0 + 0.01,
                "fastest it went was {top:.2}, the fan stops at {STRENGTH}");
    }

    /// The landing staircase, and the height it puts on each step. A drop of
    /// 34 units is free; one of 151 kills a full-health Kurt outright.
    #[test]
    fn a_landing_costs_ten_hitpoints_every_five_units_a_second() {
        assert_eq!(fall_damage(0.0), 0);
        assert_eq!(fall_damage(LAND_SAFE), 0, "45 itself takes the quiet branch");
        assert_eq!(fall_damage(49.9), 0);
        assert_eq!(fall_damage(50.0), 10);
        assert_eq!(fall_damage(55.0), 20);
        assert_eq!(fall_damage(95.0), 100, "and that is the whole of him");
        // the fall out of level 1 room 7, 687 units onto the floor below
        assert_eq!(fall_damage((2.0f64 * GRAVITY * 687.0).sqrt()), 310);
    }

    /// And the body reports the landing that the rule is about: the fall is
    /// read at touchdown, on that frame only.
    #[test]
    fn a_body_reports_the_fall_it_landed_on() {
        let world = floor();
        let mut body = Body::new([0.0, 0.0, 60.0], 0.0);
        let mut hardest = 0.0f64;
        for _ in 0..200 {
            body.step(&world, [0.0, 0.0], false, 0.0, 1.0 / 30.0);
            hardest = hardest.max(body.landed);
        }
        // it fell about 58 units to the floor, so within a frame of sqrt(2gh)
        let want = (2.0 * GRAVITY * 58.0).sqrt();
        assert!((hardest - want).abs() < GRAVITY / 30.0 + 0.5,
                "landed at {hardest:.2}, a free fall of that height ends at {want:.2}");
        assert_eq!(body.landed, 0.0, "and a frame that stands still is not a landing");
        assert!(fall_damage(hardest) > 0, "a fall like that is not free");
    }

    /// The fan is a projection, not a swing: turning further gives up more of
    /// the move, and the whole move is always tried first.
    #[test]
    fn the_slide_projects_rather_than_swings() {
        let fan: Vec<_> = slide(1.0, 0.0).collect();
        assert_eq!(fan[0], (1.0, 0.0), "the move itself comes first");
        let mut last = f64::INFINITY;
        for (i, (x, y)) in fan.iter().enumerate() {
            let along = x * 1.0 + y * 0.0; // the part still going the old way
            if i % 2 == 1 {
                assert!(along < last, "each turn gives up more of the move");
                last = along;
            }
            assert!(along >= -1e-12, "and never goes backwards");
        }
        // a wall whose tangent is 45 degrees off keeps 1/sqrt(2) of a unit
        let at45 = fan.iter().find(|(x, y)| (x - y).abs() < 1e-9 && *x > 0.0).unwrap();
        assert!((at45.0.hypot(at45.1) - 0.5f64.sqrt()).abs() < 1e-9);
    }

    /// A body left alone on flat ground must not move at all. Gravity used to
    /// run while it was grounded, and `footed` answers at 0.05 where one
    /// frame's fall is 0.022, so it bobbed for ever: over `demo1_5` the z
    /// moved on 1138 of 1348 frames where the original's moved on 318 of 706.
    #[test]
    fn a_resting_body_rests() {
        let world = floor();
        let mut body = Body::new([0.0, 0.0, 3.0], 0.0);
        for _ in 0..200 {
            body.step(&world, [0.0, 0.0], false, 0.0, 1.0 / 30.0);
        }
        let settled = body.position[2];
        for _ in 0..200 {
            body.step(&world, [0.0, 0.0], false, 0.0, 1.0 / 30.0);
            assert!((body.position[2] - settled).abs() < 1e-9,
                    "it moved to {} from {settled}", body.position[2]);
        }
        assert!(body.on_ground);
    }

    #[test]
    fn width_keeps_a_body_off_a_wall_its_centre_would_clear() {
        let world = wall();
        // one unit clear of the wall, standing
        let point = [1.0, 0.0, EYE];
        assert!(!world.blocked(point, EYE, 0.0), "a point one unit clear is clear");
        assert!(
            world.blocked(point, EYE, 3.8),
            "a grunt's 3.8 reaches 1.9 sideways, which is past the wall"
        );
        assert!(
            !world.blocked(point, EYE, 1.0),
            "and half a unit does not"
        );
    }

    /// A tree names its object, which is what turns "the body hit
    /// something" into an `OnCollision` about *that* object.
    #[test]
    fn what_the_body_stands_on_can_be_named() {
        let world = floor();
        let mut body = Body::new([0.0, 0.0, 5.0], 0.0);
        for _ in 0..60 {
            body.step(&world, [0.0, 0.0], false, WALK, 1.0 / 30.0);
        }
        assert!(body.on_ground, "it should have landed");
        let named: Vec<&str> =
            body.touching.iter().filter_map(|&t| world.owner(t)).collect();
        assert_eq!(named, ["the floor"]);
    }

    #[test]
    fn a_body_falls_onto_the_floor_and_stops_there() {
        let world = floor();
        assert!(world.solid([0.0, 0.0, -1.0]), "below the plane is solid");
        assert!(!world.solid([0.0, 0.0, 1.0]));

        let mut body = Body::new([0.0, 0.0, 10.0], 0.0);
        for _ in 0..180 {
            body.step(&world, [0.0, 0.0], false, WALK, 1.0 / 60.0);
        }
        assert!(body.on_ground, "it should have landed");
        // the eye ends about EYE above the floor, within the settle step
        assert!(
            (body.position[2] - EYE).abs() < 0.2,
            "ended at {}",
            body.position[2]
        );
        assert_eq!(body.inside, 0, "a body must never finish inside the world");
    }

    /// A body that starts **inside** the world walks out of it. Every slide
    /// candidate is solid there, so without the escape it would stand in the
    /// rock for ever — which is not a hypothetical shape of level: 39 of the
    /// game's 625 waypoints are inside a collision tree, and level 7 spawns a
    /// grunt in one at the head of a sequence.
    #[test]
    fn a_body_that_starts_buried_can_still_walk_out() {
        let world = floor();
        let mut body = Body::new([0.0, 0.0, -5.0], 0.0);
        assert!(
            world.blocked(body.position, body.height, body.width),
            "five under the floor is solid"
        );
        for _ in 0..60 {
            body.step(&world, [1.0, 0.0], false, WALK, 1.0 / 60.0);
        }
        assert!(
            (body.position[0] - WALK).abs() < 0.1,
            "it should have covered the second, got {}",
            body.position[0]
        );
    }

    /// A body stands on its feet whatever its height, so a walker five units
    /// tall settles with its head five above the floor and Kurt with his
    /// 1.7. The heights are the walker record's own `def + 0x78` — the
    /// doganboy's is 5.0 — and getting this wrong sinks every tall enemy into
    /// the ground by the difference.
    #[test]
    fn a_body_settles_on_its_feet_at_whatever_height_it_is() {
        let world = floor();
        for tall in [EYE, 5.0, 16.0] {
            let mut body = Body::sized([0.0, 0.0, 30.0], 0.0, tall);
            for _ in 0..300 {
                body.step(&world, [0.0, 0.0], false, WALK, 1.0 / 60.0);
            }
            assert!(body.on_ground, "{tall} tall should have landed");
            assert!(
                (body.position[2] - tall).abs() < 0.2,
                "{tall} tall ended with its head at {}",
                body.position[2]
            );
            assert_eq!(body.inside, 0, "and never inside the world");
        }
    }

    #[test]
    fn walking_on_flat_ground_covers_the_distance_it_should() {
        let world = floor();
        let mut body = Body::new([0.0, 0.0, EYE], 0.0);
        for _ in 0..60 {
            body.step(&world, [1.0, 0.0], false, WALK, 1.0 / 60.0);
        }
        assert!(
            (body.position[0] - WALK).abs() < 0.1,
            "a second at {WALK} units a second, got {}",
            body.position[0]
        );
        assert_eq!(body.hits, 0, "nothing to hit on an open plane");
        assert_eq!(body.inside, 0);
    }
}
