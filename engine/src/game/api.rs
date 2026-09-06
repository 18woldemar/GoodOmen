//! The engine's Lua surface, and what a level start does with it.
//!
//! `mdk2.lua:level(number, checkpoint, section)` is the whole of starting a
//! level, and BioWare drew its call graph in a comment above it — so nothing
//! here is reconstructed. Booting level 1 at checkpoint 1 touches **40** of
//! the original's functions; all ten levels at all 129 checkpoints touch
//! **68**, out of the **461** the binary registers.
//!
//! So the surface is installed in two halves, and the engine says which is
//! which rather than pretending:
//!
//! - the functions with real behaviour here — the rooms, the resources, the
//!   checkpoints, the objects — which are the ones a level start actually
//!   needs and which every check below is about;
//! - every other registered name, installed as a **recorder**: it counts its
//!   calls, answers `nil`, and appears in `unimplemented()`. That is a work
//!   list the engine keeps about itself, not an implementation.
//!
//! Answering `nil` is the right default and not a shrug: `if
//! mdkIsSomething()` then stays false rather than quietly turning true.
//! `tools/boot.py` established that all 129 checkpoints start under exactly
//! this rule.
//!
//! Two globals must be set or half of starting a level is skipped in
//! silence: `levelchanged` and `sectionchanged`. Without them
//! `doloadingscreen` takes neither branch — no `mdkPreloadRes`, no sound
//! bank, no loading screen.

use crate::game::functions::FUNCTIONS;
use crate::game::script::{Error, Scripts};
use crate::game::world::{self, Gob};
use mlua::{Lua, Value, Variadic};
use std::collections::{BTreeMap, BTreeSet};

/// A room, with the box a camera is tested against and the rooms it draws.
#[derive(Default)]
pub struct Visibility {
    pub names: Vec<String>,
    pub boxes: Vec<Option<[f64; 6]>>,
    pub visible: Vec<std::collections::BTreeSet<usize>>,
    /// The room's EAX 2.0 environment, from `mdkRoomSetEnv`. One number is
    /// all the game ever says about reverb — see `crate::audio::reverb`.
    pub env: Vec<Option<f64>>,
    /// The room's music track, from `mdkRoomSetMusic`. `Music/TrackNN`,
    /// identity and not an offset; 0 and -1 stop the music.
    pub music: Vec<Option<f64>>,
}

impl Visibility {
    /// Which rooms' boxes contain this point. **Boxes do overlap**, so this
    /// is a list and the first is taken.
    pub fn at(&self, p: [f64; 3]) -> Vec<usize> {
        (0..self.names.len())
            .filter(|&i| {
                self.boxes[i].is_some_and(|b| {
                    (0..3).all(|c| b[c] <= p[c] && p[c] <= b[c + 3])
                })
            })
            .collect()
    }
}


/// A room, as `ApplySceneGraph` builds one: it **is** an object, `visible` is
/// the authored cull list, `load` the section to stream.
#[derive(Clone, Debug, Default)]
pub struct Room {
    pub name: String,
    pub visible: Vec<usize>,
    pub music: Option<f64>,
    /// An EAX 2.0 reverb preset, 0..25.
    pub env: Option<f64>,
    pub load: Option<f64>,
    pub checkpoint: Option<f64>,
    pub bbox: Option<[f64; 6]>,
}

#[derive(Clone, Debug)]
pub struct Checkpoint {
    pub index: f64,
    pub position: [f64; 3],
    pub facing: f64,
    pub section: Option<f64>,
    /// The rooms that are gone once this checkpoint is reached, from
    /// `mdkCheckpointAddDeleteRoom` — **3199 calls in a boot, the largest
    /// single entry on the work list**, and between them the whole of the
    /// game's streaming. The lists are disjoint and every one of them names
    /// rooms *behind* its checkpoint, which is what a level frees as you walk
    /// forward. Only six of the ten levels have any.
    pub delete: Vec<String>,
    /// `mdkCheckpointSetPrevCheckpoint` — the streaming chain, which is
    /// **not** the index order: level 2 goes 3 → 5 → 9 → 10, skipping 4
    /// because 4 is in another section. `mdkSetCheckpoint` defaults it to -1.
    pub prev: Option<usize>,
}

/// **The screen shake**, read at 0x46aa98..0x46ab12. The offset is three
/// Euler angles handed to 0x46fd20 and added to the camera's own, and the
/// three are sines of the *same* phase at 6, 10 and 16 times it — mutually
/// incommensurate, which is why a shake looks like noise rather than a wobble.
/// Each is scaled by the amplitude and by a linear fade to nothing over the
/// duration.
///
/// The amplitudes the scripts ask for are 0.01 to 0.03, which is half a
/// degree to two — nonsense as a distance and exactly right as an angle.
#[derive(Clone, Copy, Debug)]
pub struct Shake {
    /// `scene + 0x11c`, radians.
    pub amplitude: f64,
    /// `scene + 0x120`.
    pub frequency: f64,
    /// `scene + 0x118`; the shake ends when the clock passes it.
    pub duration: f64,
    /// `scene + 0x114`, which `omSceneShake` zeroes.
    pub elapsed: f64,
}

impl Shake {
    /// The three angles this instant, **in the order 0x46fd20 takes them:
    /// yaw, pitch, roll**, in radians. 0x46abba pushes them in reverse, so
    /// the call is `(quat, sin 6t, sin 10t, sin 16t)`, and which slot is the
    /// yaw comes from the conehead lemming at 0x434c6a -- the one other call
    /// with a known angle in it, `(quat, heading, 0, 0)`.
    pub fn angles(&self) -> [f64; 3] {
        if self.duration <= 0.0 || self.elapsed > self.duration {
            return [0.0; 3];
        }
        let fade = 1.0 - self.elapsed / self.duration;
        let t = self.frequency * self.elapsed;
        // 0x490374, 0x48f384 and 0x490378
        [6.0, 10.0, 16.0].map(|k| (t * k).sin() * self.amplitude * fade)
    }
}

/// A command the scripts declare, and the input it answers to.
///
/// `omMakeCommand(COM_JETCHEAT, "J", CON_BUTTON_HELD, 0, 0)` declares one
/// with a default key and a trigger mode; `omBindCommandI(COM_FORWARD, 200)`
/// binds it to a **DirectInput scancode** — 200 is DIK_UP, and every id in
/// `defaultkeys.lua` checks out against the DirectInput header.
#[derive(Clone, Debug, Default)]
pub struct Command {
    pub id: f64,
    pub key: String,
    pub mode: f64,
}

/// `OBJ_STATICLIGHT`, which is the only object type that omits the trailing
/// flag — nineteen arguments to `mdkRegisterObject`, not twenty.
pub const OBJ_STATICLIGHT: f64 = 802.0;

/// `OBJ_ROOM`, out of `tools/luaconst.py`'s reading of the binary. Named
/// here because the renderer asks for it by meaning, not by number.
pub const OBJ_ROOM: f64 = 803.0;

/// `DAMAGE_GOODGUY`, out of the binary's own constant table — 1, the low bit
/// of the 13 `DAMAGE_*` flags, and half of the filter of 9 every enemy is
/// built with. It is what the driver hands an `OnDamage` handler as a probe:
/// **there is no `DAMAGE_NORMAL`**, which is what stood here until the damage
/// path became real and started reading the argument.
pub const DAMAGE_GOODGUY: f64 = 1.0;

/// `OBJ_AMBIENTSOUND`, whose `resource` slot names a `.wav` rather than a
/// model, and whose payload is (near distance, far distance, ?, volume).
pub const OBJ_AMBIENTSOUND: f64 = 1101.0;

/// A sound the scripts hung on an object: `omGobAddSound(gob, name, flag)`
/// hands back a handle, and `omGobGSPlay(handle, a, b, c, d)` fires it.
///
/// The binding at 0x41fbb0 reads a gob, a string and a number, and 0x41fa50
/// reads a handle and **four** numbers. What those four mean is not settled:
/// 93 of the 100 calls pass `0,0,0,0`, four pass `0,1,0,0`, and three pass
/// `0,0,0.5,1` — all of them doors. The third argument here is 0 on 59 of the
/// 62 `omGobAddSound` calls and 1 on three (`teleport` twice,
/// `sniper_shot`), and is likewise unexplained.
#[derive(Clone, Debug)]
pub struct GobSound {
    pub gob: String,
    /// Named without an extension, like every other resource slot.
    pub sound: String,
    pub flag: f64,
    pub played: usize,
}

/// The last scancode DirectInput defines.
const DIK_MAX: u32 = 0xED;
/// Not scancodes: the two mouse buttons and the four half-axes.
const MOUSE: std::ops::Range<u32> = 1000..1008;

#[derive(Default)]
pub struct Input {
    pub commands: Vec<Command>,
    /// `(command, the id it is bound to)`.
    pub bindings: Vec<(f64, u32)>,
    pub axes: usize,
}

impl Input {
    /// Bound ids that are neither a DirectInput scancode nor one of the six
    /// mouse ids. There should be none: `tools/walksim.py --keys` checks the
    /// same thing over the scripts and finds none.
    pub fn faults(&self) -> Vec<u32> {
        let mut out: Vec<u32> = self
            .bindings
            .iter()
            .map(|(_, id)| *id)
            .filter(|id| *id > DIK_MAX && !MOUSE.contains(id))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// The arc a walker was launched along, as **0x4301f0** works it out.
///
/// The original is a *solver*, not a mover: it computes three numbers, writes
/// them to the walker block, and lets the physics fly the thing. Those three
/// are `rise` (`walker + 0x20`), `speed` (`+0x28`) and `heading` (`+0x14`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Launch {
    /// Vertical speed at the launch, `sqrt(2 g apex)`.
    pub rise: f64,
    /// Ground speed, the horizontal distance divided by the flight time.
    pub speed: f64,
    /// Bearing to the destination, radians.
    pub heading: f64,
    /// How long the whole arc takes: up to the apex, then down to the target.
    pub time: f64,
}

/// A walker in the air: where it left from, along what, and how long ago.
#[derive(Clone, Copy, Debug, Default)]
pub struct Jump {
    pub from: [f64; 3],
    pub arc: Launch,
    pub elapsed: f64,
}

impl Jump {
    /// Where the walker is `t` seconds after the launch. Past the flight time
    /// it stays where it landed, so the last tick puts it exactly on the
    /// waypoint rather than a little beyond.
    pub fn at(&self, t: f64) -> [f64; 3] {
        let t = t.min(self.arc.time);
        [
            self.from[0] + crate::game::body::facing(self.arc.heading).0[0]
                * self.arc.speed * t,
            self.from[1] + crate::game::body::facing(self.arc.heading).0[1]
                * self.arc.speed * t,
            self.from[2] + self.arc.rise * t - 0.5 * crate::game::body::GRAVITY * t * t,
        ]
    }
}

/// Work out the arc from `from` to `to` that peaks `apex` above the launch.
///
/// This is 0x4301f0 line for line. Three things in it are read rather than
/// guessed, and all three would be easy to invent wrongly:
///
/// - **The third argument is a height, not a speed.** `[esi+0x20]` is
///   `sqrt(g * apex * -2)` — the -2 is the constant at 0x48f2ec and the
///   gravity is the world's, kept negative — which is the launch speed that
///   peaks at `apex`. The scripts pass 7, 10, 12, 75 and 100, and 100 is a
///   leap rather than a sprint.
/// - **The apex is clamped up to the destination**, not down to the distance:
///   0x430258 compares the argument against `dz` and takes the larger. A jump
///   cannot peak below where it is going.
/// - **The flight time is the two halves added**, `sqrt(2 apex / g)` up and
///   `sqrt(2 (apex - dz) / g)` down, and the ground speed is the horizontal
///   distance over that. When the time is zero the speed stays zero (the
///   `fcom` against 0 at 0x4302ae), rather than dividing.
///
/// Gravity is [`crate::game::body::GRAVITY`], positive here where the
/// original keeps it negative in the world at `[0x5d2700] + 0xc`; the sign
/// cancels against the -2 and the arithmetic is the same.
pub fn launch(from: [f64; 3], to: [f64; 3], apex: f64) -> Launch {
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let dist = (d[0] * d[0] + d[1] * d[1]).sqrt();
    let apex = apex.max(d[2]);
    let g = crate::game::body::GRAVITY;
    let time = (2.0 * apex / g).sqrt() + (2.0 * (apex - d[2]) / g).sqrt();
    Launch {
        rise: (2.0 * g * apex).sqrt(),
        speed: if time > 0.0 { dist / time } else { 0.0 },
        heading: crate::game::body::bearing(d[0], d[1]),
        time,
    }
}

/// A shot in the air, from `mdkShootBullet`. The numbers are the shot
/// table's own — see [`crate::game::world::BULLET`].
#[derive(Clone, Debug)]
pub struct Shot {
    pub kind: f64,
    /// Unit vector. `mdkShootBulletLua` takes it as three numbers and 0x403860
    /// turns them into the bullet's orientation before the launch.
    pub direction: [f64; 3],
    pub speed: f64,
    /// Seconds it has left. -1 in the table means it never times out, and
    /// 0x403d94 tests that before counting down at all.
    pub life: f64,
    pub damage: i16,
    pub filter: i16,
    pub shooter: Option<String>,
    pub target: Option<String>,
}

/// What a level start asked the engine for.
#[derive(Default)]
pub struct Boot {
    /// Named through `mdkPreloadRes` and `mdkSectionAddRes` — what the
    /// engine would have to have in memory before the level runs.
    pub resources: BTreeSet<String>,
    /// Named through `mdkPreloadHardCodedSound`, kept apart because it is a
    /// different loader and `tools/boot.py` counts the two separately.
    pub sounds: BTreeSet<String>,
    pub rooms: Vec<Room>,
    pub checkpoints: Vec<Checkpoint>,
    pub input: Input,
    /// The gob the level told the engine is the player, by name.
    pub player: Option<String>,
    /// `name -> the id of the animation the object is playing`, from
    /// `omAnimPlay`. An object that has not been told plays animation 0.
    pub playing: BTreeMap<String, f64>,
    /// Sounds the scripts hung on objects with `omGobAddSound`. The index is
    /// the handle they hold, which is why these are never removed.
    pub gob_sounds: Vec<GobSound>,
    /// Handles asked to play since the driver last looked. Drained by
    /// whoever owns a sound device, so a boot with no audio just accumulates
    /// nothing.
    pub to_play: Vec<usize>,
    /// The track `chSndSwitchMusic` last asked for. 0 and -1 stop the music.
    pub music: Option<f64>,
    /// How many times an object was moved while this level ran.
    pub moves: u64,
    /// `object -> the slot names it has asked about`, in the order asked;
    /// the index the scripts hold is a position in this list.
    pub slots: BTreeMap<String, Vec<String>>,
    /// `(object, slot)` pairs the scripts have hidden.
    pub hidden: BTreeSet<(String, String)>,
    /// The same, for collision rather than drawing (`omGobGMSetSolid`).
    pub intangible: BTreeSet<(String, String)>,
    /// `chFogStartEnd`, `chFogColor` and `chFogEnable`, which between them
    /// are the game's own draw distance. See [`crate::render::scene::Fog`].
    pub fog: crate::render::scene::Fog,
    /// Objects frozen until the player arrives — a level holds its encounters
    /// this way, and a boot of all ten puts hundreds there.
    pub stasis: BTreeSet<String>,
    /// When each shooter last fired, in seconds of the run's own clock.
    /// See [`may_fire`].
    pub last_shot: BTreeMap<String, f64>,
    /// Doors a script has locked shut. See [`prox_doors`].
    pub locked: BTreeSet<String>,
    /// Blowers a script has switched off. A blower arrives **on** — the
    /// constructor at 0x402c60 writes 1 — so this holds the exceptions.
    pub blower_off: BTreeSet<String>,
    /// Blowers whose length a script has changed from the scene graph's,
    /// which is what `mdkBlowerSetLength` is for.
    pub blower_length: BTreeMap<String, f64>,
    /// How many times a prox door has opened or shut. A run's own count of
    /// the doors it walked through.
    pub doors: usize,
    /// What each walker has been told to face, in radians — the original's
    /// `walker + 0x14`, written by `mdkWalkerHeadToGob` and its point
    /// variant. It is a *want*: nothing turns toward it until there is a
    /// walker update.
    pub heading: BTreeMap<String, f64>,
    /// Walkers currently in the air, from `mdkWalkerJumpToPoint`. See
    /// [`launch`] for the arc and [`tick_touching`] for what flies it.
    pub jumps: BTreeMap<String, Jump>,
    /// What each walker has been told to do with its legs — `walker + 0xc`,
    /// **0 still, 1 walk, 2 run, 3 back** — written by `mdkWalkerGotoPoint`,
    /// its direct variant, `mdkWalkerStop` and the attack. The tick turns it
    /// into both a move and an animation, the two things `mdkWalkerAnimUpdate`
    /// and the move at 0x42fd0d read it for.
    pub gait: BTreeMap<String, i64>,
    /// The body each walker walks with, kept between frames for its vertical
    /// speed and whether it is on the ground. Created the first time a walker
    /// is asked to move and never removed — a walker that stops still stands
    /// on something.
    pub bodies: BTreeMap<String, crate::game::body::Body>,
    /// Which walkers are **mid-turn** — `walker + 0x2c`, the memory half of
    /// the turn's hysteresis. A walker starts turning when it is more than
    /// [`FACING`] off and keeps going until it is inside [`SQUARE`].
    pub turning: BTreeSet<String>,
    /// The closest any shot has passed to the player, in units. A run that
    /// fires and never hits needs to say whether the aim is out by a metre or
    /// by a mile, and `0 shots that hit` cannot.
    pub nearest_miss: Option<f64>,
    /// How much of that nearest miss was **height**, which is the difference
    /// between "the aim is out" and "the bullet flies flat out of the feet".
    pub nearest_drop: f64,
    /// How fast each object the tick moves is going, differenced between
    /// frames. The arena keeps no velocity and the aim needs one.
    pub velocity: BTreeMap<String, [f64; 3]>,
    /// Shots in the air, by the arena id of the bullet gob — **not by name**,
    /// because the scripts create them with `mdkCreateObjectLua("", ...)` and
    /// a bullet has none.
    pub shots: BTreeMap<world::Id, Shot>,
    /// How long each object's current animation has been playing. Reset by
    /// `omAnimPlay`, advanced by the tick, and read against [`Boot::keys`].
    pub since: BTreeMap<String, f64>,
    /// What each walker has left before it may act again — `walker + 0x64`.
    /// Counted down by the tick.
    pub cooldown: BTreeMap<String, f64>,
    /// Rounds left in a walker's burst — `walker + 0x9c` while state 0 has
    /// it. Loaded from the behaviour record's first column.
    pub burst: BTreeMap<String, f64>,
    /// Objects that have been told to fight at least once. `mdkDoganboyAttack`
    /// is a *task*, so this counts the enemies whose script got that far.
    pub fighting: BTreeSet<String>,
    /// **What each character is carrying**, by `(who, bank)`. A bank lives at
    /// `character + 0x6c + bank * 0x20` — capacity at `+0x0c`, the selected
    /// slot at `+0x10`, and an array of 16-byte `{type, count, ?, the object
    /// in its hand}` at `+0x1c` — and there are **two**, chosen by the item
    /// record's `+0x24`. Two banks with a selection each is Doc's *combine*:
    /// `loaf` plus `toaster` is `toast`, itself an item in the table.
    ///
    /// 0x4157d0 is the whole of giving: walk the slots, add to the count when
    /// the type is already there, otherwise take the first empty one and
    /// refuse when there is none. Seven slots (0x40b1fb counts to 6).
    pub carried: BTreeMap<(String, i64), Vec<(f64, i64)>>,
    /// The screen shake: **amplitude in radians, frequency, duration and how
    /// far in it is**. `omSceneShake(scene, amplitude, frequency, duration,
    /// sound)` — 0x41ee00 into 0x45f5f0 — writes the first four into
    /// `scene + 0x11c, +0x120, +0x118` and zeroes the clock at `+0x114`.
    pub shake: Option<Shake>,
    /// Walkers the AI is steering, which is where the original passes
    /// **`avoid`** to the goto core: states 1, 5 and 11 all set it and none
    /// of the 52 `mdkWalkerGotoPoint` calls in the shipped scripts does.
    pub avoiding: BTreeSet<String>,
    /// When each walker may look at its path again. The probe is throttled to
    /// **0.4 seconds** (0x48fa20) on `walker + 0xb4`, and one that found
    /// something in the way waits `chRand() * 3 + 1` before looking again.
    pub probe_at: BTreeMap<String, f64>,
    /// Which way a walker goes round things: `walker + 0x94` is **±1**,
    /// tossed once in the constructor at 0x42f430 against the 0.5 at
    /// 0x48f2fc, so a walker always turns the same way.
    pub side: BTreeMap<String, f64>,
    /// Walkers `mdkWalkerCheckCliffs` has told to watch the floor as well as
    /// the wall — `walker + 0x5c`. Two live calls in the shipped scripts,
    /// both level 7's pilots.
    pub cliffs: BTreeSet<String>,
    /// How solid each object is, from `omGobGMSetTransparency` — 1 opaque,
    /// 0 invisible, and absent means 1. `level6.lua` warps an enemy in over
    /// half a second with `warptime * 2` and finishes by setting it to 1,
    /// which is what settles the direction.
    pub opacity: BTreeMap<String, f64>,
    /// Objects whose animation **wrapped this tick**, and **which
    /// animation** -- 0x461520 looks the instance up by id and 0x461850
    /// returns 0 when there is none, so an object looping animation A does
    /// not answer a script waiting on B. Cleared and refilled every tick.
    pub looped: BTreeMap<String, f64>,
    /// **`(model, animation)` pairs that do not come round again**, out of the
    /// record's `ends` field -- see [`crate::formats::model::Animation::ends`].
    /// 0x4611b0 wraps a 0, reverses a 2 and **clamps everything else**.
    pub oneshot: BTreeSet<(String, i64)>,
    /// Objects whose current animation has run to its end and clamped there.
    /// `omAnimIsPlaying` answers 0 for them, which is what `WaitForAnim` in
    /// `script.lua` waits for, and playing anything clears it.
    pub done: BTreeSet<String>,
    /// How fast each object plays its animation, from `omAnimSetSpeed`. One
    /// per object rather than per animation, because [`Boot::playing`] holds
    /// one animation. **Negative runs it backwards** — `elevators.lua` shuts
    /// a door it opened with `omAnimSetSpeed(door, ANIM_OPEN, -1)`.
    pub speed: BTreeMap<String, f64>,
    /// **Which gob plays each mode**, from `mdkSetPlayModeGobs(mode, gob,
    /// inventory)`. A level names several -- level 6 names Doc, the fish and
    /// Hyde -- and the scripts ask back with `mdkGetPlayModePlayerGob`.
    pub play_modes: BTreeMap<i64, String>,
    /// Walkers in **state 11**, running away from what they were fighting.
    /// The state is held by the cooldown, and it has to be remembered because
    /// the heading is rewritten towards the target on every call -- a
    /// retreating walker that forgot would turn round and charge.
    pub fleeing: BTreeSet<String>,
    /// Walkers that have been nailed down. `mdkWalkerSetTurret(gob, 1)` --
    /// 0x440190 into 0x431850, one store -- writes `walker + 0x90`, and the
    /// chooser at 0x4327ff reads it as the very first question it asks: with
    /// the flag set the whole movement half of the tree is skipped and only
    /// the shoot-or-taunt leaf is left. That is what a turret is -- a walker
    /// that turns and fires from where it stands.
    pub turret: BTreeSet<String>,
    /// **`walker + 0x7c`, the state**, for the machines that need to
    /// remember one. The fight re-decides from scratch on every call and gets
    /// by with the cooldown alone; the other two cannot, because a civilian
    /// standing still and one walking to a corner of its pen look identical
    /// from the outside, and a samsmite prowling looks like a samsmite
    /// charging until it arrives.
    ///
    /// The conehead civilian: 3 chooses, 0x10 looks at you, 0x11 wanders,
    /// 0x14 stands, 0x0b runs. The samsmite: 0x12 prowls, 0x13 charges.
    pub state: BTreeMap<String, i64>,
    /// **The height a flier wants**, and the fact that it flies at all.
    /// State 0x0e of the birdbrain (0x433fd0) picks one and then drives the
    /// mover's z velocity at `def + 0x30` until it is there; here the target
    /// stays and [`crate::game::body::Body`] chases it. Absent means the
    /// walker is on its feet.
    pub altitude: BTreeMap<String, f64>,
    /// **`walker + 0x98`, the goto core's re-aim clock.** 0x431b80 keeps a
    /// heading for `chRand() * 3 + 1` seconds at a time and only then points
    /// at the destination again -- and past ten units it points **off** it by
    /// `(chRand() * 2 - 1) * wobble`, the fourth argument. That is why a
    /// walker crossing a room does not draw a straight line, and it is the
    /// same field the samsmite prowls on.
    pub reaim: BTreeMap<String, f64>,
    /// **The scripts have taken the controls.** `mdkDisablePlayerControl`
    /// and `mdkEnablePlayerControl` (0x43a080 and 0x43a050, into 0x42a8d0 and
    /// 0x42a8c0) are one global each -- `DAT_004bb6a8`, cleared and set --
    /// and the scripts use them around a scripted moment: `boss.lua` three
    /// times, `level4.lua` from inside a task list.
    ///
    /// Written the negative way round so that `Default` is the player
    /// driving, which is what a level starts as.
    ///
    /// **Nothing reads it, deliberately.** A sweep of the 129 checkpoints
    /// counts **13 disables and 1 enable**: the task lists are balanced in the
    /// script -- `level9.lua` has four matched pairs -- but the sequences
    /// between them stall on things this engine does not finish, so the
    /// re-enable is never reached. Honouring the flag today strands the
    /// player for the rest of the level, which is worse than ignoring it.
    /// Wiring the driver to it cost level 9 its keys (48 struck -> 27) and
    /// dropped the player out of the world, which is how this was found.
    pub no_control: bool,
    /// **The play mode the level last asked for.** 0x42b940 parks
    /// `mdkSwitchPlayMode`'s argument in a pending slot and raises a flag;
    /// 0x42b9d0, which is what `mdkGetPlayMode` calls, answers with the
    /// pending one while the flag is up and the committed one otherwise --
    /// so the number a script sees is always the last mode *asked for*.
    ///
    /// It cannot be derived from who the player is, which is what this used
    /// to do: `mdk2.lua` gives `PLAYMODE_KURT` and `PLAYMODE_SNIPER` the
    /// **same gob**, `bob`, so the derivation could never answer 4 and every
    /// `mdkGetPlayMode() == PLAYMODE_SNIPER` in the scripts was false.
    pub mode: i64,
    /// The room the player is in, by name -- the same one `OnEnterRoom` last
    /// fired on. It is kept here because `OnModeSwitch` lands on **the room**
    /// and not on the player: 0x42b940 fires event 0x11 on `FUN_0042ecd0()`,
    /// which reads the room stack at 0x4bb750, and `level1.lua` duly hangs
    /// its handler on `l1_r1`, an `OBJ_ROOM`.
    pub room: Option<String>,
    /// **The pen**: where a walker is allowed to be. `mdkWalkerSetPen(gob,
    /// point, radius)` -- 0x43f4e0 into 0x431810, three stores -- puts the
    /// point in `walker + 0x6c`, the radius in `walker + 0x78` and sets the
    /// flag at `walker + 0x68`. Thirteen live calls in the shipped scripts,
    /// with radii from 5 to 60.
    pub pen: BTreeMap<String, ([f64; 3], f64)>,
    /// Walkers in **state 1**, walking back to their pen, and where to. Same
    /// reason as [`Boot::fleeing`]: the heading has to survive the rewrite.
    pub homing: BTreeMap<String, [f64; 3]>,
    /// **The animation keys**, by model name: `(animation, time, code)`.
    /// A channel whose target kind is 23 carries no geometry — its values are
    /// key codes, and 0x42bf80 splits them four ways:
    ///
    /// | code | what |
    /// |---|---|
    /// | >= 100 | create an object of that `OBJ_*` type |
    /// | 30..99 | `ScreenFlash(code - 29)` |
    /// | 20..29 | `Earthquake(code - 19)` |
    /// | 1..19 | `OnCustomKey(gob, slot, code)` |
    ///
    /// The first line is where an enemy's shot comes from: `hans.mod`
    /// animation 56 carries **421** at t = 0.513 and 421 is `hansshot`.
    /// Filled by whoever has the models — the arena does not.
    pub keys: BTreeMap<String, Vec<(f64, f64, f64)>>,
    /// Keys that have fired, counted so a run can be held to a number.
    pub keys_fired: usize,
    /// How long each animation lasts, `(model, animation) -> seconds`, from
    /// the record's own playback rate. Filled by the driver, because the
    /// arena has no models — the same reason [`Boot::keys`] is.
    pub spans: BTreeMap<(String, i64), f64>,
    /// How many shots a run has fired, since `shots` only holds the live ones.
    pub fired: usize,
    /// How many of them hit something that took damage.
    pub hits: usize,
    /// How many launches a run has ordered. `jumps` only holds the ones still
    /// in the air, so it is empty by the time anyone reads it.
    pub jumped: usize,
    /// Walkers that have been alerted. `mdkWalkerAlert` is idempotent —
    /// 0x431760 tests the flag before setting it — so the shout that goes
    /// with it happens exactly once per walker.
    pub alerted: BTreeSet<String>,
    /// Objects running a scripted sequence — bit **0x800000** in the
    /// original's `omgob[0xb4]`, set by `mdkGobEnableScript` (0x40e210) and
    /// cleared by `mdkGobDisableScript` (0x40e230). The engine calls the Lua
    /// global **`ScriptUpdate(gob)`** once a tick for each of them and for
    /// nothing else (0x42be11 tests the bit before the call), which is how
    /// `scripts/script.lua` drives every cutscene and sequenced action in the
    /// game — `StartScript`, `StartGlobalScript` and `StartMovie` are used
    /// 290 times across the level scripts and all three go through it.
    pub scripted: BTreeSet<String>,
    /// Every object that has *ever* been given a script, which is the measure
    /// of how much of a level a run reaches: a level's content is task lists,
    /// and a task list only runs after something calls `StartScript`.
    pub ever_scripted: BTreeSet<String>,
    /// Seconds since the last frame. A boot has not drawn one, so it holds
    /// the rate the recorded demo runs at — 30 fps — rather than zero, which
    /// would divide.
    pub delta: f64,
    /// The driver's clock, in seconds. `chZeroGlobalTime` sets it to zero
    /// and the timers are due against it.
    pub clock: f64,
    /// What `chSeedRand` was last given. The original seeds with 127 on
    /// every level start, which is why its encounters are reproducible.
    pub seed: Option<u32>,
    /// The generator `chRand` answers out of. It is the original's, and
    /// `chSeedRand(127)` on every level start is why the game's encounters
    /// repeat -- see [`crate::game::rand`].
    pub random: crate::game::rand::Random,
    /// `name -> when its `OnTimer` comes due`, in seconds on the driver's
    /// clock. `omGobSetTimer(gob, 4)` means "call `gob.OnTimer` in four".
    pub timers: BTreeMap<String, f64>,
    /// Registered names with no behaviour here yet, and how often the boot
    /// called each.
    pub unimplemented: BTreeMap<String, usize>,
    /// Every object a `mdkDestroyRoom` took out of the world, rooms and
    /// their contents alike.
    pub destroyed: Vec<String>,
    /// Set when streaming destroyed the very room the level is starting in —
    /// which would mean the delete lists are being applied at the wrong
    /// moment. It must stay at zero across all 129 checkpoints; see
    /// [`stream`].
    pub homeless: usize,
    /// Everything that has been killed, in the order it died. A name can
    /// appear only once: the built-in refuses to hit something already at
    /// zero, so `OnDie` fires exactly once per object.
    pub died: Vec<String>,
    /// `spawner name -> what it makes and when`. See [`Spawner`].
    pub spawners: BTreeMap<String, Spawner>,
    /// Every object a spawner has put in the world, in the order it did, and
    /// the hitpoints it arrived with. A spawner reuses one name, so this is
    /// longer than the arena grows; and the second number is what says the
    /// type table in [`crate::game::world`] reached something real.
    pub spawned: Vec<(String, i16)>,
    /// Every `.lua` in the installation, by lowercased file name, so that
    /// `dofile` can resolve a bare resource name without the file system.
    sources: BTreeMap<String, String>,
}

impl Boot {
    /// The functions a boot called that have no behaviour here, commonest
    /// first — the work list, in the order it is worth doing.
    pub fn work_list(&self) -> Vec<(&str, usize)> {
        let mut out: Vec<(&str, usize)> = self
            .unimplemented
            .iter()
            .map(|(n, c)| (n.as_str(), *c))
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        out
    }
}

/// What one spawner makes, and when.
///
/// The original keeps this on the spawner's own `omgob` at +0x40, and the
/// nine arguments of `mdkSpawnerSetSpawnedObject` land in it at 0x4259f0:
/// the type at +0, the waypoint string at +4, the four numbers at 0x14, 0x18,
/// 0x1c and 0x20, the **interval** at 0x28 and the room at 0x24. The queue is
/// 0x2c, the countdown 0x30 and the shut-off flag 0x44.
///
/// The four numbers are not the spawner's — they are handed straight to the
/// object it makes, into the same four slots a scene graph fills, and their
/// meaning is set by the type. Only the interval belongs to the spawner.
#[derive(Clone, Debug, Default)]
pub struct Spawner {
    /// The `OBJ_*` of the thing it makes.
    pub kind: f64,
    /// A waypoint name, which is what a character wears in `resource`.
    /// The original stores an **empty string**, not a null, when the script
    /// passes `nil`, and then passes null on — so empty means none.
    pub waypoint: Option<String>,
    pub payload: [f64; 4],
    /// Seconds between one and the next, from the eighth argument.
    pub interval: f64,
    pub room: Option<String>,
    /// How many are still owed. `mdkSpawnerQueue` adds to it.
    pub queue: i64,
    /// Counts down by the frame time; at or below zero the next one comes.
    pub timer: f64,
    /// `mdkSpawnerShutOff` sets this and nothing clears it — a spawner that
    /// has been shut off stays off, and further `Queue` calls do nothing.
    pub off: bool,
}

fn number(v: &Value) -> f64 {
    match v {
        Value::Number(n) => *n,
        Value::Integer(i) => *i as f64,
        Value::Boolean(b) => *b as i32 as f64,
        _ => 0.0,
    }
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.to_string_lossy().to_string()),
        _ => None,
    }
}

/// The room a Lua handle refers to: `mdkAddRoom` and `mdkGetRoomNum` both
/// hand back an index, and every other room call takes it back.
fn room_index(v: &Value) -> Option<usize> {
    match v {
        Value::Integer(i) if *i >= 0 => Some(*i as usize),
        Value::Number(n) if *n >= 0.0 => Some(*n as usize),
        _ => None,
    }
}

/// Install the whole surface: the implemented half, then a recorder for
/// every other name the binary registers.
pub fn install(lua: &Lua, sources: BTreeMap<String, String>) -> Result<(), Error> {
    world::install(lua)?;
    lua.set_app_data(Boot { sources, delta: 1.0 / 30.0, ..Boot::default() });
    let globals = lua.globals();

    // --- the scene ------------------------------------------------------
    // **and the two scenes are two scenes.** `mdk2.lua` swaps `scene` to
    // `mdkGetGuiScene()` around each character's inventory and puts it back
    // afterwards, so the third argument of a registration says which world
    // the object lives in. Answering the same string for both put every
    // inventory model **in the level, at the player's feet** -- the pale
    // octagonal pad with four pillars converging that stood under the player
    // on every checkpoint of every level, and hid Hyde inside it.
    globals.set("mdkGetScene", lua.create_function(|_, ()| Ok("scene"))?)?;
    globals.set("mdkGetGuiScene", lua.create_function(|_, ()| Ok("gui"))?)?;

    // `mdkCreateObjectLua(name, type, scene, parent, group)` defines a global
    // of that name, exactly as registering does. Three of the ten levels do
    // not boot without it: `level5.lua` says
    //   mdkCreateObjectLua("doorwav", OBJ_NONE, mdkGetScene(), nil, nil)
    //   doorwav.wav = omGobAddSound(doorwav, "jd_doors", 0)
    globals.set(
        "mdkCreateObjectLua",
        lua.create_function(|lua, args: Variadic<Value>| {
            let name = args.first().and_then(text).unwrap_or_default();
            let id = {
                let mut w = lua
                    .app_data_mut::<world::World>()
                    .ok_or_else(|| mlua::Error::runtime("no world"))?;
                w.register(Gob {
                    name: name.clone(),
                    kind: args.get(1).map(number).unwrap_or(0.0),
                    ..Gob::default()
                })
            };
            world::handle(lua, &name, id, [0.0; 3])
        })?,
    )?;

    // --- streaming ------------------------------------------------------
    // `mdkPreloadRes(name, ...)` and `mdkSectionAddRes(section, name)` are
    // the two that name what a level needs in memory. Recording them is not
    // a stub: it is the loader's own list.
    globals.set(
        "mdkPreloadRes",
        lua.create_function(|lua, args: Variadic<Value>| {
            if let Some(name) = args.first().and_then(text) {
                boot_mut(lua)?.resources.insert(name.to_ascii_lowercase());
            }
            Ok(())
        })?,
    )?;
    globals.set(
        "mdkSectionAddRes",
        lua.create_function(|lua, args: Variadic<Value>| {
            if let Some(name) = args.get(1).and_then(text) {
                boot_mut(lua)?.resources.insert(name.to_ascii_lowercase());
            }
            Ok(())
        })?,
    )?;
    globals.set(
        "mdkPreloadHardCodedSound",
        lua.create_function(|lua, args: Variadic<Value>| {
            if let Some(name) = args.first().and_then(text) {
                boot_mut(lua)?.sounds.insert(name.to_ascii_lowercase());
            }
            Ok(())
        })?,
    )?;

    // --- the rooms ------------------------------------------------------
    globals.set(
        "mdkAddRoom",
        lua.create_function(|lua, gob: Value| {
            let name = match &gob {
                Value::Table(t) => t.get::<String>("name").unwrap_or_default(),
                other => text(other).unwrap_or_default(),
            };
            let mut boot = boot_mut(lua)?;
            if let Some(i) = boot.rooms.iter().position(|r| r.name == name) {
                return Ok(i);
            }
            boot.rooms.push(Room { name, ..Room::default() });
            Ok(boot.rooms.len() - 1)
        })?,
    )?;
    globals.set(
        "mdkGetRoomNum",
        lua.create_function(|lua, gob: Value| {
            let name = match &gob {
                Value::Table(t) => t.get::<String>("name").unwrap_or_default(),
                other => text(other).unwrap_or_default(),
            };
            let boot = boot_ref(lua)?;
            Ok(boot.rooms.iter().position(|r| r.name == name).map(|i| i as i64))
        })?,
    )?;
    globals.set(
        "mdkRoomAddVisible",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(room) = args.first().and_then(room_index) else { return Ok(()) };
            let name = match args.get(1) {
                Some(Value::Table(t)) => t.get::<String>("name").unwrap_or_default(),
                Some(other) => text(other).unwrap_or_default(),
                None => return Ok(()),
            };
            let mut boot = boot_mut(lua)?;
            // the target may not be a room yet; resolve by name after the boot
            let seen = boot.rooms.iter().position(|r| r.name == name);
            if let (Some(target), Some(r)) = (seen, boot.rooms.get_mut(room)) {
                if !r.visible.contains(&target) {
                    r.visible.push(target);
                }
            }
            Ok(())
        })?,
    )?;
    for (name, field) in [
        ("mdkRoomSetMusic", 0usize),
        ("mdkRoomSetEnv", 1),
        ("mdkRoomSetLoad", 2),
        ("mdkRoomSetCheckpoint", 3),
    ] {
        globals.set(
            name,
            lua.create_function(move |lua, args: Variadic<Value>| {
                let Some(room) = args.first().and_then(room_index) else { return Ok(()) };
                let value = args.get(1).map(number);
                let mut boot = boot_mut(lua)?;
                if let Some(r) = boot.rooms.get_mut(room) {
                    match field {
                        0 => r.music = value,
                        1 => r.env = value,
                        2 => r.load = value,
                        _ => r.checkpoint = value,
                    }
                }
                Ok(())
            })?,
        )?;
    }
    globals.set(
        "mdkRoomSetBB",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(room) = args.first().and_then(room_index) else { return Ok(()) };
            let mut box6 = [0.0f64; 6];
            for (c, slot) in box6.iter_mut().enumerate() {
                *slot = args.get(c + 1).map(number).unwrap_or(0.0);
            }
            let mut boot = boot_mut(lua)?;
            if let Some(r) = boot.rooms.get_mut(room) {
                r.bbox = Some(box6);
            }
            Ok(())
        })?,
    )?;

    // --- input ----------------------------------------------------------
    globals.set(
        "omMakeCommand",
        lua.create_function(|lua, args: Variadic<Value>| {
            boot_mut(lua)?.input.commands.push(Command {
                id: args.first().map(number).unwrap_or(0.0),
                key: args.get(1).and_then(text).unwrap_or_default(),
                mode: args.get(2).map(number).unwrap_or(0.0),
            });
            Ok(())
        })?,
    )?;
    globals.set(
        "omBindCommandI",
        lua.create_function(|lua, args: Variadic<Value>| {
            let command = args.first().map(number).unwrap_or(0.0);
            let id = args.get(1).map(number).unwrap_or(-1.0);
            if id >= 0.0 {
                boot_mut(lua)?.input.bindings.push((command, id as u32));
            }
            Ok(())
        })?,
    )?;
    globals.set(
        "omMakeAxis",
        lua.create_function(|lua, _: Variadic<Value>| {
            boot_mut(lua)?.input.axes += 1;
            Ok(())
        })?,
    )?;

    // --- the driver's own state -----------------------------------------
    // A clock, a timer queue and stasis are the three pieces of engine state
    // the level scripts drive their own logic with.
    globals.set(
        "omGobSetTimer",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(name) = args.first().and_then(gob_name) else { return Ok(()) };
            let delay = args.get(1).map(number).unwrap_or(0.0);
            // `omGobSetTimer(gob, 4)` means "in four seconds", so it is due
            // against the driver's clock and not at an absolute four
            let mut boot = boot_mut(lua)?;
            let when = boot.clock + delay;
            boot.timers.insert(name, when);
            Ok(())
        })?,
    )?;
    globals.set(
        "omGobEnterStasis",
        lua.create_function(|lua, args: Variadic<Value>| {
            if let Some(name) = args.first().and_then(gob_name) {
                boot_mut(lua)?.stasis.insert(name);
            }
            Ok(())
        })?,
    )?;
    globals.set(
        "omGobExitStasis",
        lua.create_function(|lua, args: Variadic<Value>| {
            if let Some(name) = args.first().and_then(gob_name) {
                boot_mut(lua)?.stasis.remove(&name);
            }
            Ok(())
        })?,
    )?;
    // `omGobIsStasis(gob)` — **a number**, 0 or 1 (0x41f720 pushes an int),
    // and answering it is not optional. `script.lua`'s `StopScript` reads
    // `omGobIsStasis(self) == 0` before clearing the script flag, so a
    // recorder answering `nil` makes that test false: no script ever stops,
    // the flag stays set, and `ScriptUpdate` runs off the end of its own
    // task list on the next tick. 840 of a run's 1741 calls died that way.
    globals.set(
        "omGobIsStasis",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(name) = args.first().and_then(gob_name) else { return Ok(0.0) };
            Ok(if boot_ref(lua)?.stasis.contains(&name) { 1.0 } else { 0.0 })
        })?,
    )?;
    // the dynamic half of the event surface, writing the same slot the
    // static form does, so both end up in one place
    globals.set(
        "mdkSetLuaEvent",
        lua.create_function(|_, args: Variadic<Value>| {
            if let (Some(Value::Table(gob)), Some(slot), Some(f)) =
                (args.first(), args.get(1).and_then(text), args.get(2))
            {
                gob.set(slot, f.clone())?;
            }
            Ok(())
        })?,
    )?;
    // Distance is answerable for real: the scene graph carries every
    // object's position and every waypoint, and proximity is how this game
    // triggers nearly everything.
    globals.set(
        "mdkGobDistance",
        lua.create_function(|_, args: Variadic<Value>| {
            Ok(distance(args.first(), args.get(1)))
        })?,
    )?;
    globals.set(
        "mdkGobDistancePoint",
        lua.create_function(|lua, args: Variadic<Value>| {
            let point = args
                .get(1)
                .and_then(text)
                .and_then(|n| lua.globals().get::<mlua::Table>("points").ok()?.get::<Value>(n).ok());
            Ok(distance(args.first(), point.as_ref()))
        })?,
    )?;

    // `mdkGobOnMagicSpot(gob, point, radius, angle)` — 0x43c1a0 into
    // **0x40f290**, and it is one of the few recorders whose real answer the
    // engine already has everything for:
    //
    //     d = yaw(gob) - point.facing, wrapped into (-PI, PI]
    //     return dist(gob, point) < radius and |d| < angle
    //
    // Both comparisons are strict, and the wrap is two conditional adds of
    // 2*PI against the constants at 0x48f618 and 0x48f61c. `level3.lua` uses
    // it to know Doc is standing at a washbasin facing it.
    globals.set(
        "mdkGobOnMagicSpot",
        lua.create_function(|lua, args: Variadic<Value>| {
            let point = args
                .get(1)
                .and_then(text)
                .and_then(|n| lua.globals().get::<mlua::Table>("points").ok()?.get::<mlua::Table>(n).ok());
            let (Some(name), Some(point)) = (args.first().and_then(gob_name), point) else {
                return Ok(0.0);
            };
            let radius = args.get(2).map(number).unwrap_or(0.0);
            let angle = args.get(3).map(number).unwrap_or(0.0);
            let Some(w) = world::world(lua) else { return Ok(0.0) };
            let Some(gob) = w.find(&name).and_then(|id| w.get(id)) else { return Ok(0.0) };
            let at = [
                point.get::<f64>("x").unwrap_or(0.0),
                point.get::<f64>("y").unwrap_or(0.0),
                point.get::<f64>("z").unwrap_or(0.0),
            ];
            let far = (0..3)
                .map(|c| (gob.position[c] - at[c]).powi(2))
                .sum::<f64>()
                .sqrt();
            // the waypoint's `f` is degrees in the scene graphs, the gob's
            // yaw comes out of its quaternion in radians
            let facing = point.get::<f64>("f").unwrap_or(0.0).to_radians();
            let q = gob.rotation;
            let yaw = (2.0 * (q[0] * q[3] + q[1] * q[2]))
                .atan2(1.0 - 2.0 * (q[2] * q[2] + q[3] * q[3]));
            let mut d = yaw - facing;
            if d > std::f64::consts::PI {
                d -= std::f64::consts::TAU;
            }
            if d < -std::f64::consts::PI {
                d += std::f64::consts::TAU;
            }
            Ok(if far < radius && d.abs() < angle { 1.0 } else { 0.0 })
        })?,
    )?;

    // `mdkWalkerAlert(gob, shout)` — 0x43f610 into **0x431760**, and it is
    // the loudest thing on level 1's work list at 150 calls.
    //
    //     if walker.alerted == 0:
    //         walker.alerted = 1
    //         if shout: broadcast(gob, <noise>, 100.0, 0)
    //     return 1
    //
    // **Idempotent**: a walker already alerted is not alerted again, and the
    // shout only goes out the first time. The broadcast (0x40e4f0) walks the
    // world, skips the caller, and fires **event 4 — `OnHear`** on everything
    // within the radius, with the handler taking `(self, noise, x, y, z)`.
    // The 100 is the literal `0x42c80000` at 0x431789.
    globals.set(
        "mdkWalkerAlert",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(name) = args.first().and_then(gob_name) else { return Ok(1.0) };
            if !boot_mut(lua)?.alerted.insert(name.clone()) {
                return Ok(1.0); // already alerted, and the shout is once only
            }
            if args.get(1).map(number).unwrap_or(0.0) == 0.0 {
                return Ok(1.0);
            }
            /// The radius of a walker's shout, from 0x431789.
            const EARSHOT: f64 = 100.0;
            let heard: Vec<(String, [f64; 3])> = {
                let Some(w) = world::world(lua) else { return Ok(1.0) };
                let Some(at) = w.find(&name).and_then(|id| w.get(id)).map(|g| g.position)
                else {
                    return Ok(1.0);
                };
                w.iter()
                    .filter(|(_, g)| g.name != name && !g.name.is_empty())
                    .filter(|(_, g)| {
                        (0..3).map(|c| (g.position[c] - at[c]).powi(2)).sum::<f64>()
                            < EARSHOT * EARSHOT
                    })
                    .map(|(_, g)| (g.name.clone(), at))
                    .collect()
            };
            for (who, at) in heard {
                let Ok(gob) = lua.globals().get::<mlua::Table>(who.as_str()) else { continue };
                let Ok(handler) = gob.get::<mlua::Function>("OnHear") else { continue };
                // the noise name is a global buffer in the original, empty
                // for an alert; the position is where the shout came from
                let _ = handler.call::<Value>((gob, "", at[0], at[1], at[2]));
            }
            Ok(1.0)
        })?,
    )?;

    // `mdkWalkerHeadToGob(gob, target)` — 0x43fcd0 into **0x431940** — and
    // `mdkWalkerHeadToPoint(gob, point)`, which is 0x4318a0 and the same
    // shape. Both do two things and only two:
    //
    //     walker.heading = bearing(gob -> target)     ; written to +0x14
    //     return |yaw(gob) - walker.heading| < 0.17   ; the double at 0x490198
    //
    // **They do not turn anything.** The heading is a *want*, stored for the
    // walker update to steer toward, and the return value says whether the
    // walker is already looking there — 0.17 radians, 9.7 degrees, is the
    // whole tolerance. Ten script sites test that return against 1.
    //
    // With no walker update yet a gob never turns, so this answers 1 only
    // when the scene graph already put it facing the right way. That is the
    // correct answer for an engine whose walkers do not move, and it is a
    // computed one rather than the `nil` a recorder gave.
    for name in ["mdkWalkerHeadToGob", "mdkWalkerHeadToPoint"] {
        let to_gob = name == "mdkWalkerHeadToGob";
        globals.set(
            name,
            lua.create_function(move |lua, args: Variadic<Value>| {
                let Some(who) = args.first().and_then(gob_name) else { return Ok(0.0) };
                let to = if to_gob {
                    let Some(target) = args.get(1).and_then(gob_name) else { return Ok(0.0) };
                    let Some(w) = world::world(lua) else { return Ok(0.0) };
                    match w.find(&target).and_then(|id| w.get(id)) {
                        Some(g) => g.position,
                        None => return Ok(0.0),
                    }
                } else {
                    let Some(p) = point_at(lua, args.get(1)) else { return Ok(0.0) };
                    p
                };
                let Some((at, yaw)) = stance(lua, &who) else { return Ok(0.0) };
                let heading = crate::game::body::bearing(to[0] - at[0], to[1] - at[1]);
                boot_mut(lua)?.heading.insert(who, heading);
                Ok(if facing(yaw, heading) { 1.0 } else { 0.0 })
            })?,
        )?;
    }

    // `mdkWalkerStop(gob)` is 0x431870 and is three stores: gait 0, strafe 0,
    // and **the heading set to where the gob already looks**, which is how a
    // stop differs from an order to face forwards.
    //
    // Its two opposite numbers order a walk. `mdkWalkerGotoPointDirectly`
    // (0x43f890 into **0x431f70**) has no randomness in it at all:
    //
    //     if dist3(gob, dest) < radius: return 1        ; arrived
    //     walker.heading = bearing(gob -> dest)
    //     walker.strafe  = 0
    //     walker.gait    = facing ? (run and 2 or 1) : 0
    //     return 0
    //
    // The gait going to **0** when it is not facing yet is what "directly"
    // means: turn on the spot first, then move.
    //
    // `mdkWalkerGotoPoint(gob, point, run, mustFace, wobble, avoid, radius)`
    // is 0x43f6f0 into **0x431b80**, the same shape with three differences.
    // The distance is **horizontal** — 0x431bbe writes the destination's z
    // and 0x431bc3 immediately overwrites it with the gob's, so the vector it
    // measures against is `(dest.x, dest.y, gob.z)`. The radius **defaults to
    // 4.0** (the double the binding pushes at 0x43f825). And the heading is
    // refreshed on a random countdown in `walker + 0x98` rather than every
    // frame, with `wobble` scaling a random nudge onto it.
    //
    // Both the countdown and the nudge are dead in this game. **Every one of
    // the 52 calls in the shipped scripts is `(gob, point, 1, 0, 0, 0)`** —
    // wobble 0, so the nudge adds nothing, and avoid 0, so the probe at
    // 0x431490 never runs. What is left is deterministic, and a heading
    // refreshed every frame equals one refreshed on a timer whenever the
    // destination is a fixed waypoint, which is what a waypoint is. The
    // `mustFace` gate is implemented because it is two lines; the avoid probe
    // is not, so a blocked walker is not detected.
    //
    // **Neither stops the legs on arrival** — neither 0x431c14 nor 0x431fba
    // touches the gait — which is why the scripts follow a goto with a stop.
    for name in ["mdkWalkerGotoPoint", "mdkWalkerGotoPointDirectly"] {
        let direct = name == "mdkWalkerGotoPointDirectly";
        globals.set(
            name,
            lua.create_function(move |lua, args: Variadic<Value>| {
                let Some(who) = args.first().and_then(gob_name) else { return Ok(0.0) };
                let Some(to) = point_at(lua, args.get(1)) else { return Ok(0.0) };
                let Some((at, yaw)) = stance(lua, &who) else { return Ok(0.0) };
                let run = args.get(2).map(number).unwrap_or(0.0) != 0.0;
                let (must_face, radius) = if direct {
                    // the direct one takes its radius where the other takes
                    // its flags, and it has no default
                    (true, args.get(3).map(number).unwrap_or(0.0))
                } else {
                    /// The arrival radius `mdkWalkerGotoPoint` assumes.
                    const REACHED: f64 = 4.0;
                    (
                        args.get(3).map(number).unwrap_or(0.0) != 0.0,
                        args.get(6).map(number).unwrap_or(REACHED),
                    )
                };
                // **and only one of the two is the core.** 0x431e30 sets
                // the destination and hands it to 0x431b80; the direct one
                // (0x431f70) is its own twenty lines -- a **three**
                // dimensional distance against the radius, the bearing
                // straight at the point with no wobble and no clock, and the
                // 0.17-radian gate always on.
                if direct {
                    let d = [to[0] - at[0], to[1] - at[1], to[2] - at[2]];
                    if (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() < radius {
                        return Ok(1.0);
                    }
                    let heading = crate::game::body::bearing(d[0], d[1]);
                    let square = facing(yaw, heading);
                    let mut boot = boot_mut(lua)?;
                    boot.heading.insert(who.clone(), heading);
                    boot.avoiding.remove(&who);
                    boot.gait.insert(who, if !square { 0 } else if run { 2 } else { 1 });
                    return Ok(0.0);
                }
                let wobble = args.get(4).map(number).unwrap_or(0.0);
                let avoid = args.get(5).map(number).unwrap_or(0.0) != 0.0;
                let kind = with_gob(lua, args.first(), |g| g.kind).unwrap_or(0.0);
                let mut boot = boot_mut(lua)?;
                // every one of the 52 shipped calls passes `avoid` 0, so a
                // script's goto is the one movement that does not probe
                if !avoid {
                    boot.avoiding.remove(&who);
                }
                let there = goto_core(
                    &mut boot, &who, kind, at, yaw, to, run, must_face, wobble, avoid, radius,
                );
                Ok(if there { 1.0 } else { 0.0 })
            })?,
        )?;
    }
    // `mdkWalkerSetPen(gob, point, radius)` -- 0x43f4e0 into **0x431810**,
    // which is three stores and no arithmetic: the point goes into `walker +
    // 0x6c`, the radius into `walker + 0x78`, and `walker + 0x68` is set to 1
    // so the AI starts checking it. A null point leaves the home where the
    // constructor put it, which is where the walker was placed (0x42f3e5).
    //
    // Thirteen live calls across five levels, radii 5 to 60, and every one of
    // them names a waypoint object. Nothing clears a pen once set.
    globals.set(
        "mdkWalkerSetPen",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(who) = args.first().and_then(gob_name) else { return Ok(0.0) };
            let Some((at, _)) = stance(lua, &who) else { return Ok(0.0) };
            let home = point_at(lua, args.get(1)).unwrap_or(at);
            let leash = args.get(2).map(number).unwrap_or(0.0);
            boot_mut(lua)?.pen.insert(who, (home, leash));
            Ok(1.0)
        })?,
    )?;
    // `mdkWalkerSetTurret(gob, on)` -- 0x440190 into **0x431850**, a single
    // store of the argument into `walker + 0x90`. Eleven script sites and
    // 2860 calls over the 129 checkpoints, because most of them sit inside an
    // `OnUpdate`. See [`Boot::turret`] for what the flag costs the chooser.
    globals.set(
        "mdkWalkerSetTurret",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(who) = args.first().and_then(gob_name) else { return Ok(0.0) };
            let mut boot = boot_mut(lua)?;
            if args.get(1).map(number).unwrap_or(0.0) != 0.0 {
                boot.turret.insert(who);
            } else {
                boot.turret.remove(&who);
            }
            Ok(0.0)
        })?,
    )?;
    // **The inventory.** `mdkDocGiveItem(gob, type, count)` is 0x43e0d0 into
    // 0x40ce40, which is four lines: look the record up, force the count to
    // **-1 when the record's give column is negative** — unlimited, which is
    // `sniperbullet` and `loaf` — and hand it to 0x4157d0 with the bank the
    // record names. `mdkMaxGiveItem` is the same call for Max.
    //
    // ponytail: the original also creates the *model* in the character's hand
    // and hangs it off a node. The engine keeps the counts and not the props.
    for name in ["mdkDocGiveItem", "mdkMaxGiveItem"] {
        globals.set(
            name,
            lua.create_function(|lua, args: Variadic<Value>| {
                let Some(who) = args.first().and_then(gob_name) else { return Ok(()) };
                let kind = args.get(1).map(number).unwrap_or(0.0);
                /// The type the tables use for "nothing", which the give
                /// refuses outright (0x40b1bb and 0x4157e8).
                const NOTHING: f64 = 399.0;
                if kind == 0.0 || kind == NOTHING {
                    return Ok(());
                }
                let Some(bank) = crate::game::world::item_bank(kind) else { return Ok(()) };
                let mut count = args.get(2).map(number).unwrap_or(1.0) as i64;
                if crate::game::world::item_gives(kind).is_some_and(|g| g < 0) {
                    count = -1;
                }
                /// How many slots a bank holds — 0x40b1fb counts to 6.
                const SLOTS: usize = 7;
                let mut boot = boot_mut(lua)?;
                let held = boot.carried.entry((who, bank)).or_default();
                match held.iter().position(|&(t, _)| t == kind) {
                    Some(i) if held[i].1 >= 0 && count >= 0 => held[i].1 += count,
                    Some(i) => held[i].1 = -1,
                    None if held.len() < SLOTS => held.push((kind, count)),
                    None => {}
                }
                Ok(())
            })?,
        )?;
    }
    // `mdkDocHasItem(gob, type)` — 0x43f270 into 0x40d000, which is "the
    // count in that type's own bank is above zero".
    globals.set(
        "mdkDocHasItem",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(who) = args.first().and_then(gob_name) else { return Ok(0.0) };
            let kind = args.get(1).map(number).unwrap_or(0.0);
            let Some(bank) = crate::game::world::item_bank(kind) else { return Ok(0.0) };
            let has = boot_ref(lua)?
                .carried
                .get(&(who, bank))
                .is_some_and(|held| held.iter().any(|&(t, n)| t == kind && n != 0));
            Ok(if has { 1.0 } else { 0.0 })
        })?,
    )?;
    // `mdkKurtRemoveItem(gob, type, count)` and Doc's — 0x415980 takes the
    // count *off* the slot and, when it reaches zero, **drops the slot and
    // shifts the rest down**, which is why the original re-reads the type
    // afterwards to see whether the hand emptied.
    for name in ["mdkKurtRemoveItem", "mdkDocRemoveItem"] {
        globals.set(
            name,
            lua.create_function(|lua, args: Variadic<Value>| {
                let Some(who) = args.first().and_then(gob_name) else { return Ok(()) };
                let kind = args.get(1).map(number).unwrap_or(0.0);
                let count = args.get(2).map(number).unwrap_or(1.0) as i64;
                let Some(bank) = crate::game::world::item_bank(kind) else { return Ok(()) };
                let mut boot = boot_mut(lua)?;
                let Some(held) = boot.carried.get_mut(&(who, bank)) else { return Ok(()) };
                if let Some(i) = held.iter().position(|&(t, _)| t == kind) {
                    if held[i].1 >= 0 {
                        held[i].1 -= count;
                        if held[i].1 < 1 {
                            held.remove(i);
                        }
                    }
                }
                Ok(())
            })?,
        )?;
    }
    // `mdkDocGetHeldItem(gob, bank)` — 0x43f400 into 0x40d040, one read of
    // `character + 0xb4 + bank * 4`, which the select at 0x40b7xx writes with
    // the type it put in that hand. Nothing here switches hands, so what is
    // held is the first slot.
    globals.set(
        "mdkDocGetHeldItem",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(who) = args.first().and_then(gob_name) else { return Ok(0.0) };
            let bank = args.get(1).map(number).unwrap_or(0.0) as i64;
            Ok(boot_ref(lua)?
                .carried
                .get(&(who, bank))
                .and_then(|held| held.first())
                .map(|&(t, _)| t)
                .unwrap_or(0.0))
        })?,
    )?;
    // `omSceneShake(scene, amplitude, frequency, duration, sound)` —
    // 0x41ee00 into **0x45f5f0**, four stores and a string copy. The sound is
    // `rumble1` in every one of the game's calls and the engine does not play
    // it. See [`Shake`] for what the four numbers become.
    globals.set(
        "omSceneShake",
        lua.create_function(|lua, args: Variadic<Value>| {
            let at = |i: usize| args.get(i).map(number).unwrap_or(0.0);
            boot_mut(lua)?.shake = Some(Shake {
                amplitude: at(1),
                frequency: at(2),
                duration: at(3),
                elapsed: 0.0,
            });
            Ok(())
        })?,
    )?;
    // `mdkWalkerCheckCliffs(gob)` sets `walker + 0x5c`, and all that field
    // does is add the **second** ray to the path probe: from the far end of
    // the look-ahead, five units down, and hitting nothing is a cliff. Two
    // live calls in the shipped scripts, both level 7's pilots.
    globals.set(
        "mdkWalkerCheckCliffs",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(who) = args.first().and_then(gob_name) else { return Ok(()) };
            boot_mut(lua)?.cliffs.insert(who);
            Ok(())
        })?,
    )?;
    // `omGobGMSetTransparency(gob, alpha)` — 0x41f780 into **0x462920**,
    // which writes the value into the gob's two renderer nodes and then
    // **recurses through its children** (0x45f7b0 walks them), so a character
    // and everything it wears fade together. 3664 calls in a two-minute run
    // of the ten levels, the top of the work list after the animation clock.
    globals.set(
        "omGobGMSetTransparency",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(who) = args.first().and_then(gob_name) else { return Ok(()) };
            let alpha = args.get(1).map(number).unwrap_or(1.0);
            let family = {
                let Some(w) = world::world(lua) else { return Ok(()) };
                let Some(id) = w.find(&who) else { return Ok(()) };
                w.family(id)
            };
            let mut boot = boot_mut(lua)?;
            for name in family {
                boot.opacity.insert(name, alpha);
            }
            Ok(())
        })?,
    )?;
    // `omGobDelete(gob)` — 0x41f1c0 into 0x46e5e0, one argument and no
    // result. Thirty-seven calls in a run: a missile deleting itself
    // `OnTimer`, a level clearing its stars at a checkpoint, every conehead
    // at once. The engine already takes an object and its children out of the
    // world for `mdkDestroyRoom`, and a gob is a room's own case of that.
    globals.set(
        "omGobDelete",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(who) = args.first().and_then(gob_name) else { return Ok(()) };
            destroy_room(lua, &who)?;
            Ok(())
        })?,
    )?;
    // `mdkWalkerAnimUpdate(gob)` — 0x43f490 into **0x42f700**, and it returns
    // nothing: it is a command, and the command is "put the animation that
    // matches this walker's gait up". The tick already does that for every
    // walker it knows, so all this has to add is **knowing the walker**: a
    // tick already does that for every walker it has a gait for, so this has
    // nothing left to do.
    //
    // It **enrolled** the walker at first — a gait of 0 where there was none,
    // to put a scripted idle character into `ANIM_READY0`. That was an
    // invention and it cost level 8 its whole run: the scripts call this for
    // the player too, the tick then drove `bob` as a walker beside the
    // driver's own body, and the two fought each other into a wall for 3413
    // of 3600 frames. A gait entry is what `mdkWalkerGotoPoint`, the stop and
    // the attack create; nothing else may.
    globals.set(
        "mdkWalkerAnimUpdate",
        lua.create_function(|_, _: Variadic<Value>| Ok(()))?,
    )?;
    // `mdkWalkerPlayAnim(gob, animation)` — 0x43f580 into **0x4317b0**, and
    // it is a *request*, not an order. Three gates: `walker + 0x4` is a latch
    // meaning no one-off pose is up, 0x461650 asks whether the model even
    // carries that animation, and only then does it clear the latch, remember
    // the id in `walker + 0x48` and play it with **`ANIMFLAG_INTERRUPT`**. It
    // answers 1 when it played and 0 when it refused, which is why the
    // scripts call it every frame until it takes — 3625 times in a run.
    //
    // The engine has no latch, and does not need one: the same predicate the
    // tick uses to decide whether the legs may take a pose back is what
    // `walker + 0x4` holds. A gait animation, nothing at all, or a one-off
    // that has played its loop, and the walker is free.
    globals.set(
        "mdkWalkerPlayAnim",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(who) = args.first().and_then(gob_name) else { return Ok(0.0) };
            let id = args.get(1).map(number).unwrap_or(0.0);
            let kind = with_gob(lua, args.first(), |g| g.kind).unwrap_or(0.0);
            let Some(model) = model_for_type(kind) else { return Ok(0.0) };
            let mut boot = boot_mut(lua)?;
            if !boot.spans.contains_key(&(model, id as i64)) {
                return Ok(0.0);
            }
            let up = boot.playing.get(&who).copied();
            let free = boot.looped.contains_key(&who)
                || up.is_none_or(|a| {
                    crate::game::world::GAIT_ANIM.contains(&a)
                        || crate::game::world::GAIT_ANIM_HURT.contains(&a)
                });
            if !free {
                return Ok(0.0);
            }
            boot.playing.insert(who.clone(), id);
            boot.since.insert(who, 0.0);
            Ok(1.0)
        })?,
    )?;
    globals.set(
        "mdkWalkerStop",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(who) = args.first().and_then(gob_name) else { return Ok(0.0) };
            let Some((_, yaw)) = stance(lua, &who) else { return Ok(0.0) };
            let mut boot = boot_mut(lua)?;
            boot.heading.insert(who.clone(), yaw);
            boot.avoiding.remove(&who);
            boot.gait.insert(who, 0);
            Ok(1.0)
        })?,
    )?;

    // **The birdbrain flies**, and it is the last of the enemy machines the
    // scripts deploy in numbers -- eleven sites over levels 5, 6, 7 and 9.
    // `mdkBirdbrainAttack(gob)` is 0x440610 into **0x433a70**, four states on
    // `walker + 0x7c`:
    //
    // - **1, go home.** The goto core at the pen, and it gives up on half the
    //   leash, on you coming inside 15, or on the cooldown with a clear line.
    // - **4, hover.** Gait 0 and the nose on you. With the line clear, no
    //   friend in the way and rounds left it **fires**: past `def + 0x54`
    //   (10 for a birdbrain) that is `ANIM_SHOOT` and a **1** second wait,
    //   inside it animation **15** and **3** seconds, and either way it only
    //   fires once it is inside **0.17 radians** of the bearing. With no line
    //   or no rounds it repositions: outside the leash back to state 1, else
    //   a roll -- over **0.4** it closes (state 5, or reloads three rounds if
    //   you are already inside 30), under it it **changes height**.
    // - **5, chase.** The goto core straight at you, and out again on the
    //   leash, on 30 units, or on the cooldown with a clear line.
    // - **0x0e, change height.** Climb or dive at `def + 0x30` -- 6.5 a
    //   second for a birdbrain -- until it is within half a unit of the
    //   height state 4 picked, which is **your own z plus ten**.
    //
    // Two readings settle the flight and they agree exactly: `def + 0x14`
    // **bit 2** is set on four records and those same four are the only ones
    // with a vertical speed at `def + 0x30`. See [`world::FLIES`]. The AI's
    // own first question is that bit -- a flier is always awake, a walker has
    // to be standing -- so the flag is not a guess.
    //
    // ponytail: half the time the original picks a random height in a band
    // whose top is `character + 0xa4`, a float **nothing in the binary was
    // found to write**. Here it always takes your own z plus ten, floored at
    // the pen's, and that column stays unread rather than invented.
    globals.set(
        "mdkBirdbrainAttack",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(who) = args.first().and_then(gob_name) else { return Ok(0.0) };
            let Some((at, yaw)) = stance(lua, &who) else { return Ok(0.0) };
            /// How near you have to come for it to give up going home.
            const HOME_IF: f64 = 15.0;
            /// And how near before it stops chasing and hovers: 0x48f394.
            const CLOSE_ENOUGH: f64 = 30.0;
            /// The roll between closing and changing height: 0x48fa20.
            const CLOSES: f64 = 0.4;
            /// How high above you it wants to be: the 10 at 0x48f384.
            const ABOVE: f64 = 10.0;
            /// How square it has to be before it fires: the 0.17 at 0x490198.
            const SQUARE_ON: f64 = 0.17;
            /// And how near the height it wants before it stops climbing.
            const ARRIVED: f64 = 0.5;
            /// What a shot and a swipe cost it.
            const AFTER_SHOT: f64 = 1.0;
            const AFTER_SWIPE: f64 = 3.0;
            /// Three rounds is a full magazine, and going home is 3 seconds.
            const ROUNDS: f64 = 3.0;
            const WALK_BACK: f64 = 3.0;
            /// Animation 15, which is what it does to you close up.
            const ANIM_SWIPE: f64 = 15.0;
            let kind = with_gob(lua, args.first(), |g| g.kind).unwrap_or(0.0);
            // only a flier runs this at all -- the flag is the AI's own gate
            if crate::game::world::climb(kind).is_none() {
                return Ok(0.0);
            }
            let hero = lua
                .named_registry_value::<mlua::Table>("player")
                .ok()
                .and_then(|p| p.get::<String>("name").ok());
            let to = hero.as_deref().and_then(|n| stance(lua, n).map(|(p, _)| p));
            let Some(to) = to else {
                boot_mut(lua)?.gait.insert(who, 0);
                return Ok(0.0);
            };
            let eye = crate::game::body::EYE;
            let solid = lua
                .app_data_ref::<std::rc::Rc<crate::game::body::Collision>>()
                .map(|c| c.clone());
            let dist = (0..3).map(|c| (to[c] - at[c]).powi(2)).sum::<f64>().sqrt();
            let bearing = crate::game::body::bearing(to[0] - at[0], to[1] - at[1]);
            let mut boot = boot_mut(lua)?;
            // **The line, and the friend in it.** 0x433c30 is the ray, and
            // the outer gate is "already alerted, or inside five units, or in
            // front"; 0x402af0 is the second question -- another walker whose
            // own collision capsule the segment passes through.
            let seen = boot.fighting.contains(&who)
                || dist < 5.0
                || facing_within(yaw, bearing, std::f64::consts::FRAC_PI_2);
            let clear = seen
                && solid.as_ref().is_none_or(|c| {
                    c.sees([at[0], at[1], at[2] + eye], [to[0], to[1], to[2] + eye])
                });
            // ponytail: a friend is a point with a body's own width here, and
            // the width the table gives its type is unread, so one unit.
            let blocked_by_friend = {
                let w = match world::world(lua) {
                    Some(w) => w,
                    None => return Ok(0.0),
                };
                let seg = [to[0] - at[0], to[1] - at[1]];
                let len2 = seg[0] * seg[0] + seg[1] * seg[1];
                let out = w
                    .iter()
                    .filter(|(_, g)| g.hitpoints > 0 && g.name != who)
                    .filter(|(_, g)| hero.as_deref() != Some(g.name.as_str()))
                    .filter(|(_, g)| crate::game::world::base_hitpoints(g.kind).is_some())
                    .any(|(_, g)| {
                        if len2 <= 1e-9 {
                            return false;
                        }
                        let d = [g.position[0] - at[0], g.position[1] - at[1]];
                        let t = ((d[0] * seg[0] + d[1] * seg[1]) / len2).clamp(0.0, 1.0);
                        let off = [d[0] - seg[0] * t, d[1] - seg[1] * t];
                        (off[0] * off[0] + off[1] * off[1]).sqrt() < 1.0
                    });
                out
            };
            if !boot.fighting.contains(&who) {
                if !clear {
                    return Ok(0.0);
                }
                boot.fighting.insert(who.clone());
            }
            let state = boot.state.get(&who).copied().unwrap_or(4);
            let cool = boot.cooldown.get(&who).copied().unwrap_or(0.0);
            let left = boot.burst.get(&who).copied().unwrap_or(ROUNDS);
            let (home, leash) = boot.pen.get(&who).copied().unwrap_or((at, 0.0));
            let dhome = (0..3).map(|c| (home[c] - at[c]).powi(2)).sum::<f64>().sqrt();
            boot.heading.insert(who.clone(), bearing);
            match state {
                1 => {
                    // 0x433cf0 hands the core `(run 1, mustFace 1, wobble 0,
                    // avoid 1, radius 4)` -- the same arguments the doganboy's
                    // walk home uses
                    goto_core(
                        &mut boot, &who, kind, at, yaw, home, true, true, 0.0, true, 4.0,
                    );
                    if dhome < leash * 0.5
                        || dist < HOME_IF
                        || (cool <= 0.0 && clear && !blocked_by_friend)
                    {
                        boot.state.insert(who.clone(), 4);
                        boot.burst.insert(who.clone(), ROUNDS);
                        boot.cooldown.insert(who, 0.0);
                    }
                }
                5 => {
                    // and the chase is `(run 1, mustFace 0, wobble pi/6,
                    // avoid 1, radius 4)` -- 0x433e6a, and the thirty degrees
                    // is why a closing birdbrain does not come in straight
                    goto_core(
                        &mut boot,
                        &who,
                        kind,
                        at,
                        yaw,
                        to,
                        true,
                        false,
                        std::f64::consts::FRAC_PI_6,
                        true,
                        4.0,
                    );
                    if dhome > leash && leash > 0.0
                        || dist < CLOSE_ENOUGH
                        || (cool <= 0.0 && clear && !blocked_by_friend)
                    {
                        boot.state.insert(who.clone(), 4);
                        boot.burst.insert(who.clone(), ROUNDS);
                        if cool > 1.0 {
                            boot.cooldown.insert(who, 1.0);
                        }
                    }
                }
                0x0e => {
                    // the climb itself is the body's; this only says when it
                    // has arrived
                    boot.gait.insert(who.clone(), 0);
                    let want = boot.altitude.get(&who).copied().unwrap_or(at[2]);
                    if (want - at[2]).abs() <= ARRIVED {
                        boot.state.insert(who.clone(), 4);
                        boot.burst.insert(who, 1.0);
                    }
                }
                _ => {
                    boot.gait.insert(who.clone(), 0);
                    if !clear || blocked_by_friend || left < 1.0 {
                        if leash > 0.0 && dhome > leash {
                            boot.state.insert(who.clone(), 1);
                            boot.cooldown.insert(who, WALK_BACK);
                            return Ok(0.0);
                        }
                        if boot.random.next() >= CLOSES {
                            if dist <= CLOSE_ENOUGH {
                                boot.burst.insert(who, ROUNDS);
                                return Ok(0.0);
                            }
                            boot.state.insert(who.clone(), 5);
                            boot.cooldown.insert(who, WALK_BACK);
                            return Ok(0.0);
                        }
                        boot.state.insert(who.clone(), 0x0e);
                        boot.altitude.insert(who, (to[2] + ABOVE).max(home[2]));
                        return Ok(0.0);
                    }
                    if cool > 0.0 || !facing(yaw, bearing) {
                        return Ok(0.0);
                    }
                    let far = dist >= crate::game::world::shoot_within(kind);
                    boot.playing.insert(who.clone(), if far { ANIM_SHOOT } else { ANIM_SWIPE });
                    boot.since.insert(who.clone(), 0.0);
                    boot.burst.insert(who.clone(), left - 1.0);
                    boot.cooldown.insert(who, if far { AFTER_SHOT } else { AFTER_SWIPE });
                    let _ = SQUARE_ON;
                }
            }
            Ok(0.0)
        })?,
    )?;
    // **The samsmite is a kamikaze**, and it is the smallest of the three AI
    // machines: `mdkSamsmiteAttack(gob)` is 0x4403e0 into **0x434340**, two
    // states on `walker + 0x7c`.
    //
    // - **0x12, prowl.** While `walker + 0x98` has time left it walks (gait
    //   1), and if the path ahead is blocked within **3** units it turns a
    //   right angle (0x4901b4 is pi/2) and keeps the clock. When the clock is
    //   out it looks: inside **18** units (0x4901c8), with the target
    //   reachable, no more than **0.5** above it, in front of it, and with
    //   the target inside its own pen, it turns to face you and **charges**.
    //   Otherwise it sets the clock to `chRand() + 0.3` and walks on -- at
    //   you with a `(chRand() - 0.5) * pi/2` wobble if you are in its patch,
    //   and at a random point in the pen if you are not.
    // - **0x13, charge.** Gait 2 straight at you while the path is clear,
    //   you are inside **36** units (0x4901cc) and it is not past its leash
    //   less two. Any of those fails and it stops, turns round and prowls
    //   again. Inside **2** units it arrives.
    //
    // **Arriving is an explosion.** 0x40e930 into 0x40e960 is the game's area
    // damage: every gob within the radius takes `damage - distance` of the
    // given type. A samsmite spends **15 at 5 units** and then dies -- event
    // 10, `OnDie(gob, 1)`, straight out of 0x40e1b0. The one exception is
    // type 0xd7, and that is **`OBJ_FLAMINGSAMSMITE`, 215** -- one of the
    // eight constants that read as 1.4e-312 until today. It spends **7**,
    // turns round and prowls again, which is why the level 5 spawners keep
    // making them.
    globals.set(
        "mdkSamsmiteAttack",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(who) = args.first().and_then(gob_name) else { return Ok(0.0) };
            let Some((at, yaw)) = stance(lua, &who) else { return Ok(0.0) };
            /// Inside this it decides to charge, and past this it gives one
            /// up: the floats at 0x4901c8 and 0x4901cc.
            const CHARGE_AT: f64 = 18.0;
            const GIVE_UP: f64 = 36.0;
            /// How close is contact, and how much of the leash it will not
            /// charge past: the 2.0 at 0x48f598, used for both.
            const CONTACT: f64 = 2.0;
            /// The right angle a blocked one turns, and the wobble it walks
            /// with: the pi/2 at 0x4901b4, used for both.
            const AWAY: f64 = std::f64::consts::FRAC_PI_2;
            /// How far ahead it looks while prowling and while charging.
            const LOOK_PROWL: f64 = 3.0;
            const LOOK_CHARGE: f64 = 4.0;
            /// How long it walks before looking again: `chRand() + 0.3`.
            const AGAIN: f64 = 0.3;
            /// It will not charge something more than this far above it.
            const STEP_UP: f64 = 0.5;
            /// The blast: five units, fifteen points, and seven for the
            /// flaming one that survives its own.
            const BLAST: f64 = 5.0;
            const SPENDS: i64 = 15;
            const FLAMING_SPENDS: i64 = 7;
            /// `OBJ_FLAMINGSAMSMITE`, the one that walks away from it.
            const FLAMING: f64 = 215.0;
            /// `DAMAGE_KNOCKDOWN | DAMAGE_BADGUY`, the 0x402 it is dealt with.
            const KNOCKDOWN: i64 = 0x402;
            let hero = lua
                .named_registry_value::<mlua::Table>("player")
                .ok()
                .and_then(|p| p.get::<String>("name").ok());
            let to = hero.as_deref().and_then(|n| stance(lua, n).map(|(p, _)| p));
            let Some(to) = to else {
                boot_mut(lua)?.gait.insert(who, 0);
                return Ok(0.0);
            };
            let kind = with_gob(lua, args.first(), |g| g.kind).unwrap_or(0.0);
            let solid = lua
                .app_data_ref::<std::rc::Rc<crate::game::body::Collision>>()
                .map(|c| c.clone());
            // the same ray the tick's probe casts, without its throttle or
            // its cliff leg -- 0x434340 passes 0 for the cliff flag
            let clear = |heading: f64, reach: f64| {
                let ahead = crate::game::body::facing(heading).0;
                let from = [at[0], at[1], at[2] + 1.0];
                let end = [from[0] + ahead[0] * reach, from[1] + ahead[1] * reach, from[2]];
                solid.as_ref().is_none_or(|c| c.sees(from, end))
            };
            let dist = (0..3).map(|c| (to[c] - at[c]).powi(2)).sum::<f64>().sqrt();
            let bearing = crate::game::body::bearing(to[0] - at[0], to[1] - at[1]);
            let mut boot = boot_mut(lua)?;
            let state = boot.state.get(&who).copied().unwrap_or(0);
            if state != 0x12 && state != 0x13 {
                boot.state.insert(who, 0x12);
                return Ok(0.0);
            }
            let (home, leash) = boot.pen.get(&who).copied().unwrap_or((at, 0.0));
            let cool = boot.cooldown.get(&who).copied().unwrap_or(0.0);
            let heading = boot.heading.get(&who).copied().unwrap_or(yaw);
            if state == 0x13 {
                // out of bounds stops a charge, and so does losing sight
                let out = leash > 0.0
                    && (0..3).map(|c| (home[c] - at[c]).powi(2)).sum::<f64>().sqrt()
                        > leash - CONTACT;
                if dist >= CONTACT {
                    if clear(heading, LOOK_CHARGE) && dist <= GIVE_UP && !out {
                        boot.gait.insert(who, 2);
                        return Ok(0.0);
                    }
                    boot.gait.insert(who.clone(), 0);
                    if !out {
                        boot.heading.insert(who.clone(), heading + std::f64::consts::PI);
                    }
                    boot.state.insert(who, 0x12);
                    return Ok(0.0);
                }
                let flaming = kind == FLAMING;
                drop(boot);
                blast(
                    lua,
                    &who,
                    at,
                    BLAST,
                    if flaming { FLAMING_SPENDS } else { SPENDS },
                    KNOCKDOWN,
                )?;
                let mut boot = boot_mut(lua)?;
                if flaming {
                    boot.gait.insert(who.clone(), 0);
                    boot.heading.insert(who.clone(), heading + std::f64::consts::PI);
                    boot.state.insert(who, 0x12);
                    return Ok(0.0);
                }
                boot.state.remove(&who);
                drop(boot);
                // 0x40e1b0 is `OnDie(gob, 1)` and nothing else; [`die`] is
                // that plus what the walker's own damage handler does to a
                // corpse, which is what the rest of the engine expects
                die(lua, &who)?;
                return Ok(0.0);
            }
            // 0x12
            if cool > 0.0 {
                if !clear(heading, LOOK_PROWL) {
                    let side = match boot.side.get(&who) {
                        Some(&s) => s,
                        None => {
                            let s = if boot.random.next() < 0.5 { 1.0 } else { -1.0 };
                            boot.side.insert(who.clone(), s);
                            s
                        }
                    };
                    boot.heading.insert(who, heading + side * AWAY);
                    return Ok(0.0);
                }
                boot.gait.insert(who, 1);
                return Ok(0.0);
            }
            let inside = leash <= 0.0
                || (0..3).map(|c| (to[c] - home[c]).powi(2)).sum::<f64>().sqrt() < leash;
            if dist < CHARGE_AT
                && to[2] - at[2] < STEP_UP
                && inside
                && facing_within(yaw, bearing, std::f64::consts::FRAC_PI_2)
                && solid.as_ref().is_none_or(|c| {
                    let eye = crate::game::body::EYE;
                    c.sees([at[0], at[1], at[2] + eye], [to[0], to[1], to[2] + eye])
                })
            {
                boot.gait.insert(who.clone(), 2);
                boot.heading.insert(who.clone(), bearing);
                boot.state.insert(who, 0x13);
                return Ok(0.0);
            }
            let wait = boot.random.next() + AGAIN;
            boot.cooldown.insert(who.clone(), wait);
            boot.gait.insert(who.clone(), 1);
            let want = if inside {
                bearing + (boot.random.next() - 0.5) * AWAY
            } else {
                let far = boot.random.next() * leash;
                let angle = boot.random.next() * std::f64::consts::TAU - std::f64::consts::PI;
                let spot = [home[0] + angle.sin() * far, home[1] + angle.cos() * far];
                crate::game::body::bearing(spot[0] - at[0], spot[1] - at[1])
            };
            boot.heading.insert(who, want);
            Ok(0.0)
        })?,
    )?;
    // **The conehead civilian**, which is the crowd rather than the fight.
    // `mdkConeheadCivUpdate(gob)` is 0x440440 into **0x4347e0**, and
    // `script.lua` hangs it on `OBJ_CONEHEADCIV1` in the default table, so
    // every one of the 23 the levels place gets it. Four states on the same
    // `walker + 0x7c` the fight uses:
    //
    // - **3, choose.** Gait 0, then one roll decides everything. It ignores
    //   you when there is no target, when the target is **`OBJ_DOC`**
    //   (0x4347fd compares the type with 0x66), when you are **20** units off
    //   (0x48f388) or when the roll comes up under **0.2** (0x48f5b0) -- so
    //   even a civilian looking straight at you shrugs one time in five. Then
    //   over 0.5 it stands for `chRand() * 5 + 1` seconds, and under it picks
    //   a point in its pen and walks there.
    // - **0x10, look.** It plays `ANIM_SCARED` or the one after it -- a coin
    //   toss stored in `walker + 0x9c` -- and holds for **2** seconds.
    // - **0x11, wander.** The goto core at walking pace, up to **20**
    //   seconds, and it goes back to choosing the moment it arrives.
    // - **0x14, stand.** Still, until the clock runs out or you come inside
    //   the twenty.
    // - **0x0b, run.** Only `OnHear` sets it, and it runs at the pen point it
    //   just picked rather than away from anything.
    //
    // The point in the pen is `home + (sin a, cos a) * chRand() * leash` with
    // `a = chRand() * 2pi - pi`, and 0x402a60 decides whether it is worth
    // walking to. There is no navigation mesh here, so that becomes
    // [`Collision::sees`] between the two -- the same segment test the
    // shooting uses, which is the nearest honest thing the engine has.
    for name in ["mdkConeheadCivUpdate", "mdkConeheadCivOnHear"] {
        let hearing = name == "mdkConeheadCivOnHear";
        globals.set(
            name,
            lua.create_function(move |lua, args: Variadic<Value>| {
                let Some(who) = args.first().and_then(gob_name) else { return Ok(0.0) };
                let Some((at, yaw)) = stance(lua, &who) else { return Ok(0.0) };
                /// How near you have to be before a civilian reacts at all --
                /// the 20 at 0x48f388.
                const NOTICE: f64 = 20.0;
                /// And how often it shrugs anyway, the 0.2 at 0x48f5b0.
                const SHRUG: f64 = 0.2;
                /// How long it looks at you, and how long it will walk.
                const LOOKING: f64 = 2.0;
                const WANDERING: f64 = 20.0;
                /// `ANIM_SCARED`, and the one after it.
                const SCARED: f64 = 0x12 as f64;
                /// `OBJ_DOC`, whom a civilian does not mind at all.
                const DOC: f64 = 102.0;
                let hero = lua
                    .named_registry_value::<mlua::Table>("player")
                    .ok()
                    .and_then(|p| p.get::<String>("name").ok());
                let seen = hero.as_deref().and_then(|n| {
                    let w = world::world(lua)?;
                    let g = w.get(w.find(n)?)?;
                    Some((g.position, g.kind))
                });
                let near = seen
                    .filter(|&(_, kind)| kind != DOC)
                    .map(|(p, _)| {
                        (0..3).map(|c| (p[c] - at[c]).powi(2)).sum::<f64>().sqrt()
                    })
                    .filter(|d| *d < NOTICE)
                    .is_some();
                // a point in the pen, and whether anything is in the way
                let spot = |boot: &mut Boot, at: [f64; 3]| -> Option<[f64; 3]> {
                    // **and it wanders whether or not it has been penned.**
                    // 0x434841 reads `walker + 0x6c` and `+0x78` without ever
                    // looking at the flag at `+0x68`, and the constructor
                    // fills both -- the home where the walker was placed
                    // (0x42f3e5) and the leash at **25** (0x42f400). Only the
                    // fight asks about the flag. So a civilian with no pen
                    // gets the constructor's, recorded the first time it is
                    // asked, which is the frame it starts on.
                    const LEASH: f64 = 25.0;
                    let (home, leash) = *boot.pen.entry(who.clone()).or_insert((at, LEASH));
                    let far = boot.random.next() * leash;
                    let angle = boot.random.next() * std::f64::consts::TAU
                        - std::f64::consts::PI;
                    Some([home[0] + angle.sin() * far, home[1] + angle.cos() * far, home[2]])
                };
                let reachable = |to: [f64; 3]| {
                    let eye = crate::game::body::EYE;
                    lua.app_data_ref::<std::rc::Rc<crate::game::body::Collision>>()
                        .is_none_or(|c| {
                            c.sees(
                                [at[0], at[1], at[2] + eye],
                                [to[0], to[1], to[2] + eye],
                            )
                        })
                };
                let mut boot = boot_mut(lua)?;
                let state = boot.state.get(&who).copied().unwrap_or(3);
                let cool = boot.cooldown.get(&who).copied().unwrap_or(0.0);
                // `OnHear`: 0x434b00 is two branches and neither of them
                // looks at the noise. Standing, it looks up; walking or
                // looking, it picks a new point and **runs** to it.
                if hearing {
                    match state {
                        0x14 => {
                            let coin = boot.random.next() <= 0.5;
                            boot.playing.insert(who.clone(), SCARED + coin as i64 as f64);
                            boot.since.insert(who.clone(), 0.0);
                            boot.state.insert(who.clone(), 0x10);
                            boot.cooldown.insert(who, LOOKING);
                        }
                        0x10 | 0x11 => {
                            if let Some(to) = spot(&mut boot, at) {
                                if reachable(to) {
                                    boot.homing.insert(who.clone(), to);
                                    boot.state.insert(who.clone(), 0x0b);
                                    boot.cooldown.insert(who, WANDERING);
                                }
                            }
                        }
                        _ => {}
                    }
                    return Ok(0.0);
                }
                let mut state = state;
                if state == 3 {
                    boot.gait.insert(who.clone(), 0);
                    let roll = boot.random.next();
                    if !near || roll <= SHRUG {
                        if roll >= 0.5 {
                            state = 0x14;
                            let wait = boot.random.next() * 5.0 + 1.0;
                            boot.cooldown.insert(who.clone(), wait);
                        } else {
                            let to = spot(&mut boot, at);
                            match to.filter(|&t| reachable(t)) {
                                Some(to) => {
                                    boot.homing.insert(who.clone(), to);
                                    state = 0x11;
                                    boot.cooldown.insert(who.clone(), WANDERING);
                                }
                                None => {
                                    state = 0x14;
                                    let wait = boot.random.next() * 5.0 + 1.0;
                                    boot.cooldown.insert(who.clone(), wait);
                                }
                            }
                        }
                    } else {
                        let coin = boot.random.next() <= 0.5;
                        boot.playing.insert(who.clone(), SCARED + coin as i64 as f64);
                        boot.since.insert(who.clone(), 0.0);
                        state = 0x10;
                        boot.cooldown.insert(who.clone(), LOOKING);
                    }
                }
                // and the switch reads the clock the chooser has just set,
                // not the one it walked in with: state 3 falls **into** the
                // switch in the original and every state below asks
                // `walker + 0x64` for itself
                let cool = boot.cooldown.get(&who).copied().unwrap_or(cool);
                match state {
                    // running and walking are the same call at two gaits
                    // and two wobbles: 0x434903 hands the goto core pi/4 for
                    // the wander and 0x4348c1 hands it pi/2 for the run
                    0x0b | 0x11 if cool > 0.0 => {
                        let to = boot.homing.get(&who).copied().unwrap_or(at);
                        let run = state == 0x0b;
                        let wobble = if run {
                            std::f64::consts::FRAC_PI_2
                        } else {
                            std::f64::consts::FRAC_PI_4
                        };
                        let kind = with_gob(lua, args.first(), |g| g.kind).unwrap_or(0.0);
                        if goto_core(&mut boot, &who, kind, at, yaw, to, run, false, wobble, false, 4.0)
                        {
                            boot.gait.insert(who.clone(), 0);
                            boot.homing.remove(&who);
                            boot.state.insert(who, 3);
                            return Ok(0.0);
                        }
                    }
                    0x10 if cool > 0.0 => {
                        boot.gait.insert(who.clone(), 0);
                    }
                    0x14 if cool > 0.0 && !near => {
                        boot.gait.insert(who.clone(), 0);
                    }
                    _ => {
                        boot.gait.insert(who.clone(), 0);
                        boot.homing.remove(&who);
                        state = 3;
                    }
                }
                boot.state.insert(who, state);
                Ok(0.0)
            })?,
        )?;
    }
    // `mdkDoganboyAttack(gob)` — 0x440380 into **0x4324f0**, class 4's slot
    // +0x2c and the enemy AI: a twelve-state machine on `walker + 0x7c` whose
    // jump table is at 0x433a38. Every one of the 41 script sites has it as
    // the **last** task in a list, and it returns 0 forever, which is how a
    // task list ends in something rather than finishing.
    //
    // All twelve are read. What this function collapses them into, in the
    // order it decides:
    //
    // - **no target at all** (0x42a850 answers nothing): gait 0, strafe 0,
    //   state 3, cooldown **0.3** (the float at 0x43254c), and return.
    // - the heading, with the lead of **state 4** (0x432d90) on it.
    // - **state 5**, advance: the chooser's two ways out, gait 2 for the
    //   3.0 seconds at 0x432cde, and it holds the gait while they last.
    // - **state 9**, play what the chooser picked — a taunt or `ANIM_SCARED`
    //   — and stand still for as long as the animation lasts.
    // - **states 4 and 0**, the burst: `record[+0x00]` rounds, one every
    //   `record[+0x04]` seconds, each playing `ANIM_SHOOT`, the last of a
    //   doganboy's throwing a grenade instead.
    // - **state 2**, back away: inside the record's near distance and with
    //   the cooldown spent, the gait goes to **3** — backwards, the negative
    //   speed in the enemy table — while the heading stays on the target.
    //
    // Not built, and each named where it would go: the leap (7), the charge
    // (11), the walk home past the leash (1) and getting up (8).
    globals.set(
        "mdkDoganboyAttack",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(who) = args.first().and_then(gob_name) else { return Ok(0.0) };
            let Some((at, yaw)) = stance(lua, &who) else { return Ok(0.0) };
            boot_mut(lua)?.fighting.insert(who.clone());
            let kind = {
                let Some(w) = world::world(lua) else { return Ok(0.0) };
                w.find(&who).and_then(|i| w.get(i)).map(|g| g.kind).unwrap_or(0.0)
            };
            let hero = lua
                .named_registry_value::<mlua::Table>("player")
                .ok()
                .and_then(|p| p.get::<String>("name").ok());
            let target = hero.as_deref().and_then(|n| stance(lua, n).map(|(p, _)| p));
            /// The cooldown a walker with nothing to fight falls back to,
            /// from the float stored at 0x43254c.
            const IDLE: f64 = 0.3;
            let Some(to) = target else {
                let mut boot = boot_mut(lua)?;
                boot.gait.insert(who.clone(), 0);
                boot.cooldown.insert(who, IDLE);
                return Ok(0.0);
            };
            let d = [to[0] - at[0], to[1] - at[1], to[2] - at[2]];
            let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            let near = crate::game::world::ai(kind).map(|r| r.near).unwrap_or(0.0);
            // **and it leads.** 0x432d90 aims at `target + velocity * (dist *
            // 0.025)`, which is the flight time at the shot's own speed, and
            // the AI record's last column says whether this type bothers.
            // Without it a walker aims where the player *was*: level 4 fired
            // 45 shots and the nearest passed 2.9 units away, which is four
            // units a second times the three quarters of a second the bullet
            // was in the air.
            const LEAD: f64 = 0.025; // 0x4901c4
            let lead = crate::game::world::ai(kind).is_some_and(|r| r.lead != 0.0);
            let mut boot = boot_mut(lua)?;
            // **a turret answers the first question in the tree.** 0x4327ff
            // asks `walker + 0x90` before anything else and, with it set,
            // skips the whole movement half -- the walk home, the leap, the
            // advance and both ways of giving ground -- leaving the taunt and
            // the burst. Every guard below is that one branch.
            let turret = boot.turret.contains(&who);
            let moving = hero.as_deref().and_then(|n| boot.velocity.get(n)).copied();
            let d = match moving {
                Some(v) if lead => [0, 1, 2].map(|c| d[c] + v[c] * dist * LEAD),
                _ => d,
            };
            let cool = boot.cooldown.get(&who).copied().unwrap_or(0.0);
            // **two states point a walker somewhere other than at you**, and
            // both have to survive the rewrite the next call would make:
            // state 1 walks it back to its pen and state 11 runs it away.
            // Each recomputes its bearing every call, because the walker is
            // moving and so, for the retreat, is what it is running from.
            let held = if cool <= 0.0 {
                None
            } else if let Some(home) = boot.homing.get(&who).copied() {
                Some(crate::game::body::bearing(home[0] - at[0], home[1] - at[1]))
            } else if boot.fleeing.contains(&who) {
                Some(crate::game::body::bearing(at[0] - to[0], at[1] - to[1]))
            } else {
                None
            };
            if cool <= 0.0 {
                boot.fleeing.remove(&who);
                boot.homing.remove(&who);
            }
            let look = held
                .unwrap_or_else(|| crate::game::body::bearing(d[0], d[1]));
            boot.heading.insert(who.clone(), look);
            // too close and off cooldown: give ground, still facing
            // **the burst**, states 4 and 0. State 4 loads `record[+0x00]`
            // into `walker + 0x9c` (0x4328e6) and state 0 counts it down one
            // a second (0x43345e, cooldown 0x3f800000), firing on each round.
            //
            // And on the **last** round a doganboy throws a grenade instead
            // (0x433094): type 0xcf, distance **between 25 and 45**
            // (0x48fb54 and 0x48f7e0), `chRand() < 0.7` (0x48f6bc), only when
            // it has none already in the air (`walker + 0x3c` is -1), and it
            // comes out of the slot the definition names at `def + 0x68` —
            // `DOGGNBOY_TARGET`, which is the engine's stand-in for the hand.
            let reach = crate::game::world::ai(kind).map(|r| r.reach).unwrap_or(0.0);
            let rounds = crate::game::world::ai(kind).map(|r| r.burst).unwrap_or(0.0);
            let left = boot.burst.get(&who).copied().unwrap_or(0.0);
            let interval = crate::game::world::ai(kind).map(|r| r.interval).unwrap_or(0.0);
            // **And this is what makes an enemy walk at you.** The chooser
            // at 0x432740..0x432ce0 is read now, and the branch that matters
            // hangs off two gates the first attempt at this missed:
            //
            //   dist >= record.reach   ->  roll `act`, and under it: state 5
            //   dist <  record.reach   ->  roll `act`; **over** it: state 4,
            //                              the fight. Under it, roll `taunt`;
            //                              over that, roll `close` -- over
            //                              `close` state 5, under it state 0,
            //                              two rounds.
            //
            // Reading the last leaf alone made a bif -- whose `close` is 0 --
            // advance for ever and never fire. With the gates it does the
            // opposite seven times in ten, which is what a bif is.
            //
            // ponytail: states 4 and 0 are collapsed here into the burst
            // below, so what this adds is the two ways out to state 5.
            const ADVANCING: f64 = 3.0; // 0x432cde, 0x432ce1
            // **and it stays in it.** The original enters state 5 and the
            // state runs until something changes it; this function re-decides
            // on every call, so without this the gait went back to 0 on the
            // very next one. At 30 frames a second that cost most of the walk
            // and in the window, which runs at over a thousand, it cost all of
            // it: the same level gave 177 units headless and 8 in a window.
            // and a walker on its way home **stops when it is back inside
            // half the leash** (0x4335da halves `walker + 0x78` with the 0.5
            // at 0x48f2fc), not when it reaches the point itself
            if let Some(home) = boot.homing.get(&who).copied() {
                let leash = boot.pen.get(&who).map(|p| p.1).unwrap_or(0.0);
                let back = (0..3).map(|c| (home[c] - at[c]).powi(2)).sum::<f64>().sqrt();
                if back < leash * 0.5 {
                    boot.homing.remove(&who);
                    boot.cooldown.insert(who.clone(), 0.0);
                }
            }
            if cool > 0.0 && boot.gait.get(&who) == Some(&2) {
                return Ok(0.0);
            }
            // **State 1, go home**, and it comes before every other choice:
            // 0x4328f0 checks the pen flag, measures the walker against
            // `walker + 0x78` and, outside it, sets state 1 and a **3 second**
            // cooldown before anything about the fight is looked at. The state
            // runs the goto core with `(run 1, mustFace 1, wobble 0, avoid 1,
            // radius 4)` and gives up on the cooldown or on half the leash.
            //
            // Without a pen a walker is never out of bounds, which is why the
            // flag exists: the constructor leaves it clear and sets the leash
            // to 25 (0x42f400) that nothing then reads.
            if let Some((home, leash)) = boot.pen.get(&who).copied().filter(|_| !turret) {
                let out = (0..3).map(|c| (home[c] - at[c]).powi(2)).sum::<f64>().sqrt();
                if out > leash && boot.homing.get(&who) != Some(&home) {
                    /// What being out of the pen costs before it may choose
                    /// again, from the float 0x43291e writes.
                    const WALK_BACK: f64 = 3.0;
                    boot.homing.insert(who.clone(), home);
                    boot.heading
                        .insert(who.clone(), crate::game::body::bearing(home[0] - at[0], home[1] - at[1]));
                    boot.avoiding.insert(who.clone());
                    boot.gait.insert(who.clone(), 2);
                    boot.cooldown.insert(who.clone(), WALK_BACK);
                    return Ok(0.0);
                }
            }

            // **State 7, the leap, and the health split above it.** 0x432940
            // divides the whole tree before anything else: over `def + 0x40`
            // hitpoints a walker may leap, at or below it it is limping and
            // rolls `act` for the retreat instead. Only the doganboy has that
            // threshold set, at 20 of its 100 — see [`world::LIMP_AT`].
            //
            // The leap has four gates and three of them were unread. The type
            // must carry `def + 0x14 & 2` ([`world::MAY_LEAP`]) — which is a
            // flags word and **not part of the inline name**, as the record's
            // first reading had it, so a hoser never leaps however its own
            // chance is set and a poopsy on the same behaviour record always
            // may. Then `chRand()` against **`payload[1]`**: 0x4329c1 rolls it
            // against `mdkGob + 0xa4`, and 0x42ad30 fills that from the second
            // number after the model name. The levels set it to 0.15 on most
            // and 0.6 on two doganboys. Then the distance, against the
            // record's own `leap`.
            //
            // Where it lands is `target + normalize(target - self) * h` with
            // `h = (chRand() + 1) * 0.5 * record.leap` — it goes **past** you,
            // by half to all of that distance. The apex is 0.75 of the way it
            // has to travel (0x48f5cc), **capped by `payload[2]`** when that
            // is set, which the levels set to 50. The launch is 0x4301f0, the
            // arc `mdkWalkerJumpToPoint` already flies.
            //
            // It turns first: 0x433624 leaves the whole state alone until the
            // gob is inside [`FACING`] of the bearing, and the heading written
            // above is what walks it there.
            //
            // And it will not leap out of its pen: 0x433720 measures the
            // landing against `walker + 0x6c` and gives the whole thing up if
            // it is past `+0x78`.
            let (hitpoints, payload) =
                with_gob(lua, args.first(), |g| (g.hitpoints, g.payload)).unwrap_or((0, [0.0; 4]));
            let choosing = cool <= 0.0 && left <= 0.0 && !turret;
            if let Some(rec) = crate::game::world::ai(kind) {
                if choosing && crate::game::world::limping(kind, hitpoints) {
                    if boot.random.next() <= rec.act {
                        retreat(&mut boot, &who, at, to);
                        return Ok(0.0);
                    }
                } else if choosing
                    && crate::game::world::may_leap(kind)
                    && boot.random.next() < payload[1]
                    && dist < rec.leap
                {
                    let to_target = crate::game::body::bearing(to[0] - at[0], to[1] - at[1]);
                    if !facing(yaw, to_target) {
                        // still turning; the heading above is doing that
                        return Ok(0.0);
                    }
                    /// How far past the target it lands, as a share of the
                    /// record's `leap`: `(chRand() + 1) * 0.5` at 0x433674.
                    const HALF: f64 = 0.5;
                    /// And how high, as a share of the way it has to go —
                    /// the float at 0x48f5cc.
                    const APEX: f64 = 0.75;
                    let h = (boot.random.next() + 1.0) * HALF * rec.leap;
                    let step = h / dist.max(1e-9);
                    let land = [0, 1, 2].map(|c| to[c] + (to[c] - at[c]) * step);
                    let far = (0..3).map(|c| (land[c] - at[c]).powi(2)).sum::<f64>().sqrt();
                    let apex = if payload[2] > 0.0 {
                        (far * APEX).min(payload[2])
                    } else {
                        far * APEX
                    };
                    let penned = boot.pen.get(&who).is_some_and(|&(home, leash)| {
                        (0..3).map(|c| (land[c] - home[c]).powi(2)).sum::<f64>().sqrt() > leash
                    });
                    let arc = launch(at, land, apex);
                    if !penned && arc.time.is_finite() && arc.time > 0.0 {
                        boot.heading.insert(who.clone(), arc.heading);
                        boot.gait.insert(who.clone(), 0);
                        boot.jumps.insert(who.clone(), Jump { from: at, arc, elapsed: 0.0 });
                        boot.jumped += 1;
                        return Ok(0.0);
                    }
                }
            }
            if let Some(rec) = crate::game::world::ai(kind) {
                let choosing = cool <= 0.0 && left <= 0.0 && dist >= near && !turret;
                let advance = if dist >= rec.reach {
                    // too far to shoot at all
                    choosing && boot.random.next() < rec.act
                } else {
                    choosing
                        && boot.random.next() <= rec.act
                        && kind != INVISOGRUNT
                        && boot.random.next() >= rec.taunt
                        && boot.random.next() >= rec.close
                };
                if advance {
                    boot.cooldown.insert(who.clone(), ADVANCING);
                    boot.avoiding.insert(who.clone());
                    boot.gait.insert(who, 2);
                    return Ok(0.0);
                }
            }

            // **State 9: turn, and play what the chooser picked.** The
            // engine can hold one now, because it knows how long an animation
            // lasts — see [`Boot::spans`]. Three branches of the tree end
            // here and all three are the same shape: pick an id, play it,
            // stand still until it is over.
            //
            // - **the taunts** (0x432ba3): under `taunt`, a grunt or an
            //   invisogrunt plays `ANIM_TAUNT0 - floor(rand * 3)` — the three
            //   taunts at 0x70..0x72 — and anything else tosses a coin
            //   between `ANIM_TAUNT1` and `ANIM_TAUNT0`
            // - **the scared one** (0x432a36): below `hurt` of its health and
            //   under `scared`, `ANIM_SCARED`
            //
            // ponytail: the original checks the model actually carries the
            // animation (0x461650) before choosing it; here an id it does not
            // have simply plays nothing.
            const ANIM_TAUNT0: f64 = 0x70 as f64;
            const ANIM_TAUNT1: f64 = 0x71 as f64;
            const ANIM_SCARED: f64 = 0x12 as f64;
            if let Some(rec) = crate::game::world::ai(kind) {
                if cool <= 0.0 && left <= 0.0 {
                    let hurt = with_gob(lua, args.first(), |g| {
                        g.max_hitpoints > 0
                            && (g.hitpoints as f64) < g.max_hitpoints as f64 * rec.hurt
                    })
                    .unwrap_or(false);
                    let scared = hurt && boot.random.next() < rec.scared;
                    // and a **conehead** that is scared runs instead, half the
                    // time: 0x432a4f tests the def's type for 0xcb and only
                    // then chooses between state 11 and the animation
                    if scared && !turret && kind == CONEHEAD && boot.random.next() < 0.5 {
                        retreat(&mut boot, &who, at, to);
                        return Ok(0.0);
                    }
                    let show = if scared {
                        Some(ANIM_SCARED)
                    } else if boot.random.next() < rec.taunt {
                        Some(if kind == GRUNT || kind == INVISOGRUNT {
                            ANIM_TAUNT0 - (boot.random.next() * 3.0).floor()
                        } else if boot.random.next() < 0.5 {
                            ANIM_TAUNT1
                        } else {
                            ANIM_TAUNT0
                        })
                    } else {
                        None
                    };
                    if let Some(id) = show {
                        let span = crate::game::api::model_for_type(kind)
                            .and_then(|m| boot.spans.get(&(m, id as i64)).copied())
                            .unwrap_or(0.0);
                        if span > 0.0 {
                            boot.playing.insert(who.clone(), id);
                            boot.since.insert(who.clone(), 0.0);
                            boot.cooldown.insert(who.clone(), span);
                            boot.gait.insert(who, 0);
                            return Ok(0.0);
                        }
                    }
                }
            }

            // What is **not** built, written down
            // from the same tree: the leap (state 7, inside `record.leap`),
            // the charge (state 11, which a limping doganboy and a scared
            // conehead take), `ANIM_SCARED` and the three taunts, and the
            // walk home when it is past its leash (state 1).
            if cool <= 0.0 {
                if left > 0.0 {
                    boot.burst.insert(who.clone(), left - 1.0);
                    // **the record's own second column** is the wait between
                    // rounds — 0x433219 writes `record[+0x04]` straight into
                    // `walker + 0x64` after every shot. Half a second for a
                    // doganboy, two for a hans.
                    boot.cooldown.insert(who.clone(), interval);
                    /// The band a doganboy throws a grenade in, and how often.
                    const THROW: std::ops::Range<f64> = 25.0..45.0;
                    const OFTEN: f64 = 0.7;
                    let roll = boot.random.next();
                    if kind == DOGANBOY && left == 1.0 && THROW.contains(&dist) && roll < OFTEN {
                        // ANIM_THROW, and the grenade comes straight out
                        boot.playing.insert(who.clone(), ANIM_THROW);
                        boot.since.insert(who.clone(), 0.0);
                        drop(boot);
                        fire_key_object(lua, &who, DBGRENADE).ok();
                        return Ok(0.0);
                    }
                    // **and this is how an enemy shoots.** 0x4331f8 plays
                    // `ANIM_SHOOT` and nothing else: the projectile comes off
                    // the animation's own key channel, which is why no column
                    // of the enemy table names a shot. See [`Boot::keys`].
                    //
                    // The hurt variant (animation 0x4f when the hitpoints are
                    // under `def[0x40]`) is left out — that threshold is not
                    // one of the columns the engine keeps.
                    boot.playing.insert(who.clone(), ANIM_SHOOT);
                    boot.since.insert(who.clone(), 0.0);
                } else if dist < reach && rounds > 0.0 {
                    boot.burst.insert(who.clone(), rounds);
                    boot.cooldown.insert(who.clone(), interval);
                }
            }
            // **And inside `near` there are two ways to give ground.**
            // 0x432aa9 rolls `act`; over it the walker just fights. Under it,
            // an invisogrunt fights anyway (0x432ac9), and everything else
            // rolls `scared`: under that it **runs** — state 11 — and over it
            // it backs away on its feet, state 2.
            if dist < near && cool <= 0.0 && !turret {
                if let Some(rec) = crate::game::world::ai(kind) {
                    if boot.random.next() <= rec.act
                        && kind != INVISOGRUNT
                        && boot.random.next() <= rec.scared
                    {
                        retreat(&mut boot, &who, at, to);
                        return Ok(0.0);
                    }
                }
            }
            let gait = if dist < near && cool <= 0.0 && !turret { 3 } else { 0 };
            if gait == 3 {
                /// What giving ground costs, from the float 0x432af4 writes
                /// into `walker + 0x64` on the way into state 2. Without it a
                /// crowded walker backs away every single frame.
                const BACKED_OFF: f64 = 3.0;
                boot.cooldown.insert(who.clone(), BACKED_OFF);
            }
            boot.gait.insert(who, gait);
            Ok(0.0)
        })?,
    )?;

    // `mdkShootBulletLua(bullet, shooter, target, x, y, z)` — 0x4414d0 into
    // 0x403860, which turns `(x, y, z)` into the bullet's orientation and
    // hands it to **0x4038b0**, the launch. `mdkShootBullet(bullet, shooter,
    // target, aim)` is 0x441410 into the same place with a gob's orientation
    // instead of a vector; the engine aims it at that gob.
    //
    // What the bullet is worth comes from **the shot table at 0x497388** —
    // 69 records, damage, damage type, lifetime and speed — and not from the
    // call, which carries only a direction. See
    // [`crate::game::world::BULLET`].
    //
    // The two events are read rather than guessed, out of 0x404280:
    // **`OnShotLanded` (12) and `OnShotExploded` (13) both fire on the
    // shooter**, not on the bullet, and the first only when what was hit is
    // the gob the shot was aimed at (0x4042e1 compares the ids). `OnShotLanded`
    // takes the bullet's *type* as a number; `OnShotExploded` takes the type
    // and then the bullet itself.
    for name in ["mdkShootBulletLua", "mdkShootBullet"] {
        let by_vector = name == "mdkShootBulletLua";
        globals.set(
            name,
            lua.create_function(move |lua, args: Variadic<Value>| {
                let Some(Value::Table(bullet)) = args.first() else { return Ok(0.0) };
                let Some(id) = world::id_of(bullet) else { return Ok(0.0) };
                let (kind, at) = {
                    let Some(w) = world::world(lua) else { return Ok(0.0) };
                    match w.get(id) {
                        Some(g) => (g.kind, g.position),
                        None => return Ok(0.0),
                    }
                };
                let Some((_, filter, damage, life, speed, _, _)) = crate::game::world::bullet(kind)
                else {
                    return Ok(0.0); // not a shot type, so there is nothing to fly
                };
                let target = args.get(2).and_then(gob_name).filter(|n| !n.is_empty());
                let mut d = if by_vector {
                    [3, 4, 5].map(|i| args.get(i).map(number).unwrap_or(0.0))
                } else {
                    // aimed at a gob: the direction is the way to it
                    let to = args.get(3).and_then(gob_name).and_then(|n| {
                        let w = world::world(lua)?;
                        w.find(&n).and_then(|i| w.get(i)).map(|g| g.position)
                    });
                    match to {
                        Some(p) => [0, 1, 2].map(|c| p[c] - at[c]),
                        None => [1.0, 0.0, 0.0],
                    }
                };
                let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                if length <= 0.0 {
                    return Ok(0.0);
                }
                d = d.map(|c| c / length);
                let mut boot = boot_mut(lua)?;
                boot.shots.insert(
                    id,
                    Shot {
                        kind,
                        direction: d,
                        speed,
                        // -1 in the table means it never times out; the
                        // countdown at 0x403d94 is gated on the field being
                        // positive at all
                        life: if life < 0.0 { f64::INFINITY } else { life },
                        damage,
                        filter,
                        shooter: args.get(1).and_then(gob_name),
                        target,
                    },
                );
                boot.fired += 1;
                Ok(1.0)
            })?,
        )?;
    }

    // `mdkWalkerJumpToPoint(gob, point, apex)` — 0x43f980 into **0x430360**,
    // which is four lines: copy the waypoint into `walker + 0x80`, then call
    // **0x4301f0** to solve the arc. The binding takes the three arguments in
    // that order and the third really is a height — see [`launch`], where the
    // arithmetic is.
    //
    // Two effects beyond the arc, and both are in the launch rather than
    // added here. It **writes the heading** to `walker + 0x14`, the same
    // field `mdkWalkerHeadToPoint` writes, from `0x470930`'s bearing. And it
    // **turns the gob outright** — 0x4302e5 calls 0x46fd20 on `gob + 0x24`
    // with that bearing, so unlike the heading this one is not a want. A
    // walker faces its jump the instant it is told to make it.
    globals.set(
        "mdkWalkerJumpToPoint",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(who) = args.first().and_then(gob_name) else { return Ok(0.0) };
            let Some(to) = point_at(lua, args.get(1)) else { return Ok(0.0) };
            let apex = args.get(2).map(number).unwrap_or(0.0);
            let Some((id, from)) = ({
                let w = world::world(lua);
                w.and_then(|w| {
                    let id = w.find(&who)?;
                    Some((id, w.get(id)?.position))
                })
            }) else {
                return Ok(0.0);
            };
            let arc = launch(from, to, apex);
            // a negative apex would take the square root of a negative and
            // poison the position with a NaN; the original would too, and no
            // script asks for one, so refuse it rather than fly it
            if !arc.time.is_finite() || arc.time <= 0.0 {
                return Ok(0.0);
            }
            {
                let mut boot = boot_mut(lua)?;
                boot.heading.insert(who.clone(), arc.heading);
                boot.jumps.insert(who.clone(), Jump { from, arc, elapsed: 0.0 });
                boot.jumped += 1;
            }
            if let Some(mut w) = lua.app_data_mut::<world::World>() {
                let half = arc.heading / 2.0;
                w.set_rotation(id, [half.cos(), 0.0, 0.0, half.sin()]);
            }
            Ok(1.0)
        })?,
    )?;

    // `mdkAILineOfSight(watcher, target, fov, range)` — 0x43c840 into
    // **0x402950**, and three things in it are read rather than guessed.
    // Both ends are lifted by an **eye height taken from `omgob + 8`**, which
    // is per type and which the engine does not hold: [`EYE`] stands in for
    // it, and that is ours. The cone is **`cos(fov * 0.5)`** — the 0.5 is the
    // constant at 0x48f2fc — so the angle a script passes is the *full*
    // width, and `2*PI` really does mean all round. And the occlusion test
    // comes last, after the range and the cone, because it is the expensive
    // one.
    //
    // `mdkWalkerCanSeeGob(watcher, target)` is the same question with the
    // walker's own cone and reach, which the engine has no walker to ask —
    // so it is all round and unlimited, and only the geometry answers.
    for name in ["mdkAILineOfSight", "mdkWalkerCanSeeGob"] {
        let bounded = name == "mdkAILineOfSight";
        globals.set(
            name,
            lua.create_function(move |lua, args: Variadic<Value>| {
                let (Some(a), Some(b)) =
                    (args.first().and_then(gob_name), args.get(1).and_then(gob_name))
                else {
                    return Ok(0.0);
                };
                let fov = if bounded {
                    args.get(2).map(number).unwrap_or(std::f64::consts::TAU)
                } else {
                    std::f64::consts::TAU
                };
                let range =
                    if bounded { args.get(3).map(number).unwrap_or(f64::MAX) } else { f64::MAX };
                let (from, to, facing) = {
                    let Some(w) = world::world(lua) else { return Ok(0.0) };
                    let Some(watcher) = w.find(&a).and_then(|id| w.get(id)) else {
                        return Ok(0.0);
                    };
                    let Some(target) = w.find(&b).and_then(|id| w.get(id)) else {
                        return Ok(0.0);
                    };
                    let q = watcher.rotation;
                    let yaw = (2.0 * (q[0] * q[3] + q[1] * q[2]))
                        .atan2(1.0 - 2.0 * (q[2] * q[2] + q[3] * q[3]));
                    (watcher.position, target.position, yaw)
                };
                let to_target = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
                let far = to_target.iter().map(|c| c * c).sum::<f64>().sqrt();
                if far > range || far <= 0.0 {
                    return Ok(0.0);
                }
                // the cone, against the watcher's own facing
                let mut off =
                    crate::game::body::bearing(to_target[0], to_target[1]) - facing;
                while off > std::f64::consts::PI {
                    off -= std::f64::consts::TAU;
                }
                while off < -std::f64::consts::PI {
                    off += std::f64::consts::TAU;
                }
                // `cos` is only monotonic over a half-angle of 0..PI, so an
                // `fov` past 2*PI *narrows* this cone rather than widening
                // it. That is the original's arithmetic and not a slip here;
                // the widest the scripts ever ask for is exactly `2*PI`.
                if off.cos() < (fov * 0.5).cos() {
                    return Ok(0.0);
                }
                // and only then the geometry. With no collision world loaded
                // -- a boot never builds one -- nothing can block the view,
                // which is the honest answer for a world that has no walls.
                let clear = match lua.app_data_ref::<std::rc::Rc<crate::game::body::Collision>>() {
                    Some(c) => {
                        let eye = crate::game::body::EYE;
                        c.sees(
                            [from[0], from[1], from[2] + eye],
                            [to[0], to[1], to[2] + eye],
                        )
                    }
                    None => true,
                };
                Ok(if clear { 1.0 } else { 0.0 })
            })?,
        )?;
    }

    // Four more getters the scripts compare against a *number*, which is the
    // shape that makes a recorder's `nil` actively wrong rather than merely
    // absent — see `omGobIsStasis`, which cost every cutscene in the game.
    //
    // `mdkIsCutSceneAllowed` (0x43dd50 into 0x42a9a0) switches on the play
    // mode and **its default arm returns 1** (0x42a9f6); the four other arms
    // ask the character's own routine, which is AI the engine does not have.
    // 1 is therefore both the default and the truthful answer here: nothing
    // is stopping a cutscene. 54 call sites, the most of any of them.
    //
    // `chIsLoadingResources` (0x450a50) counts a queue. Loading here is
    // synchronous, so the queue is always empty.
    //
    // `mdkDialogIsDone` — nothing is speaking, so it is done.
    //
    // `mdkDocHasItem` was here, answering a constant 0 because the engine
    // held no inventory. It holds one now and the real binding is above; a
    // constant left in this list would have quietly won, since this loop runs
    // later, and it did for exactly one test run.
    for (name, answer) in [
        ("mdkIsCutSceneAllowed", 1.0),
        ("chIsLoadingResources", 0.0),
        ("mdkDialogIsDone", 1.0),
    ] {
        globals.set(name, lua.create_function(move |_, _: Variadic<Value>| Ok(answer))?)?;
    }

    // The three scalar getters. The recorder's rule -- a name with Get in
    // it hands back a handle -- is wrong for these, and the handlers do
    // arithmetic on what they return, which is the shape of nearly every
    // failure `--events` reports. They are engine state, not stubs: the
    // frame time, and what the input is doing this instant.
    globals.set(
        "chGetDeltaT",
        lua.create_function(|lua, ()| Ok(boot_ref(lua)?.delta))?,
    )?;
    globals.set("omGetAxisValue", lua.create_function(|_, _: Variadic<Value>| Ok(0.0))?)?;
    globals.set("omGetCommandValue", lua.create_function(|_, _: Variadic<Value>| Ok(0.0))?)?;

    globals.set(
        "chZeroGlobalTime",
        lua.create_function(|lua, ()| {
            boot_mut(lua)?.clock = 0.0;
            Ok(())
        })?,
    )?;
    globals.set(
        "chSeedRand",
        lua.create_function(|lua, seed: Option<f64>| {
            let seed = seed.unwrap_or(0.0) as u32;
            let mut boot = boot_mut(lua)?;
            boot.seed = Some(seed);
            boot.random.seed(seed);
            Ok(())
        })?,
    )?;

    // --- the player -----------------------------------------------------
    // `CreateKurt()` makes the player with `mdkCreateObjectLua("bob",
    // OBJ_KURT, ...)` and then tells the engine which gob it is:
    // `mdkSetPlayModeGobs(PLAYMODE_KURT, bob, kurtinventory)`. So the engine
    // learns the player from the script rather than guessing at a name, and
    // `mdkGetPlayerGob` can answer with the object itself -- which is what
    // the handlers reach through for `.position`.
    globals.set(
        "mdkSetPlayModeGobs",
        lua.create_function(|lua, args: Variadic<Value>| {
            if let Some(Value::Table(gob)) = args.get(1) {
                // the registry, not a second global: an object reachable
                // under two names would have its handlers fired twice by
                // anything that walks `_G`
                lua.set_named_registry_value("player", gob.clone())?;
                if let Some(name) = gob.get::<Option<String>>("name")? {
                    let mode = args.first().map(number).unwrap_or(0.0) as i64;
                    let mut boot = boot_mut(lua)?;
                    boot.play_modes.insert(mode, name.clone());
                    boot.player = Some(name);
                }
            }
            Ok(())
        })?,
    )?;
    // `mdkGetPlayMode()` — 0x439f90 into 0x42b9c0, an int the engine keeps;
    // `mdkGetPlayModePlayerGob(mode)` — 0x439e90 into 0x42bbc0, the gob that
    // mode is played with. The engine has no second player to switch to, so
    // the mode it answers with is **the mode whose gob is the current
    // player**, which is the same number for every level that has one.
    //
    // Answering these two at all matters more than it looks: they are called
    // 3600 times each in a two-minute run, and `PLAYMODE_NONE` is 0, so every
    // `mdkGetPlayMode() == PLAYMODE_DOC` in the scripts was false.
    globals.set(
        "mdkGetPlayMode",
        lua.create_function(|lua, ()| Ok(boot_ref(lua)?.mode as f64))?,
    )?;
    globals.set(
        "mdkGetPlayModePlayerGob",
        lua.create_function(|lua, args: Variadic<Value>| {
            let mode = args.first().map(number).unwrap_or(0.0) as i64;
            let name = boot_ref(lua)?.play_modes.get(&mode).cloned();
            let Some(name) = name else { return Ok(Value::Nil) };
            Ok(lua.globals().get::<Value>(name).unwrap_or(Value::Nil))
        })?,
    )?;
    // `mdkGetAITargetGob()` — 0x439d20 into **0x42a850**, the same lookup the
    // enemy AI makes for what to fight. It takes no argument: there is one
    // target and it is the player.
    globals.set(
        "mdkGetAITargetGob",
        lua.create_function(|lua, ()| {
            Ok(lua.named_registry_value::<Value>("player").unwrap_or(Value::Nil))
        })?,
    )?;
    // `mdkSwitchPlayMode(mode)` — 0x439e40 into **0x42b940**, which takes the
    // gob and the inventory for that mode out of the table at 0x4bb6b0
    // (stride three dwords, filled by `mdkSetPlayModeGobs`) and makes them
    // the current pair. **This is what names the player**, and the engine had
    // been guessing at the last `mdkSetPlayModeGobs` instead: on levels 3 and
    // 9 that landed on a gob with no hitpoints at all, so a played session
    // reported "0 of 0" and nothing could hurt it.
    //
    // `mdk2.lua` sets the gobs for a mode and switches to it in the same
    // breath — 0x815 then 0x816 for Max, 0x840 then 0x842 for Kurt — so the
    // switch is the level's own statement of who is being played.
    globals.set(
        "mdkSwitchPlayMode",
        lua.create_function(|lua, args: Variadic<Value>| {
            switch_play_mode(lua, args.first().map(number).unwrap_or(0.0) as i64)
        })?,
    )?;
    // one flag each, and the driver reads it -- see [`Boot::controlled`]
    for name in ["mdkEnablePlayerControl", "mdkDisablePlayerControl"] {
        let on = name == "mdkEnablePlayerControl";
        globals.set(
            name,
            lua.create_function(move |lua, ()| {
                boot_mut(lua)?.no_control = !on;
                Ok(())
            })?,
        )?;
    }
    // `mdkGetCameraGob()` -- 0x439fd0, which hands back whatever 0x42a980
    // answers with. `mdk2.lua` makes exactly one `OBJ_DEFAULTCAMERA` and
    // calls it `camera`, so that is the object.
    globals.set(
        "mdkGetCameraGob",
        lua.create_function(|lua, ()| {
            Ok(lua.globals().get::<Value>("camera").unwrap_or(Value::Nil))
        })?,
    )?;
    globals.set(
        "mdkGetPlayerGob",
        lua.create_function(|lua, ()| {
            Ok(lua.named_registry_value::<Value>("player").unwrap_or(Value::Nil))
        })?,
    )?;
    // and the engine then warps it to the checkpoint, which is the other
    // half of starting a level at one
    globals.set(
        "mdkWarpToCheckpoint",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(Value::Table(gob)) = args.first() else { return Ok(()) };
            let n = args.get(1).map(number).unwrap_or(0.0);
            let at = boot_ref(lua)?
                .checkpoints
                .iter()
                .find(|c| c.index == n)
                .map(|c| c.position);
            if let Some(p) = at {
                let position = lua.create_table()?;
                position.set("x", p[0])?;
                position.set("y", p[1])?;
                position.set("z", p[2])?;
                gob.set("position", position)?;
            }
            Ok(())
        })?,
    )?;

    // --- moving things --------------------------------------------------
    // `mdkGobSetPosition(gob, "l10r2_mbad1")` puts an object on a **named
    // waypoint**; `mdkGobSetPositionXYZ(gob, x, y, z)` puts it at a point.
    // Both write the arena and the Lua-side table, because the scripts read
    // `gob.position.x` straight back -- `boss.lua` does
    // `mdkGobSetPositionXYZ(v, v.position.x, v.position.y, v.position.z + 0.1)`.
    globals.set(
        "mdkGobSetPosition",
        lua.create_function(|lua, args: Variadic<Value>| {
            let at = match args.get(1) {
                // **by name, and the fields are named too.** The scene graphs
                // write `points.l1_bwp01 = {x=78.4479, y=147.954, z=-27.5037,
                // f=0}` — 5633 of them — so reading `[1]`, `[2]`, `[3]` gets
                // three nils and puts the object at the origin. It did, for
                // every scripted placement in the game, until a walker asked
                // the collision world whether it could step forward and was
                // told no because it was standing inside the level.
                Some(Value::String(_)) => point_at(lua, args.get(1)),
                other => position(other),
            };
            if let (Some(Value::Table(gob)), Some(at)) = (args.first(), at) {
                place(lua, gob, at)?;
            }
            Ok(())
        })?,
    )?;
    globals.set(
        "mdkGobSetPositionXYZ",
        lua.create_function(|lua, args: Variadic<Value>| {
            if let Some(Value::Table(gob)) = args.first() {
                let at = [
                    args.get(1).map(number).unwrap_or(0.0),
                    args.get(2).map(number).unwrap_or(0.0),
                    args.get(3).map(number).unwrap_or(0.0),
                ];
                place(lua, gob, at)?;
            }
            Ok(())
        })?,
    )?;

    // `omAnimPlay(door, ANIM_OPEN, ANIMFLAG_NOREWIND + ANIMFLAG_NOTRANS)`
    // chooses which of a model's animations runs. Until this, everything
    // played animation 0 -- which is an animation, not a bind pose, so a
    // door was always mid-swing.
    globals.set(
        "omAnimPlay",
        lua.create_function(|lua, args: Variadic<Value>| {
            if let Some(name) = args.first().and_then(gob_name) {
                let id = args.get(1).map(number).unwrap_or(0.0);
                let mut boot = boot_mut(lua)?;
                boot.playing.insert(name.clone(), id);
                boot.since.insert(name.clone(), 0.0);
                boot.done.remove(&name);
            }
            Ok(())
        })?,
    )?;
    // `omAnimJustLooped(gob, slot)` — 0x420250 into 0x461850, which finds the
    // animation instance and returns one field of it. The engine's tick sets
    // it on the frame the clock wraps; see [`Boot::looped`].
    globals.set(
        "omAnimJustLooped",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(name) = args.first().and_then(gob_name) else { return Ok(0.0) };
            // **and the animation matters.** 0x461520 looks the instance
            // up *by id* and 0x461850 returns 0 when there is none, so an
            // object looping animation A does not answer a script waiting on
            // B. Called with no id -- which no shipped site does -- this
            // answers for whatever is up.
            let want = args.get(1).map(number);
            let boot = boot_ref(lua)?;
            let looped = boot.looped.get(&name).copied();
            Ok(match (looped, want) {
                (Some(a), Some(b)) => (a as i64 == b as i64) as i64 as f64,
                (Some(_), None) => 1.0,
                (None, _) => 0.0,
            })
        })?,
    )?;
    // `omAnimSetSpeed(gob, animation, speed)` — a multiplier on the record's
    // own rate, and the sign is the point: `elevators.lua` opens a door with
    // `omAnimPlay(door, ANIM_OPEN, ...)` and **shuts it with a speed of -1**
    // on the same animation rather than a second one.
    globals.set(
        "omAnimSetSpeed",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(name) = args.first().and_then(gob_name) else { return Ok(()) };
            let speed = args.get(2).map(number).unwrap_or(1.0);
            boot_mut(lua)?.speed.insert(name, speed);
            Ok(())
        })?,
    )?;
    globals.set(
        "omAnimStop",
        lua.create_function(|lua, args: Variadic<Value>| {
            if let Some(name) = args.first().and_then(gob_name) {
                boot_mut(lua)?.playing.remove(&name);
            }
            Ok(())
        })?,
    )?;
    globals.set(
        "omAnimIsPlaying",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(name) = args.first().and_then(gob_name) else { return Ok(0) };
            // **and one that has ended is not playing.** `WaitForAnim` in
            // `script.lua` is `omAnimIsPlaying(self, anim) == 0`, so an
            // engine whose animations never end waits for ever -- which is
            // what `l3_bathroom03` and `l5_r11` were doing.
            let boot = boot_ref(lua)?;
            let up = boot.playing.get(&name);
            let want = args.get(1).map(number);
            Ok(match (up, want) {
                (Some(_), _) if boot.done.contains(&name) => 0,
                (Some(a), Some(b)) => (*a as i64 == b as i64) as i32,
                (Some(_), None) => 1,
                (None, _) => 0,
            })
        })?,
    )?;

    // --- parts of a model -----------------------------------------------
    // `omGobGMGetSltIndexByName(animgob, "EL_CENTER")` asks for a **named
    // node** of the object's model -- `EL_CENTER`, `ZIZZY_BEAM`,
    // `ZIZS2_HIT` are all node names out of the `.mod` node table -- and
    // `omGobGMSetSltVisible(gob, slot, 0)` then hides it.
    //
    // The index handed back is a **handle**, not the model's own node index:
    // resolving the real one needs the model loaded, and the boot does not
    // load models. The name is interned per object and the renderer resolves
    // it when it has the model in front of it.
    globals.set(
        "omGobGMGetSltIndexByName",
        lua.create_function(|lua, args: Variadic<Value>| {
            let (Some(gob), Some(slot)) = (args.first().and_then(gob_name), args.get(1).and_then(text))
            else {
                return Ok(-1i64);
            };
            let mut boot = boot_mut(lua)?;
            let slots = boot.slots.entry(gob).or_default();
            Ok(match slots.iter().position(|s| *s == slot) {
                Some(i) => i as i64,
                None => {
                    slots.push(slot);
                    slots.len() as i64 - 1
                }
            })
        })?,
    )?;
    // `omGobAddSound(gob, "glass_break", 0)` attaches a sound to an object and
    // hands back the handle the script keeps -- `l1_r2.shattersound` -- and
    // `omGobGSPlay(handle, 0,0,0,0)` fires it. 62 attachments and 100 plays
    // over the shipped scripts, which is most of what a level's noise is.
    globals.set(
        "omGobAddSound",
        lua.create_function(|lua, args: Variadic<Value>| {
            let (Some(gob), Some(sound)) =
                (args.first().and_then(gob_name), args.get(1).and_then(text))
            else {
                return Ok(-1i64);
            };
            let flag = args.get(2).map(number).unwrap_or(0.0);
            let mut boot = boot_mut(lua)?;
            boot.gob_sounds.push(GobSound { gob, sound, flag, played: 0 });
            Ok(boot.gob_sounds.len() as i64 - 1)
        })?,
    )?;
    globals.set(
        "omGobGSPlay",
        lua.create_function(|lua, args: Variadic<Value>| {
            let handle = args.first().map(number).unwrap_or(-1.0);
            if handle < 0.0 {
                return Ok(());
            }
            let mut boot = boot_mut(lua)?;
            let i = handle as usize;
            if let Some(s) = boot.gob_sounds.get_mut(i) {
                s.played += 1;
                boot.to_play.push(i);
            }
            Ok(())
        })?,
    )?;
    // `chRand()` is the original's MT19937, seeded by `chSeedRand` — see
    // `crate::game::rand`, which is held to the original's own generator run
    // under emulation. It is answered for real rather than recorded because
    // **a getter that returns nil kills the handler that does arithmetic on
    // it**: "attempt to perform arithmetic on a nil value" is the commonest
    // reason a handler stops, and this is called 238 times.
    globals.set(
        "chRand",
        lua.create_function(|lua, _: Variadic<Value>| {
            Ok(boot_mut(lua)?.random.next())
        })?,
    )?;
    // the type an object was registered with, out of the arena. Answering a
    // table here — which the recorder's "a name with Get in it hands back a
    // handle" rule would — is what "attempt to compare table with number"
    // was, 51 times over.
    globals.set(
        "mdkGetGobType",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(name) = args.first().and_then(gob_name) else { return Ok(-1.0) };
            let Some(w) = world::world(lua) else { return Ok(-1.0) };
            Ok(w.find(&name).and_then(|id| w.get(id)).map(|g| g.kind).unwrap_or(-1.0))
        })?,
    )?;
    // --- damage, which is in the binary and did not have to be invented ---
    //
    // A gob's `omgob` is at `gob + 0x84`, and three `i16` in it are the whole
    // model: **0x10 the damage filter, 0x12 the hitpoints, 0x14 the most it
    // can have**. The getters name themselves — `mdkGobGetDamageFilter`
    // reads 0x10 (0x4108e0), `mdkGetHitpoints` reads 0x12 (0x40e920) — and
    // `mdkGobGetHealth` (0x40f340) is **the quotient of the two**, so health
    // is a fraction and not a count.
    //
    // The 13 `DAMAGE_*` constants are every one a power of two, from
    // `DAMAGE_GOODGUY` 1 to `DAMAGE_PUNCH` 4096, so all of them together are
    // 8191 and the `i16` is exactly wide enough.
    globals.set(
        "mdkIsDamageType",
        // 0x4108b0, and it is that short: `(a & b) != 0`. Neither argument is
        // a gob -- it tests two masks against each other.
        lua.create_function(|_, args: Variadic<Value>| {
            let a = args.first().map(number).unwrap_or(0.0) as i64;
            let b = args.get(1).map(number).unwrap_or(0.0) as i64;
            Ok(if a & b != 0 { 1.0 } else { 0.0 })
        })?,
    )?;
    globals.set(
        "mdkGetHitpoints",
        lua.create_function(|lua, args: Variadic<Value>| {
            Ok(with_gob(lua, args.first(), |g| g.hitpoints as f64).unwrap_or(0.0))
        })?,
    )?;
    globals.set(
        "mdkGobGetHealth",
        // a fraction of the maximum, not a count -- and a maximum of zero is
        // not a division, it is an object that has no health to speak of
        lua.create_function(|lua, args: Variadic<Value>| {
            Ok(with_gob(lua, args.first(), |g| {
                if g.max_hitpoints > 0 {
                    g.hitpoints as f64 / g.max_hitpoints as f64
                } else {
                    0.0
                }
            })
            .unwrap_or(0.0))
        })?,
    )?;
    globals.set(
        "mdkGobGetDamageFilter",
        lua.create_function(|lua, args: Variadic<Value>| {
            Ok(with_gob(lua, args.first(), |g| g.damage_filter as f64).unwrap_or(0.0))
        })?,
    )?;
    globals.set(
        "mdkGobSetDamageFilter",
        // 0x4108c0, and it truncates to `i16` on the way in
        lua.create_function(|lua, args: Variadic<Value>| {
            let mask = args.get(1).map(number).unwrap_or(0.0) as i64 as i16;
            edit_gob(lua, args.first(), |g| g.damage_filter = mask);
            Ok(())
        })?,
    )?;
    globals.set(
        "mdkBlowerEnable",
        // 0x402cc0: `block[0] = 1`, and the gob plays `ANIM_ENABLED`
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(name) = args.first().and_then(gob_name) else { return Ok(()) };
            let mut boot = boot_mut(lua)?;
            boot.blower_off.remove(&name);
            boot.playing.insert(name, 61.0);
            Ok(())
        })?,
    )?;
    globals.set(
        "mdkBlowerDisable",
        // 0x402d20: `block[0] = 0`, and the gob plays `ANIM_DISABLED`
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(name) = args.first().and_then(gob_name) else { return Ok(()) };
            let mut boot = boot_mut(lua)?;
            boot.blower_off.insert(name.clone());
            boot.playing.insert(name, 62.0);
            Ok(())
        })?,
    )?;
    globals.set(
        "mdkBlowerSetLength",
        // 0x440d80 into the block's `[0xc]`, which is the length the scene
        // graph put there
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(name) = args.first().and_then(gob_name) else { return Ok(()) };
            let length = args.get(1).map(number).unwrap_or(0.0);
            boot_mut(lua)?.blower_length.insert(name, length);
            Ok(())
        })?,
    )?;
    globals.set(
        "mdkProxDoorLock",
        // 0x441150 into 0x425130: locking a door that is open **shuts it
        // first** -- it plays `ANIM_CLOSE` and clears the open state before
        // it sets the flag -- and unlocking one only clears the flag, which
        // leaves the next tick of `prox_doors` to notice the player.
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(name) = args.first().and_then(gob_name) else { return Ok(()) };
            let lock = args.get(1).map(number).unwrap_or(0.0) != 0.0;
            let mut boot = boot_mut(lua)?;
            if lock {
                if boot.playing.get(&name) == Some(&66.0) {
                    boot.playing.insert(name.clone(), 67.0);
                }
                boot.locked.insert(name);
            } else {
                boot.locked.remove(&name);
            }
            Ok(())
        })?,
    )?;
    globals.set(
        "mdkSubtractHitpoints",
        lua.create_function(|lua, args: Variadic<Value>| {
            let n = args.get(1).map(number).unwrap_or(0.0) as i64 as i16;
            let died = change_gob(lua, args.first(), |w, id| w.hurt(id, n)).unwrap_or(false);
            Ok(if died { 1.0 } else { 0.0 })
        })?,
    )?;
    globals.set(
        "mdkAddHitpoints",
        lua.create_function(|lua, args: Variadic<Value>| {
            let n = args.get(1).map(number).unwrap_or(0.0) as i64 as i16;
            change_gob(lua, args.first(), |w, id| w.heal(id, n));
            Ok(())
        })?,
    )?;

    // The scripted-sequence flag. `script.lua`'s `StartScript` sets it and
    // the driver then calls `ScriptUpdate` on the object every tick.
    for name in ["mdkGobEnableScript", "mdkGobDisableScript"] {
        let on = name == "mdkGobEnableScript";
        globals.set(
            name,
            lua.create_function(move |lua, args: Variadic<Value>| {
                let Some(gob) = args.first().and_then(gob_name) else { return Ok(()) };
                let mut boot = boot_mut(lua)?;
                if on {
                    boot.ever_scripted.insert(gob.clone());
                    boot.scripted.insert(gob);
                } else {
                    boot.scripted.remove(&gob);
                }
                Ok(())
            })?,
        )?;
    }

    // --- damage, and dying ----------------------------------------------
    // `mdkDealDamage(source, victim, amount, type, part)` — 0x43bb20 into
    // 0x40e660, and the argument order is the scripts' own: 48 call sites,
    // all of them `mdkDealDamage(what did it, what took it, how much, a
    // DAMAGE_ mask, -1)`.
    globals.set(
        "mdkDealDamage",
        lua.create_function(|lua, args: Variadic<Value>| {
            deal_damage(
                lua,
                args.first().cloned(),
                args.get(1).cloned().unwrap_or(Value::Nil),
                args.get(2).map(number).unwrap_or(0.0) as i64,
                args.get(3).map(number).unwrap_or(0.0) as i64,
                args.get(4).map(number).unwrap_or(-1.0) as i64,
                true,
            )
        })?,
    )?;
    // The built-in reaction, exposed so that a script's own `OnDamage` can
    // hand the damage back to it — which is exactly what these three are
    // for, and why they take the handler's own five arguments.
    //
    // **These are the walker's, the grunt's and the decoy's, and the engine
    // gives all three the same behaviour**: the shared part, from the class
    // handler at 0x424f60. The walker's real one (0x430a60) then adds AI —
    // it refuses damage from its own kind's shots and picks a reaction per
    // type — and none of that is here. The ceiling is stated rather than
    // hidden: an enemy loses the right hitpoints and dies at the right time,
    // and does not flinch or turn.
    for name in [
        "mdkWalkerDefaultOnDamage",
        "mdkGruntOnDamage",
        "mdkDecoyDefaultOnDamage",
    ] {
        globals.set(
            name,
            lua.create_function(|lua, args: Variadic<Value>| {
                deal_damage(
                    lua,
                    args.get(1).cloned(),
                    args.first().cloned().unwrap_or(Value::Nil),
                    args.get(2).map(number).unwrap_or(0.0) as i64,
                    args.get(3).map(number).unwrap_or(0.0) as i64,
                    args.get(4).map(number).unwrap_or(-1.0) as i64,
                    // it is being called *from* OnDamage, so asking the
                    // victim for OnDamage again would recurse for ever
                    false,
                )
            })?,
        )?;
    }

    // --- the spawners ---------------------------------------------------
    // Between them the four are 2246 calls in a boot of all ten levels, and
    // they are what puts an enemy in a room: nothing in a scene graph is one.
    globals.set(
        "mdkSpawnerSetSpawnedObject",
        // 0x440e70 into 0x4259f0. Nine arguments:
        //   (spawner, type, waypoint, p1, p2, p3, p4, interval, room)
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(name) = args.first().and_then(gob_name) else { return Ok(()) };
            let mut boot = boot_mut(lua)?;
            // 0x4259f0 writes the definition and **nothing else** -- it never
            // touches 0x2c, 0x30 or 0x44 -- so redefining a spawner keeps
            // its queue, its countdown and, above all, its shut-off flag.
            // `level1.lua` shuts a generator off when it is destroyed and
            // then runs its setup function again; clearing the flag here
            // would bring the generator back.
            let s = boot.spawners.entry(name).or_default();
            s.kind = args.get(1).map(number).unwrap_or(0.0);
            s.waypoint = args.get(2).and_then(text).filter(|w| !w.is_empty());
            s.payload = [3, 4, 5, 6].map(|i| args.get(i).map(number).unwrap_or(0.0));
            s.interval = args.get(7).map(number).unwrap_or(0.0);
            s.room = args.get(8).and_then(gob_name);
            Ok(())
        })?,
    )?;
    globals.set(
        "mdkSpawnerQueue",
        // 0x425c00. The reset of the countdown when the queue was empty is
        // what makes the first of a batch arrive at once rather than one
        // interval late.
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(name) = args.first().and_then(gob_name) else { return Ok(()) };
            let n = args.get(1).map(number).unwrap_or(0.0) as i64;
            if let Some(s) = boot_mut(lua)?.spawners.get_mut(&name) {
                if !s.off {
                    if s.queue == 0 {
                        s.timer = 0.0;
                    }
                    s.queue += n;
                }
            }
            Ok(())
        })?,
    )?;
    globals.set(
        "mdkSpawnerShutOff",
        // 0x425c50
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(name) = args.first().and_then(gob_name) else { return Ok(()) };
            if let Some(s) = boot_mut(lua)?.spawners.get_mut(&name) {
                s.queue = 0;
                s.off = true;
            }
            Ok(())
        })?,
    )?;
    globals.set(
        "mdkSpawnerSpawnObject",
        // 0x441040 into 0x425a80, which bypasses the queue entirely: three
        // calls in a row make three objects on the same frame, and
        // `boss.lua` does exactly that.
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(name) = args.first().and_then(gob_name) else { return Ok(Value::Nil) };
            Ok(spawn(lua, &name)?.map(Value::Table).unwrap_or(Value::Nil))
        })?,
    )?;

    // The difficulty, which is the other half of every hitpoint in the game.
    // `menu.lua` is the only caller of the setter, with 0.2, 0.35, 0.5 and
    // 1.0 -- Easy, Medium, Hard and "Jinkies!".
    globals.set(
        "mdkSetDifficulty",
        // 0x43aa60, straight into the global at 0x4bb71c
        lua.create_function(|lua, args: Variadic<Value>| {
            let d = args.first().map(number).unwrap_or(0.0) as f32;
            if let Some(mut w) = world::world_mut(lua) {
                w.set_difficulty(d);
            }
            Ok(())
        })?,
    )?;
    globals.set(
        "mdkGetDifficulty",
        lua.create_function(|lua, _: Variadic<Value>| {
            Ok(world::world(lua).map(|w| w.difficulty()).unwrap_or(world::DEFAULT_DIFFICULTY) as f64)
        })?,
    )?;
    globals.set(
        "mdkDiffScale",
        // 0x43aaf0, which is the scaling routine itself exposed to scripts
        lua.create_function(|lua, args: Variadic<Value>| {
            let base = args.first().map(number).unwrap_or(0.0) as i64 as i32;
            let d = world::world(lua).map(|w| w.difficulty()).unwrap_or(world::DEFAULT_DIFFICULTY);
            Ok(world::diff_scale(d, base) as f64)
        })?,
    )?;
    globals.set(
        "mdkCreateDestructable",
        // 0x440e00. `boss.lua` is its only caller -- 16 times, giving
        // Zizzy's parts their own health as the fight goes on.
        lua.create_function(|lua, args: Variadic<Value>| {
            let base = args.get(1).map(number).unwrap_or(0.0) as i64 as i32;
            change_gob(lua, args.first(), |w, id| w.make_destructable(id, base));
            Ok(())
        })?,
    )?;

    // `chSndSwitchMusic(N)` is the same numbering as a room's `music`:
    // `Music/TrackNN`, with 0 and -1 stopping it. 148 calls, more than any
    // other sound function in the scripts.
    globals.set(
        "chSndSwitchMusic",
        lua.create_function(|lua, args: Variadic<Value>| {
            let n = args.first().map(number).unwrap_or(0.0);
            boot_mut(lua)?.music = Some(n);
            Ok(())
        })?,
    )?;
    for name in ["omGobGMSetSltVisible", "omGobGMSetSolid"] {
        let drawing = name == "omGobGMSetSltVisible";
        globals.set(
            name,
            lua.create_function(move |lua, args: Variadic<Value>| {
                let Some(gob) = args.first().and_then(gob_name) else { return Ok(()) };
                let handle = args.get(1).map(number).unwrap_or(-1.0);
                let on = args.get(2).map(number).unwrap_or(1.0) != 0.0;
                let mut boot = boot_mut(lua)?;
                let Some(slot) = boot
                    .slots
                    .get(&gob)
                    .and_then(|s| s.get(handle as usize))
                    .cloned()
                else {
                    return Ok(());
                };
                let set = if drawing { &mut boot.hidden } else { &mut boot.intangible };
                if on {
                    set.remove(&(gob, slot));
                } else {
                    set.insert((gob, slot));
                }
                Ok(())
            })?,
        )?;
    }

    // `chFogStartEnd(50, 400)` is the game's own draw distance, and it is
    // what the renderer should fade to rather than a number invented here.
    globals.set(
        "chFogStartEnd",
        lua.create_function(|lua, args: Variadic<Value>| {
            let mut boot = boot_mut(lua)?;
            boot.fog.near = args.first().map(number).unwrap_or(0.0) as f32;
            boot.fog.far = args.get(1).map(number).unwrap_or(0.0) as f32;
            Ok(())
        })?,
    )?;

    // `chFogEnable()` is `glEnable(GL_FOG)` and `chFogDisable()` its
    // opposite; 56 of the scripts' 61 fog calls are the bare enable.
    for name in ["chFogEnable", "chFogDisable"] {
        let on = name == "chFogEnable";
        globals.set(
            name,
            lua.create_function(move |lua, _: Variadic<Value>| {
                boot_mut(lua)?.fog.on = on;
                Ok(())
            })?,
        )?;
    }
    // `chFogColor(r, g, b, a)` is `glFogfv(GL_FOG_COLOR, ...)`. The alpha is
    // always 1 in the shipped scripts and fog has no alpha to give, so only
    // the three channels are kept.
    globals.set(
        "chFogColor",
        lua.create_function(|lua, args: Variadic<Value>| {
            let c = |i: usize| args.get(i).map(number).unwrap_or(0.0) as f32;
            boot_mut(lua)?.fog.colour = [c(0), c(1), c(2)];
            Ok(())
        })?,
    )?;

    // --- the checkpoints ------------------------------------------------
    // `mdkSetCheckpoint(n, x, y, z, facing, section)`
    globals.set(
        "mdkSetCheckpoint",
        lua.create_function(|lua, args: Variadic<Value>| {
            let at = |i: usize| args.get(i).map(number).unwrap_or(0.0);
            boot_mut(lua)?.checkpoints.push(Checkpoint {
                index: at(0),
                position: [at(1), at(2), at(3)],
                facing: at(4),
                section: args.get(5).map(number),
                // 0x42ebc0 clears the delete list and writes -1 to the
                // previous-checkpoint field, so a checkpoint set twice keeps
                // neither
                delete: Vec::new(),
                prev: None,
            });
            Ok(())
        })?,
    )?;
    // `mdkCheckpointAddDeleteRoom(n, room)` — 0x441e60 into 0x42ec20, which
    // appends the room to the list at +0x14 of checkpoint `n`'s 36-byte
    // record (the table at 0x4bba90 holds 50 of them).
    globals.set(
        "mdkCheckpointAddDeleteRoom",
        lua.create_function(|lua, args: Variadic<Value>| {
            let n = args.first().map(number).unwrap_or(-1.0);
            let Some(room) = args.get(1).and_then(gob_name) else { return Ok(()) };
            let mut boot = boot_mut(lua)?;
            if let Some(cp) = boot.checkpoints.iter_mut().find(|c| c.index == n) {
                cp.delete.push(room);
            }
            Ok(())
        })?,
    )?;
    // `mdkCheckpointSetPrevCheckpoint(n, prev)` — 0x42ec50, one word at +0x20.
    globals.set(
        "mdkCheckpointSetPrevCheckpoint",
        lua.create_function(|lua, args: Variadic<Value>| {
            let n = args.first().map(number).unwrap_or(-1.0);
            let prev = args.get(1).map(number).unwrap_or(-1.0);
            let mut boot = boot_mut(lua)?;
            if let Some(cp) = boot.checkpoints.iter_mut().find(|c| c.index == n) {
                cp.prev = if prev >= 0.0 { Some(prev as usize) } else { None };
            }
            Ok(())
        })?,
    )?;
    // `mdkDestroyRoom(room)` — 0x441fc0 into 0x42e4a0, which drops the room
    // from the engine's list of live rooms and then destroys the gob. Here
    // that is the gob **and everything parented to it**, because a room's
    // contents are its children and the original destroys a tree.
    globals.set(
        "mdkDestroyRoom",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(room) = args.first().and_then(gob_name) else { return Ok(()) };
            destroy_room(lua, &room)?;
            Ok(())
        })?,
    )?;

    // --- what the engine answers ----------------------------------------
    // Both steer the boot, and both are the engine's own state rather than
    // something a script decides.
    //
    // `mdkLoadLevelIsInstant` is **0**, and the difference is the whole
    // level: `PreInitLevel` reads it to choose between `mdkGobCreateInstant()`
    // -- resuming from an instant save -- and `dofile(Level.file)`, which is
    // what loads the scene graph. Answering 1 starts the level with no
    // objects in it at all, and the first handler that names one fails.
    globals.set("chGetGameWasReset", lua.create_function(|_, ()| Ok(0))?)?;
    globals.set("mdkLoadLevelIsInstant", lua.create_function(|_, ()| Ok(0))?)?;

    // --- dofile ---------------------------------------------------------
    // The engine's takes a bare resource name and finds the file; a name
    // that is not there is not an error, because the scripts try for files
    // that only some levels ship.
    globals.set(
        "dofile",
        lua.create_function(|lua, name: String| {
            let mut key = name.to_ascii_lowercase();
            if !key.contains('.') {
                key.push_str(".lua");
            }
            let source = boot_ref(lua)?.sources.get(&key).cloned();
            let Some(source) = source else { return Ok(Value::Nil) };
            let text = crate::game::script::preprocess(&source, &[])
                .map_err(|e| mlua::Error::runtime(e.to_string()))?;
            lua.load(&text).set_name(&key).eval::<Value>()
        })?,
    )?;

    // --- everything else, recorded --------------------------------------
    //
    // A recorder answers `nil`, except for the ones the scripts' own naming
    // convention says make or fetch something -- Create, Make, New, Get --
    // which answer a fresh table, because the next line always gives it
    // fields: `menu.lang = mdkCreateMenu(...)` then `menu.lang.Cancel =
    // function() end`. Reading the convention rather than guessing is what
    // `tools/luarun.py` established, and it is what gets all 129 checkpoints
    // through.
    for (name, _address) in FUNCTIONS {
        if globals.contains_key(name)? {
            continue;
        }
        let makes = ["Create", "Make", "New", "Get"].iter().any(|w| name.contains(w));
        globals.set(
            name,
            lua.create_function(move |lua, _: Variadic<Value>| {
                *boot_mut(lua)?
                    .unimplemented
                    .entry(name.to_string())
                    .or_insert(0) += 1;
                if makes {
                    Ok(Value::Table(lua.create_table()?))
                } else {
                    Ok(Value::Nil)
                }
            })?,
        )?;
    }
    Ok(())
}

/// The `OBJ_*` name of a type value, out of the table the binary registers.
pub fn type_name(kind: f64) -> Option<&'static str> {
    crate::game::constants::CONSTANTS
        .iter()
        .find(|(n, v)| *v == kind && n.starts_with("OBJ_"))
        .map(|(n, _)| *n)
}

/// The model a character wears, which the scene graph does **not** say: a
/// character's `resource` slot holds a **waypoint name**, so the model comes
/// from its type.
///
/// The engine's own mapping lives in the per-type constructor below
/// `0x42ac60` and has not been read. What is used here is the naming
/// convention the data keeps — `OBJ_KURT` wears `kurt.mod`, `OBJ_MAX` wears
/// `max.mod` — and it is a convention and not a rule: **67 of the 149
/// `OBJ_*` types have a model named after them**, and the rest do not.
/// Everything it does not cover simply goes undrawn, which is the honest
/// failure.
/// **The three definition tables name their own models**, so ask them before
/// guessing. Between them they cover 137 types — 49 items, 69 shots, 19
/// enemies — and they disagree with the convention where it matters:
/// `OBJ_LASERCANNON` wears `lasergatgun.mod`, not `lasercannon.mod`. The
/// naming convention stays as the fallback for everything else.
/// **The model an object actually wears**, which is not the same question as
/// what its *type* wears. The order is the original's own:
///
/// 1. a table that names the type -- the walker definitions, the item table,
///    the shot table, the four characters. A walker's `resource` slot holds a
///    **waypoint name**, not a model, so asking it first drew nothing at all
///    for every enemy the levels place with a pen.
/// 2. the registration's own resource, which is what the object factory's
///    default case loads.
/// 3. the `OBJ_*` name, which agrees with the factory wherever it fires.
pub fn model_of(kind: f64, resource: Option<&str>) -> Option<String> {
    if let Some(m) = crate::game::world::table_model(kind) {
        return Some(m.to_string());
    }
    if let Some(r) = resource.filter(|r| !r.to_ascii_lowercase().ends_with(".wav")) {
        return Some(r.to_string());
    }
    model_for_type(kind)
}

pub fn model_for_type(kind: f64) -> Option<String> {
    if let Some(m) = crate::game::world::table_model(kind) {
        return Some(m.to_string());
    }
    // **and the guess is wrong for `OBJ_CHECKPOINT`.** `CheckPoint.mod` is a
    // file, so guessing from the name found it and drew it -- a pale
    // octagonal pad with four beams converging above, standing on several
    // checkpoints with the player *inside* it. Level 6's Hyde could not be
    // seen at four of his checkpoints because of it.
    //
    // The object factory settles it: 0x42ac60's switch on the type has no
    // case for **0x244**, so a checkpoint falls to the default and loads
    // whatever resource it was registered with, which for every one of them
    // is nothing. The model belongs to **`OBJ_PORTAL`** -- case **0x208** is
    // the only place `"checkpoint"` appears in the binary, at 0x42b5a1, and
    // it is that type's *default* name.
    const CHECKPOINT: f64 = 580.0;
    if kind == CHECKPOINT {
        return None;
    }
    // **And the guess is right everywhere else it fires, which was checked
    // rather than assumed.** Turning it off costs level 1 and level 8 nothing
    // at all -- the tables already name everything they draw -- and level 4
    // eight objects and 4264 triangles, every one of them an
    // `OBJ_BOXINGGLOVE`. The factory's own case for that type, **0x24e at
    // 0x427970**, pushes the literal `"boxingglove"` and hands it to the
    // model loader, so the guess agrees with the binary. The same holds for
    // the other constructors that name a model outright: `ladderspot`
    // (0x23f), `lightflare02` and `stars` (0x259).
    type_name(kind).map(|n| n[4..].to_ascii_lowercase())
}

/// Which animation a walker should be playing, from where it is going.
///
/// The original has a name for this — `mdkWalkerAnimUpdate` — so the
/// **engine** drives a character's locomotion, not the scripts. The names
/// are the game's own: every one of `kurt.mod`'s 61 animations carries an id
/// the binary names `ANIM_*`, and 6292 of the corpus's 6311 do.
///
/// `forward` and `right` are the movement in the body's own frame.
pub fn walk_animation(forward: f64, right: f64) -> &'static str {
    const STILL: f64 = 0.1;
    let (f, r) = (forward > STILL, right > STILL);
    let (b, l) = (forward < -STILL, right < -STILL);
    match (f, b, l, r) {
        (true, _, true, _) => "ANIM_RUNFL",
        (true, _, _, true) => "ANIM_RUNFR",
        (true, ..) => "ANIM_RUNF",
        (_, true, true, _) => "ANIM_RUNBL",
        (_, true, _, true) => "ANIM_RUNBR",
        (_, true, ..) => "ANIM_RUNB",
        (_, _, true, _) => "ANIM_RUNL",
        (_, _, _, true) => "ANIM_RUNR",
        // `ANIM_DEFAULT` is the still pose, and it is first in every one of
        // the 1146 animated models — which is why animation 0 never moves.
        _ => "ANIM_DEFAULT",
    }
}

/// Tell an object to play a named animation, the way `omAnimPlay` does.
pub fn play_named(scripts: &Scripts, gob: &str, animation: &str) -> Result<(), Error> {
    let Some(id) = crate::game::constants::CONSTANTS
        .iter()
        .find(|(n, _)| *n == animation)
        .map(|(_, v)| *v)
    else {
        return Ok(());
    };
    if let Some(mut boot) = scripts.lua.app_data_mut::<Boot>() {
        boot.playing.insert(gob.to_string(), id);
    }
    Ok(())
}

/// Read a field off the gob a script handed us.
fn with_gob<T>(lua: &Lua, v: Option<&Value>, f: impl Fn(&Gob) -> T) -> Option<T> {
    let name = v.and_then(gob_name)?;
    let w = world::world(lua)?;
    w.find(&name).and_then(|id| w.get(id)).map(f)
}

/// Change one, in place.
fn edit_gob(lua: &Lua, v: Option<&Value>, f: impl FnOnce(&mut Gob)) {
    let Some(name) = v.and_then(gob_name) else { return };
    let Some(mut w) = world::world_mut(lua) else { return };
    if let Some(id) = w.find(&name) {
        if let Some(g) = w.get_mut(id) {
            f(g);
        }
    }
}

/// And the two that are arithmetic on the arena rather than on one field.
fn change_gob<T>(lua: &Lua, v: Option<&Value>, f: impl FnOnce(&mut world::World, world::Id) -> T) -> Option<T> {
    let name = v.and_then(gob_name)?;
    let mut w = world::world_mut(lua)?;
    let id = w.find(&name)?;
    Some(f(&mut w, id))
}

/// Destroy a room: the gob, everything parented to it, and their globals.
///
/// The Lua tables go too. A script that still holds one would otherwise get a
/// handle whose `__gob` names an emptied arena slot, which is a subtler
/// failure than the `nil` the original leaves behind.
fn destroy_room(lua: &Lua, room: &str) -> mlua::Result<usize> {
    let gone = {
        let mut w = world::world_mut(lua).ok_or_else(|| mlua::Error::runtime("no world"))?;
        let Some(id) = w.find(room) else { return Ok(0) };
        w.destroy(id)
    };
    let globals = lua.globals();
    for name in &gone {
        let _ = globals.set(name.as_str(), Value::Nil);
    }
    boot_mut(lua)?.destroyed.extend(gone.iter().cloned());
    Ok(gone.len())
}

/// Apply the delete lists of every checkpoint up to and including the one the
/// level is starting at — the game's streaming, done in one step because the
/// engine arrives at a checkpoint rather than walking to it.
///
/// **The trigger is inferred, and the inference has two supports.** The
/// bookkeeping is read outright (0x42ec20 appends, 0x42ebc0 clears, 0x42e4a0
/// destroys), but the site that walks a checkpoint's list when it is reached
/// is not in the binary at all.
///
/// It is in `mdk2.lua`, as `DeleteCheckpointRooms(cp)` — which walks
/// `Level.scenegraph.checkpoints[cp].delete` and calls `mdkDestroyRoom` on
/// each. **Nothing calls it.** No script does, and its name does not occur in
/// `mdk2Main.exe`, so it is not a Lua callback either: it is dead code that
/// BioWare left behind. Dead, but it states the contract — *one* checkpoint's
/// list, applied when that checkpoint is reached — and arriving at checkpoint
/// N means having reached every streaming checkpoint before it.
///
/// The second support is a check rather than a reading: **all 129 checkpoints
/// still stand in a room that exists** afterwards, and `--boot` fails if one
/// does not.
fn stream(lua: &Lua, checkpoint: f64) -> mlua::Result<()> {
    let lists: Vec<Vec<String>> = {
        let boot = boot_ref(lua)?;
        boot.checkpoints
            .iter()
            .filter(|c| c.index <= checkpoint && !c.delete.is_empty())
            .map(|c| c.delete.clone())
            .collect()
    };
    for rooms in lists {
        for room in rooms {
            destroy_room(lua, &room)?;
        }
    }

    // and the check: the checkpoint the level is starting at must still be
    // standing in a room. A room's box comes from the scene graph, which has
    // already run, so this asks the same question the driver asks every tick.
    let here: Vec<String> = {
        let boot = boot_ref(lua)?;
        let Some(cp) = boot.checkpoints.iter().find(|c| c.index == checkpoint) else {
            return Ok(());
        };
        boot.rooms
            .iter()
            .filter(|r| {
                r.bbox.is_some_and(|b| {
                    (0..3).all(|i| b[i] <= cp.position[i] && cp.position[i] <= b[i + 3])
                })
            })
            .map(|r| r.name.clone())
            .collect()
    };
    if !here.is_empty() {
        let w = world::world(lua).ok_or_else(|| mlua::Error::runtime("no world"))?;
        if !here.iter().any(|n| w.find(n).is_some()) {
            drop(w);
            boot_mut(lua)?.homeless += 1;
        }
    }
    Ok(())
}

/// Deal damage, from 0x40e660.
///
/// The structure is the thing, and it is not what a reimplementation would
/// invent: **if the victim has an `OnDamage` handler, the script gets the
/// damage and the built-in never runs.** The engine's own reaction is the
/// `else` branch, not a step the handler decorates — which is why the game
/// exposes three `*OnDamage` functions for a handler to call back into.
///
/// Three more things are read rather than guessed. **Nothing happens at all
/// for an amount of zero or less** (0x40e768), and that test comes *before*
/// the handler, so a script's `OnDamage` never sees a harmless hit. The
/// **filter is only consulted on the built-in path** (0x40e885) — a script
/// handler is called whatever the object is vulnerable to. And `part`
/// reaches Lua as the **name** of a model slot, or `nil` for -1 (0x40e7d8),
/// not as a number: `level7.lua` compares it against `"SHWANG_PALML"`.
/// Every one of the 48 call sites in the shipped scripts passes -1.
/// **The goto core**, 0x431b80 -- the one movement primitive the machines
/// share. `mdkWalkerGotoPoint` wraps it, `mdkWalkerGotoPointDirectly` wraps
/// it, the conehead civilian's wander is it and the birdbrain's go-home and
/// chase are it. Returns true the frame the walker has arrived.
///
/// What it does, in the order it does it:
///
/// * arrival is **two-dimensional** -- the third component of its own
///   distance is the walker's z, not the destination's -- inside `radius`,
///   and a **flier** must also be within **0.5** of the destination's height
///   before it counts. `def + 0x14` bit 2 is what makes that test apply.
/// * with the re-aim clock at `walker + 0x98` spent, it points at the
///   destination -- and **past ten units** it points off it by
///   `(chRand() * 2 - 1) * wobble`. So a walker crossing a room wanders and
///   only straightens up over the last ten.
/// * with time left on the clock it keeps the heading, walks or runs, and
///   stands still while it is still turning if `mustFace` is set. With
///   `avoid` it probes `def + 0x80` ahead **with the cliff leg on**, and a
///   block turns it a right angle to its own side.
/// * either way, a flier climbs or dives at `def + 0x30` toward the
///   destination's height.
///
/// The clock is re-armed at `chRand() * 3 + 1` (0x48f2f0 and 0x48f2f4).
pub(crate) fn goto_core(
    boot: &mut Boot,
    who: &str,
    kind: f64,
    at: [f64; 3],
    yaw: f64,
    dest: [f64; 3],
    run: bool,
    must_face: bool,
    wobble: f64,
    avoid: bool,
    radius: f64,
) -> bool {
    /// How far off before the wobble is applied at all: the 10 at 0x48f384.
    const WANDER_PAST: f64 = 10.0;
    /// The height a flier has to be inside before it has arrived, and the
    /// band it holds: the 0.5 at 0x48f2fc.
    const LEVEL: f64 = 0.5;
    /// And how long a heading is kept: `chRand() * 3 + 1`.
    const KEEP: (f64, f64) = (3.0, 1.0);
    let flies = crate::game::world::climb(kind).is_some();
    let flat = ((dest[0] - at[0]).powi(2) + (dest[1] - at[1]).powi(2)).sqrt();
    let dz = if flies { (dest[2] - at[2]).abs() } else { 0.0 };
    if flies {
        boot.altitude.insert(who.to_string(), dest[2]);
    }
    if flat < radius {
        if dz < LEVEL {
            boot.reaim.remove(who);
            return true;
        }
        boot.gait.insert(who.to_string(), 0);
        return false;
    }
    let bearing = crate::game::body::bearing(dest[0] - at[0], dest[1] - at[1]);
    if boot.reaim.get(who).copied().unwrap_or(0.0) <= 0.0 {
        let want = if flat > WANDER_PAST {
            bearing + (boot.random.next() * 2.0 - 1.0) * wobble
        } else {
            bearing
        };
        boot.heading.insert(who.to_string(), want);
        let keep = boot.random.next() * KEEP.0 + KEEP.1;
        boot.reaim.insert(who.to_string(), keep);
        // and **the gait is not touched here**: 0x431bfe aims, arms the
        // clock and falls straight through to the climb. Only the branch
        // with time left on it decides whether the legs move, which is why a
        // walker given a new heading spends one frame on whatever it was
        // already doing.
        return false;
    }
    let heading = boot.heading.get(who).copied().unwrap_or(yaw);
    let square = facing(yaw, heading);
    if avoid {
        boot.avoiding.insert(who.to_string());
        boot.cliffs.insert(who.to_string());
    }
    boot.gait.insert(
        who.to_string(),
        if must_face && !square { 0 } else if run { 2 } else { 1 },
    );
    false
}

/// **What every live task list is waiting on.** `ScriptUpdate` in
/// `script.lua` reads a task's return value as a *step*: `nexttask += res`,
/// with `nil` counting as 1, so a task that returns **0** holds its list at
/// the same index for ever. That is the shipped way to spell "wait", and it
/// is also what a function the engine answers wrongly looks like from the
/// outside -- the list stops and everything queued behind it never happens.
///
/// This walks every named object, finds the ones with a `stack`, and returns
/// `(object, the function at stack.script[stack.nexttask][1], nexttask)`. The
/// name comes from the globals table by identity, which is exact for the
/// engine's own bindings and for anything `Level.*` puts there.
///
/// **Waiting is not stalling**, and telling them apart needs the index over
/// time rather than at the end. `l9r5aconekiss01` sits on
/// `{ omAnimJustLooped, { ANIM_ACTION00 } }` at almost any instant you look,
/// and it is perfectly healthy: the animation is 0.2 seconds long, so five
/// ticks in six the answer is 0 and the sixth it is 1 and the list loops. So
/// the driver samples this every second and reports only the objects whose
/// index **and position** both never moved -- level 9's `ch1` runs 89 units,
/// arrives, deletes itself and is made again, and its index is 1 whenever you
/// look. The position is what tells the two apart.
pub fn stalls(lua: &Lua) -> Vec<(String, String, i64, [f64; 3])> {
    let globals = lua.globals();
    let mut named: Vec<(String, mlua::Function)> = Vec::new();
    if let Ok(pairs) = globals.clone().pairs::<String, Value>().collect::<mlua::Result<Vec<_>>>() {
        for (name, v) in pairs {
            if let Value::Function(f) = v {
                named.push((name, f));
            }
        }
    }
    let mut out: Vec<(String, String, i64, [f64; 3])> = Vec::new();
    let Some(w) = world::world(lua) else { return Vec::new() };
    let (frozen_now, ticking) = match boot_ref(lua) {
        Ok(b) => (b.stasis.clone(), b.scripted.clone()),
        Err(_) => Default::default(),
    };
    for (id, g) in w.iter() {
        // **a frozen object's list is not stuck, it is asleep.** 0x46d505
        // skips a frozen object's whole update, so its `nexttask` cannot
        // move and it would read as a stall for ever.
        if g.name.is_empty() || frozen(&w, &frozen_now, id) {
            continue;
        }
        let Ok(gob) = globals.get::<mlua::Table>(g.name.as_str()) else { continue };
        let Ok(stack) = gob.get::<mlua::Table>("stack") else { continue };
        let Ok(next) = stack.get::<i64>("nexttask") else { continue };
        let Ok(script) = stack.get::<mlua::Table>("script") else { continue };
        let Ok(task) = script.get::<mlua::Table>(next) else { continue };
        let Ok(Value::Function(f)) = task.get::<Value>(1) else { continue };
        // **and what the task was called with**, because "waiting on
        // `omAnimJustLooped`" is not an answer and "waiting for
        // `ml10x_allmovgob` to loop animation 77" is
        let about = task
            .get::<mlua::Table>(2)
            .ok()
            .map(|args| {
                let who = match args.get::<Value>(1) {
                    Ok(Value::Table(t)) => t.get::<String>("name").unwrap_or_default(),
                    Ok(Value::Number(n)) => format!("{n}"),
                    _ => String::new(),
                };
                let what = args.get::<Value>(2).ok().and_then(|v| match v {
                    Value::Number(n) => Some(format!(" {n}")),
                    _ => None,
                });
                format!("{who}{}", what.unwrap_or_default())
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_default();
        let name = named
            .iter()
            .find(|(_, other)| *other == f)
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| "an anonymous function".into());
        // **and whether it is being ticked at all**, which is a different
        // fault from waiting: `StartScript` calls `mdkGobEnableScript`, and
        // an object that is not in that set has a task list nothing runs.
        let mut name = if ticking.contains(&g.name) { name } else { format!("{name} (untouched)") };
        if !about.is_empty() {
            name = format!("{name}({about})");
        }
        // and `script.lua`'s own clock if the object is on one, because
        // "waiting on Wait" and "waiting on Wait with eight seconds still on
        // it every time you look" are different faults
        if let Ok(left) = gob.get::<f64>("waittimer") {
            name = format!("{name} [{left:.2} left]");
        }
        out.push((g.name.clone(), name, next, g.position));
    }
    out
}

/// **The game's area damage**, 0x40e930 into **0x40e960**: walk every gob,
/// skip the source, and for anything inside `radius` deal `damage - distance`
/// of `kind`. The falloff is a plain subtraction and the distance is measured
/// centre to centre, less the victim's own collision radius.
///
/// ponytail: a gob is a point here, so the radius is not subtracted. That
/// makes a blast slightly weaker at the edge than the original's and never
/// stronger, which is the safe direction for a thing that kills the player.
fn blast(
    lua: &Lua,
    source: &str,
    from: [f64; 3],
    radius: f64,
    damage: i64,
    kind: i64,
) -> mlua::Result<()> {
    let caught: Vec<(String, i64)> = {
        let Some(w) = world::world(lua) else { return Ok(()) };
        w.iter()
            .filter(|(_, g)| g.hitpoints > 0 && !g.name.is_empty() && g.name != source)
            .filter_map(|(_, g)| {
                let d = (0..3).map(|c| (g.position[c] - from[c]).powi(2)).sum::<f64>().sqrt();
                (d < radius).then(|| (g.name.clone(), damage - d as i64))
            })
            .collect()
    };
    let globals = lua.globals();
    let me = globals.get::<mlua::Table>(source).ok().map(Value::Table);
    for (name, hurt) in caught {
        if hurt <= 0 {
            continue;
        }
        if let Ok(gob) = globals.get::<mlua::Table>(name.as_str()) {
            deal_damage(lua, me.clone(), Value::Table(gob), hurt, kind, -1, true)?;
        }
    }
    Ok(())
}

fn deal_damage(
    lua: &Lua,
    source: Option<Value>,
    victim: Value,
    amount: i64,
    kind: i64,
    part: i64,
    scripted: bool,
) -> mlua::Result<()> {
    if amount <= 0 {
        return Ok(());
    }
    let Some(name) = gob_name(&victim) else { return Ok(()) };
    let Ok(gob) = lua.globals().get::<mlua::Table>(name.as_str()) else { return Ok(()) };

    if scripted {
        if let Ok(handler) = gob.get::<mlua::Function>("OnDamage") {
            let from = source.unwrap_or(Value::Nil);
            let slot = match part_name(lua, &name, part)? {
                Some(s) => Value::String(lua.create_string(&s)?),
                None => Value::Nil,
            };
            let _ = handler.call::<Value>((gob, from, amount, kind, slot));
            return Ok(());
        }
    }

    let hit = {
        let mut w = world::world_mut(lua).ok_or_else(|| mlua::Error::runtime("no world"))?;
        let Some(id) = w.find(&name) else { return Ok(()) };
        // the filter gates the built-in path and only that
        match w.get(id) {
            Some(g) if (g.damage_filter as i64 & kind) == 0 => world::Hit::Ignored,
            Some(_) => w.take_damage(id, amount.min(i16::MAX as i64) as i16),
            None => world::Hit::Ignored,
        }
    };
    if hit == world::Hit::Died {
        die(lua, &name)?;
    }
    Ok(())
}

/// `ANIM_DIE`, which is what a walker plays when its hitpoints reach zero.
/// 0x430be2 in the walker's own `OnDamage` (0x430a60): stop it, clear the
/// gait and the strafe, play 17, and switch the collision body at
/// `gob + 0x68` off with `+0xcc = 0`.
const ANIM_DIE: f64 = 17.0;

/// `OnDie(gob, 1)`, from 0x40e1b0 — **two arguments**, the object and a
/// literal 1.0 the original pushes as a number.
fn die(lua: &Lua, name: &str) -> mlua::Result<()> {
    // and a walker falls over where it stood. The body goes too, so the
    // corpse stops blocking and stops walking.
    let walker = world::world(lua)
        .and_then(|w| w.find(name).and_then(|i| w.get(i)).map(|g| g.kind))
        .is_some_and(|k| crate::game::world::base_hitpoints(k).is_some());
    if walker {
        let mut boot = boot_mut(lua)?;
        boot.playing.insert(name.to_string(), ANIM_DIE);
        boot.since.insert(name.to_string(), 0.0);
        boot.gait.remove(name);
        boot.bodies.remove(name);
        boot.fighting.remove(name);
    }
    boot_mut(lua)?.died.push(name.to_string());
    if let Ok(gob) = lua.globals().get::<mlua::Table>(name) {
        if let Ok(handler) = gob.get::<mlua::Function>("OnDie") {
            let _ = handler.call::<Value>((gob, 1.0));
        }
    }
    Ok(())
}

/// The name a model slot index stands for, which is what an `OnDamage`
/// handler is given. The engine learns slot names from the scripts' own
/// `omGobGMGetSltIndexByName` calls, so it can only answer for a slot some
/// script has already asked about — and since every shipped call site passes
/// -1, that has never yet been asked of it.
fn part_name(lua: &Lua, gob: &str, part: i64) -> mlua::Result<Option<String>> {
    if part < 0 {
        return Ok(None);
    }
    Ok(boot_ref(lua)?
        .slots
        .get(gob)
        .and_then(|names| names.get(part as usize))
        .cloned())
}

/// Make one object from a spawner's definition, register it, and hand back
/// its Lua table. `None` if the spawner has no definition — the original
/// tests `sp[0] == 0` and returns null, which is what a script that queues a
/// spawner it never set up gets.
///
/// From 0x425a80. The new object is **named after the spawner**: the format
/// string at 0x4a6a54 is `"%s_spawn"`, one name per spawner, so the second
/// one replaces the first as a global exactly as a repeated scene-graph name
/// does. It stands where the spawner stands, faces where it faces, wears the
/// waypoint as its resource and carries the spawner's four numbers — and
/// since `World::register` reads the type, it arrives with its hitpoints.
fn spawn(lua: &Lua, spawner: &str) -> mlua::Result<Option<mlua::Table>> {
    let def = match boot_ref(lua)?.spawners.get(spawner) {
        Some(s) if s.kind != 0.0 => s.clone(),
        _ => return Ok(None),
    };
    let name = format!("{spawner}_spawn");
    let (at, facing) = {
        let w = world::world(lua).ok_or_else(|| mlua::Error::runtime("no world"))?;
        match w.find(spawner).and_then(|id| w.get(id)) {
            Some(g) => (g.position, g.rotation),
            None => ([0.0; 3], [1.0, 0.0, 0.0, 0.0]),
        }
    };
    let (id, hitpoints) = {
        let mut w = world::world_mut(lua).ok_or_else(|| mlua::Error::runtime("no world"))?;
        let id = w.register(Gob {
            name: name.clone(),
            kind: def.kind,
            position: at,
            rotation: facing,
            resource: def.waypoint.clone(),
            payload: def.payload,
            ..Gob::default()
        });
        (id, w.get(id).map(|g| g.hitpoints).unwrap_or(0))
    };
    boot_mut(lua)?.spawned.push((name.clone(), hitpoints));
    let made = world::handle(lua, &name, id, at)?;

    // `OnSpawn(spawner, spawned)` — event 9 in the original's own table, and
    // 0x425b4f asks the spawner for it before doing anything else with the
    // new object. `level8.lua` uses it to point the thing at the player.
    if let Ok(gob) = lua.globals().get::<mlua::Table>(spawner) {
        if let Ok(handler) = gob.get::<mlua::Function>("OnSpawn") {
            let _ = handler.call::<Value>((gob, made.clone()));
        }
    }
    Ok(Some(made))
}

/// **Kurt's ordinary gun is hitscan, not a projectile.** 0x417ebe builds a
/// segment **100 units** (0x42c80000) along the muzzle's forward, hands it to
/// the world ray at 0x471c50, and on a hit calls `0x40e660(kurt, victim,
/// damage, 1, -1)` — damage **5 when `kurt[0x58]` is 1 and 2 otherwise**,
/// type 1, which is `DAMAGE_GOODGUY`. Only weapon mode 2 makes a real bullet
/// (type 431, `lasershot2`), which is why no column of the item table links a
/// weapon to a shot: for most of the list there is nothing to link.
///
/// `mode` is `kurt + 0x58`. Returns what was hit, if anything.
///
/// ponytail: the original rays against the world's own hulls; here a gob is a
/// point, so the shot takes the nearest thing inside a narrow cone that the
/// collision world can see. A wall stops it — [`Collision::sees`] is exact —
/// but a near miss on a wide target counts as a hit.
/// **Who is being played, and as what.** The commit at 0x42b9f0 takes the gob
/// and the inventory for a mode out of the table at 0x4bb6b0 (stride three
/// dwords, filled by `mdkSetPlayModeGobs`), shows them, hands the camera to
/// 0x42a760 and only then writes the mode down. The engine keeps the two
/// halves that are visible from Lua: the player and the number.
///
/// The window calls this too -- the sniper scope is a play mode, not a
/// weapon, and 0x4198e0 enters it with exactly this call on **4**.
/// **The sniper's zoom**, in degrees of field of view. 0x41ad00 is the whole
/// of it: `fov += (fov * 0.4 + 1) * dt * step`, with the step **-5 for
/// `COM_SMZOOMIN` and +5 for `COM_SMZOOMOUT`** (0x41a302 and 0x41a343 push
/// the two literals), clamped to **0.8** at 0x48fa18 and **60** at 0x48fa1c.
///
/// The rate is proportional, which is what makes the scope feel the way it
/// does: held down at thirty frames a second it goes 60 -> 5.4 in the first
/// second and then crawls, because at one degree the step is a sixtieth of
/// what it was at sixty. `direction` is negative to zoom in.
pub fn zoom(fov: f64, direction: f64, dt: f64) -> f64 {
    /// The 0.4 at 0x48fa20 and the 1.0 at 0x48f2f4.
    const RATE: f64 = 0.4;
    /// The +-5 the two zoom commands push.
    const STEP: f64 = 5.0;
    /// The two clamps, 0x48fa18 and 0x48fa1c.
    const NARROWEST: f64 = 0.8;
    const WIDEST: f64 = 60.0;
    (fov + (fov * RATE + 1.0) * dt * direction * STEP).clamp(NARROWEST, WIDEST)
}

pub fn switch_play_mode(lua: &Lua, mode: i64) -> mlua::Result<()> {
    let was = boot_ref(lua)?.mode;
    boot_mut(lua)?.mode = mode;
    // **`OnModeSwitch` (event 0x11) fires on the room**, which is how level
    // 1's tutorial knows you have found the scope: `l1_r1.OnModeSwitch`
    // advances its task list to state 16 and then unhooks itself. The whole
    // event table is 19 names at 0x49bb20, sixteen bytes each, indexed by the
    // number 0x40e010 is handed -- 0 is `OnUpdate` and 17 is this one.
    if mode != was {
        let here = boot_ref(lua)?.room.clone();
        if let Some(gob) = here.and_then(|n| lua.globals().get::<mlua::Table>(n).ok()) {
            if let Ok(handler) = gob.get::<mlua::Function>("OnModeSwitch") {
                handler.call::<()>((gob, mode as f64))?;
            }
        }
    }
    let who = boot_ref(lua)?.play_modes.get(&mode).cloned();
    let Some(who) = who else { return Ok(()) };
    if let Ok(gob) = lua.globals().get::<mlua::Table>(who.as_str()) {
        lua.set_named_registry_value("player", gob)?;
    }
    boot_mut(lua)?.player = Some(who);
    Ok(())
}

pub fn hitscan(lua: &Lua, shooter: &str, mode: i64) -> Option<String> {
    /// How far Kurt's gun reaches, from the 100.0 pushed at 0x417e9c.
    const REACH: f64 = 100.0;
    /// Ours: how far off the nose a gob still counts as under the crosshair.
    const CONE: f64 = 0.15;
    let (at, yaw) = stance(lua, shooter)?;
    let eye = [at[0], at[1], at[2] + crate::game::body::EYE];
    let solid = lua.app_data_ref::<std::rc::Rc<crate::game::body::Collision>>();
    let victim = {
        let w = world::world(lua)?;
        let mut best: Option<(f64, String)> = None;
        for (_, g) in w.iter() {
            if g.hitpoints <= 0 || g.name.is_empty() || g.name == shooter {
                continue;
            }
            let d = [g.position[0] - at[0], g.position[1] - at[1], g.position[2] - at[2]];
            let flat = (d[0] * d[0] + d[1] * d[1]).sqrt();
            if flat > REACH
                || flat <= 0.0
                || !facing_within(yaw, crate::game::body::bearing(d[0], d[1]), CONE)
            {
                continue;
            }
            let head = [g.position[0], g.position[1], g.position[2] + crate::game::body::EYE];
            if solid.as_ref().is_some_and(|c| !c.sees(eye, head)) {
                continue;
            }
            if best.as_ref().is_none_or(|(near, _)| flat < *near) {
                best = Some((flat, g.name.clone()));
            }
        }
        best.map(|(_, n)| n)?
    };
    drop(solid);
    let globals = lua.globals();
    let source = globals.get::<mlua::Table>(shooter).ok().map(Value::Table);
    let hit = globals.get::<mlua::Table>(victim.as_str()).ok()?;
    let damage = if mode == 1 { 5 } else { 2 };
    deal_damage(lua, source, Value::Table(hit), damage, DAMAGE_GOODGUY as i64, -1, true).ok()?;
    Some(victim)
}

/// **The proximity doors**, out of `mdkObject.c`: the constructor at 0x4250b0
/// (line 191) and the update at 0x425010.
///
/// A door of type `OBJ_PROXDOOR1` keeps five words at `gob + 0x40` — whether
/// it is open, two flags out of the scene graph, its radius, and whether a
/// script has locked it. The update is the whole of the behaviour:
///
/// ```text
/// 0x42501f  the player's gob
/// 0x425031  if door->0x10 (locked): nothing
/// 0x42503a  d = distance(door, player)          ; three axes, gob + 0x18
/// 0x42503f  if d < door->0xc (the radius):
/// 0x425060      if not already open: play ANIM_OPEN, and it is open
///           else:
/// 0x425093      if open: play ANIM_CLOSE, and it is shut
/// ```
///
/// The two animation ids are `0x42` and `0x43`, which are `ANIM_OPEN` (66)
/// and `ANIM_CLOSE` (67) in the constant table — the reading lands on the
/// two names, which is what makes it a reading.
///
/// **The radius is `payload[0]`**, and the shipped scene graphs prove it: the
/// 175 prox doors carry 5, 6, 8, 10, 14, 15, 16 and 20 there and nothing in
/// the other three slots. The three that carry 0 get **20.0**, which the
/// constructor substitutes at 0x425120 when the argument is exactly zero.
///
/// ponytail: no guard on the opposite animation still running (0x42504c and
/// 0x425076 check it), because a gob here plays one animation at a time and
/// the open/shut state already changes exactly once per crossing. Add the
/// guard when animations have a length.
pub fn prox_doors(lua: &Lua, player: [f64; 3]) -> Result<(), Error> {
    const OBJ_PROXDOOR1: f64 = 700.0;
    const ANIM_OPEN: f64 = 66.0;
    const ANIM_CLOSE: f64 = 67.0;
    /// The radius the constructor substitutes for a payload of zero.
    const REACH: f64 = 20.0;
    let doors: Vec<(String, f64)> = {
        let w = world::world(lua).ok_or_else(|| Error::Pragma("no world".into()))?;
        w.iter()
            .filter(|(_, g)| g.kind == OBJ_PROXDOOR1 && !g.name.is_empty())
            .map(|(_, g)| {
                let d = (0..3)
                    .map(|c| (g.position[c] - player[c]).powi(2))
                    .sum::<f64>()
                    .sqrt();
                (g.name.clone(), d - if g.payload[0] == 0.0 { REACH } else { g.payload[0] })
            })
            .collect()
    };
    let mut boot = boot_mut(lua)?;
    for (name, over) in doors {
        if boot.locked.contains(&name) {
            continue;
        }
        let open = boot.playing.get(&name) == Some(&ANIM_OPEN);
        if over < 0.0 && !open {
            boot.playing.insert(name, ANIM_OPEN);
            boot.doors += 1;
        } else if over >= 0.0 && open {
            boot.playing.insert(name, ANIM_CLOSE);
            boot.doors += 1;
        }
    }
    Ok(())
}

/// **May this shooter fire yet**, and if so, remember that it did.
///
/// `mdkKurt.c` at 0x419eee holds the clock against the item table's own
/// interval — see [`crate::game::world::fire_interval`] for the listing —
/// and refuses the shot when not enough of it has passed. An interval of
/// zero, which is what the uzi and the gatling gun carry, lets every frame
/// through.
///
/// The original keeps the timestamp in `kurt + 0x90` and the interval is
/// per **weapon**, so switching guns does not reset it. Here it is per
/// shooter, which is the same thing while nothing carries two guns.
pub fn may_fire(lua: &Lua, shooter: &str, item: f64, now: f64) -> bool {
    let Some(interval) = crate::game::world::fire_interval(item) else { return true };
    let Ok(mut boot) = boot_mut(lua) else { return true };
    if let Some(&last) = boot.last_shot.get(shooter) {
        if now - last <= interval {
            return false;
        }
    }
    boot.last_shot.insert(shooter.to_string(), now);
    true
}

/// **What the blowers do to the player**, as an acceleration to add this
/// tick. `mdkBlower.c`, and the whole of it is three addresses:
///
/// * **0x402c60** builds a 24-byte block at `gob + 0x40` and switches the
///   blower **on**. `mdkBlowerEnable` (0x402cc0) and `mdkBlowerDisable`
///   (0x402d20) write `block[0]` and play `ANIM_ENABLED` / `ANIM_DISABLED`.
/// * **0x402d80** fills the block for `OBJ_BLOWERCYLINDER`, which is the only
///   blower the game ships — 63 of them and not one of the other two shapes.
///   It writes the scene graph's `payload` straight in: `[8]` the **radius**,
///   `[0xc]` the **length**, `[0x10]` the **strength**, `[0x14]` flags. The
///   63 payloads read `(2.2..30, 10..110, 6..45, 0)` and nothing else fits
///   three columns of those magnitudes — `l4_r4vortexblow` is 30 across and
///   110 long at strength 10, `l4_r8blow00` is 10 by 77 at 6.
/// * **0x40312c** is the cylinder's own test, and **0x403250** the push.
///
/// The volume, in the order the original asks it:
///
/// ```text
/// axis = the blower's local +Z            ; 0x46fa10 off gob + 0x24
/// a = axis . blower;  b = axis . target
/// if b <= a or a + length < b:  outside the slab
/// radial = (target - blower) - axis * (b - a)
/// if radial . radial > radius * radius:   outside the tube
/// ```
///
/// **The axis is +Z and not the +Y a model faces**: 0x46fa10 computes
/// `(2(xz + yw), 2(yz - xw), 1 - 2(x² + y²))`, which is the third column of
/// the rotation — the same column `render::camera::Mat4::rotation` builds,
/// and the two must agree. Fifty-five of the 63 carry a quaternion with no x
/// or y, so they blow **straight up**; the other eight lean.
///
/// And the push, 0x4032ab. The player takes his own branch — 0x4032b5
/// compares the target with the player gob — and it is the plainest law in
/// the game:
///
/// > while his speed **along the axis** has not reached the strength, add
/// > `axis * 40` to his acceleration, signed by the strength.
///
/// 40.0 is the literal at 0x48f33c and it is not the strength: the strength
/// is the **speed it stops at**. Against `body::GRAVITY` of 29.8 a blower
/// pointing up wins by 10.2, which is what an updraft in this game feels
/// like. Everything that is not the player takes 0x403379 instead, where the
/// velocity is set toward the strength rather than accelerated at.
///
/// ponytail: only the vertical part of the acceleration is returned to a
/// caller that can use it — [`crate::game::body::Body`] keeps a `velocity_z`
/// and no horizontal velocity, because its walk is pinned frame for frame
/// against the original's own demo and giving it momentum would move that.
/// The full vector is computed and the horizontal part is the eight leaning
/// blowers' worth of it. Give the body a horizontal velocity, then use it.
pub fn blowers(lua: &Lua, at: [f64; 3], velocity: [f64; 3]) -> [f64; 3] {
    const OBJ_BLOWERCYLINDER: f64 = 500.0;
    /// The acceleration a blower puts on the player, out of 0x48f33c.
    const PUSH: f64 = 40.0;
    let Some(w) = world::world(lua) else { return [0.0; 3] };
    let Ok(boot) = boot_ref(lua) else { return [0.0; 3] };
    let mut out = [0.0; 3];
    for (_, g) in w.iter().filter(|(_, g)| g.kind == OBJ_BLOWERCYLINDER) {
        if boot.blower_off.contains(&g.name) {
            continue;
        }
        let (radius, strength) = (g.payload[0], g.payload[2]);
        let length = boot.blower_length.get(&g.name).copied().unwrap_or(g.payload[1]);
        // the local +Z of the quaternion, in (w, x, y, z) order
        let [qw, qx, qy, qz] = g.rotation;
        let axis = [
            2.0 * (qx * qz + qy * qw),
            2.0 * (qy * qz - qx * qw),
            1.0 - 2.0 * (qx * qx + qy * qy),
        ];
        let dot = |u: [f64; 3], v: [f64; 3]| u[0] * v[0] + u[1] * v[1] + u[2] * v[2];
        let (a, b) = (dot(axis, g.position), dot(axis, at));
        if b <= a || a + length < b {
            continue;
        }
        let radial: f64 = (0..3)
            .map(|c| (at[c] - g.position[c] - axis[c] * (b - a)).powi(2))
            .sum();
        if radial > radius * radius {
            continue;
        }
        // and it stops pushing once he is going that fast along it
        let along = dot(axis, velocity);
        if (strength > 0.0 && along < strength) || (strength < 0.0 && along > strength) {
            let sign = if strength < 0.0 { -1.0 } else { 1.0 };
            for c in 0..3 {
                out[c] += axis[c] * sign * PUSH;
            }
        }
    }
    out
}

/// `DAMAGE_FALLING`, out of the binary's own constant table -- 16, and the
/// type Kurt's landing handler pushes at 0x4183b9.
pub const DAMAGE_FALLING: i64 = 16;

/// Hurt a gob by name, the way the world hurts it: through the same
/// [`deal_damage`] a script's `mdkDealDamage` reaches, so the victim's
/// `OnDamage` runs and its filter is honoured. The source is the victim
/// itself, which is what 0x4183be passes for a fall -- nothing threw it.
pub fn hurt(lua: &Lua, victim: &str, amount: i64, kind: i64) -> bool {
    let Ok(gob) = lua.globals().get::<mlua::Table>(victim) else { return false };
    deal_damage(lua, Some(Value::Table(gob.clone())), Value::Table(gob), amount, kind, -1, true)
        .is_ok()
}

/// The same wrap as [`facing`], to an angle the caller chooses.
fn facing_within(yaw: f64, heading: f64, slack: f64) -> bool {
    let mut d = yaw - heading;
    if d < -std::f64::consts::PI {
        d += std::f64::consts::TAU;
    } else if d > std::f64::consts::PI {
        d -= std::f64::consts::TAU;
    }
    d.abs() < slack
}

/// An animation key of 100 or more **creates an object of that type** at the
/// object playing it (0x42c02e). When the type is one the shot table names,
/// the new object is a projectile.
///
/// Where it goes is the shot table's own business: bit **0x800** of `+0x54`
/// says "at the player", and it is set on almost every enemy shot. Without it
/// the bullet leaves along the shooter's yaw, flat, out of its feet — and
/// the run said exactly what that costs: level 4 fired 45 shots and the
/// nearest passed 2.9 units away with **2.8 of it height**.
/// The shortest a shot's flight time is worked out from — the float at
/// 0x48f37c. A target ten units away is led as though it were fifty.
const MIN_LEAD_RANGE: f64 = 50.0;

fn fire_key_object(lua: &Lua, who: &str, kind: f64) -> Result<(), Error> {
    let Some((at, yaw)) = stance(lua, who) else { return Ok(()) };
    let Some((_, _, _, _, speed, lead, flags)) = crate::game::world::bullet(kind) else {
        return Ok(()); // an effect or a prop, and the engine has nowhere to put it
    };
    let ahead = crate::game::body::facing(yaw).0;
    let mut direction = [ahead[0], ahead[1], 0.0];
    if flags & crate::game::world::AT_PLAYER != 0 {
        let hero = lua
            .named_registry_value::<mlua::Table>("player")
            .ok()
            .and_then(|p| p.get::<String>("name").ok())
            .and_then(|n| stance(lua, &n).map(|(p, _)| p));
        if let Some(to) = hero {
            let mut d = [0, 1, 2].map(|c| to[c] - at[c]);
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            // **And it leads its target**, which is the launch's own
            // arithmetic at 0x4039cd and not the AI's: the flight time is
            // `max(distance, 50) / speed` — the 50 is the float at 0x48f37c
            // and the clamp is skipped only for a shot with flag 0x20000 —
            // and the aim point is `target + velocity * time * record[0x4c]`.
            // That column is **0 for 55 of the 69 shots**, 1.0 for thirteen
            // and 1.4 for one, so most shots do not lead at all.
            //
            // One gate is deliberately missing and is marked rather than
            // guessed: when the target is Kurt the original also calls
            // 0x419060, which reads `kurt + 0x40`'s field at +0x3c, and drops
            // the velocity when that is zero. What that field is has not been
            // read, and defaulting to "lead" is the branch a moving target
            // takes.
            if lead != 0.0 && len > 1e-6 {
                let travel = len.max(MIN_LEAD_RANGE) / speed.max(1e-6);
                let name = lua
                    .named_registry_value::<mlua::Table>("player")
                    .ok()
                    .and_then(|p| p.get::<String>("name").ok());
                let moving = boot_ref(lua)
                    .ok()
                    .and_then(|b| name.and_then(|n| b.velocity.get(&n).copied()));
                if let Some(v) = moving {
                    d = [0, 1, 2].map(|c| d[c] + v[c] * travel * lead);
                }
            }
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            if len > 1e-6 {
                direction = d.map(|c| c / len);
            }
        }
    }
    fire_along(lua, who, kind, at, direction)
}

/// The launch itself, once something has decided where the shot starts and
/// which way it goes: register the gob, hand it the shot table's own damage,
/// lifetime and speed, and count it. `fire_key_object` is this with the
/// animation key's own direction; the sniper scope is this with the camera's.
pub fn fire_along(
    lua: &Lua,
    who: &str,
    kind: f64,
    at: [f64; 3],
    direction: [f64; 3],
) -> Result<(), Error> {
    let Some((_, filter, damage, life, speed, _, _)) = crate::game::world::bullet(kind) else {
        return Ok(());
    };
    let name = format!("{who}_key{}", kind as i64);
    let id = {
        let Some(mut w) = world::world_mut(lua) else { return Ok(()) };
        w.register(Gob { name: name.clone(), kind, position: at, ..Gob::default() })
    };
    world::handle(lua, &name, id, at)?;
    let mut boot = boot_mut(lua)?;
    boot.shots.insert(
        id,
        Shot {
            kind,
            direction,
            speed,
            life: if life < 0.0 { f64::INFINITY } else { life },
            damage,
            filter,
            shooter: Some(who.to_string()),
            target: None,
        },
    );
    boot.fired += 1;
    Ok(())
}

/// A gob's name, from the table the scripts hold it by.
/// `OBJ_DOGANBOY` and the grenade it throws, both out of the tables.
const DOGANBOY: f64 = 207.0;
/// `OBJ_INVISOGRUNT`, which 0x432b77 sends straight to the fight rather than
/// letting it choose to close.
const INVISOGRUNT: f64 = 219.0;
/// `OBJ_GRUNT`, which taunts out of the three rather than the two.
const GRUNT: f64 = 202.0;
/// `OBJ_CONEHEAD`, the one type that runs when it is scared rather than
/// playing `ANIM_SCARED` — 0x432a49 compares the def's type with 0xcb.
const CONEHEAD: f64 = 203.0;
const DBGRENADE: f64 = 417.0;

/// `ANIM_SHOOT` and `ANIM_THROW`. 0x4331f8 plays the first for every round of
/// a burst and 0x433185 the second for a grenade; the projectile in both
/// cases comes off the animation's key channel.
const ANIM_SHOOT: f64 = 56.0;
const ANIM_THROW: f64 = 57.0;

/// **State 11, which is a retreat and not a charge.** 0x433791 puts the
/// destination at `self + normalize(self - target) * 10` — ten units the
/// *other* way, `[0x5d2758]` being `a -= b` and `[0x5d271c]` `a += b` — and
/// hands it to the goto core with run set and `mustFace` clear, so the walker
/// turns its back and runs. It rebuilds that destination on every call, so
/// the ten units are always ten units ahead of wherever it has got to, and
/// what ends the state is the cooldown: **`chRand() * 3 + 2` seconds**, from
/// 0x432b16 with the 3.0 at 0x48f2f0 and the 2.0 at 0x48f598.
///
/// Three branches reach it and all three are a flinch: a walker limping below
/// `def + 0x40` that rolls under `act`, a **scared conehead** on a coin toss
/// (0x432a4f tests the type for 0xcb), and anything inside the record's
/// `near` that rolls under `act` and then under `scared`.
///
/// A destination ten units ahead of something already running is the same
/// thing as a heading, so the engine keeps the heading and holds the state
/// with the cooldown and [`Boot::fleeing`].
///
/// Not built: 0x433826 gives the retreat up early when the target has got
/// further away than three times the record's `reach`, and goes home.
fn retreat(boot: &mut Boot, who: &str, at: [f64; 3], from: [f64; 3]) {
    /// How long it runs for: `chRand() * 3 + 2`.
    const HELD: (f64, f64) = (3.0, 2.0);
    let away = crate::game::body::bearing(at[0] - from[0], at[1] - from[1]);
    boot.heading.insert(who.to_string(), away);
    boot.avoiding.insert(who.to_string());
    boot.gait.insert(who.to_string(), 2);
    let held = boot.random.next() * HELD.0 + HELD.1;
    boot.cooldown.insert(who.to_string(), held);
    boot.fleeing.insert(who.to_string());
}

/// How often a walker may look at its path, from the float at 0x48fa20.
const PROBE_EVERY: f64 = 0.4;

/// How near a heading counts as facing it, from the double at 0x490198.
/// Three walker functions share the constant and the angle-wrap idiom around
/// it — 0x4318a0, 0x431b80 and 0x431f70, all with 0x48f618 PI, 0x48f61c -PI
/// and 0x48f5a0 2*PI.
const FACING: f64 = 0.17;

/// And how near it has to come back before it stops turning, from the
/// **double** at 0x4901b8 — read as a float it looks like 2.0, and the
/// instruction that reads it is `fcomp QWORD`, not `fcomp DWORD`. `walker +
/// 0x2c` remembers which of the two thresholds applies.
const SQUARE: f64 = 0.02;

fn facing(yaw: f64, heading: f64) -> bool {
    let mut d = yaw - heading;
    if d < -std::f64::consts::PI {
        d += std::f64::consts::TAU;
    } else if d > std::f64::consts::PI {
        d -= std::f64::consts::TAU;
    }
    d.abs() < FACING
}

/// Where a gob is and which way it looks: its position and the yaw out of its
/// quaternion, which is what `0x46faa0` hands back off `gob + 0x24`.
fn stance(lua: &Lua, name: &str) -> Option<([f64; 3], f64)> {
    let w = world::world(lua)?;
    let g = w.find(name).and_then(|id| w.get(id))?;
    let q = g.rotation;
    Some((
        g.position,
        (2.0 * (q[0] * q[3] + q[1] * q[2])).atan2(1.0 - 2.0 * (q[2] * q[2] + q[3] * q[3])),
    ))
}

/// A named waypoint out of the `points` table the scene graph fills.
fn point_at(lua: &Lua, v: Option<&Value>) -> Option<[f64; 3]> {
    let p: mlua::Table = lua
        .globals()
        .get::<mlua::Table>("points")
        .ok()?
        .get(v.and_then(text)?)
        .ok()?;
    Some([
        p.get::<f64>("x").unwrap_or(0.0),
        p.get::<f64>("y").unwrap_or(0.0),
        p.get::<f64>("z").unwrap_or(0.0),
    ])
}

fn gob_name(v: &Value) -> Option<String> {
    match v {
        Value::Table(t) => t.get::<String>("name").ok(),
        other => text(other),
    }
}

/// A position from a gob or a waypoint.
///
/// A gob's position is the **sub-table** `gob.position`, not flat fields: 26
/// places read it that way. `gob.x` exists too, in 82 places, but that is the
/// scripts' own state — the minigame ship integrates `gob.x = gob.x + gob.vx
/// * dt` — so the driver must not squat on it.
fn position(v: Option<&Value>) -> Option<[f64; 3]> {
    let t = match v {
        Some(Value::Table(t)) => t,
        _ => return None,
    };
    let inner = t.get::<Option<mlua::Table>>("position").ok().flatten();
    let t = inner.as_ref().unwrap_or(t);
    Some([
        t.get::<f64>("x").unwrap_or(0.0),
        t.get::<f64>("y").unwrap_or(0.0),
        t.get::<f64>("z").unwrap_or(0.0),
    ])
}

/// Far away when either end is not a thing with a position, so that a
/// proximity test on a missing object reads as "not near" rather than "here".
fn distance(a: Option<&Value>, b: Option<&Value>) -> f64 {
    match (position(a), position(b)) {
        (Some(a), Some(b)) => (0..3).map(|c| (a[c] - b[c]).powi(2)).sum::<f64>().sqrt(),
        _ => 1e9,
    }
}

/// Write a position into both halves: the arena, which is the truth, and the
/// Lua table, which is what the scripts read back.
fn place(lua: &Lua, gob: &mlua::Table, at: [f64; 3]) -> mlua::Result<()> {
    if let Some(id) = world::id_of(gob) {
        if let Some(mut w) = lua.app_data_mut::<world::World>() {
            w.set_position(id, at);
        }
    }
    let position = lua.create_table()?;
    position.set("x", at[0])?;
    position.set("y", at[1])?;
    position.set("z", at[2])?;
    gob.set("position", position)
}

fn boot_mut(lua: &Lua) -> mlua::Result<mlua::AppDataRefMut<'_, Boot>> {
    lua.app_data_mut::<Boot>()
        .ok_or_else(|| mlua::Error::runtime("no boot state"))
}

fn boot_ref(lua: &Lua) -> mlua::Result<mlua::AppDataRef<'_, Boot>> {
    lua.app_data_ref::<Boot>()
        .ok_or_else(|| mlua::Error::runtime("no boot state"))
}

/// Fire every handler the level script hung on an object global.
///
/// Calling one out of context is a **survey of the surface, not a
/// simulation**: what it is for is the engine functions the handlers reach
/// for, and the ones that fail name the state the engine still has to hold.
/// `tools/boot.py --events` does the same and is the reference.
///
/// In **name order**, and each object's slots in name order too: `pairs` is
/// hash order, and a handler that installs another object's method would
/// otherwise make the result depend on it.
///
/// -> `(fired, ran to the end, why the rest stopped)`. The reasons are the
/// point: each one names state the engine does not hold yet.
pub fn fire_events(
    scripts: &Scripts,
) -> Result<(usize, usize, BTreeMap<String, usize>), Error> {
    let globals = scripts.lua.globals();
    let mut names: Vec<String> = Vec::new();
    for pair in globals.pairs::<String, Value>() {
        let (name, value) = pair?;
        if let Value::Table(t) = &value {
            if t.contains_key("__gob").unwrap_or(false) {
                names.push(name);
            }
        }
    }
    names.sort();

    let (mut fired, mut survived) = (0usize, 0usize);
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    for name in names {
        let Ok(gob) = globals.get::<mlua::Table>(name.as_str()) else { continue };
        let mut slots: Vec<String> = Vec::new();
        for pair in gob.clone().pairs::<Value, Value>() {
            let (slot, value) = pair?;
            if let (Value::String(s), Value::Function(_)) = (&slot, &value) {
                let s = s.to_string_lossy().to_string();
                // `OnCreate` is excluded because it is **not** a slot waiting
                // to be probed: [`create`] has already fired it for real, and
                // the original's 0x1000000 bit exists precisely so that no
                // object is created twice. Firing it here would be a second
                // creation, which the game does not have.
                if s.starts_with("On") && s != "OnCreate" {
                    slots.push(s);
                }
            }
        }
        slots.sort();
        for slot in slots {
            let Ok(handler) = gob.get::<mlua::Function>(slot.as_str()) else { continue };
            fired += 1;
            // the arguments the handlers between them expect: the object
            // twice, a number, a damage kind, and another number
            match handler.call::<Value>((gob.clone(), gob.clone(), 1, DAMAGE_GOODGUY, 1)) {
                Ok(_) => survived += 1,
                Err(e) => {
                    // the message without its position, so the same fault in
                    // twenty scripts counts as one kind
                    let text = e.to_string();
                    let line = text.lines().next().unwrap_or("").to_string();
                    let kind = line
                        .rfind(": ")
                        .map(|i| line[i + 2..].to_string())
                        .unwrap_or(line);
                    *reasons.entry(kind).or_insert(0) += 1;
                }
            }
        }
    }
    Ok((fired, survived, reasons))
}

/// Start a level, the way the game does.
///
/// `levelchanged` and `sectionchanged` are set because the engine sets them;
/// without them `doloadingscreen` takes neither branch and half of starting a
/// level is skipped in silence.
pub fn level(scripts: &Scripts, number: u32, checkpoint: u32, section: &str) -> Result<(), Error> {
    scripts
        .lua
        .load(&format!(
            "levelchanged, sectionchanged = 1, 1\nlevel({number}, {checkpoint}, \"{section}\")"
        ))
        .set_name("level")
        .exec()?;
    stream(&scripts.lua, checkpoint as f64)?;
    create(scripts)?;
    Ok(())
}

/// `OnCreate(gob)` over everything the level made, which is a step of its own
/// and not something a script does.
///
/// 0x42e170 walks the whole object tree once at the end of the load sequence
/// (0x4012e0 calls it, and `mdk2.lua`'s own call graph names the step: "setup
/// scripts for existing objects"). The handler cannot fire when the object is
/// built, because a level script assigns its handlers *after* the scene graph
/// has run — so the sweep comes last, when every `gob.OnCreate = function` has
/// been seen.
///
/// **Once per object, ever.** 0x42e3e7 tests bit 0x1000000 in `omgob[0xb4]`
/// and sets it before firing, so nothing gets a second `OnCreate` — which is
/// what makes this safe to call from a driver that may reload a level. The
/// call takes **one** argument, unlike `OnDamage`'s five.
///
/// This is what fills the spawner queues: 396 of the 682 spawners a boot sets
/// up are queued by an `OnCreate` and by nothing else.
pub fn create(scripts: &Scripts) -> Result<(), Error> {
    let globals = scripts.lua.globals();
    let fresh: Vec<String> = {
        let w = world::world(&scripts.lua).ok_or_else(|| Error::Pragma("no world".into()))?;
        w.iter().filter(|(_, g)| !g.created).map(|(_, g)| g.name.clone()).collect()
    };
    // **A scene graph's trailing flag of 1 starts the object frozen**, and
    // this is where it is applied -- before any `OnCreate`, because an
    // `OnCreate` may be the thing that thaws it. It is a reading of the data
    // and it is checked by what it removes: level 1 registers `kurtgame`
    // with a 1 and hangs fifteen children off it at 0, and without this
    // `ktgame_kurt.OnUpdate` ran from the moment the level loaded and read
    // `kurtgame.left`, which `Level.StartMinigame` had not written --
    // `level1.lua:1811`, 2700 failures in a ninety-second run at every one
    // of the level's checkpoints.
    let frozen_by_flag: Vec<String> = {
        let w = world::world(&scripts.lua).ok_or_else(|| Error::Pragma("no world".into()))?;
        w.iter()
            .filter(|(_, g)| g.flag == 1.0 && !g.name.is_empty() && !g.created)
            .map(|(_, g)| g.name.clone())
            .collect()
    };
    if !frozen_by_flag.is_empty() {
        let mut boot = boot_mut(&scripts.lua)?;
        boot.stasis.extend(frozen_by_flag);
    }
    for name in fresh {
        {
            let mut w =
                world::world_mut(&scripts.lua).ok_or_else(|| Error::Pragma("no world".into()))?;
            let Some(id) = w.find(&name) else { continue };
            match w.get_mut(id) {
                Some(g) if !g.created => g.created = true,
                _ => continue,
            }
        }
        if let Ok(gob) = globals.get::<mlua::Table>(name.as_str()) {
            if let Ok(handler) = gob.get::<mlua::Function>("OnCreate") {
                let _ = handler.call::<Value>(gob);
            }
        }
    }
    Ok(())
}

/// Where the driver is in time and space between ticks.
#[derive(Default)]
pub struct Ticking {
    pub clock: f64,
    /// The room the player was in last tick, so a change is an entry.
    pub room: Option<usize>,
    pub rooms_entered: usize,
    /// `slot -> (fired, ran to the end)`.
    pub fired: BTreeMap<String, (usize, usize)>,
    /// The objects the body was against last tick, so entering and leaving
    /// are both events.
    pub touching: BTreeSet<String>,
    pub collisions: usize,
    /// Every object the body was ever against. Reported beside the collision
    /// count because the two answer different questions: whether the body
    /// meets anything, and whether what it meets is scripted.
    pub touched: BTreeSet<String>,
    /// Why handlers stopped, by message without its position — the same
    /// grouping `fire_events` uses, so a run's failures can be read the way
    /// a boot's are.
    pub why: BTreeMap<String, usize>,
}

impl Ticking {
    /// `OnCollision(gob, target, part)`, with the arguments the scripts
    /// name: what was hit, **what hit it** — and `nil` there is how they
    /// spell "the collision ended", which six of the 67 handlers test for —
    /// and which model part took it.
    ///
    /// **`part` is always -1 and that is a stated limit, not an oversight.**
    /// The collision world is one BSP per object, not one per model node, so
    /// there is nothing to resolve a part from. 46 of the 67 handlers never
    /// read it; the 21 that do compare it against
    /// `omGobGMGetSltIndexByName`, which never returns -1 for a real slot,
    /// so they take the branch they take today — the one where nothing
    /// happened.
    fn collide(&mut self, gob: &mlua::Table, target: Option<mlua::Table>) {
        let Ok(handler) = gob.get::<mlua::Function>("OnCollision") else { return };
        let entry = self.fired.entry("OnCollision".to_string()).or_insert((0, 0));
        entry.0 += 1;
        self.collisions += 1;
        let hit: Value = target.map(Value::Table).unwrap_or(Value::Nil);
        if handler.call::<Value>((gob.clone(), hit, -1i64)).is_ok() {
            entry.1 += 1;
        }
    }

    fn call(&mut self, gob: &mlua::Table, slot: &str) {
        let Ok(handler) = gob.get::<mlua::Function>(slot) else { return };
        let entry = self.fired.entry(slot.to_string()).or_insert((0, 0));
        entry.0 += 1;
        match handler.call::<Value>((gob.clone(), gob.clone(), 1, DAMAGE_GOODGUY, 1)) {
            Ok(_) => entry.1 += 1,
            // **With** its position, unlike a boot's grouping. A boot fires
            // every handler once and twenty scripts share one fault, so the
            // position is noise there; a run fires the same few handlers
            // every tick, so a count of 900 means *one* handler failing 900
            // times and the line number is the whole answer.
            Err(e) => {
                let text = e.to_string();
                let line = text.lines().next().unwrap_or("").trim().to_string();
                *self.why.entry(line).or_insert(0) += 1;
            }
        }
    }

    pub fn total(&self) -> (usize, usize) {
        self.fired.values().fold((0, 0), |(f, s), (a, b)| (f + a, s + b))
    }
}
/// Is this gob frozen — itself, or by something it hangs off?
///
/// **Stasis is inherited.** A scene graph's trailing flag is 1 for an object
/// that starts frozen, and level 1 registers `kurtgame` that way with
/// **fifteen children at flag 0** hanging off it. Freezing only the parent
/// left `ktgame_kurt.OnUpdate` running from the moment the level loaded, and
/// it reads `kurtgame.left`, which `Level.StartMinigame` has not written
/// yet: `level1.lua:1811, attempt to compare number with nil`, **2700 times
/// in a ninety-second run, at every one of level 1's checkpoints**. That is
/// six per cent of every handler call the level makes.
///
/// The rule is the tree's, not a special case. `mdkDestroyRoom` already
/// destroys a room *and everything parented to it*, because a room's
/// contents are its children; the update sweep walks the same tree, and
/// 0x46d505 skips a frozen object's **whole** update.
fn frozen(w: &crate::game::world::World,
          stasis: &std::collections::BTreeSet<String>, id: crate::game::world::Id) -> bool {
    let mut at = Some(id);
    // a graph that pointed at itself would otherwise hang the tick, and the
    // scene graphs are data
    for _ in 0..64 {
        let Some(i) = at else { return false };
        let Some(g) = w.get(i) else { return false };
        if !g.name.is_empty() && stasis.contains(&g.name) {
            return true;
        }
        at = g.parent;
    }
    false
}


/// One tick of the driver, with the player at `at`.
///
/// The **order** is what this models faithfully, and it is the order
/// `tools/boot.py --play` established by reading the scripts: `OnCreate` has
/// already run at boot, **`OnEnterRoom` fires when the room under the player
/// changes**, timers fire when they come due, and **`OnUpdate` goes only to
/// gobs that are not in stasis** — which is how a level holds its encounters
/// until the player arrives.
pub fn tick(
    scripts: &Scripts,
    rooms: &Visibility,
    at: [f64; 3],
    facing: f64,
    dt: f64,
    state: &mut Ticking,
) -> Result<(), Error> {
    tick_touching(scripts, rooms, at, facing, dt, state, &Default::default())
}

/// The same tick, told what the body is against. `touching` is object names,
/// which is what `crate::game::body::Collision::owner` hands back.
pub fn tick_touching(
    scripts: &Scripts,
    rooms: &Visibility,
    at: [f64; 3],
    facing: f64,
    dt: f64,
    state: &mut Ticking,
    touching: &BTreeSet<String>,
) -> Result<(), Error> {
    state.clock += dt;
    let globals = scripts.lua.globals();

    // **The player's own object has to move with the body.** Everything this
    // game triggers, it triggers by proximity: `elevators.lua` opens a door
    // with `mdkGobDistance(door, mdkGetPlayerGob())`, and the scripts call
    // `mdkGetPlayerGob` 320 times and `mdkGobDistance` 143. Leave the player
    // gob at the checkpoint and every one of those measures a distance that
    // never changes, so nothing in a level ever fires.
    if let Ok(player) = scripts.lua.named_registry_value::<mlua::Table>("player") {
        // the body's position is its **eye**; a model stands on its feet, so
        // the object goes an eye-height lower
        let feet = [at[0], at[1], at[2] - crate::game::body::EYE];
        // and how fast it is going, which is what the AI's aim leads by. The
        // arena keeps no velocity, so it is differenced here, before the warp.
        if let Ok(name) = player.get::<String>("name") {
            let was = world::world(&scripts.lua)
                .and_then(|w| w.find(&name).and_then(|i| w.get(i)).map(|g| g.position));
            if let Some(was) = was.filter(|_| dt > 0.0) {
                let v = [0, 1, 2].map(|c| (feet[c] - was[c]) / dt);
                boot_mut(&scripts.lua)?.velocity.insert(name, v);
            }
        }
        place(&scripts.lua, &player, feet)?;
        // and it faces where the body faces: a yaw about Z, in the (w,x,y,z)
        // order everything here stores a quaternion in
        if let (Some(id), Some(mut w)) = (
            world::id_of(&player),
            scripts.lua.app_data_mut::<world::World>(),
        ) {
            let half = facing / 2.0;
            w.set_rotation(id, [half.cos(), 0.0, 0.0, half.sin()]);
        }
    }

    // the room under the player, and an entry when it changes
    let here = rooms.at(at).first().copied();
    if here != state.room {
        state.room = here;
        boot_mut(&scripts.lua)?.room = here.map(|i| rooms.names[i].clone());
        if let Some(i) = here {
            state.rooms_entered += 1;
            if let Ok(gob) = globals.get::<mlua::Table>(rooms.names[i].as_str()) {
                state.call(&gob, "OnEnterRoom");
            }
        }
    }

    // what the body is against, and both edges of it: a name that has just
    // appeared is a collision, and one that has just gone is the same
    // handler called with nil, which is how the scripts spell the end of one
    if *touching != state.touching {
        let player = scripts.lua.named_registry_value::<mlua::Table>("player").ok();
        let began: Vec<String> = touching.difference(&state.touching).cloned().collect();
        let ended: Vec<String> = state.touching.difference(touching).cloned().collect();
        for name in began {
            state.touched.insert(name.clone());
            if let Ok(gob) = globals.get::<mlua::Table>(name.as_str()) {
                state.collide(&gob, player.clone());
            }
        }
        for name in ended {
            if let Ok(gob) = globals.get::<mlua::Table>(name.as_str()) {
                state.collide(&gob, None);
            }
        }
        state.touching = touching.clone();
    }

    // timers that have come due, taken out of the queue first so a handler
    // that sets a new one does not fire it in the same tick
    let due: Vec<String> = {
        let boot = boot_ref(&scripts.lua)?;
        boot.timers
            .iter()
            .filter(|(_, &when)| when <= state.clock)
            .map(|(n, _)| n.clone())
            .collect()
    };
    for name in &due {
        boot_mut(&scripts.lua)?.timers.remove(name);
    }
    for name in due {
        if let Ok(gob) = globals.get::<mlua::Table>(name.as_str()) {
            state.call(&gob, "OnTimer");
        }
    }

    // the doors, which open on the player being near and on nothing else
    prox_doors(&scripts.lua, [at[0], at[1], at[2] - crate::game::body::EYE])?;

    // the spawners, which are the only thing that puts an enemy in a room.
    //
    // The countdown runs **only while something is owed** — the original
    // returns before touching it when the queue is empty (0x425e43) — so an
    // idle spawner does not accumulate credit and then empty its whole queue
    // at once when a script fills it.
    let due: Vec<String> = {
        let mut boot = boot_mut(&scripts.lua)?;
        let mut ready = Vec::new();
        for (name, s) in boot.spawners.iter_mut() {
            if s.queue <= 0 {
                continue;
            }
            s.timer -= dt;
            if s.timer <= 0.0 {
                s.queue -= 1;
                s.timer = s.interval;
                ready.push(name.clone());
            }
        }
        ready
    };
    for name in due {
        spawn(&scripts.lua, &name)?;
    }

    // and OnUpdate, to everything awake.
    //
    // The names come from the **arena**, not from a walk of `_G`. Both hold
    // the same set -- registering an object does `_G[name] = gob` -- but the
    // arena is already a list, and walking every global with a table check
    // and a sort, sixty times a second, was the single largest cost in the
    // loop.
    let awake: Vec<String> = {
        let boot = boot_ref(&scripts.lua)?;
        let w = crate::game::world::world(&scripts.lua)
            .ok_or_else(|| Error::Pragma("no world".into()))?;
        let mut names: Vec<String> = w
            .iter()
            .filter(|(id, _)| !frozen(&w, &boot.stasis, *id))
            .map(|(_, g)| g.name.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    };
    for name in awake {
        if let Ok(gob) = globals.get::<mlua::Table>(name.as_str()) {
            state.call(&gob, "OnUpdate");
        }
    }

    // the walkers in the air. `mdkWalkerJumpToPoint` only *aims* — the
    // original hands the walker a rise, a ground speed and a bearing and the
    // physics flies it — so this is that arc, sampled at the tick rate, and
    // the numbers are the launch's rather than anything chosen here.
    //
    // ponytail: nothing is swept along the way, so a jump through a wall
    // still arrives; give it `Collision` when non-player gobs have a body.
    let hops: Vec<(String, [f64; 3])> = {
        let mut boot = boot_mut(&scripts.lua)?;
        let mut out = Vec::new();
        boot.jumps.retain(|name, j| {
            j.elapsed += dt;
            out.push((name.clone(), j.at(j.elapsed)));
            j.elapsed < j.arc.time
        });
        out
    };
    for (name, at) in hops {
        if let Ok(gob) = globals.get::<mlua::Table>(name.as_str()) {
            place(&scripts.lua, &gob, at)?;
        }
    }

    // and the walkers, which turn toward what they were told to face and
    // then move along their own nose. Both halves are the original's:
    //
    // 0x42fbbb, inside `mdkWalkerAnimUpdate`, is the turn. It is the only
    // place in the binary that closes the loop from `walker + 0x14` back onto
    // the gob — every other `fsub [reg+0x14]` in the walker files is the
    // *test*, never the turn — and it has **hysteresis, not a single
    // threshold**: a walker starts turning when it is more than **0.17 rad**
    // off (0x490198) and keeps turning until it is inside **0.01** (the
    // double at 0x48f620), with `walker + 0x2c` remembering which of the two
    // it is in. The step is `dt * def[0x28]`, clamped both ways.
    //
    // 0x42fd0d is the move: the speed is `def[0x18 + gait * 4]` and it is
    // spent along the **gob's own facing**, not along the heading, which is
    // why the turn has to come first and why a walker corners in an arc.
    //
    // ponytail: the collision is the player's `Body`, not the original's
    // sweep. 0x46de70 takes the velocity and slides it along the surface
    // normal at `omgob + 0x30`; this probes a column and slides along the two
    // axes, which is the same idea a rung down.
    //
    // A bare point refusal was tried first and taken out, and what it found
    // is why `Body` had to grow an escape. Exactly **one** walker in a
    // thirty-second run of level 7 is ever refused — `l7r2_spn1_spawn`, at
    // (-346.7, -140.5, 29.3), inside the tree owned by `c9` — and that one is
    // enough to stall the level: it is a spawner's grunt at the head of a
    // sequence, it is *already* inside the geometry when it appears, so all
    // three slide candidates are refused too, and every sequence behind it
    // waits. A body that is already buried now moves anyway.
    // **The path probe, which is what keeps a walker out of a wall and off a
    // ledge.** 0x431490, throttled on `walker + 0xb4` to the **0.4 seconds**
    // at 0x48fa20: a ray from the gob along its heading for `def + 0x80` —
    // 4 to 15 units by type, and the last unread column of the walker record
    // — with both ends **raised by 1** (0x48f2f4). If that hits, the path is
    // blocked. If it does not and `walker + 0x5c` is set, a second ray goes
    // **five down** (0x48f7d8) from the far end and hitting nothing is a
    // cliff, which counts the same.
    //
    // The goto core reads it only when `avoid` is set and no shipped script
    // sets it; what does is the AI, in states 1, 5 and 11 — see
    // [`Boot::avoiding`]. When it answers, 0x431d03 puts the gait to **0**
    // and 0x431d1d turns the heading by `walker[0x94] * PI/2`, a right angle
    // to the walker's own side, and waits `chRand() * 3 + 1` to look again.
    {
        /// Both ends of the look-ahead ray, off the ground.
        const OFF_THE_FLOOR: f64 = 1.0;
        /// How far down a cliff check looks before it counts as a drop.
        const CLIFF: f64 = 5.0;
        /// The turn a blocked walker makes, `walker[0x94]` times this.
        const AWAY: f64 = std::f64::consts::FRAC_PI_2;
        /// And how long it walks before looking again: `chRand() * 3 + 1`.
        const AGAIN: (f64, f64) = (3.0, 1.0);
        let solid = scripts
            .lua
            .app_data_ref::<std::rc::Rc<crate::game::body::Collision>>()
            .map(|c| c.clone());
        if let Some(solid) = solid {
            let due: Vec<(String, [f64; 3], f64, f64, bool)> = {
                let boot = boot_ref(&scripts.lua)?;
                let Some(w) = crate::game::world::world(&scripts.lua) else {
                    return Err(Error::Pragma("no world".into()));
                };
                boot.avoiding
                    .iter()
                    .filter(|n| boot.gait.get(*n).is_some_and(|&g| g > 0))
                    .filter(|n| boot.probe_at.get(*n).is_none_or(|&t| state.clock >= t))
                    .filter_map(|n| {
                        let g = w.get(w.find(n)?)?;
                        let reach = crate::game::world::look_ahead(g.kind)?;
                        let heading = *boot.heading.get(n)?;
                        Some((n.clone(), g.position, heading, reach, boot.cliffs.contains(n)))
                    })
                    .collect()
            };
            for (name, at, heading, reach, cliff) in due {
                let ahead = crate::game::body::facing(heading).0;
                let from = [at[0], at[1], at[2] + OFF_THE_FLOOR];
                let to = [
                    from[0] + ahead[0] * reach,
                    from[1] + ahead[1] * reach,
                    from[2],
                ];
                let wall = !solid.sees(from, to);
                let drop = cliff && solid.sees(to, [to[0], to[1], to[2] - CLIFF]);
                if !(wall || drop) {
                    let mut boot = boot_mut(&scripts.lua)?;
                    boot.probe_at.insert(name, state.clock + PROBE_EVERY);
                    continue;
                }
                let mut boot = boot_mut(&scripts.lua)?;
                let side = match boot.side.get(&name) {
                    Some(&s) => s,
                    None => {
                        let s = if boot.random.next() < 0.5 { 1.0 } else { -1.0 };
                        boot.side.insert(name.clone(), s);
                        s
                    }
                };
                boot.gait.insert(name.clone(), 0);
                if let Some(h) = boot.heading.get_mut(&name) {
                    *h += side * AWAY;
                }
                let wait = boot.random.next() * AGAIN.0 + AGAIN.1;
                boot.probe_at.insert(name, state.clock + wait);
            }
        }
    }

    let steps: Vec<(world::Id, String, [f64; 3], f64, f64, f64, f64, bool, Option<(f64, f64)>)> = {
        let boot = boot_ref(&scripts.lua)?;
        let Some(w) = crate::game::world::world(&scripts.lua) else {
            return Err(Error::Pragma("no world".into()));
        };
        boot.gait
            .iter()
            .filter(|(name, _)| !boot.jumps.contains_key(*name))
            .filter(|(name, _)| w.find(name).is_none_or(|id| !frozen(&w, &boot.stasis, id)))
            .filter_map(|(name, &gait)| {
                let id = w.find(name)?;
                let g = w.get(id)?;
                let want = *boot.heading.get(name)?;
                let (mut speed, turn) = crate::game::world::locomotion(g.kind, gait)?;
                if crate::game::world::limping(g.kind, g.hitpoints) {
                    speed *= 0.5; // 0x42fd4c, the 0.5 at 0x48f2fc
                }
                let q = g.rotation;
                let yaw = (2.0 * (q[0] * q[3] + q[1] * q[2]))
                    .atan2(1.0 - 2.0 * (q[2] * q[2] + q[3] * q[3]));
                let mut d = want - yaw;
                if d < -std::f64::consts::PI {
                    d += std::f64::consts::TAU;
                } else if d > std::f64::consts::PI {
                    d -= std::f64::consts::TAU;
                }
                // both halves of the hysteresis, and the inner one matters
                // more than it looks: stopping at 0.17 rad leaves five units
                // of lateral error at thirty out, so an enemy that fired
                // would miss the player every time.
                let turning = if boot.turning.contains(name) {
                    d.abs() > SQUARE
                } else {
                    d.abs() > FACING
                };
                // **and a flier is in the list even when it is still**,
                // because holding an altitude is a move: a hovering birdbrain
                // has gait 0 and a target height above it.
                // **and it flies because of what it is, not what it is
                // doing**: the flag is on the record (`def + 0x14` bit 2), so
                // a birdbrain holds its height from the frame it is made,
                // long before its AI has picked one. Without that a spawned
                // one fell out of level 8 while it was still deciding.
                let fly = crate::game::world::climb(g.kind)
                    .map(|c| (boot.altitude.get(name).copied().unwrap_or(g.position[2]), c));
                if !turning && speed == 0.0 && fly.is_none() {
                    return None; // standing still and already square
                }
                let step = (dt * turn).min(d.abs()) * d.signum();
                let yaw = if turning { yaw + step } else { yaw };
                // `def + 0x78`, and the player's height for a type the table
                // does not name — a spawner's own gob, say
                let (tall, wide) = crate::game::world::size(g.kind)
                    .unwrap_or((crate::game::body::EYE, 0.0));
                Some((id, name.clone(), g.position, yaw, speed, tall, wide, turning, fly))
            })
            .collect()
    };
    // and the body that carries it. A walker gets the **same** `Body` the
    // player has — the column probe, the axis slide, the step-up and the
    // gravity — because a second mover would be a second set of bugs.
    //
    // Its height and width are its type's own — `def + 0x78` and `+0x7c`,
    // which the constructor puts in the collision block at `gob + 0x68`.
    let solid = scripts
        .lua
        .app_data_ref::<std::rc::Rc<crate::game::body::Collision>>()
        .map(|c| c.clone())
        .filter(|c| !c.is_empty());
    for (id, name, from, yaw, speed, tall, wide, turning, fly) in steps {
        let at = match &solid {
            Some(world) => {
                let mut boot = boot_mut(&scripts.lua)?;
                let body = boot
                    .bodies
                    .entry(name.clone())
                    .or_insert_with(|| crate::game::body::Body::shaped([0.0; 3], yaw, tall, wide));
                // the arena keeps a gob's feet and a body its head
                body.position = [from[0], from[1], from[2] + tall];
                body.yaw = yaw;
                // and a flier chases its altitude at the record's own climb
                body.flying = fly.is_some();
                if let Some((want, climb)) = fly {
                    body.velocity_z = (want - from[2]).clamp(-climb, climb);
                }
                body.step(world, crate::game::body::facing(yaw).0, false, speed, dt);
                let p = body.position;
                [p[0], p[1], p[2] - tall]
            }
            // no level loaded: a test, and there is nothing to walk into
            None => [
                from[0] + crate::game::body::facing(yaw).0[0] * speed * dt,
                from[1] + crate::game::body::facing(yaw).0[1] * speed * dt,
                from[2],
            ],
        };
        {
            let mut boot = boot_mut(&scripts.lua)?;
            if turning {
                boot.turning.insert(name.clone());
            } else {
                boot.turning.remove(&name);
            }
        }
        if let Ok(gob) = globals.get::<mlua::Table>(name.as_str()) {
            place(&scripts.lua, &gob, at)?;
        }
        if let Some(mut w) = scripts.lua.app_data_mut::<world::World>() {
            let half = yaw / 2.0;
            w.set_rotation(id, [half.cos(), 0.0, 0.0, half.sin()]);
        }
    }

    // and the legs. `mdkWalkerAnimUpdate` plays the gait's own animation every
    // frame, and 0x461670 sorts out what may interrupt what.
    //
    // ponytail: the engine has no animation priorities, so it plays the gait
    // animation only when the walker is **moving**, or when what is up is
    // already a gait animation. That keeps an attack pose from being wiped the
    // frame after the AI struck it, at the cost of a walker that stands still
    // holding the last pose it was given — which is what it looks like anyway.
    let poses: Vec<(String, f64)> = {
        let boot = boot_ref(&scripts.lua)?;
        let Some(w) = crate::game::world::world(&scripts.lua) else {
            return Err(Error::Pragma("no world".into()));
        };
        boot.gait
            .iter()
            .filter(|(name, _)| !boot.jumps.contains_key(*name))
            .filter(|(name, _)| w.find(name).is_none_or(|id| !frozen(&w, &boot.stasis, id)))
            .filter_map(|(name, &gait)| {
                let g = w.get(w.find(name)?)?;
                let want = crate::game::world::gait_animation(g.kind, gait, g.hitpoints)?;
                let now = boot.playing.get(name).copied();
                // **or what is up has played itself out.** An attack pose is
                // held so the frame after the AI struck it does not wipe it,
                // and before the clock wrapped that hold was for ever: a
                // standing walker kept `ANIM_SHOOT` up and, once animations
                // looped, fired its key again every pass. It holds for one
                // loop now, which is what an animation priority buys in the
                // original, and then the legs take it back.
                let idle = boot.looped.contains_key(name)
                    || now.is_none_or(|a| {
                        crate::game::world::GAIT_ANIM.contains(&a)
                            || crate::game::world::GAIT_ANIM_HURT.contains(&a)
                    });
                (now != Some(want) && (gait != 0 || idle)).then(|| (name.clone(), want))
            })
            .collect()
    };
    for (name, id) in poses {
        let mut boot = boot_mut(&scripts.lua)?;
        boot.playing.insert(name.clone(), id);
        boot.since.insert(name, 0.0);
    }

    // the shots. A bullet travels its type's speed along the direction it was
    // launched with, and it ends one of three ways: it runs out of life
    // (`life > 0` gates the countdown, 0x403d94), it reaches something it can
    // hurt, or the level ends with it still going.
    //
    // ponytail: no geometry. `Collision::sees` is exactly the query — the
    // segment against the trees — but the walker's lesson applies here too and
    // 39 of the game's own waypoints are inside a tree, so a shot that stopped
    // at the first solid point would die on the muzzle. The hit test is
    // against **objects that can take damage**, which is what a shot is for.
    let shots: Vec<(world::Id, [f64; 3], Option<String>, Shot)> = {
        let mut boot = boot_mut(&scripts.lua)?;
        let Some(w) = crate::game::world::world(&scripts.lua) else {
            return Err(Error::Pragma("no world".into()));
        };
        /// How near a shot has to pass to count as a hit. Ours: the original
        /// sweeps the bullet's own hull against the world.
        const REACH: f64 = 2.0;
        let player = boot.player.clone().and_then(|n| w.find(&n));
        let mut near = boot.nearest_miss;
        let mut drop = boot.nearest_drop;
        let mut out = Vec::new();
        boot.shots.retain(|&id, shot| {
            let Some(from) = w.get(id).map(|g| g.position) else { return false };
            let at = [0, 1, 2].map(|c| from[c] + shot.direction[c] * shot.speed * dt);
            shot.life -= dt;
            let hit = w
                .iter()
                .filter(|(i, g)| *i != id && g.hitpoints > 0 && !g.name.is_empty())
                .filter(|(_, g)| Some(&g.name) != shot.shooter.as_ref())
                // and it has to be able to hurt what it meets. 0x40e87d is
                // the whole rule -- `damagetype & mdkGob[0x10]` -- and
                // without it a `gruntshot`, which is `DAMAGE_BADGUY`, stops
                // dead on the next grunt, whose filter is `DAMAGE_GOODGUY |
                // DAMAGE_SNIPER`. Level 8 fired 25 and reported 25 hits.
                .filter(|(_, g)| g.damage_filter & shot.filter != 0)
                .find(|(_, g)| {
                    (0..3).map(|c| (g.position[c] - at[c]).powi(2)).sum::<f64>() < REACH * REACH
                })
                .map(|(_, g)| g.name.clone());
            // how near it came to the player, whoever that is this frame
            if let Some(me) = player.and_then(|p| w.get(p)) {
                let d2: f64 = (0..3).map(|c| (me.position[c] - at[c]).powi(2)).sum();
                let d = d2.sqrt();
                if near.is_none_or(|n: f64| d < n) {
                    drop = (me.position[2] - at[2]).abs();
                }
                near = Some(near.map_or(d, |n: f64| n.min(d)));
            }
            let alive = hit.is_none() && shot.life > 0.0;
            out.push((id, at, hit, shot.clone()));
            alive
        });
        boot.nearest_miss = near;
        boot.nearest_drop = drop;
        out
    };
    for (id, at, hit, shot) in shots {
        if let Some(victim) = &hit {
            let source = shot
                .shooter
                .as_ref()
                .and_then(|n| globals.get::<mlua::Table>(n.as_str()).ok())
                .map(Value::Table);
            if let Ok(v) = globals.get::<mlua::Table>(victim.as_str()) {
                deal_damage(
                    &scripts.lua,
                    source,
                    Value::Table(v),
                    shot.damage as i64,
                    shot.filter as i64,
                    -1,
                    true,
                )?;
                boot_mut(&scripts.lua)?.hits += 1;
            }
        }
        let over = hit.is_some() || shot.life <= 0.0;
        if !over {
            if let Some(mut w) = scripts.lua.app_data_mut::<world::World>() {
                w.set_position(id, at);
            }
            continue;
        }
        // both events go to the **shooter**, and the first only when what was
        // hit is what was aimed at
        if let Some(shooter) = shot.shooter.as_ref() {
            if let Ok(gob) = globals.get::<mlua::Table>(shooter.as_str()) {
                if hit.is_some() && hit == shot.target {
                    if let Ok(h) = gob.get::<mlua::Function>("OnShotLanded") {
                        let _ = h.call::<Value>((gob.clone(), shot.kind));
                    }
                }
                if let Ok(h) = gob.get::<mlua::Function>("OnShotExploded") {
                    let bullet = world::world(&scripts.lua)
                        .and_then(|w| w.get(id).map(|g| g.name.clone()))
                        .and_then(|n| globals.get::<mlua::Table>(n.as_str()).ok());
                    let _ = h.call::<Value>((gob.clone(), shot.kind, bullet));
                }
            }
        }
    }

    // the walkers' cooldowns, `walker + 0x64`, which every AI branch tests
    // before it does anything
    {
        let mut boot = boot_mut(&scripts.lua)?;
        for left in boot.cooldown.values_mut() {
            *left -= dt;
        }
        // and `walker + 0x98`, which is the goto core's own -- how long
        // before it looks up from its heading and aims again
        for left in boot.reaim.values_mut() {
            *left -= dt;
        }
    }

    // the animation clock, and the keys it passes.
    //
    // A key channel is target kind **23** in the model, its values are codes,
    // and 0x478ad8 hands each one to 0x42bf80 as the animation reaches it.
    // The split is that function's: **>= 100 creates an object of that type**,
    // 30..99 a screen flash, 20..29 an earthquake, 1..19 `OnCustomKey`. See
    // [`Boot::keys`].
    //
    // ponytail: a created shot flies along the **gob's own yaw**, where the
    // original launches it with the muzzle node's orientation. The walker aims
    // its whole body at what it is shooting, so the two agree for a walker and
    // differ for a turret. Screen flashes and earthquakes are counted and not
    // shown, because there is no camera to shake.
    let struck: Vec<(String, f64)> = {
        let mut boot = boot_mut(&scripts.lua)?;
        let live: Vec<(String, f64, String)> = {
            let Some(w) = crate::game::world::world(&scripts.lua) else {
                return Err(Error::Pragma("no world".into()));
            };
            w.iter()
                .filter(|(_, g)| !g.name.is_empty())
                .filter_map(|(_, g)| {
                    // **the model it is actually wearing**, not its type's:
                    // every cutscene actor is an `OBJ_SCENERY` with its own
                    // model in the resource slot, and asking the type gave
                    // `scenery`, which has no animation spans -- so the clock
                    // never wrapped and every movie waiting on
                    // `omAnimJustLooped` waited for ever.
                    Some((
                        g.name.clone(),
                        *boot.playing.get(&g.name)?,
                        model_of(g.kind, g.resource.as_deref())?,
                    ))
                })
                .collect()
        };
        let (mut out, mut advanced, mut looped) = (Vec::new(), Vec::new(), Vec::new());
        let (mut stopped, mut running) = (Vec::new(), Vec::new());
        for (name, anim, model) in live {
            let was = boot.since.get(&name).copied().unwrap_or(0.0);
            let speed = boot.speed.get(&name).copied().unwrap_or(1.0);
            let mut now = was + dt * speed;
            // **an animation is a loop.** The record's rate is a rate, so one
            // pass lasts `1 / |rate|` and the clock wraps at it; without the
            // wrap `since` ran to infinity and every key on a model fired
            // once in the life of the object. A conehead's fart is on nearly
            // every animation it has and it went off once.
            //
            // ponytail: a tick longer than a whole loop fires each key once
            // rather than once per loop crossed. At 30 frames a second and a
            // shortest loop of 0.2s that cannot happen.
            let span = boot.spans.get(&(model.clone(), anim as i64)).copied().unwrap_or(0.0);
            let wrapped = span > 0.0 && !(0.0..span).contains(&now);
            if let Some(keys) = boot.keys.get(&model) {
                for &(a, at, code) in keys {
                    if a != anim {
                        continue;
                    }
                    let struck = match (wrapped, speed < 0.0) {
                        (false, false) => at > was && at <= now,
                        (false, true) => at < was && at >= now,
                        // over the wrap the window is two pieces
                        (true, false) => at > was || at <= now - span,
                        (true, true) => at < was || at >= now + span,
                    };
                    if struck {
                        out.push((name.clone(), code));
                    }
                }
            }
            if wrapped {
                looped.push((name.clone(), anim));
                // **and it only comes round if the record says so.** A
                // one-shot clamps at its end and stays there, which is both
                // what 0x4611b0 does and what leaves the last frame on
                // screen instead of snapping back to the bind pose.
                if boot.oneshot.contains(&(model.clone(), anim as i64)) {
                    now = span;
                    stopped.push(name.clone());
                } else {
                    now -= span * (now / span).floor();
                    running.push(name.clone());
                }
            }
            advanced.push((name, now));
        }
        boot.looped = looped.into_iter().collect();
        for name in stopped {
            boot.done.insert(name);
        }
        for name in running {
            boot.done.remove(&name);
        }
        for (name, now) in advanced {
            boot.since.insert(name, now);
        }
        out
    };
    for (name, code) in struck {
        boot_mut(&scripts.lua)?.keys_fired += 1;
        if code >= 100.0 {
            fire_key_object(&scripts.lua, &name, code)?;
        } else if (1.0..20.0).contains(&code) {
            if let Ok(gob) = globals.get::<mlua::Table>(name.as_str()) {
                if let Ok(h) = gob.get::<mlua::Function>("OnCustomKey") {
                    let _ = h.call::<Value>((gob.clone(), "", code));
                }
            }
        } else if let Some((f, n)) = match code {
            // 0x42c02a and 0x42c05e: the dispatch calls the **Lua** globals,
            // and both read their magnitude out of `quakeparams` and
            // `flashparams` in `mdk2.lua`, so all the numbers are the game's
            // own. `Earthquake` ends in `omSceneShake`.
            c if (20.0..30.0).contains(&c) => Some(("Earthquake", c - 19.0)),
            c if (30.0..100.0).contains(&c) => Some(("ScreenFlash", c - 29.0)),
            _ => None,
        } {
            if let (Ok(gob), Ok(call)) = (
                globals.get::<mlua::Table>(name.as_str()),
                globals.get::<mlua::Function>(f),
            ) {
                let _ = call.call::<Value>((gob, n));
            }
        }
    }
    // and the shake runs down. 0x46aa4b stops it when the clock passes the
    // duration; the offset itself is [`Shake::angles`], read by whoever holds
    // a camera.
    {
        let mut boot = boot_mut(&scripts.lua)?;
        if let Some(shake) = boot.shake.as_mut() {
            shake.elapsed += dt;
            if shake.elapsed > shake.duration {
                boot.shake = None;
            }
        }
    }

    // and the scripted sequences. 0x42bd60 tests bit 0x800000 on each gob
    // and calls the Lua global `ScriptUpdate` for the ones that have it —
    // **not a handler on the object**, a global taking the object, which is
    // why `script.lua` defines exactly one of them for the whole game.
    //
    // The clock has to be written first: `ScriptUpdate` opens with
    // `chGetDeltaT()` and every wait in every cutscene counts down by it.
    boot_mut(&scripts.lua)?.delta = dt;
    // **and not while in stasis.** 0x46d505 tests bit 3 of `gob + 0xa6` —
    // the bit `omGobEnterStasis` sets (0x46e329) — and skips the object's
    // *whole* update, the `ScriptUpdate` call at 0x46d5b1 included. That is
    // exactly why `StopScript`'s `omGobIsStasis(self) == 0` guard is safe in
    // the original: a script that ends while its object is frozen simply
    // stops being ticked, so the flag it could not clear never matters.
    // Without this, level 9 walked off the end of a task list 897 times.
    let running: Vec<String> = {
        let boot = boot_ref(&scripts.lua)?;
        let w = crate::game::world::world(&scripts.lua);
        boot.scripted
            .iter()
            .filter(|n| match &w {
                Some(w) => w.find(n).is_none_or(|id| !frozen(w, &boot.stasis, id)),
                None => !boot.stasis.contains(*n),
            })
            .cloned()
            .collect()
    };
    if !running.is_empty() {
        if let Ok(update) = globals.get::<mlua::Function>("ScriptUpdate") {
            for name in running {
                let Ok(gob) = globals.get::<mlua::Table>(name.as_str()) else { continue };
                let entry = state.fired.entry("ScriptUpdate".to_string()).or_insert((0, 0));
                entry.0 += 1;
                match update.call::<Value>(gob) {
                    Ok(_) => entry.1 += 1,
                    Err(e) => {
                        // grouped the way `fire_events` groups: the message
                        // without its position
                        let text = e.to_string();
                        let line = text.lines().next().unwrap_or("").to_string();
                        let kind = line
                            .rfind(": ")
                            .map(|i| line[i + 2..].to_string())
                            .unwrap_or(line);
                        *state.why.entry(kind).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    // the clock the scripts read
    boot_mut(&scripts.lua)?.clock = state.clock;
    boot_mut(&scripts.lua)?.delta = dt;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_of(scripts: &Scripts, name: &str) -> [f64; 3] {
        let w = world::world(&scripts.lua).unwrap();
        w.get(w.find(name).unwrap()).unwrap().position
    }

    /// The names are the game's own, and the eight directions are the eight
    /// `ANIM_RUN*` the models carry.
    #[test]
    fn locomotion_picks_the_direction_it_is_going() {
        assert_eq!(walk_animation(1.0, 0.0), "ANIM_RUNF");
        assert_eq!(walk_animation(-1.0, 0.0), "ANIM_RUNB");
        assert_eq!(walk_animation(0.0, 1.0), "ANIM_RUNR");
        assert_eq!(walk_animation(0.0, -1.0), "ANIM_RUNL");
        assert_eq!(walk_animation(1.0, 1.0), "ANIM_RUNFR");
        assert_eq!(walk_animation(-1.0, -1.0), "ANIM_RUNBL");
        assert_eq!(walk_animation(0.0, 0.0), "ANIM_DEFAULT");
        // a twitch is not a walk
        assert_eq!(walk_animation(0.01, -0.01), "ANIM_DEFAULT");
    }

    /// The launch is a solver, so the check is that it solves: sampled at the
    /// flight time the arc is **on** the waypoint, and its peak is the height
    /// the script asked for. The sampler is the tick's own, so the two cannot
    /// drift apart.
    #[test]
    fn a_launch_arrives_and_peaks_where_it_was_told_to() {
        let (from, to) = ([0.0, 0.0, 0.0], [30.0, 40.0, 5.0]);
        let jump = Jump { from, arc: launch(from, to, 12.0), elapsed: 0.0 };
        let end = jump.at(jump.arc.time);
        for c in 0..3 {
            assert!((end[c] - to[c]).abs() < 1e-9, "{end:?} should be {to:?}");
        }
        // the apex comes when the rise has been spent, and it is a height
        // above the *launch*, not above the ground
        let peak = jump.at(jump.arc.rise / crate::game::body::GRAVITY)[2];
        assert!((peak - 12.0).abs() < 1e-9, "peaked at {peak}, asked for 12");
    }

    /// **The apex is clamped up to the destination, never down.** 0x430258
    /// takes the larger of the argument and the climb, so a 2-unit hop onto a
    /// 10-unit ledge becomes a 10-unit one — and lands with nothing left,
    /// because the ledge *is* the apex.
    #[test]
    fn a_jump_cannot_peak_below_where_it_is_going() {
        let g = crate::game::body::GRAVITY;
        let arc = launch([0.0; 3], [10.0, 0.0, 10.0], 2.0);
        assert!((arc.rise - (2.0 * g * 10.0).sqrt()).abs() < 1e-9);
        assert!((arc.time - (2.0 * 10.0 / g).sqrt()).abs() < 1e-9, "no fall to make");
    }

    /// Heading is a *want* and an answer, not a turn: it records where the
    /// walker should look and says whether it already does, to within the
    /// 0.17 radians the original allows.
    #[test]
    fn heading_records_the_want_and_answers_whether_it_is_met() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                // the walker faces +x; one target is ahead of it, one is at
                // right angles, one is eight degrees off -- inside the 9.7
                // a yaw of 0 faces +y, so ahead is due +y and askew is
                // eight degrees off it in x
                "points.side = {x = 10, y = 0, z = 0, f = 0}\n\
                 mdkRegisterObject('w',     OBJ_NONE, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('ahead', OBJ_NONE, scene, nil, -1, 0,10,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('askew', OBJ_NONE, scene, nil, -1, -1.4,10,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 facing    = mdkWalkerHeadToGob(w, ahead)\n\
                 nearly    = mdkWalkerHeadToGob(w, askew)\n\
                 sideways  = mdkWalkerHeadToPoint(w, 'side')",
            )
            .exec()
            .unwrap();
        let g = scripts.lua.globals();
        assert_eq!(g.get::<f64>("facing").unwrap(), 1.0);
        assert_eq!(g.get::<f64>("nearly").unwrap(), 1.0, "8 degrees is inside 9.7");
        assert_eq!(g.get::<f64>("sideways").unwrap(), 0.0, "a right angle is not");
        // and the last call left the want behind, pointing at the waypoint
        let want = scripts.lua.app_data_ref::<Boot>().unwrap().heading["w"];
        assert!((want + std::f64::consts::FRAC_PI_2).abs() < 1e-9, "due +x");
    }

    /// The two walking orders differ in three readable ways, and this checks
    /// all three: the direct one measures in **all three axes** and refuses
    /// to move until it is facing, the other measures **horizontally** and
    /// has a default radius of 4.0.
    #[test]
    fn a_goto_orders_the_gait_and_the_direct_one_turns_first() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                // the walker faces +y and stands at the origin; the waypoint
                // is due +x, ten out and three up
                "points.wp = {x = 10, y = 0, z = 3, f = 0}\n\
                 mdkRegisterObject('w', OBJ_NONE, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 far    = mdkWalkerGotoPoint(w, 'wp', 1, 0, 0, 0)\n\
                 direct = mdkWalkerGotoPointDirectly(w, 'wp', 1, 4)",
            )
            .exec()
            .unwrap();
        let g = scripts.lua.globals();
        assert_eq!(g.get::<f64>("far").unwrap(), 0.0, "ten units is not four");
        assert_eq!(g.get::<f64>("direct").unwrap(), 0.0);
        {
            let boot = scripts.lua.app_data_ref::<Boot>().unwrap();
            // the direct call ran last and the walker still faces +y, a right
            // angle off the waypoint, so it turns before it moves
            assert_eq!(boot.gait["w"], 0, "not facing yet");
            assert!((boot.heading["w"] + std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        }
        // now put it square on the waypoint's x/y and ask again. The plain
        // goto is horizontal, so three units of height is arrival; the direct
        // one measures the height too, so three units is not.
        scripts
            .lua
            .load(
                "mdkGobSetPositionXYZ(w, 10, 0, 0)\n\
                 flat = mdkWalkerGotoPoint(w, 'wp', 1, 0, 0, 0)\n\
                 solid = mdkWalkerGotoPointDirectly(w, 'wp', 1, 2)",
            )
            .exec()
            .unwrap();
        assert_eq!(g.get::<f64>("flat").unwrap(), 1.0, "arrived in the plane");
        assert_eq!(g.get::<f64>("solid").unwrap(), 0.0, "three up is not two");
    }

    /// The mover, which is the point of all of it: a grunt told to run at a
    /// waypoint behind it **turns before it travels**, and once round it
    /// covers its own type's run speed out of the table — 20 a second, not
    /// the player's 4.
    #[test]
    fn a_walker_turns_first_and_then_runs_at_its_own_speed() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                // OBJ_GRUNT is 202, and it faces +y with the waypoint due -y
                "points.wp = {x = 0, y = -400, z = 0, f = 0}\n\
                 mdkRegisterObject('g', 202, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)",
            )
            .exec()
            .unwrap();
        let rooms = Visibility::default();
        let mut state = Ticking::default();
        let mut travelled = 0.0;
        let mut turned_before_moving = None;
        for i in 0..90 {
            scripts
                .lua
                .load("mdkWalkerGotoPoint(g, 'wp', 1, 0, 0, 0)")
                .exec()
                .unwrap();
            let before = at_of(&scripts, "g");
            tick(&scripts, &rooms, [0.0, 0.0, 0.0], 0.0, 1.0 / 30.0, &mut state).unwrap();
            let after = at_of(&scripts, "g");
            let d = ((after[0] - before[0]).powi(2) + (after[1] - before[1]).powi(2)).sqrt();
            travelled += d;
            // the first tick is a half turn away from the waypoint, so it
            // must not already be closing on it
            // ...on the tick after the aim frame, which is the first one
            // the core lets the legs run on
            if i == 1 {
                turned_before_moving = Some(after[1] - before[1] > 0.0);
            }
        }
        assert_eq!(
            turned_before_moving,
            Some(true),
            "it should still be drifting the wrong way while it turns"
        );
        // three seconds at 20 a second is 60 units, less the half-turn at 4
        // radians a second that starts it — a grunt's own two numbers
        assert!((45.0..60.0).contains(&travelled), "travelled {travelled}");
        let at = at_of(&scripts, "g");
        assert!(at[1] < -40.0, "and it ends up toward the waypoint, at {at:?}");
    }

    /// A shot carries none of its own numbers: the call gives it a direction
    /// and the **shot table** gives it everything else. `lasershot` is type
    /// 430, 25 damage at 90 a second, and its damage type is 1 —
    /// `DAMAGE_GOODGUY`, which is what lets it through a conehead's filter.
    /// A `gruntshot` next to it is `DAMAGE_BADGUY` and would bounce off, and
    /// that is the original's rule rather than a quirk of this test.
    #[test]
    fn a_shot_flies_at_its_type_s_speed_and_hurts_what_it_reaches() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                // OBJ_GRUNT 202 shoots OBJ_GRUNTSHOT 427 due +x at a conehead
                "mdkRegisterObject('shooter', 202, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('victim', 203, scene, nil, -1, 20,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 bul = mdkRegisterObject('', 430, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 landed = 0\n\
                 shooter.OnShotExploded = function(g, kind, b) landed = kind end\n\
                 fired = mdkShootBulletLua(bul, shooter, victim, 1, 0, 0)",
            )
            .exec()
            .unwrap();
        let g = scripts.lua.globals();
        assert_eq!(g.get::<f64>("fired").unwrap(), 1.0);
        let health = |name: &str| {
            let w = world::world(&scripts.lua).unwrap();
            w.get(w.find(name).unwrap()).unwrap().hitpoints
        };
        let full = health("victim");
        let rooms = Visibility::default();
        let mut state = Ticking::default();
        // 20 units at 90 a second is under a quarter second
        for _ in 0..20 {
            tick(&scripts, &rooms, [0.0, 0.0, 0.0], 0.0, 1.0 / 30.0, &mut state).unwrap();
        }
        assert_eq!(health("victim"), full - 25, "a lasershot is worth 25");
        assert_eq!(g.get::<f64>("landed").unwrap(), 430.0, "and the shooter heard it");
        assert!(scripts.lua.app_data_ref::<Boot>().unwrap().shots.is_empty(), "spent");
    }

    /// The scope shoots along the camera and not along the nose, which is
    /// the only reason `fire_along` exists: a sniper aims **up**, and every
    /// other shot in the game leaves flat. A target thirty units out and ten
    /// up is unreachable by the nose and hit by the camera.
    #[test]
    fn the_scope_shoots_where_it_is_looking() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('bob', 100, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('victim', 203, scene, nil, -1, 30,0,10, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)",
            )
            .exec()
            .unwrap();
        let health = || {
            let w = world::world(&scripts.lua).unwrap();
            w.get(w.find("victim").unwrap()).unwrap().hitpoints
        };
        let full = health();
        // `OBJ_SNIPERBULLET`, up the slope the camera is looking along
        let d = (30.0f64 * 30.0 + 10.0 * 10.0).sqrt();
        fire_along(&scripts.lua, "bob", 406.0, [0.0, 0.0, 0.0], [30.0 / d, 0.0, 10.0 / d])
            .unwrap();
        let rooms = Visibility::default();
        let mut state = Ticking::default();
        for _ in 0..60 {
            tick(&scripts, &rooms, [0.0, 0.0, 0.0], 0.0, 1.0 / 30.0, &mut state).unwrap();
        }
        assert!(health() < full, "the sniper bullet reached what the camera pointed at");
        // and the same shot along the nose goes under it, which is what makes
        // the test about the pitch and not about the shot
        let hurt = health();
        fire_along(&scripts.lua, "bob", 406.0, [0.0, 0.0, 0.0], [1.0, 0.0, 0.0]).unwrap();
        for _ in 0..60 {
            tick(&scripts, &rooms, [0.0, 0.0, 0.0], 0.0, 1.0 / 30.0, &mut state).unwrap();
        }
        assert_eq!(health(), hurt, "flat, it passes ten units beneath");
    }

    /// **An animation key is where an enemy's shot comes from.** `hans.mod`
    /// animation 56 carries the code 421 at t = 0.513, and 421 is `hansshot`
    /// — so a hans playing that animation fires one 0.513 seconds in, and
    /// exactly once however long it plays.
    #[test]
    fn an_animation_key_fires_the_shot_the_model_names() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                // OBJ_HANS is 204 and wears hans.mod, per the enemy table
                "mdkRegisterObject('h', 204, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 struck = 0\n\
                 h.OnCustomKey = function(g, slot, code) struck = code end\n\
                 omAnimPlay(h, 56, 0)",
            )
            .exec()
            .unwrap();
        {
            let mut boot = scripts.lua.app_data_mut::<Boot>().unwrap();
            // what `mod2obj.py --keys` reads out of hans.mod, and one custom
            // key beside it to show the other half of the split
            boot.keys.insert("hans".into(), vec![(56.0, 0.513, 421.0), (56.0, 0.6, 12.0)]);
        }
        let rooms = Visibility::default();
        let mut state = Ticking::default();
        for _ in 0..30 {
            tick(&scripts, &rooms, [0.0, 0.0, 0.0], 0.0, 1.0 / 30.0, &mut state).unwrap();
        }
        let boot = scripts.lua.app_data_ref::<Boot>().unwrap();
        assert_eq!(boot.keys_fired, 2, "one shot and one custom key, once each");
        assert_eq!(boot.fired, 1, "and the shot is a real one");
        drop(boot);
        assert_eq!(scripts.lua.globals().get::<f64>("struck").unwrap(), 12.0);
        // the shot exists, carries hansshot's numbers, and is flying
        let w = world::world(&scripts.lua).unwrap();
        let id = w.find("h_key421").expect("the key made a bullet");
        assert_eq!(w.get(id).unwrap().kind, 421.0);
    }

    /// The scene graph's trailing flag freezes an object, and **its children
    /// with it**. Level 1 is the case: `kurtgame` carries a 1 and fifteen
    /// objects hang off it at 0, and one of them reads state that only the
    /// minigame's own start writes.
    #[test]
    fn a_frozen_parent_freezes_what_hangs_off_it() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('held', OBJ_SCENERY, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 1)\n\
                 mdkRegisterObject('kid', OBJ_SCENERY, scene, held, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('free', OBJ_SCENERY, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 ticks = {held = 0, kid = 0, free = 0}\n\
                 held.OnUpdate = function(g) ticks.held = ticks.held + 1 end\n\
                 kid.OnUpdate  = function(g) ticks.kid  = ticks.kid  + 1 end\n\
                 free.OnUpdate = function(g) ticks.free = ticks.free + 1 end",
            )
            .exec()
            .unwrap();
        create(&scripts).unwrap();
        let rooms = Visibility::default();
        let mut state = Ticking::default();
        for _ in 0..5 {
            tick(&scripts, &rooms, [0.0; 3], 0.0, 1.0 / 30.0, &mut state).unwrap();
        }
        let ticks: mlua::Table = scripts.lua.globals().get("ticks").unwrap();
        assert_eq!(ticks.get::<i64>("free").unwrap(), 5, "nothing holds it");
        assert_eq!(ticks.get::<i64>("held").unwrap(), 0, "the flag froze it");
        assert_eq!(ticks.get::<i64>("kid").unwrap(), 0, "and its child with it");
        // and thawing the parent thaws the subtree
        scripts.lua.load("omGobExitStasis(held)").exec().unwrap();
        tick(&scripts, &rooms, [0.0; 3], 0.0, 1.0 / 30.0, &mut state).unwrap();
        let ticks: mlua::Table = scripts.lua.globals().get("ticks").unwrap();
        assert_eq!(ticks.get::<i64>("kid").unwrap(), 1, "the child came back too");
    }

    /// The enemy AI, in the three states that are built: a doganboy with
    /// something to fight **turns to face it**, and once it is closer than
    /// the behaviour record's near distance it **gives ground backwards**
    /// while still facing — gait 3, which is the negative speed in the enemy
    /// table.
    #[test]
    fn an_enemy_faces_what_it_is_fighting_and_backs_off_when_crowded() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                // OBJ_DOGANBOY is 207 and uses behaviour 0, whose near
                // distance is 10. Put the player due +y, well outside it.
                "mdkRegisterObject('d', 207, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('kurt', 100, scene, nil, -1, 0,40,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkSetPlayModeGobs(0, kurt)\n\
                 far = mdkDoganboyAttack(d)",
            )
            .exec()
            .unwrap();
        assert_eq!(scripts.lua.globals().get::<f64>("far").unwrap(), 0.0, "never done");
        {
            let boot = scripts.lua.app_data_ref::<Boot>().unwrap();
            // forty out is past its near of ten, so the chooser is free to
            // send it in — what matters here is that it is **not** backing off
            assert_ne!(boot.gait["d"], 3, "forty out is not crowded");
            // a yaw of 0 is what faces due +y
            assert!(boot.heading["d"].abs() < 1e-9);
        }
        // now stand on top of it. It is mid-burst, so it holds its ground
        // until the round's second is up — which is itself the original's
        // shape, not an accident of the test.
        scripts
            .lua
            .load("mdkGobSetPositionXYZ(kurt, 0, 5, 0)\n mdkDoganboyAttack(d)")
            .exec()
            .unwrap();
        assert_eq!(
            scripts.lua.app_data_ref::<Boot>().unwrap().gait["d"],
            0,
            "a walker in the middle of a burst does not step back"
        );
        let rooms = Visibility::default();
        let mut ticking = Ticking::default();
        // four seconds, which outlasts the three the chooser's advance costs
        for _ in 0..120 {
            let eye = [0.0, 5.0, crate::game::body::EYE];
            tick(&scripts, &rooms, eye, 0.0, 1.0 / 30.0, &mut ticking).unwrap();
        }
        scripts.lua.load("mdkDoganboyAttack(d)").exec().unwrap();
        let boot = scripts.lua.app_data_ref::<Boot>().unwrap();
        assert_eq!(boot.gait["d"], 3, "five is inside a doganboy's ten");
    }

    /// **Only the doganboy limps.** `def + 0x40` holds 0xffffffff in eighteen
    /// of the nineteen walker records and **20** in the doganboy's, and that
    /// one threshold does two things: 0x42fd4c halves the gait's speed and
    /// 0x42fdc6 swaps the animation table at 0x48ff58 for the one at 0x48ff68.
    /// So a doganboy on its last twenty hitpoints covers three units a second
    /// instead of six, and plays `ANIM_ACTION00` where it played `ANIM_WALK`.
    #[test]
    fn a_doganboy_on_its_last_hitpoints_walks_at_half_speed() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                // due +y, a hundred out, and the walker already faces it.
                // **Twice**, because the goto core's first call is an aim
                // frame: 0x431bfe points the walker and arms the clock at
                // `walker + 0x98` without touching the gait, and only the
                // call after that decides whether the legs move.
                "points.wp = {x = 0, y = 100, z = 0, f = 0}\n\
                 mdkRegisterObject('d', 207, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkWalkerGotoPoint(d, 'wp', 0, 0, 0, 0)\n\
                 mdkWalkerGotoPoint(d, 'wp', 0, 0, 0, 0)\n\
                 hp = mdkGetHitpoints(d)",
            )
            .exec()
            .unwrap();
        let rooms = Visibility::default();
        let mut ticking = Ticking::default();
        let walk = |scripts: &Scripts, ticking: &mut Ticking| {
            for _ in 0..30 {
                tick(scripts, &rooms, [0.0; 3], 0.0, 1.0 / 30.0, ticking).unwrap();
            }
            let w = world::world(&scripts.lua).unwrap();
            w.get(w.find("d").unwrap()).unwrap().position[1]
        };
        let hale = walk(&scripts, &mut ticking);
        assert!((hale - 6.0).abs() < 1e-6, "a doganboy walks at six: {hale}");
        assert_eq!(
            scripts.lua.app_data_ref::<Boot>().unwrap().playing["d"],
            6.0,
            "ANIM_WALK, from the gait table the engine reads for it"
        );
        // now leave it twenty. `mdkDealDamage` wants a dealer, and the walker
        // may deal to itself -- the filter is what gates the path, not who.
        let hp = scripts.lua.globals().get::<f64>("hp").unwrap();
        scripts
            .lua
            .load(format!("mdkDealDamage(d, d, {}, DAMAGE_GOODGUY, -1)", hp - 20.0))
            .exec()
            .unwrap();
        let hurt = walk(&scripts, &mut ticking) - hale;
        assert!((hurt - 3.0).abs() < 1e-6, "and half of six when hurt: {hurt}");
        assert_eq!(
            scripts.lua.app_data_ref::<Boot>().unwrap().playing["d"],
            77.0,
            "ANIM_ACTION00, off the second table"
        );
    }

    /// **The player's gun is hitscan**: 100 units along the nose, 2 damage,
    /// `DAMAGE_GOODGUY`, and the collision world decides whether the shot
    /// arrives. A conehead in front loses two hitpoints; one behind loses
    /// none, because 0x417ebe rays forwards and nowhere else.
    #[test]
    fn the_player_s_gun_reaches_a_hundred_units_and_only_forwards() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                // the player faces +y; one conehead ahead, one behind, one
                // beyond the hundred
                "mdkRegisterObject('kurt', 100, scene, nil, -1, 0,0,0,                  1,0,0,0, nil,0,0,0,0, nil, nil, 0)
                 mdkRegisterObject('ahead', 203, scene, nil, -1, 0,30,0,                  1,0,0,0, nil,0,0,0,0, nil, nil, 0)
                 mdkRegisterObject('behind', 203, scene, nil, -1, 0,-30,0,                  1,0,0,0, nil,0,0,0,0, nil, nil, 0)
                 mdkRegisterObject('far', 203, scene, nil, -1, 0,300,0,                  1,0,0,0, nil,0,0,0,0, nil, nil, 0)
                 mdkSetPlayModeGobs(0, kurt)",
            )
            .exec()
            .unwrap();
        let health = |name: &str| {
            let w = world::world(&scripts.lua).unwrap();
            w.get(w.find(name).unwrap()).unwrap().hitpoints
        };
        let (was_ahead, was_behind) = (health("ahead"), health("behind"));
        assert_eq!(hitscan(&scripts.lua, "kurt", 0).as_deref(), Some("ahead"));
        assert_eq!(health("ahead"), was_ahead - 2, "two, which is mode 0");
        assert_eq!(health("behind"), was_behind, "and nothing behind is touched");
        // mode 1 is the five-damage one
        assert_eq!(hitscan(&scripts.lua, "kurt", 1).as_deref(), Some("ahead"));
        assert_eq!(health("ahead"), was_ahead - 7);
    }

    /// **An enemy shoots**, and the whole chain runs: the AI loads a burst
    /// from the behaviour record, each round plays `ANIM_SHOOT`, and the
    /// model's own key channel on that animation makes the projectile. A
    /// hoser fires `hosershot` — which its `hoser.mod` names at t = 0.742 of
    /// animation 56 and nothing else in the game does.
    #[test]
    fn an_enemy_shoots_what_its_own_animation_names() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                // OBJ_HOSER is 205; its reach is 75, so twenty out is well in
                "mdkRegisterObject('h', 205, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('kurt', 100, scene, nil, -1, 0,20,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkSetPlayModeGobs(0, kurt)",
            )
            .exec()
            .unwrap();
        {
            // the one key hoser.mod actually carries
            let mut boot = scripts.lua.app_data_mut::<Boot>().unwrap();
            boot.keys.insert("hoser".into(), vec![(56.0, 0.742, 428.0)]);
        }
        let rooms = Visibility::default();
        let mut ticking = Ticking::default();
        // long enough that the chooser's advance roll cannot starve it: a
        // hoser closes seven times in ten and each advance costs three
        // seconds, so three seconds of trying is not enough and thirty is
        let mut aimed = false;
        for _ in 0..900 {
            scripts.lua.load("mdkDoganboyAttack(h)").exec().unwrap();
            let eye = [0.0, 20.0, crate::game::body::EYE];
            tick(&scripts, &rooms, eye, 0.0, 1.0 / 30.0, &mut ticking).unwrap();
            // it does not *end* on `ANIM_SHOOT` any more, because the chooser
            // sends it walking again afterwards -- what matters is that the
            // animation came up at all, since that is what fires the shot
            aimed |= scripts.lua.app_data_ref::<Boot>().unwrap().playing.get("h") == Some(&56.0);
        }
        let boot = scripts.lua.app_data_ref::<Boot>().unwrap();
        assert!(boot.fired > 0, "it should have fired by now");
        assert!(aimed, "and the shot came off ANIM_SHOOT");
        drop(boot);
        let w = world::world(&scripts.lua).unwrap();
        assert!(
            w.iter().any(|(_, g)| g.kind == 428.0),
            "and a hosershot exists, which only hoser.mod could have named"
        );
    }

    /// **The doganboy throws a grenade**, and every gate on it is the
    /// original's: a burst of three (the record's first column), one round a
    /// second, and on the **last** round — between 25 and 45 units out, seven
    /// times in ten — a `dbgrenade` instead.
    #[test]
    fn a_doganboy_ends_its_burst_with_a_grenade() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                // thirty-five units apart, which is inside the throwing band
                "mdkRegisterObject('d', 207, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('kurt', 100, scene, nil, -1, 0,35,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkSetPlayModeGobs(0, kurt)",
            )
            .exec()
            .unwrap();
        let rooms = Visibility::default();
        let mut ticking = Ticking::default();
        let mut thrown = 0;
        // thirty seconds: the burst of three is over in two, but the chooser
        // sends a doganboy in seven times in ten and each of those costs
        // three, so it takes a while to reach a last round at all
        for _ in 0..900 {
            scripts.lua.load("mdkDoganboyAttack(d)").exec().unwrap();
            // the tick warps the player gob to where the body is, so the body
            // has to stand where the test put the gob -- and the walker is
            // put back on its mark every frame, because the subject here is
            // the throw and not the walk that would carry it out of the band
            scripts.lua.load("mdkGobSetPositionXYZ(d, 0, 0, 0)").exec().unwrap();
            let eye = [0.0, 35.0, crate::game::body::EYE];
            tick(&scripts, &rooms, eye, 0.0, 1.0 / 30.0, &mut ticking).unwrap();
            let w = world::world(&scripts.lua).unwrap();
            thrown = w.iter().filter(|(_, g)| g.kind == 417.0).count();
            if thrown > 0 {
                break;
            }
        }
        assert!(thrown > 0, "the last round of the burst should be a grenade");
        let boot = scripts.lua.app_data_ref::<Boot>().unwrap();
        assert!(boot.fired > 0, "and it is a real shot, with the table's numbers");
    }

    /// **The inventory keeps counts, and the table says which hand.** Giving
    /// the same type twice adds; a type whose give column is negative is
    /// unlimited and stays so; removing the last one drops the slot.
    #[test]
    fn what_a_character_carries_is_counted_and_two_handed() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('doc', 190, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkDocGiveItem(doc, 317, 5)\n\
                 mdkDocGiveItem(doc, 317, 5)\n\
                 mdkDocGiveItem(doc, 328, 1)\n\
                 mdkDocGiveItem(doc, 329, 1)",
            )
            .exec()
            .unwrap();
        let carried = |s: &Scripts, bank: i64| {
            s.lua.app_data_ref::<Boot>().unwrap().carried.get(&("doc".into(), bank)).cloned()
        };
        // 317 grenade and 329 toaster are bank 0, 328 loaf is bank 1
        assert_eq!(carried(&scripts, 0), Some(vec![(317.0, 10.0 as i64), (329.0, 1)]));
        assert_eq!(carried(&scripts, 1), Some(vec![(328.0, -1)]), "a loaf is unlimited");
        let ask = |q: &str| {
            scripts.lua.load(format!("answer = {q}")).exec().unwrap();
            scripts.lua.globals().get::<f64>("answer").unwrap()
        };
        assert_eq!(ask("mdkDocHasItem(doc, 328)"), 1.0);
        assert_eq!(ask("mdkDocHasItem(doc, 330)"), 0.0, "booze it never had");
        assert_eq!(ask("mdkDocGetHeldItem(doc, 1)"), 328.0, "the other hand");
        scripts.lua.load("mdkDocRemoveItem(doc, 329, 1)").exec().unwrap();
        assert_eq!(carried(&scripts, 0), Some(vec![(317.0, 10)]), "the slot goes with the last one");
    }

    /// **The screen shake is three sines of one phase**, at 6, 10 and 16
    /// times it, each on the amplitude and on a linear fade to nothing. An
    /// `Earthquake` key is the same thing with the game's own numbers.
    #[test]
    fn a_shake_is_three_sines_that_fade_to_nothing() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load("omSceneShake(0, 0.02, 3, 1, 'rumble1')")
            .exec()
            .unwrap();
        let angles = |s: &Scripts, t: f64| {
            let mut b = s.lua.app_data_mut::<Boot>().unwrap();
            b.shake.as_mut().unwrap().elapsed = t;
            b.shake.unwrap().angles()
        };
        let t: f64 = 0.1;
        for (k, mul) in [6.0f64, 10.0, 16.0].into_iter().enumerate() {
            let want = (3.0 * t * mul).sin() * 0.02 * (1.0 - t);
            let got = angles(&scripts, t)[k];
            assert!((got - want).abs() < 1e-12, "axis {k}: {got} against {want}");
        }
        assert_eq!(angles(&scripts, 1.0), [0.0; 3], "and it is nothing at the end");
        // and the tick runs it down
        let rooms = Visibility::default();
        let mut ticking = Ticking::default();
        for _ in 0..40 {
            tick(&scripts, &rooms, [0.0, 0.0, 0.0], 0.0, 1.0 / 30.0, &mut ticking).unwrap();
        }
        assert!(
            scripts.lua.app_data_ref::<Boot>().unwrap().shake.is_none(),
            "a one-second shake is over after forty thirtieths"
        );
    }

    /// **A walker that looks into a wall stops.** The path probe is what the
    /// AI passes `avoid` for, and a doganboy advancing at the player with a
    /// wall ten units in front of it -- its own `def + 0x80` -- gives up the
    /// gait rather than walking through. Without it eleven bodies left the
    /// world across the ten levels; with it, six.
    #[test]
    fn a_walker_stops_at_the_wall_its_probe_finds() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        // solid everywhere past x = 5, and the walker faces +x
        scripts.lua.set_app_data(std::rc::Rc::new(
            crate::game::body::Collision::one_plane([-1.0, 0.0, 0.0], 5.0, "a wall"),
        ));
        scripts
            .lua
            .load(
                "mdkRegisterObject('d', 207, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)",
            )
            .exec()
            .unwrap();
        {
            let mut boot = scripts.lua.app_data_mut::<Boot>().unwrap();
            boot.avoiding.insert("d".into());
            boot.gait.insert("d".into(), 2);
            // straight at the wall: `bearing(dx, dy)` is `atan2(-dx, dy)`
            boot.heading.insert("d".into(), crate::game::body::bearing(1.0, 0.0));
        }
        let rooms = Visibility::default();
        let mut ticking = Ticking::default();
        tick(&scripts, &rooms, [0.0, 0.0, 0.0], 0.0, 1.0 / 30.0, &mut ticking).unwrap();
        let boot = scripts.lua.app_data_ref::<Boot>().unwrap();
        assert_eq!(boot.gait["d"], 0, "the probe found the wall, so it stops");
        assert!(
            boot.probe_at["d"] > 1.0,
            "and waits a second or more before looking again"
        );
    }

    /// **An animation is a loop**, so its keys come round again. Before the
    /// clock wrapped, `since` ran to infinity and a key fired once in the
    /// life of the object -- a conehead's fart is on nearly every animation
    /// it owns and it went off exactly once.
    ///
    /// `omAnimJustLooped` is the same fact from the script's side: true on
    /// the frame the clock wrapped and no other.
    #[test]
    fn an_animation_comes_round_and_its_key_fires_again() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('h', 205, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)",
            )
            .exec()
            .unwrap();
        /// One second a loop, and the key a third of the way through.
        const SPAN: f64 = 1.0;
        const AT: f64 = 0.3;
        {
            let mut boot = scripts.lua.app_data_mut::<Boot>().unwrap();
            boot.keys.insert("hoser".into(), vec![(56.0, AT, 421.0)]);
            boot.spans.insert(("hoser".into(), 56), SPAN);
            boot.playing.insert("h".into(), 56.0);
            boot.since.insert("h".into(), 0.0);
        }
        let rooms = Visibility::default();
        let mut ticking = Ticking::default();
        let (mut struck, mut loops) = (0usize, 0usize);
        // 95 ticks is 3.167 seconds: three wraps, and three passes of a key
        // at 0.3. Not 90, because thirty thirtieths sum to a hair under one
        // and the third wrap would fall outside the loop.
        for _ in 0..95 {
            let before = scripts.lua.app_data_ref::<Boot>().unwrap().keys_fired;
            {
                // the gait would take the pose back the frame after a loop,
                // and this test is about the clock, not the legs
                let mut boot = scripts.lua.app_data_mut::<Boot>().unwrap();
                boot.playing.insert("h".into(), 56.0);
            }
            tick(&scripts, &rooms, [0.0, 0.0, 0.0], 0.0, 1.0 / 30.0, &mut ticking).unwrap();
            let boot = scripts.lua.app_data_ref::<Boot>().unwrap();
            struck += boot.keys_fired - before;
            loops += boot.looped.contains_key("h") as usize;
            // and `omAnimJustLooped` answers **for the animation asked
            // about**: 0x461520 looks the instance up by id and 0x461850
            // returns 0 when there is none. 56 is what is playing; 57 is not.
            if boot.looped.contains_key("h") {
                drop(boot);
                let ask = |a: f64| {
                    scripts
                        .lua
                        .load(&format!("answer = omAnimJustLooped(h, {a})"))
                        .exec()
                        .unwrap();
                    scripts.lua.globals().get::<f64>("answer").unwrap()
                };
                assert_eq!(ask(56.0), 1.0, "the one that looped");
                assert_eq!(ask(57.0), 0.0, "and not one that did not");
            }
        }
        assert_eq!(loops, 3, "three seconds of a one-second loop wraps three times");
        assert_eq!(struck, 3, "and the key comes round with it");
    }

    /// **A walker will not leave its pen.** `mdkWalkerSetPen(gob, point,
    /// radius)` gives it a home and a leash, and the first thing the attack
    /// does from then on is measure itself against them: outside the leash it
    /// sets state 1, runs home for three seconds, and looks at nothing about
    /// the fight until it is back inside half of it.
    #[test]
    fn a_penned_walker_goes_home_before_it_fights() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "points = {home = {x=0, y=0, z=0}}\n\
                 mdkRegisterObject('d', 207, scene, nil, -1, 30,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('kurt', 100, scene, nil, -1, 35,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkSetPlayModeGobs(0, kurt)\n\
                 mdkWalkerSetPen(d, 'home', 5)\n\
                 mdkDoganboyAttack(d)",
            )
            .exec()
            .unwrap();
        let b = scripts.lua.app_data_ref::<Boot>().unwrap();
        assert_eq!(b.gait["d"], 2, "it runs");
        assert_eq!(b.cooldown["d"], 3.0, "for three seconds");
        // the player is the other way; home is at -x, whose bearing is +PI/2
        let home = std::f64::consts::FRAC_PI_2;
        assert!(
            (b.heading["d"] - home).abs() < 1e-9,
            "and towards the pen, not the player: {}",
            b.heading["d"]
        );
    }

    /// **An enemy leaps at you**, and past you: the destination is
    /// `target + normalize(target - self) * h` with `h` half to all of the
    /// record's `leap`, so a doganboy 15 units away lands 15 to 30 units
    /// beyond where you stand. The chance is the object's own `payload[1]`,
    /// which this one sets to 1 so the roll always passes.
    #[test]
    fn an_enemy_leaps_past_you_when_its_own_chance_says_so() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('d', 207, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,1,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('kurt', 100, scene, nil, -1, 0,15,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkSetPlayModeGobs(0, kurt)\n\
                 mdkDoganboyAttack(d)",
            )
            .exec()
            .unwrap();
        let boot = scripts.lua.app_data_ref::<Boot>().unwrap();
        let jump = boot.jumps.get("d").expect("a doganboy whose chance is 1 leaps");
        let land = jump.at(jump.arc.time);
        assert!(land[1] > 15.0, "it lands past you, not on you: {land:?}");
        assert!(
            (land[1] - 15.0) >= 15.0 && (land[1] - 15.0) <= 30.0,
            "half to all of the record's 30-unit leap beyond: {land:?}"
        );
    }

    /// `Wait` is `script.lua`'s own clock -- `self.waittimer` counted down by
    /// `chGetDeltaT()` -- and a task list that waits a second has to get past
    /// it. Level 7's spawner sits on one for ever, so this asks whether the
    /// mechanism works at all.
    #[test]
    fn a_task_list_gets_past_a_wait() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        // `script.lua` is not loaded in a unit test, so this is the two
        // engine-side facts `Wait` rests on and nothing else: an object that
        // called `mdkGobEnableScript` gets `ScriptUpdate` every tick, and
        // `chGetDeltaT` answers with the tick's own dt while it runs.
        scripts
            .lua
            .load(
                "mdkRegisterObject('g', 800, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 past = 0\n\
                 function ScriptUpdate(self) past = past + chGetDeltaT() end\n\
                 mdkGobEnableScript(g)",
            )
            .exec()
            .unwrap();
        let rooms = Visibility::default();
        let mut state = Ticking::default();
        for _ in 0..60 {
            tick(&scripts, &rooms, [0.0; 3], 0.0, 1.0 / 30.0, &mut state).unwrap();
        }
        let past = scripts.lua.globals().get::<f64>("past").unwrap();
        assert!((past - 2.0).abs() < 1e-6, "sixty thirtieths of a second: {past}");
    }

    /// `mdkGetScene` and `mdkGetGuiScene` are two scenes, and an object
    /// registered into the second is not in the world -- nor is anything
    /// hanging off it.
    #[test]
    fn the_gui_scene_is_not_the_world() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('inworld', 800, mdkGetScene(), nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 scene = mdkGetGuiScene()\n\
                 mdkRegisterObject('panel', 800, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('dial', 800, scene, panel, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 scene = mdkGetScene()",
            )
            .exec()
            .unwrap();
        let w = world::world(&scripts.lua).unwrap();
        let gui = |n: &str| w.get(w.find(n).unwrap()).unwrap().gui;
        assert!(!gui("inworld"));
        assert!(gui("panel"), "registered into the GUI scene");
        assert!(gui("dial"), "and a child of it is GUI whatever it was told");
    }

    /// The four the player wears are four literals in four constructors, and
    /// only three of them match the `OBJ_*` name. Doc's is `dr`.
    #[test]
    fn the_doctor_has_a_model() {
        assert_eq!(model_for_type(100.0).as_deref(), Some("kurt"));
        assert_eq!(model_for_type(101.0).as_deref(), Some("max"));
        assert_eq!(model_for_type(103.0).as_deref(), Some("hyde"));
        assert_eq!(
            model_for_type(102.0).as_deref(),
            Some("dr"),
            "0x4084c4, and `doc.mod` is not a file in the game"
        );
    }

    /// **A one-shot animation stops at its end**, which is what
    /// `WaitForAnim` in `script.lua` is waiting for -- it is
    /// `omAnimIsPlaying(gob, anim) == 0`. A looping one never stops.
    #[test]
    fn a_one_shot_animation_ends_and_a_loop_does_not() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('a', 800, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('b', 800, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)",
            )
            .exec()
            .unwrap();
        {
            let mut boot = scripts.lua.app_data_mut::<Boot>().unwrap();
            // both play animation 17 of `scenery`; only one of them is told
            // that 17 does not come round again
            boot.spans.insert(("scenery".into(), 17), 0.5);
            boot.oneshot.insert(("scenery".into(), 17));
            boot.spans.insert(("scenery".into(), 6), 0.5);
        }
        scripts.lua.load("omAnimPlay(a, 17)\n omAnimPlay(b, 6)").exec().unwrap();
        let rooms = Visibility::default();
        let mut state = Ticking::default();
        for _ in 0..30 {
            tick(&scripts, &rooms, [0.0; 3], 0.0, 1.0 / 30.0, &mut state).unwrap();
        }
        let ask = |who: &str, anim: f64| {
            scripts
                .lua
                .load(&format!("answer = omAnimIsPlaying({who}, {anim})"))
                .exec()
                .unwrap();
            scripts.lua.globals().get::<f64>("answer").unwrap()
        };
        assert_eq!(ask("a", 17.0), 0.0, "a one-shot has stopped");
        assert_eq!(ask("b", 6.0), 1.0, "and a loop has not");
        // and playing it again starts it
        scripts.lua.load("omAnimPlay(a, 17)").exec().unwrap();
        assert_eq!(ask("a", 17.0), 1.0, "asking again starts it");
    }

    /// A birdbrain fires from the air: it hovers, faces you, spends its
    /// three rounds and then repositions -- either closing or picking a new
    /// height, which is **your own z plus ten**.
    #[test]
    fn a_birdbrain_shoots_and_then_moves() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                // OBJ_BIRDBRAIN1 is 208 -- one of the eight constants that
                // read as 1.4e-312 until the scanner was fixed
                "mdkRegisterObject('bb', 208, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('kurt', 100, scene, nil, -1, 0,20,-4, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkSetPlayModeGobs(0, kurt)",
            )
            .exec()
            .unwrap();
        let boot = || scripts.lua.app_data_ref::<Boot>().unwrap();
        let mut shots = 0;
        let mut height: Option<f64> = None;
        for _ in 0..40 {
            {
                let mut b = scripts.lua.app_data_mut::<Boot>().unwrap();
                b.cooldown.insert("bb".into(), 0.0);
                b.playing.remove("bb");
            }
            scripts.lua.load("mdkBirdbrainAttack(bb)").exec().unwrap();
            let b = boot();
            shots += (b.playing.get("bb") == Some(&56.0)) as i32;
            height = height.or_else(|| b.altitude.get("bb").copied());
        }
        assert!(shots > 0, "twenty units out is past its ten, so it shoots");
        // and when it changes height it wants ten above the player, who is
        // four below it here
        assert_eq!(height, Some(6.0), "the player's own z plus ten");
        assert_eq!(boot().gait.get("bb"), Some(&0), "and it hovers to do it");
    }

    /// A samsmite charges when you are close enough and in front, and its
    /// arrival is an explosion: 15 points at 5 units, falling off by the
    /// distance, and then it dies. A flaming one -- `OBJ_FLAMINGSAMSMITE`,
    /// 215, one of the eight constants that read as 1.4e-312 until today --
    /// spends 7 and walks away from its own blast.
    #[test]
    fn a_samsmite_charges_and_goes_off() {
        let charge = |kind: f64| {
            let scripts = Scripts::new().unwrap();
            install(&scripts.lua, Default::default()).unwrap();
            scripts
                .lua
                .load(&format!(
                    "mdkRegisterObject('s', {kind}, scene, nil, -1, 0,0,0, \
                     1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                     mdkRegisterObject('kurt', 100, scene, nil, -1, 0,1,0, \
                     1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                     mdkSetPlayModeGobs(0, kurt)"
                ))
                .exec()
                .unwrap();
            let health = || {
                let w = world::world(&scripts.lua).unwrap();
                w.get(w.find("kurt").unwrap()).unwrap().hitpoints
            };
            let full = health();
            // the first call only claims the state; the second decides, and
            // one unit away is inside contact, so the third arrives
            for _ in 0..4 {
                scripts.lua.app_data_mut::<Boot>().unwrap().cooldown.insert("s".into(), 0.0);
                scripts.lua.load("mdkSamsmiteAttack(s)").exec().unwrap();
            }
            let out =
                (full - health(), scripts.lua.app_data_ref::<Boot>().unwrap().died.clone());
            out
        };
        // one unit out: 15 (or 7) less the distance, which rounds to nothing
        let (hurt, died) = charge(201.0);
        assert_eq!(hurt, 14, "fifteen at the centre, less the one unit");
        assert_eq!(died, vec!["s".to_string()], "and it goes with the blast");
        let (hurt, died) = charge(215.0);
        assert_eq!(hurt, 6, "a flaming one spends seven");
        assert!(died.is_empty(), "and walks away from it");
    }

    /// A conehead civilian mills about and reacts to you, and does neither
    /// when the player is Doc -- 0x4347fd compares the target's type with
    /// 0x66 and shrugs. The states are reached by driving the clock, because
    /// each of them is held by a cooldown.
    #[test]
    fn a_civilian_looks_at_kurt_and_ignores_doc() {
        let civilian = |player: f64| {
            let scripts = Scripts::new().unwrap();
            install(&scripts.lua, Default::default()).unwrap();
            scripts
                .lua
                .load(&format!(
                    "mdkRegisterObject('cc', 250, scene, nil, -1, 0,0,0, \
                     1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                     mdkRegisterObject('hero', {player}, scene, nil, -1, 5,0,0, \
                     1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                     mdkSetPlayModeGobs(0, hero)\n\
                     mdkSwitchPlayMode(0)"
                ))
                .exec()
                .unwrap();
            // five units away is well inside the twenty; drive it until it
            // either looks or has plainly settled on ignoring
            let mut looked = false;
            for _ in 0..200 {
                scripts.lua.app_data_mut::<Boot>().unwrap().cooldown.insert("cc".into(), 0.0);
                scripts.lua.load("mdkConeheadCivUpdate(cc)").exec().unwrap();
                let b = scripts.lua.app_data_ref::<Boot>().unwrap();
                looked |= b.state.get("cc") == Some(&0x10);
            }
            looked
        };
        assert!(civilian(100.0), "OBJ_KURT is worth looking at");
        assert!(!civilian(102.0), "OBJ_DOC is not");
    }

    /// And a noise sends a standing one into a look and a walking one into a
    /// run -- 0x434b00, which never reads the noise itself.
    #[test]
    fn a_noise_moves_a_civilian_on() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('cc', 250, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)",
            )
            .exec()
            .unwrap();
        let state = |s: &Scripts| s.lua.app_data_ref::<Boot>().unwrap().state.get("cc").copied();
        scripts.lua.app_data_mut::<Boot>().unwrap().state.insert("cc".into(), 0x14);
        scripts.lua.load("mdkConeheadCivOnHear(cc, 0, 0, 0, 0)").exec().unwrap();
        assert_eq!(state(&scripts), Some(0x10), "standing, it looks up");
        scripts.lua.app_data_mut::<Boot>().unwrap().state.insert("cc".into(), 0x11);
        scripts.lua.load("mdkConeheadCivOnHear(cc, 0, 0, 0, 0)").exec().unwrap();
        assert_eq!(state(&scripts), Some(0x0b), "walking, it runs");
    }

    /// `OnModeSwitch` lands on the **room**, not on the player, and only on
    /// a change. That is where `level1.lua` hangs it -- `l1_r1` is an
    /// `OBJ_ROOM` -- and it is how the tutorial knows you found the scope.
    #[test]
    fn the_mode_switch_reaches_the_room() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('l1_r1', 803, scene, nil, -1, 0,0,0, \
                 1,0,0,0, 'l1_r1',0,0,0,0, nil, nil, 0)\n\
                 heard = 0\n\
                 mdkSetLuaEvent(l1_r1, 'OnModeSwitch', function(g, m) heard = heard + m end)",
            )
            .exec()
            .unwrap();
        scripts.lua.app_data_mut::<Boot>().unwrap().room = Some("l1_r1".into());
        let heard = || scripts.lua.globals().get::<f64>("heard").unwrap();
        switch_play_mode(&scripts.lua, 4).unwrap();
        assert_eq!(heard(), 4.0, "the scope reaches the room that asked");
        switch_play_mode(&scripts.lua, 4).unwrap();
        assert_eq!(heard(), 4.0, "and only on a change");
        switch_play_mode(&scripts.lua, 1).unwrap();
        assert_eq!(heard(), 5.0, "coming back out is a switch too");
        assert_eq!(scripts.lua.app_data_ref::<Boot>().unwrap().mode, 1);
    }

    /// The scope's zoom is proportional, and both ends are hard stops.
    #[test]
    fn the_scope_zooms_and_stops() {
        // a second of holding zoom-in at thirty frames, from the wide end
        let mut fov = 60.0;
        for _ in 0..30 {
            fov = zoom(fov, -1.0, 1.0 / 30.0);
        }
        assert!((5.0..6.0).contains(&fov), "a second in takes 60 to about 5.4: {fov}");
        // and it never passes 0.8 however long it is held
        for _ in 0..3000 {
            fov = zoom(fov, -1.0, 1.0 / 30.0);
        }
        assert_eq!(fov, 0.8);
        // nor 60 the other way, and the way back is not the way in: the rate
        // is a share of where it already is
        for _ in 0..3000 {
            fov = zoom(fov, 1.0, 1.0 / 30.0);
        }
        assert_eq!(fov, 60.0);
    }

    /// A turret is a walker with the movement half of the chooser switched
    /// off. Stood on top of the player -- inside `near`, where an ordinary
    /// doganboy backs away or runs within a handful of calls -- it never
    /// leaves gait 0, and it never picks up a pen, a leap or an advance.
    #[test]
    fn a_turret_never_moves() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('d', 207, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('kurt', 100, scene, nil, -1, 0,5,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkSetPlayModeGobs(0, kurt)\n\
                 mdkWalkerSetTurret(d, 1)",
            )
            .exec()
            .unwrap();
        for _ in 0..200 {
            {
                let mut b = scripts.lua.app_data_mut::<Boot>().unwrap();
                b.cooldown.insert("d".into(), 0.0);
                b.burst.insert("d".into(), 0.0);
            }
            scripts.lua.load("mdkDoganboyAttack(d)").exec().unwrap();
            let b = scripts.lua.app_data_ref::<Boot>().unwrap();
            assert_eq!(b.gait.get("d"), Some(&0), "a turret holds its ground");
            assert!(b.fleeing.is_empty() && b.homing.is_empty() && b.jumps.is_empty());
        }
        // and clearing the flag hands the walker back its legs
        scripts.lua.load("mdkWalkerSetTurret(d, 0)").exec().unwrap();
        for tries in 0.. {
            {
                let mut b = scripts.lua.app_data_mut::<Boot>().unwrap();
                b.cooldown.insert("d".into(), 0.0);
                b.burst.insert("d".into(), 0.0);
            }
            scripts.lua.load("mdkDoganboyAttack(d)").exec().unwrap();
            let moved = {
                let b = scripts.lua.app_data_ref::<Boot>().unwrap();
                b.gait.get("d") != Some(&0) || !b.fleeing.is_empty()
            };
            if moved {
                break;
            }
            assert!(tries < 100, "an unpinned doganboy inside `near` gives ground");
        }
    }

    /// Giving ground is not something a crowded walker does every frame:
    /// entering state 2 sets the cooldown to **3** (0x432af4), and the state
    /// refuses to run again until that has expired.
    #[test]
    fn giving_ground_costs_three_seconds() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('d', 207, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('kurt', 100, scene, nil, -1, 0,5,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkSetPlayModeGobs(0, kurt)\n\
                 mdkDoganboyAttack(d)",
            )
            .exec()
            .unwrap();
        let state = |s: &Scripts| {
            let b = s.lua.app_data_ref::<Boot>().unwrap();
            (b.gait["d"], b.cooldown["d"])
        };
        // inside `near` a walker either backs away on its feet (state 2) or
        // turns and runs (state 11), and which one is two rolls; drive it
        // until it takes the one this test is about
        for tries in 0.. {
            if state(&scripts).0 == 3 {
                break;
            }
            assert!(tries < 100, "a doganboy inside `near` should give ground");
            {
                let mut b = scripts.lua.app_data_mut::<Boot>().unwrap();
                b.cooldown.insert("d".into(), 0.0);
                b.fleeing.clear();
            }
            scripts.lua.load("mdkDoganboyAttack(d)").exec().unwrap();
        }
        assert_eq!(state(&scripts), (3, 3.0), "give ground, then wait");
        scripts.lua.load("mdkDoganboyAttack(d)").exec().unwrap();
        assert_eq!(state(&scripts).0, 0, "still waiting, so it stands");
        // run the clock out and it may give ground again
        let rooms = Visibility::default();
        let mut ticking = Ticking::default();
        for _ in 0..100 {
            tick(&scripts, &rooms, [0.0, 0.0, 0.0], 0.0, 1.0 / 30.0, &mut ticking).unwrap();
        }
        scripts.lua.load("mdkDoganboyAttack(d)").exec().unwrap();
        assert!(
            matches!(state(&scripts).0, 2 | 3),
            "three seconds later it gives ground again -- on its feet or at a run"
        );
    }

    /// A stop is not an order to face forwards: 0x431870 writes the heading
    /// **from the gob's own yaw**, so a walker halted mid-turn keeps looking
    /// where it looks.
    #[test]
    fn stopping_a_walker_keeps_the_way_it_is_already_looking() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "points.wp = {x = 0, y = 10, z = 0, f = 0}\n\
                 mdkRegisterObject('w', OBJ_NONE, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkWalkerGotoPoint(w, 'wp', 1, 0, 0, 0)\n\
                 mdkWalkerStop(w)",
            )
            .exec()
            .unwrap();
        let boot = scripts.lua.app_data_ref::<Boot>().unwrap();
        assert_eq!(boot.gait["w"], 0);
        // the goto had asked elsewhere; the stop replaced that with the
        // gob's own facing, which is a yaw of 0 -- due +y
        assert!(boot.heading["w"].abs() < 1e-9, "{}", boot.heading["w"]);
    }

    /// What the binding leaves behind: the arc, the heading, and a gob that
    /// has **turned**. The turn is the part a heading alone never does.
    #[test]
    fn a_walker_faces_the_jump_it_is_told_to_make() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                // due +x, so the walker really has to turn a quarter
                "points.ledge = {x = 10, y = 0, z = 4, f = 0}\n\
                 mdkRegisterObject('w', OBJ_NONE, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 went = mdkWalkerJumpToPoint(w, 'ledge', 10)",
            )
            .exec()
            .unwrap();
        assert_eq!(scripts.lua.globals().get::<f64>("went").unwrap(), 1.0);
        let jump = {
            let boot = scripts.lua.app_data_ref::<Boot>().unwrap();
            assert!(
                (boot.heading["w"] + std::f64::consts::FRAC_PI_2).abs() < 1e-9,
                "the waypoint is due +x"
            );
            boot.jumps["w"]
        };
        let end = jump.at(jump.arc.time);
        assert!((end[0] - 10.0).abs() < 1e-9 && (end[2] - 4.0).abs() < 1e-9, "{end:?}");
        // a quarter turn about Z, which is half of it in the quaternion
        let w = world::world(&scripts.lua).unwrap();
        let q = w.get(w.find("w").unwrap()).unwrap().rotation;
        let half = -std::f64::consts::FRAC_PI_4;
        assert!(
            (q[0] - half.cos()).abs() < 1e-9 && (q[3] - half.sin()).abs() < 1e-9,
            "{q:?} is not a quarter turn"
        );
    }

    /// The cone is `cos(fov * 0.5)`, so the angle a script passes is the
    /// **full** width and `2*PI` means all round. With no collision world —
    /// which is a boot, and this test — nothing can block the view, so what
    /// is being checked here is the range and the cone.
    #[test]
    fn line_of_sight_is_a_full_angle_and_a_range() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                // the watcher faces +y (identity quaternion); one target is
                // straight ahead, one straight behind
                "mdkRegisterObject('eye',    OBJ_NONE, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('ahead',  OBJ_NONE, scene, nil, -1, 0,10,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('behind', OBJ_NONE, scene, nil, -1, 0,-10,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 -- PI is a global the game's own mdk2.lua defines, not an
                 -- engine constant, so the literals are spelled out -- and
                 -- 2*PI has to be exact: the comparison is `cos(off) <
                 -- cos(fov/2)` and a target exactly behind sits exactly on
                 -- the boundary, so 6.2832 would round the cone shut
                 narrow_ahead  = mdkAILineOfSight(eye, ahead,  0.3927, 100)\n\
                 narrow_behind = mdkAILineOfSight(eye, behind, 0.3927, 100)\n\
                 all_round     = mdkAILineOfSight(eye, behind, 6.283185307179586, 100)\n\
                 out_of_range  = mdkAILineOfSight(eye, ahead,  6.283185307179586, 5)\n\
                 can_see       = mdkWalkerCanSeeGob(eye, behind)",
            )
            .exec()
            .unwrap();
        let g = scripts.lua.globals();
        assert_eq!(g.get::<f64>("narrow_ahead").unwrap(), 1.0);
        assert_eq!(g.get::<f64>("narrow_behind").unwrap(), 0.0, "PI/8 is the whole cone");
        assert_eq!(g.get::<f64>("all_round").unwrap(), 1.0, "2*PI really is all round");
        assert_eq!(g.get::<f64>("out_of_range").unwrap(), 0.0, "ten units, five of range");
        assert_eq!(
            g.get::<f64>("can_see").unwrap(),
            1.0,
            "the walker's own cone is not something the engine has, so it is all round"
        );
    }

    /// A shout reaches everything within 100 units and nothing outside it,
    /// fires `OnHear` rather than a handler on the shouter, and happens
    /// **once**: a walker already alerted does not alert again.
    #[test]
    fn an_alerted_walker_shouts_once_and_is_heard_within_a_hundred_units() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "heard = 0\n\
                 where = 0\n\
                 local function ear(gob, noise, x, y, z) heard = heard + 1; where = y end\n\
                 mdkRegisterObject('shouter', OBJ_NONE, scene, nil, -1, 0,7,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('near',    OBJ_NONE, scene, nil, -1, 0,50,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('far',     OBJ_NONE, scene, nil, -1, 0,500,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 shouter.OnHear = ear; near.OnHear = ear; far.OnHear = ear\n\
                 quiet = mdkWalkerAlert(shouter, 0)\n\
                 after_quiet = heard\n\
                 mdkWalkerAlert(shouter, 1)",
            )
            .exec()
            .unwrap();
        let g = scripts.lua.globals();
        assert_eq!(g.get::<f64>("quiet").unwrap(), 1.0, "it always answers 1");
        assert_eq!(g.get::<f64>("after_quiet").unwrap(), 0.0, "a silent alert is silent");
        assert_eq!(
            g.get::<f64>("heard").unwrap(),
            0.0,
            "and the second alert is ignored entirely -- 0x431760 tests the flag first"
        );

        // now one that has not been alerted before
        scripts.lua.load("mdkWalkerAlert(near, 1)").exec().unwrap();
        assert_eq!(g.get::<f64>("heard").unwrap(), 1.0, "the shouter hears, the far one does not");
        assert_eq!(g.get::<f64>("where").unwrap(), 50.0, "and is told where the shout came from");
    }

    /// `mdkGobOnMagicSpot` is the one getter of the nine whose real answer
    /// the engine already had everything for. Both comparisons are strict
    /// and the angle wraps, so the four cases below are the whole of it.
    #[test]
    fn a_gob_is_on_a_magic_spot_only_when_close_and_facing_right() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                // the gob stands a metre from the spot, turned a tenth of
                // a radian: quaternion (cos(t/2), 0, 0, sin(t/2)) about Z
                "points.wash = {x = 10, y = 0, z = 0, f = 0}\n\
                 points.deg  = {x = 10, y = 0, z = 0, f = 5.72958}\n\
                 mdkRegisterObject('doc', OBJ_NONE, scene, nil, -1, 10,1,0, \
                 0.99875, 0, 0, 0.04998, nil,0,0,0,0, nil, nil, 0)\n\
                 near_and_facing = mdkGobOnMagicSpot(doc, 'wash', 3, 1)\n\
                 too_far         = mdkGobOnMagicSpot(doc, 'wash', 0.5, 1)\n\
                 turned_away     = mdkGobOnMagicSpot(doc, 'wash', 3, 0.01)\n\
                 no_such_spot    = mdkGobOnMagicSpot(doc, 'nowhere', 3, 1)\n\
                 -- 5.72958 degrees IS a tenth of a radian, so the gob is\n\
                 -- exactly on this one and the tightest angle passes\n\
                 spot_in_degrees = mdkGobOnMagicSpot(doc, 'deg', 3, 0.001)",
            )
            .exec()
            .unwrap();
        let g = scripts.lua.globals();
        assert_eq!(g.get::<f64>("near_and_facing").unwrap(), 1.0);
        assert_eq!(g.get::<f64>("too_far").unwrap(), 0.0, "distance is strict");
        assert_eq!(g.get::<f64>("turned_away").unwrap(), 0.0, "so is the angle");
        assert_eq!(g.get::<f64>("no_such_spot").unwrap(), 0.0);
        assert_eq!(
            g.get::<f64>("spot_in_degrees").unwrap(),
            1.0,
            "a waypoint's f is degrees -- the scene graphs write -180.091 and 90.0457 -- \
             while the angle a script passes is radians (PI/8, 8*PI/4)"
        );
    }

    /// The fog is three separate calls and a level uses all three — this is
    /// `level9.lua`'s `Level.Init`, verbatim.
    #[test]
    fn the_three_fog_calls_make_one_state() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        assert!(!scripts.lua.app_data_ref::<Boot>().unwrap().fog.on, "off until asked");
        scripts
            .lua
            .load(
                "chFogStartEnd(50, 200)\n\
                 chFogColor(0.0, 0.1, 0.2, 1)\n\
                 chFogEnable()",
            )
            .exec()
            .unwrap();
        let fog = scripts.lua.app_data_ref::<Boot>().unwrap().fog;
        assert_eq!((fog.near, fog.far), (50.0, 200.0));
        assert_eq!(fog.colour, [0.0, 0.1, 0.2], "the alpha is dropped, fog has none");
        assert!(fog.on);

        // and `l3_elev02dead.OnEnterRoom` turns it off again without
        // disturbing the distances
        scripts.lua.load("chFogDisable()").exec().unwrap();
        let fog = scripts.lua.app_data_ref::<Boot>().unwrap().fog;
        assert!(!fog.on);
        assert_eq!((fog.near, fog.far), (50.0, 200.0));
    }

    /// A grunt is 40 hitpoints on Hard and `DAMAGE_GOODGUY` is 1, which is
    /// in its filter of 9. Nothing in a boot reaches this path, because a
    /// boot fires `OnDamage` with a probe rather than a real hit.
    #[test]
    fn damage_takes_hitpoints_and_the_last_of_them_kills() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('gen', OBJ_NONE, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkSpawnerSetSpawnedObject(gen, OBJ_GRUNT, nil, 0,0,0,0, 1, nil)\n\
                 g = mdkSpawnerSpawnObject(gen)\n\
                 dead = 0\n\
                 g.OnDie = function(gob, n) dead = n end\n\
                 mdkDealDamage(gen, g, 39, DAMAGE_GOODGUY, -1)\n\
                 left = mdkGetHitpoints(g)\n\
                 mdkDealDamage(gen, g, 1, DAMAGE_GOODGUY, -1)\n\
                 after = mdkGetHitpoints(g)\n\
                 mdkDealDamage(gen, g, 100, DAMAGE_GOODGUY, -1)",
            )
            .exec()
            .unwrap();
        let g = scripts.lua.globals();
        assert_eq!(g.get::<f64>("left").unwrap(), 1.0, "40 - 39");
        assert_eq!(g.get::<f64>("after").unwrap(), 0.0, "hitpoints clamp at zero");
        assert_eq!(g.get::<f64>("dead").unwrap(), 1.0, "OnDie(gob, 1), not OnDie(gob)");
        // the third hit found it already dead, so OnDie fired once
        assert_eq!(scripts.lua.app_data_ref::<Boot>().unwrap().died, ["gen_spawn"]);
    }

    /// The blower's volume and its one stopping rule. The blower stands at
    /// the origin pointing up, five across and thirty-five long, and stops
    /// pushing at ten units a second.
    #[test]
    fn a_blower_pushes_up_its_own_tube_until_the_speed_it_names() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('fan', OBJ_BLOWERCYLINDER, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil, 5,35,10,0, nil, nil, 0)",
            )
            .exec()
            .unwrap();
        let up = |at: [f64; 3], v: [f64; 3]| blowers(&scripts.lua, at, v)[2];
        let still = [0.0; 3];
        assert_eq!(up([0.0, 0.0, 10.0], still), 40.0, "up the middle");
        assert_eq!(up([4.9, 0.0, 10.0], still), 40.0, "just inside the radius");
        assert_eq!(up([5.1, 0.0, 10.0], still), 0.0, "just outside it");
        assert_eq!(up([0.0, 0.0, 34.9], still), 40.0, "just inside the length");
        assert_eq!(up([0.0, 0.0, 35.1], still), 0.0, "past the end");
        assert_eq!(up([0.0, 0.0, -0.1], still), 0.0, "behind the mouth");
        // and it lets go once he is going that fast **along the axis**
        assert_eq!(up([0.0, 0.0, 10.0], [0.0, 0.0, 9.9]), 40.0);
        assert_eq!(up([0.0, 0.0, 10.0], [0.0, 0.0, 10.0]), 0.0, "at the strength");
        assert_eq!(up([0.0, 0.0, 10.0], [99.0, 0.0, 0.0]), 40.0, "sideways is not along");
        // a script can switch it off and lengthen it
        scripts.lua.load("mdkBlowerDisable(fan)").exec().unwrap();
        assert_eq!(up([0.0, 0.0, 10.0], still), 0.0, "off");
        scripts.lua.load("mdkBlowerEnable(fan)\nmdkBlowerSetLength(fan, 50)").exec().unwrap();
        assert_eq!(up([0.0, 0.0, 40.0], still), 40.0, "longer than the scene graph said");
    }

    /// A prox door opens on the player being near and shuts on his leaving,
    /// its radius is the scene graph's own `payload[0]`, and a lock both
    /// shuts it and holds it shut.
    #[test]
    fn a_prox_door_opens_when_the_player_is_inside_its_radius() {
        const ANIM_OPEN: f64 = 66.0;
        const ANIM_CLOSE: f64 = 67.0;
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                // one door with a radius of 8 out of the payload, and one
                // with 0, which the constructor turns into 20
                "mdkRegisterObject('near', OBJ_PROXDOOR1, scene, nil, -1, 0,0,0, \
                 1,0,0,0, 'dr1', 8,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('far', OBJ_PROXDOOR1, scene, nil, -1, 0,0,0, \
                 1,0,0,0, 'dr1', 0,0,0,0, nil, nil, 0)",
            )
            .exec()
            .unwrap();
        let anim = |lua: &Lua, n: &str| boot_ref(lua).unwrap().playing.get(n).copied();
        // fifteen units out: inside the default 20, outside the payload's 8
        prox_doors(&scripts.lua, [15.0, 0.0, 0.0]).unwrap();
        assert_eq!(anim(&scripts.lua, "near"), None, "8 away is not 15");
        assert_eq!(anim(&scripts.lua, "far"), Some(ANIM_OPEN), "a payload of 0 reaches 20");
        // walk in, and the near one opens too
        prox_doors(&scripts.lua, [0.0, 5.0, 0.0]).unwrap();
        assert_eq!(anim(&scripts.lua, "near"), Some(ANIM_OPEN));
        // walk out, and both shut
        prox_doors(&scripts.lua, [100.0, 0.0, 0.0]).unwrap();
        assert_eq!(anim(&scripts.lua, "near"), Some(ANIM_CLOSE));
        assert_eq!(anim(&scripts.lua, "far"), Some(ANIM_CLOSE));
        // a locked door stays shut however near he stands
        scripts.lua.load("mdkProxDoorLock(near, 1)").exec().unwrap();
        prox_doors(&scripts.lua, [0.0, 0.0, 0.0]).unwrap();
        assert_eq!(anim(&scripts.lua, "near"), Some(ANIM_CLOSE), "locked");
        assert_eq!(anim(&scripts.lua, "far"), Some(ANIM_OPEN), "and the other is not");
        // and unlocking lets the next tick notice him
        scripts.lua.load("mdkProxDoorLock(near, 0)").exec().unwrap();
        prox_doors(&scripts.lua, [0.0, 0.0, 0.0]).unwrap();
        assert_eq!(anim(&scripts.lua, "near"), Some(ANIM_OPEN));
    }

    /// A landing reaches the player, and it reaches him **because his own
    /// filter lets it**: Kurt's 0x9d6 out of 0x4168b8 has `DAMAGE_FALLING`
    /// in it, which is also why `boss.lua:628` has to subtract the bit to
    /// stop him taking fall damage in a boss fight.
    #[test]
    fn a_fall_takes_the_player_s_hitpoints() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('kurt', OBJ_KURT, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)",
            )
            .exec()
            .unwrap();
        let left = |lua: &Lua| {
            world::world(lua).unwrap().get(world::world(lua).unwrap().find("kurt").unwrap())
                .unwrap().hitpoints
        };
        assert_eq!(left(&scripts.lua), 100);
        // a fall of 95 units a second, which is exactly the whole of him
        let cost = crate::game::body::fall_damage(95.0);
        assert!(hurt(&scripts.lua, "kurt", cost, DAMAGE_FALLING));
        assert_eq!(left(&scripts.lua), 0, "{cost} hitpoints of fall");
    }

    /// The filter gates the built-in path and only that: a kind the object
    /// is not vulnerable to does nothing, and neither does an amount of zero.
    #[test]
    fn the_filter_and_the_amount_both_gate_the_built_in() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('gen', OBJ_NONE, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkSpawnerSetSpawnedObject(gen, OBJ_GRUNT, nil, 0,0,0,0, 1, nil)\n\
                 g = mdkSpawnerSpawnObject(gen)\n\
                 mdkDealDamage(gen, g, 10, DAMAGE_BADGUY, -1)\n\
                 wrong_kind = mdkGetHitpoints(g)\n\
                 mdkDealDamage(gen, g, 0, DAMAGE_GOODGUY, -1)\n\
                 no_amount = mdkGetHitpoints(g)\n\
                 mdkGobSetDamageFilter(g, 0)\n\
                 mdkDealDamage(gen, g, 10, DAMAGE_GOODGUY, -1)\n\
                 invulnerable = mdkGetHitpoints(g)",
            )
            .exec()
            .unwrap();
        let g = scripts.lua.globals();
        for name in ["wrong_kind", "no_amount", "invulnerable"] {
            assert_eq!(g.get::<f64>(name).unwrap(), 40.0, "{name} should have done nothing");
        }
    }

    /// The one that would have been invented backwards: a script's own
    /// `OnDamage` **replaces** the built-in rather than running beside it,
    /// and it is called whatever the filter says.
    #[test]
    fn a_scripted_handler_takes_the_damage_instead_of_the_engine() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('gen', OBJ_NONE, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkSpawnerSetSpawnedObject(gen, OBJ_GRUNT, nil, 0,0,0,0, 1, nil)\n\
                 g = mdkSpawnerSpawnObject(gen)\n\
                 seen = 0\n\
                 part_was = 'unset'\n\
                 g.OnDamage = function(gob, from, n, kind, part)\n\
                   seen = seen + n; part_was = part\n\
                 end\n\
                 mdkDealDamage(gen, g, 7, DAMAGE_BADGUY, -1)\n\
                 left = mdkGetHitpoints(g)\n\
                 mdkWalkerDefaultOnDamage(g, gen, 7, DAMAGE_GOODGUY, -1)\n\
                 then_left = mdkGetHitpoints(g)",
            )
            .exec()
            .unwrap();
        let g = scripts.lua.globals();
        assert_eq!(g.get::<f64>("seen").unwrap(), 7.0, "the handler ran");
        assert_eq!(g.get::<f64>("left").unwrap(), 40.0, "and the engine did not");
        assert_eq!(
            g.get::<Value>("part_was").unwrap(),
            Value::Nil,
            "part -1 reaches Lua as nil, not as a number"
        );
        // and the default the handler can call back into does hit, without
        // asking for OnDamage a second time and recursing
        assert_eq!(g.get::<f64>("then_left").unwrap(), 33.0);
    }

    /// A spawner, driven the way `level1.lua` drives one: set it up, queue
    /// one, and let the clock run. No level in the game puts the player in
    /// front of a spawner within the seconds a driver check runs for, so
    /// this is the only place the countdown is exercised at all.
    #[test]
    fn a_queued_spawner_makes_one_object_per_interval() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        // the shape `level1.lua` uses, with a 3-second interval
        scripts
            .lua
            .load(
                "mdkRegisterObject('gen', OBJ_NONE, scene, nil, -1, 5,6,7, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkSpawnerSetSpawnedObject(gen, OBJ_DOGANBOY, 'wp', 60,0.7,0,0, 3, nil)\n\
                 mdkSpawnerQueue(gen, 2)",
            )
            .exec()
            .unwrap();

        let rooms = Visibility::default();
        let mut state = Ticking::default();
        let step = |state: &mut Ticking| {
            tick(&scripts, &rooms, [0.0; 3], 0.0, 1.0, state).unwrap()
        };

        // the queue was empty, so the countdown was reset and the first one
        // arrives on the tick after the call rather than an interval later
        step(&mut state);
        let spawned = |lua: &mlua::Lua| lua.app_data_ref::<Boot>().unwrap().spawned.clone();
        assert_eq!(spawned(&scripts.lua).len(), 1, "the first comes at once");
        for _ in 0..2 {
            step(&mut state);
        }
        assert_eq!(spawned(&scripts.lua).len(), 1, "and the next waits out the 3s");
        step(&mut state);
        let made = spawned(&scripts.lua);
        assert_eq!(made.len(), 2);
        assert_eq!(made[0].0, "gen_spawn", "named after the spawner, once");

        // it stands where the spawner stands, wears the waypoint, carries
        // the four numbers, and -- the point of all of it -- has hitpoints
        let w = world::world(&scripts.lua).unwrap();
        let g = w.get(w.find("gen_spawn").unwrap()).unwrap();
        assert_eq!(g.position, [5.0, 6.0, 7.0]);
        assert_eq!(g.resource.as_deref(), Some("wp"));
        assert_eq!(g.payload, [60.0, 0.7, 0.0, 0.0]);
        assert_eq!(g.hitpoints, 100, "OBJ_DOGANBOY, on Hard");
        drop(w);

        // nothing is owed now, so the clock may run without making any more
        for _ in 0..10 {
            step(&mut state);
        }
        assert_eq!(spawned(&scripts.lua).len(), 2);
    }

    /// Shutting one off empties the queue and is permanent — `level1.lua`
    /// does it when the generator is destroyed, and nothing turns it back on,
    /// **including setting it up again**, which that same script then does.
    #[test]
    fn a_spawner_that_has_been_shut_off_stays_off() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('gen', OBJ_NONE, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkSpawnerSetSpawnedObject(gen, OBJ_GRUNT, nil, 0,0,0,0, 1, nil)\n\
                 mdkSpawnerQueue(gen, 5)\n\
                 mdkSpawnerShutOff(gen)\n\
                 mdkSpawnerQueue(gen, 5)\n\
                 mdkSpawnerSetSpawnedObject(gen, OBJ_GRUNT, nil, 0,0,0,0, 1, nil)\n\
                 mdkSpawnerQueue(gen, 5)",
            )
            .exec()
            .unwrap();

        let rooms = Visibility::default();
        let mut state = Ticking::default();
        for _ in 0..20 {
            tick(&scripts, &rooms, [0.0; 3], 0.0, 1.0, &mut state).unwrap();
        }
        assert!(scripts.lua.app_data_ref::<Boot>().unwrap().spawned.is_empty());
    }

    /// `mdkSpawnerSpawnObject` goes round the queue entirely: `boss.lua`
    /// calls it three times in a row and expects three.
    #[test]
    fn spawning_by_hand_ignores_the_queue_and_the_clock() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('gen', OBJ_NONE, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 seen = 0\n\
                 gen.OnSpawn = function(g, made) seen = seen + 1 end\n\
                 mdkSpawnerSetSpawnedObject(gen, OBJ_ZIZZY, nil, 0,0,0,0, 99, nil)\n\
                 mdkSpawnerSpawnObject(gen)\n\
                 mdkSpawnerSpawnObject(gen)\n\
                 mdkSpawnerSpawnObject(gen)",
            )
            .exec()
            .unwrap();

        let made = scripts.lua.app_data_ref::<Boot>().unwrap().spawned.clone();
        assert_eq!(made.len(), 3, "three calls, three objects");
        assert!(made.iter().all(|(_, hp)| *hp == 2000), "zizzy on Hard");
        assert_eq!(scripts.lua.globals().get::<i64>("seen").unwrap(), 3, "OnSpawn each time");
    }

    /// A spawner nobody set up makes nothing rather than an object of type
    /// zero — the original tests its type field and returns null.
    #[test]
    fn an_unconfigured_spawner_makes_nothing() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua, Default::default()).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('gen', OBJ_NONE, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 made = mdkSpawnerSpawnObject(gen)\n\
                 mdkSpawnerQueue(gen, 3)",
            )
            .exec()
            .unwrap();
        assert_eq!(scripts.lua.globals().get::<Value>("made").unwrap(), Value::Nil);
        assert!(scripts.lua.app_data_ref::<Boot>().unwrap().spawned.is_empty());
    }

    /// Every one of the eight has to exist in the table the binary
    /// registers, or the engine is naming animations the game does not have.
    #[test]
    fn every_locomotion_name_is_one_the_binary_defines() {
        for name in [
            "ANIM_DEFAULT", "ANIM_RUNF", "ANIM_RUNB", "ANIM_RUNL", "ANIM_RUNR",
            "ANIM_RUNFL", "ANIM_RUNFR", "ANIM_RUNBL", "ANIM_RUNBR",
        ] {
            assert!(
                crate::game::constants::CONSTANTS.iter().any(|(n, _)| *n == name),
                "{name} is not a constant the game defines"
            );
        }
    }
}
