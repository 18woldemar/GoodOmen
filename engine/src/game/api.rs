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
            self.from[0] + self.arc.heading.cos() * self.arc.speed * t,
            self.from[1] + self.arc.heading.sin() * self.arc.speed * t,
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
        heading: d[1].atan2(d[0]),
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
    let scene = lua.create_function(|_, ()| Ok("scene"))?;
    globals.set("mdkGetScene", &scene)?;
    globals.set("mdkGetGuiScene", &scene)?;

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
                let heading = (to[1] - at[1]).atan2(to[0] - at[0]);
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
                let d = [to[0] - at[0], to[1] - at[1], to[2] - at[2]];
                let dist = if direct {
                    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
                } else {
                    (d[0] * d[0] + d[1] * d[1]).sqrt()
                };
                if dist < radius {
                    return Ok(1.0);
                }
                let heading = d[1].atan2(d[0]);
                let moving = !must_face || facing(yaw, heading);
                let mut boot = boot_mut(lua)?;
                boot.heading.insert(who.clone(), heading);
                boot.gait.insert(who, if !moving { 0 } else if run { 2 } else { 1 });
                Ok(0.0)
            })?,
        )?;
    }
    globals.set(
        "mdkWalkerStop",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(who) = args.first().and_then(gob_name) else { return Ok(0.0) };
            let Some((_, yaw)) = stance(lua, &who) else { return Ok(0.0) };
            let mut boot = boot_mut(lua)?;
            boot.heading.insert(who.clone(), yaw);
            boot.gait.insert(who, 0);
            Ok(1.0)
        })?,
    )?;

    // `mdkDoganboyAttack(gob)` — 0x440380 into **0x4324f0**, class 4's slot
    // +0x2c and the enemy AI: a twelve-state machine on `walker + 0x7c` whose
    // jump table is at 0x433a38. Every one of the 41 script sites has it as
    // the **last** task in a list, and it returns 0 forever, which is how a
    // task list ends in something rather than finishing.
    //
    // Three of the twelve are built, and they are the three an enemy spends
    // most of its time in:
    //
    // - **no target at all** (0x42a850 answers nothing): gait 0, strafe 0,
    //   state 3, cooldown **0.3** (the float at 0x43254c), and return.
    // - **state 4**, stand ready and aim: the heading goes to the bearing to
    //   the target and the gait to 0, so the walker turns on the spot.
    // - **state 2**, back away: inside the record's near distance and with
    //   the cooldown spent, the gait goes to **3** — backwards, the negative
    //   speed in the enemy table — while the heading stays on the target. It
    //   walks backwards facing you.
    //
    // What is *not* built is the firing, and the reason is honest rather than
    // tidy: the branches that fire are states 0, 1, 5, 7, 8 and 11, which are
    // unread. What state 3 picks in the branches that *are* read is a taunt
    // (`ANIM_TAUNT0`, 0x70) or `ANIM_SCARED` (0x12), not an attack.
    //
    // The lead is left out too. 0x432d90 aims at `target + velocity * (dist *
    // 0.025)` when the record's flag is set, and the arena keeps no velocity
    // for a gob.
    globals.set(
        "mdkDoganboyAttack",
        lua.create_function(|lua, args: Variadic<Value>| {
            let Some(who) = args.first().and_then(gob_name) else { return Ok(0.0) };
            let Some((at, _)) = stance(lua, &who) else { return Ok(0.0) };
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
            let moving = hero.as_deref().and_then(|n| boot.velocity.get(n)).copied();
            let d = match moving {
                Some(v) if lead => [0, 1, 2].map(|c| d[c] + v[c] * dist * LEAD),
                _ => d,
            };
            boot.heading.insert(who.clone(), d[1].atan2(d[0]));
            let cool = boot.cooldown.get(&who).copied().unwrap_or(0.0);
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
            if cool > 0.0 && boot.gait.get(&who) == Some(&2) {
                return Ok(0.0);
            }
            if let Some(rec) = crate::game::world::ai(kind) {
                let choosing = cool <= 0.0 && left <= 0.0 && dist >= near;
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
                    let show = if hurt && boot.random.next() < rec.scared {
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
            let gait = if dist < near && cool <= 0.0 { 3 } else { 0 };
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
                let mut off = to_target[1].atan2(to_target[0]) - facing;
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
    // `mdkDocHasItem(gob, OBJ_*)` — Doc's inventory, which the engine does
    // not hold. **0 is what a fresh game answers**, so this is right until
    // there is an inventory and wrong only in the same place a real one
    // would be.
    for (name, answer) in [
        ("mdkIsCutSceneAllowed", 1.0),
        ("chIsLoadingResources", 0.0),
        ("mdkDialogIsDone", 1.0),
        ("mdkDocHasItem", 0.0),
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
                    boot_mut(lua)?.player = Some(name);
                }
            }
            Ok(())
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
                boot.since.insert(name, 0.0);
            }
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
            Ok(boot_ref(lua)?.playing.contains_key(&name) as i32)
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
pub fn model_for_type(kind: f64) -> Option<String> {
    if let Some(m) = crate::game::world::table_model(kind) {
        return Some(m.to_string());
    }
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
            if flat > REACH || flat <= 0.0 || !facing_within(yaw, d[1].atan2(d[0]), CONE) {
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
    let Some((_, filter, damage, life, speed, lead, flags)) = crate::game::world::bullet(kind) else {
        return Ok(()); // an effect or a prop, and the engine has nowhere to put it
    };
    let mut direction = [yaw.cos(), yaw.sin(), 0.0];
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
const DBGRENADE: f64 = 417.0;

/// `ANIM_SHOOT` and `ANIM_THROW`. 0x4331f8 plays the first for every round of
/// a burst and 0x433185 the second for a grenade; the projectile in both
/// cases comes off the animation's key channel.
const ANIM_SHOOT: f64 = 56.0;
const ANIM_THROW: f64 = 57.0;

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
            .map(|(_, g)| g.name.clone())
            .filter(|n| !boot.stasis.contains(n))
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
    let steps: Vec<(world::Id, String, [f64; 3], f64, f64, f64, f64, bool)> = {
        let boot = boot_ref(&scripts.lua)?;
        let Some(w) = crate::game::world::world(&scripts.lua) else {
            return Err(Error::Pragma("no world".into()));
        };
        boot.gait
            .iter()
            .filter(|(name, _)| !boot.stasis.contains(*name) && !boot.jumps.contains_key(*name))
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
                if !turning && speed == 0.0 {
                    return None; // standing still and already square
                }
                let step = (dt * turn).min(d.abs()) * d.signum();
                let yaw = if turning { yaw + step } else { yaw };
                // `def + 0x78`, and the player's height for a type the table
                // does not name — a spawner's own gob, say
                let (tall, wide) = crate::game::world::size(g.kind)
                    .unwrap_or((crate::game::body::EYE, 0.0));
                Some((id, name.clone(), g.position, yaw, speed, tall, wide, turning))
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
    for (id, name, from, yaw, speed, tall, wide, turning) in steps {
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
                body.step(world, [yaw.cos(), yaw.sin()], false, speed, dt);
                let p = body.position;
                [p[0], p[1], p[2] - tall]
            }
            // no level loaded: a test, and there is nothing to walk into
            None => [
                from[0] + yaw.cos() * speed * dt,
                from[1] + yaw.sin() * speed * dt,
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
            .filter(|(name, _)| !boot.stasis.contains(*name) && !boot.jumps.contains_key(*name))
            .filter_map(|(name, &gait)| {
                let g = w.get(w.find(name)?)?;
                let want = crate::game::world::gait_animation(g.kind, gait, g.hitpoints)?;
                let now = boot.playing.get(name).copied();
                let idle = now.is_none_or(|a| {
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
                    Some((g.name.clone(), *boot.playing.get(&g.name)?, model_for_type(g.kind)?))
                })
                .collect()
        };
        let (mut out, mut advanced) = (Vec::new(), Vec::new());
        for (name, anim, model) in live {
            let was = boot.since.get(&name).copied().unwrap_or(0.0);
            let now = was + dt;
            if let Some(keys) = boot.keys.get(&model) {
                for &(a, at, code) in keys {
                    if a == anim && at > was && at <= now {
                        out.push((name.clone(), code));
                    }
                }
            }
            advanced.push((name, now));
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
        boot.scripted.iter().filter(|n| !boot.stasis.contains(*n)).cloned().collect()
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
                "points.side = {x = 0, y = 10, z = 0, f = 0}\n\
                 mdkRegisterObject('w',     OBJ_NONE, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('ahead', OBJ_NONE, scene, nil, -1, 10,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('askew', OBJ_NONE, scene, nil, -1, 10,1.4,0, \
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
        assert!((want - std::f64::consts::FRAC_PI_2).abs() < 1e-9, "due +y");
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
                // the walker faces +x and stands at the origin; the waypoint
                // is due +y, ten out and three up
                "points.wp = {x = 0, y = 10, z = 3, f = 0}\n\
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
            // the direct call ran last and the walker still faces +x, a right
            // angle off the waypoint, so it turns before it moves
            assert_eq!(boot.gait["w"], 0, "not facing yet");
            assert!((boot.heading["w"] - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        }
        // now put it square on the waypoint's x/y and ask again. The plain
        // goto is horizontal, so three units of height is arrival; the direct
        // one measures the height too, so three units is not.
        scripts
            .lua
            .load(
                "mdkGobSetPositionXYZ(w, 0, 10, 0)\n\
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
                // OBJ_GRUNT is 202, and it faces +x with the waypoint due -x
                "points.wp = {x = -400, y = 0, z = 0, f = 0}\n\
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
            if i == 0 {
                turned_before_moving = Some(after[0] - before[0] > 0.0);
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
        assert!(at[0] < -40.0, "and it ends up toward the waypoint, at {at:?}");
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
            assert!((boot.heading["d"] - std::f64::consts::FRAC_PI_2).abs() < 1e-9);
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
                // due +x, a hundred out, and the walker already faces it
                "points.wp = {x = 100, y = 0, z = 0, f = 0}\n\
                 mdkRegisterObject('d', 207, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
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
            w.get(w.find("d").unwrap()).unwrap().position[0]
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
                // the player faces +x; one conehead ahead, one behind, one
                // beyond the hundred
                "mdkRegisterObject('kurt', 100, scene, nil, -1, 0,0,0,                  1,0,0,0, nil,0,0,0,0, nil, nil, 0)
                 mdkRegisterObject('ahead', 203, scene, nil, -1, 30,0,0,                  1,0,0,0, nil,0,0,0,0, nil, nil, 0)
                 mdkRegisterObject('behind', 203, scene, nil, -1, -30,0,0,                  1,0,0,0, nil,0,0,0,0, nil, nil, 0)
                 mdkRegisterObject('far', 203, scene, nil, -1, 300,0,0,                  1,0,0,0, nil,0,0,0,0, nil, nil, 0)
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
        assert_eq!(state(&scripts).0, 3, "three seconds later it may again");
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
        // the goto had asked for +y; the stop replaced that with the gob's
        // own facing, which is still +x
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
                "points.ledge = {x = 0, y = 10, z = 4, f = 0}\n\
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
                (boot.heading["w"] - std::f64::consts::FRAC_PI_2).abs() < 1e-9,
                "the waypoint is due +y"
            );
            boot.jumps["w"]
        };
        let end = jump.at(jump.arc.time);
        assert!((end[1] - 10.0).abs() < 1e-9 && (end[2] - 4.0).abs() < 1e-9, "{end:?}");
        // a quarter turn about Z, which is half of it in the quaternion
        let w = world::world(&scripts.lua).unwrap();
        let q = w.get(w.find("w").unwrap()).unwrap().rotation;
        let half = std::f64::consts::FRAC_PI_4;
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
                // the watcher faces +x (identity quaternion); one target is
                // straight ahead, one straight behind
                "mdkRegisterObject('eye',    OBJ_NONE, scene, nil, -1, 0,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('ahead',  OBJ_NONE, scene, nil, -1, 10,0,0, \
                 1,0,0,0, nil,0,0,0,0, nil, nil, 0)\n\
                 mdkRegisterObject('behind', OBJ_NONE, scene, nil, -1, -10,0,0, \
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
