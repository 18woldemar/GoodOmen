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
//! smallest headroom over the 129 checkpoints is 2.9. Hence an eye at 1.7, a
//! step of 0.6, gravity 20 and a walk of 4 units a second.
//!
//! It is still a **vertical segment** rather than a capsule, and that is the
//! known ceiling: `tools/walksim.py` measures six transient clips in 2557
//! runs, all of them a frame or ten of brushing through a tight spot. A swept
//! capsule is the real fix.
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

/// **These six are ours**, and the lead on the real ones is written down.
/// Kurt has his own mover in mdkKurt.c — his constructor allocates its own
/// block at `gob + 0x40` (0x416832) rather than the walker one — and the
/// turn is at 0x419d0b: an axis, times the first float of `kurt + 0x64`,
/// times **1/60** (0x48f9f4), with **0.05** (0x48f380) on the branch where
/// the body is on the ground. The buttons 0x65 and 0x66 at 0x419e57 set the
/// gait to 0 or 2, so the walk-versus-run split is the walker's own shape.
///
/// The 2011 HD build settles both constants by compiling the same arithmetic
/// differently: at 0x41a4a2 it divides by the **double 60.0** and the
/// on-ground branch is `* 3.0 / 60.0`, so 1/60 is confirmed and 0.05 is not
/// a tuned number — it is **three units a second**.
///
/// What is not read in either build is where `kurt + 0x64` points, which is
/// where the numbers themselves are: both constructors zero it (0x416981
/// here, 0x41841e there) and neither assigns it a static address.
pub const EYE: f64 = 1.7;
pub const STEP: f64 = 0.6;
pub const GRAVITY: f64 = 20.0;
pub const WALK: f64 = 4.0;
pub const SPRINT: f64 = 9.0;
pub const JUMP_SPEED: f64 = 7.0;
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

    /// Which tree stops the body here, sampling exactly where
    /// [`Collision::blocked`] does — otherwise the two could disagree about
    /// whether there was a collision at all.
    pub fn blocking(&self, p: [f64; 3], tall: f64, wide: f64) -> Option<usize> {
        let mut h = STEP;
        while h <= tall + 1e-9 {
            for (dx, dy) in Collision::ring(wide) {
                if let Some(t) = self.at([p[0] + dx, p[1] + dy, p[2] - tall + h]) {
                    return Some(t);
                }
            }
            h += (tall - STEP).max(1e-9) / 2.0;
        }
        None
    }

    /// Where a probe of this width samples in the horizontal plane: the
    /// centre, and the four points half a width out on the axes.
    ///
    /// ponytail: four points, not a swept hull. `def + 0x7c` is a full width
    /// and omCollision halves it, so half of it is the radius; a cross of
    /// five samples is the cheapest thing that notices a wall a body's
    /// shoulder would touch and its centre would not.
    fn ring(wide: f64) -> [(f64, f64); 5] {
        let r = (wide / 2.0).max(0.0);
        [(0.0, 0.0), (r, 0.0), (-r, 0.0), (0.0, r), (0.0, -r)]
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

    /// Only the body above step height stops it; below is a kerb to walk over.
    pub fn blocked(&self, p: [f64; 3], tall: f64, wide: f64) -> bool {
        let mut h = STEP;
        while h <= tall + 1e-9 {
            for (dx, dy) in Collision::ring(wide) {
                if self.solid([p[0] + dx, p[1] + dy, p[2] - tall + h]) {
                    return true;
                }
            }
            // a walker shorter than the kerb still gets its three samples
            h += (tall - STEP).max(1e-9) / 2.0;
        }
        false
    }

    pub fn footed(&self, p: [f64; 3], tall: f64) -> bool {
        self.solid([p[0], p[1], p[2] - tall + 0.05])
    }
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
            touching: Default::default(),
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
            self.on_ground = true;
            self.velocity_z = 0.0;
            return z + lift;
        }
        if self.on_ground && self.velocity_z < 0.0 {
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

    /// One frame: a direction in the horizontal plane, a jump, and `dt`.
    pub fn step(&mut self, world: &Collision, direction: [f64; 2], jump: bool, speed: f64, dt: f64) {
        let was = [self.position[0], self.position[1]];
        let start = self.position;
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
                for (ax, ay) in [(dx, dy), (dx, 0.0), (0.0, dy)] {
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
        self.velocity_z -= GRAVITY * dt;

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
    /// `mouse` is radians per unit of the demo's axis values and it is a
    /// guess: the file does not carry the sensitivity and neither does any
    /// script. Below about 0.2 the body grinds along walls.
    pub fn replay(&mut self, world: &Collision, frames: &[omn::Frame], mouse: f64, speed: f64) {
        for frame in frames {
            let dt = (frame.dt as f64).clamp(1e-4, 0.2);
            self.yaw -= mouse * frame.held(omn::TURN_RIGHT).unwrap_or(0.0) as f64;
            self.yaw += mouse * frame.held(omn::TURN_LEFT).unwrap_or(0.0) as f64;
            let (fx, fy) = (self.yaw.cos(), self.yaw.sin());
            let (rx, ry) = (-self.yaw.sin(), self.yaw.cos());
            let mut d = [0.0f64, 0.0];
            if frame.held(omn::FORWARD).is_some() {
                d = [d[0] + fx, d[1] + fy];
            }
            if frame.held(omn::BACKWARD).is_some() {
                d = [d[0] - fx, d[1] - fy];
            }
            if frame.held(omn::RIGHT).is_some() {
                d = [d[0] + rx, d[1] + ry];
            }
            if frame.held(omn::LEFT).is_some() {
                d = [d[0] - rx, d[1] - ry];
            }
            self.step(world, d, frame.held(omn::JUMP).is_some(), speed, dt);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One plane at z = 0 with solid below it, in the mirrored frame the
    /// trees are authored in, boxed generously.
    fn floor() -> Collision {
        let mut d = Vec::new();
        for v in [0.0f32, 0.0, 1.0, 0.0] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        d.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        Collision {
            nodes: 1,
            trees: vec![Tree {
                bsp: Bsp::parse(&d).unwrap(),
                lo: [-100.0; 3],
                hi: [100.0; 3],
                gob: "the floor".into(),
            }],
        }
    }

    /// The same one plane stood on end: solid everywhere below x = 0.
    fn wall() -> Collision {
        let mut d = Vec::new();
        for v in [1.0f32, 0.0, 0.0, 0.0] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        d.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        Collision {
            nodes: 1,
            trees: vec![Tree {
                bsp: Bsp::parse(&d).unwrap(),
                lo: [-100.0; 3],
                hi: [100.0; 3],
                gob: "the wall".into(),
            }],
        }
    }

    /// **A wide body does not fit where a narrow one does.** `def + 0x7c` is
    /// the full width and omCollision halves it, so half of it is how far the
    /// probe reaches sideways: a grunt is 3.8 wide and stops 1.9 short of a
    /// wall its centre would have walked into.
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
