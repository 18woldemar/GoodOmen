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
    /// `(near, far)` from `chFogStartEnd` — the game's own draw distance.
    pub fog: Option<(f64, f64)>,
    /// Objects frozen until the player arrives — a level holds its encounters
    /// this way, and a boot of all ten puts hundreds there.
    pub stasis: BTreeSet<String>,
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
                Some(Value::String(name)) => {
                    let points = lua.globals().get::<mlua::Table>("points")?;
                    points
                        .get::<Option<mlua::Table>>(name.to_string_lossy().to_string())?
                        .map(|p| {
                            [
                                p.get::<f64>(1).unwrap_or(0.0),
                                p.get::<f64>(2).unwrap_or(0.0),
                                p.get::<f64>(3).unwrap_or(0.0),
                            ]
                        })
                }
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
                boot_mut(lua)?.playing.insert(name, id);
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
            boot.fog = Some((
                args.first().map(number).unwrap_or(0.0),
                args.get(1).map(number).unwrap_or(0.0),
            ));
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
            });
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
pub fn model_for_type(kind: f64) -> Option<String> {
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

/// `OnDie(gob, 1)`, from 0x40e1b0 — **two arguments**, the object and a
/// literal 1.0 the original pushes as a number.
fn die(lua: &Lua, name: &str) -> mlua::Result<()> {
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

/// A gob's name, from the table the scripts hold it by.
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
                if s.starts_with("On") {
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
        if handler
            .call::<Value>((gob.clone(), gob.clone(), 1, DAMAGE_GOODGUY, 1))
            .is_ok()
        {
            entry.1 += 1;
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
        place(
            &scripts.lua,
            &player,
            [at[0], at[1], at[2] - crate::game::body::EYE],
        )?;
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

    // the clock the scripts read
    boot_mut(&scripts.lua)?.clock = state.clock;
    boot_mut(&scripts.lua)?.delta = dt;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
