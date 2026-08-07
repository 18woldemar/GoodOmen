//! The objects, and the one Lua function that makes them.
//!
//! An object — a *gob*, the original's word — lives in an **arena indexed by
//! id**, and that is not a preference: it is the game's own model.
//! `mdkRegisterObject` does `_G[name] = gob`, so the scripts hold objects by
//! name and reach them as globals, and `mdkGetPlayerGob()` hands one back.
//! A pointer graph would be a translation of that; an arena is a copy of it.
//!
//! Two things about the scene graphs are easy to get wrong and both are
//! silent:
//!
//! - **Names are not unique.** 74 of the 5633 objects repeat a name, up to
//!   three times (`l4_light203`), and `l4_r5entrdoor` is two different doors.
//!   Since registering assigns a global, **the last one wins** and the
//!   earlier objects cannot be reached from any script — so the arena keeps
//!   all of them and the name index keeps the last, exactly as Lua does.
//! - **The bounding box is in the model's frame**, not the world's. Never add
//!   the object's position to it.
//!
//! The Lua side of a gob is a *table*, because the scripts treat it as one:
//! they hang handlers on it (`NAME.OnEnterRoom = function ...`), add fields
//! (`doorwav.wav = omGobAddSound(...)`), and read `gob.position`. The table
//! carries `__gob`, its id in the arena, and the arena carries the truth.

use crate::game::script::Error;
use mlua::{Lua, Table, Value};

pub type Id = u32;

/// One object, as `mdkRegisterObject` describes it. The argument layout is
/// stated outright by the original's own binding at 0x43ac80, which pushes
/// each parameter *number* as a literal before fetching it and uses three
/// different getters — number, string, vector.
#[derive(Clone, Debug, Default)]
pub struct Gob {
    pub name: String,
    /// One of the `OBJ_*` constants — `OBJ_ROOM` is 803, `OBJ_SCENERY` 800.
    pub kind: f64,
    pub parent: Option<Id>,
    pub group: f64,
    pub position: [f64; 3],
    /// `(w, x, y, z)`, the order the models use too.
    pub rotation: [f64; 4],
    /// Polymorphic: a `.mod` usually, a `.wav` for `OBJ_AMBIENTSOUND`, a
    /// `.tex` for `OBJ_STARS`, and a **waypoint name** for every character.
    pub resource: Option<String>,
    /// Four numbers whose meaning is set by the object's type.
    pub payload: [f64; 4],
    /// Hitpoints and the most it can have. Both are **`i16`** in the
    /// original — `gob + 0x84` is its `omgob`, and 0x10, 0x12, 0x14 there are
    /// the damage filter, the hitpoints and the maximum. See
    /// [`crate::game::api`] for the arithmetic, which has two rules that are
    /// easy to get wrong and are in the code rather than invented.
    pub hitpoints: i16,
    pub max_hitpoints: i16,
    /// A bitmask of `DAMAGE_*`. 13 flags, every one a power of two, so all of
    /// them together are 8191 and fit the `i16` the original keeps them in.
    pub damage_filter: i16,
    pub bbox_min: Option<[f64; 3]>,
    pub bbox_max: Option<[f64; 3]>,
    pub flag: f64,
}

/// The four difficulties the menu offers, and the number each one hands to
/// `mdkSetDifficulty`. Out of `scripts/menu.lua` (the only caller in the whole
/// game) and `mdk2.str` 680, 681, 682 and **685** for the names — the fourth
/// is not 683, which is "Configure Joystick".
///
/// The number is not the multiplier: [`diff_scale`] doubles it first, so
/// **Hard is 1.0x and the table below is what you fight on Hard.**
pub const DIFFICULTY: [(&str, f32); 4] = [
    ("Easy", 0.2),
    ("Medium", 0.35),
    ("Hard", 0.5),
    ("Jinkies!", 1.0),
];

/// Ours. The original leaves the global at 0x4bb71c **uninitialised** — the
/// bytes in the file are `00 ff ff 00`, and `mdkSetDifficulty` from the
/// new-game menu is its only writer, so a level reached any other way scales
/// its enemies by whatever was in that memory. Hard is the one that makes
/// [`BASE_HITPOINTS`] literal.
pub const DEFAULT_DIFFICULTY: f32 = DIFFICULTY[2].1;

/// **The enemy table**, at 0x4ab2e8 in the original: 19 records of 0x88 bytes,
/// terminated by a zero first field. Field 0 is the `OBJ_*` type, +0x04 the
/// name below, +0x3c the base hitpoints. The constructor at 0x42f2e0 walks it
/// linearly for a matching type and passes +0x3c through [`diff_scale`].
///
/// Five of the types have no `OBJ_*` constant, so they are unreachable from a
/// script and named here only by the table's own string. `grunt` appears
/// twice because `OBJ_INVISOGRUNT` is a grunt with a separate record.
///
/// Hunting for a *constant* here found nothing for a whole session: the
/// hitpoints are not literals in the code, they are this table scaled at run
/// time. See [`crate::game::api`] for the rest of the damage model.
pub const BASE_HITPOINTS: [(f64, &str, i32); 19] = [
    (201.0, "samsmite", 20),   // OBJ_SAMSMITE
    (215.0, "samfire", 20),    // no OBJ_ constant
    (216.0, "samrock", 20),    // OBJ_OBSIDIANSAMSMITE
    (203.0, "conehead", 45),   // OBJ_CONEHEAD
    (250.0, "coneciv", 50),    // OBJ_CONEHEADCIV1
    (200.0, "bif", 400),       // OBJ_BIF
    (204.0, "hans", 700),      // OBJ_HANS
    (205.0, "hoser", 55),      // OBJ_HOSER
    (202.0, "grunt", 40),      // OBJ_GRUNT
    (219.0, "grunt", 40),      // OBJ_INVISOGRUNT
    (207.0, "doganboy", 100),  // OBJ_DOGANBOY
    (214.0, "ultradogan", 225),
    (211.0, "bfb", 500),
    (220.0, "shwang", 1000), // OBJ_SHWANG
    (210.0, "badmax", 5000),
    (217.0, "poopsy", 65), // OBJ_POOPSY
    (206.0, "angel", 500), // OBJ_ANGEL
    (208.0, "birdbrain1", 200),
    (260.0, "zizzy", 2000), // OBJ_ZIZZY
];

/// The base hitpoints for a type, if it is one the table names.
pub fn base_hitpoints(kind: f64) -> Option<i32> {
    BASE_HITPOINTS.iter().find(|(k, _, _)| *k == kind).map(|(_, _, hp)| *hp)
}

/// Scale a base by the difficulty, exactly as 0x42d020 does — the routine the
/// scripts see as `mdkDiffScale`.
///
/// Two details are in the code and would not be guessed. The multiplier is
/// **twice** the difficulty (`fadd st,st` before the multiply), which is why
/// Hard's 0.5 leaves the base alone. And a **non-zero base never scales to
/// zero**: the result is pushed off zero to ±1 in whichever direction the base
/// went, so the weakest enemy on Easy still takes a hit to kill.
pub fn diff_scale(difficulty: f32, base: i32) -> i32 {
    // `ftol` truncates toward zero, which `as i32` also does
    let scaled = (2.0 * difficulty * base as f32) as i32;
    match (scaled, base) {
        (0, b) if b > 0 => 1,
        (0, b) if b < 0 => -1,
        _ => scaled,
    }
}

/// The damage filter every type constructor writes as a literal **9** —
/// `DAMAGE_GOODGUY | DAMAGE_SNIPER`, the two kinds an enemy is hurt by.
pub const FILTER_GOODGUY_SNIPER: i16 = 9;

#[derive(Default)]
pub struct World {
    gobs: Vec<Gob>,
    /// Name to id, **last registration winning**, because that is what
    /// `_G[name] = gob` does.
    by_name: std::collections::HashMap<String, Id>,
    /// Bumped by every move, so nothing has to diff the world to notice one.
    generation: u64,
    /// `None` until a script picks one, which is the original's state too —
    /// see [`DEFAULT_DIFFICULTY`] for what that costs it.
    difficulty: Option<f32>,
}

impl World {
    pub fn new() -> World {
        World::default()
    }

    pub fn register(&mut self, mut gob: Gob) -> Id {
        // The original builds the gob and then constructs it by type, and the
        // enemy constructor is where the hitpoints come from. Here that is one
        // step, so the table is applied on the way in.
        if let Some(base) = base_hitpoints(gob.kind) {
            gob.max_hitpoints = diff_scale(self.difficulty(), base) as i16;
            gob.hitpoints = gob.max_hitpoints;
            gob.damage_filter = FILTER_GOODGUY_SNIPER;
        }
        let id = self.gobs.len() as Id;
        self.by_name.insert(gob.name.clone(), id);
        self.gobs.push(gob);
        id
    }

    /// What `mdkGetDifficulty` answers, and what [`diff_scale`] is fed.
    pub fn difficulty(&self) -> f32 {
        self.difficulty.unwrap_or(DEFAULT_DIFFICULTY)
    }

    pub fn set_difficulty(&mut self, d: f32) {
        self.difficulty = Some(d);
    }

    /// Give something a maximum and fill it — what `mdkCreateDestructable`
    /// (0x440e00, into the constructor at 0x424f00) does with the base a
    /// script hands it. The **negative fallback** is the original's: if the
    /// scaled value comes out below zero it keeps the base instead.
    pub fn make_destructable(&mut self, id: Id, base: i32) {
        let scaled = diff_scale(self.difficulty(), base) as i16;
        let Some(gob) = self.gobs.get_mut(id as usize) else { return };
        gob.max_hitpoints = if scaled >= 0 { scaled } else { base as i16 };
        gob.hitpoints = gob.max_hitpoints;
        gob.damage_filter = FILTER_GOODGUY_SNIPER;
    }

    /// Move an object. The scripts do this through `mdkGobSetPosition` and
    /// `mdkGobSetPositionXYZ`, and it is what makes a door a door.
    ///
    /// The **generation** counts every move, so a renderer can tell whether
    /// anything has happened without comparing every transform.
    pub fn set_position(&mut self, id: Id, at: [f64; 3]) {
        if let Some(gob) = self.gobs.get_mut(id as usize) {
            if gob.position != at {
                gob.position = at;
                self.generation += 1;
            }
        }
    }

    /// Turn an object. The player's does this every tick to face where the
    /// body is walking.
    pub fn set_rotation(&mut self, id: Id, q: [f64; 4]) {
        if let Some(gob) = self.gobs.get_mut(id as usize) {
            if gob.rotation != q {
                gob.rotation = q;
                self.generation += 1;
            }
        }
    }

    /// Take `n` hitpoints off, and say whether that killed it.
    ///
    /// Read out of the original at **0x40e8c0**, and both halves matter:
    /// hitpoints **clamp at zero** rather than going negative, and the
    /// routine's return value *is* the death condition — the engine does not
    /// test for it separately.
    pub fn hurt(&mut self, id: Id, n: i16) -> bool {
        let Some(gob) = self.gobs.get_mut(id as usize) else { return false };
        gob.hitpoints = gob.hitpoints.saturating_sub(n);
        if gob.hitpoints <= 0 {
            gob.hitpoints = 0;
            return true;
        }
        false
    }

    /// Put `n` hitpoints back, up to the maximum.
    ///
    /// From 0x40e8f0, and the first line is the one worth having: **something
    /// already at zero is not healed at all.** A corpse stays a corpse, and
    /// an implementation that only added and clamped would quietly bring the
    /// dead back.
    pub fn heal(&mut self, id: Id, n: i16) {
        let Some(gob) = self.gobs.get_mut(id as usize) else { return };
        if gob.hitpoints <= 0 {
            return;
        }
        gob.hitpoints = gob.hitpoints.saturating_add(n).min(gob.max_hitpoints);
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn get(&self, id: Id) -> Option<&Gob> {
        self.gobs.get(id as usize)
    }

    pub fn find(&self, name: &str) -> Option<Id> {
        self.by_name.get(name).copied()
    }

    pub fn len(&self) -> usize {
        self.gobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.gobs.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (Id, &Gob)> {
        self.gobs.iter().enumerate().map(|(i, g)| (i as Id, g))
    }

    /// Forget every object, which is **step two of starting a level** —
    /// `mdk2.lua` lists it as such.
    pub fn clear(&mut self) {
        self.gobs.clear();
        self.by_name.clear();
    }
}

fn number(v: &Value) -> f64 {
    match v {
        Value::Number(n) => *n,
        Value::Integer(i) => *i as f64,
        _ => 0.0,
    }
}

fn vector(v: &Value) -> Option<[f64; 3]> {
    let t = match v {
        Value::Table(t) => t,
        _ => return None,
    };
    Some([
        t.get::<f64>(1).unwrap_or(0.0),
        t.get::<f64>(2).unwrap_or(0.0),
        t.get::<f64>(3).unwrap_or(0.0),
    ])
}

/// Install `mdkRegisterObject`, and the two globals the scene graphs expect
/// to find already there.
///
/// The arena lives in the Lua state's app data, so the closure reaches it
/// without an `Rc<RefCell<_>>` anywhere in the engine's own types.
pub fn install(lua: &Lua) -> Result<(), Error> {
    lua.set_app_data(World::new());
    let globals = lua.globals();

    // `scene` is what the engine passes as the third argument of every
    // registration, and `points` is where the waypoints go. A scene graph
    // reads both before it writes anything.
    globals.set("scene", "scene")?;
    globals.set("points", lua.create_table()?)?;

    let register = lua.create_function(|lua, args: mlua::MultiValue| {
        let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Nil);
        let name: String = match arg(0) {
            Value::String(s) => s.to_string_lossy().to_string(),
            other => other.to_string()?,
        };
        // the parent comes in as another gob's table, and its id is in it
        let parent = match arg(3) {
            Value::Table(t) => t.get::<Option<Id>>("__gob").unwrap_or(None),
            _ => None,
        };
        let gob = Gob {
            name: name.clone(),
            kind: number(&arg(1)),
            parent,
            group: number(&arg(4)),
            position: [number(&arg(5)), number(&arg(6)), number(&arg(7))],
            rotation: [
                number(&arg(8)),
                number(&arg(9)),
                number(&arg(10)),
                number(&arg(11)),
            ],
            resource: match arg(12) {
                Value::String(s) => Some(s.to_string_lossy().to_string()),
                _ => None,
            },
            payload: [
                number(&arg(13)),
                number(&arg(14)),
                number(&arg(15)),
                number(&arg(16)),
            ],
            // The scene graph carries none of this -- no object spends a
            // payload slot on health -- and the original's type constructors
            // set it. `World::register` fills all three in from the type,
            // because that is the step the original takes next.
            hitpoints: 0,
            max_hitpoints: 0,
            damage_filter: 0,
            bbox_min: vector(&arg(17)),
            bbox_max: vector(&arg(18)),
            // OBJ_STATICLIGHT omits the trailing flag: nineteen arguments,
            // not twenty, so this reads 0 rather than raising
            flag: number(&arg(19)),
        };

        let id = {
            let mut world = lua
                .app_data_mut::<World>()
                .ok_or_else(|| mlua::Error::runtime("no world"))?;
            world.register(gob)
        };

        handle(lua, &name, id, [number(&arg(5)), number(&arg(6)), number(&arg(7))])
    })?;
    globals.set("mdkRegisterObject", register)?;
    Ok(())
}

/// The Lua side of a gob: a table the scripts can hang handlers and fields
/// on, carrying its id in the arena, and installed as a global under its own
/// name because that is what `mdkRegisterObject` does. Everything that makes
/// an object goes through here — the scene graphs, `mdkCreateObjectLua` and
/// the spawners — so they all produce the same shape.
///
/// `position` is a *table* with `x`, `y` and `z`, not an array: the scripts
/// read `gob.position.x` and nothing reads `gob.position[1]`.
pub fn handle(lua: &Lua, name: &str, id: Id, at: [f64; 3]) -> mlua::Result<Table> {
    let handle = lua.create_table()?;
    handle.set("name", name)?;
    handle.set("__gob", id)?;
    let position = lua.create_table()?;
    position.set("x", at[0])?;
    position.set("y", at[1])?;
    position.set("z", at[2])?;
    handle.set("position", position)?;
    lua.globals().set(name, &handle)?;
    Ok(handle)
}

/// Read the world back out of a Lua state.
pub fn world(lua: &Lua) -> Option<mlua::AppDataRef<'_, World>> {
    lua.app_data_ref::<World>()
}

/// The same, to change something in it.
pub fn world_mut(lua: &Lua) -> Option<mlua::AppDataRefMut<'_, World>> {
    lua.app_data_mut::<World>()
}

/// One object, to change.
impl World {
    pub fn get_mut(&mut self, id: Id) -> Option<&mut Gob> {
        self.gobs.get_mut(id as usize)
    }
}

/// Read a table's `__gob` — the id a Lua-side handle carries.
pub fn id_of(handle: &Table) -> Option<Id> {
    handle.get::<Option<Id>>("__gob").ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::script::Scripts;

    #[test]
    fn a_registration_makes_a_global_and_an_arena_entry() {
        let scripts = Scripts::new().unwrap();
        install(&scripts.lua).unwrap();
        scripts
            .lua
            .load(
                "mdkRegisterObject('l1_r1', OBJ_ROOM, scene, nil, -1, 1,2,3, 1,0,0,0, \
                 'l1_r1',0,0,0,0, {-1,-2,-3}, {4,5,6}, 0)\n\
                 mdkRegisterObject('dr2', OBJ_SCENERY, scene, l1_r1, -1, 7,8,9, \
                 1,0,0,0, 'dr2',0,0,0,0, nil, nil, 0)",
            )
            .exec()
            .unwrap();

        let world = world(&scripts.lua).unwrap();
        assert_eq!(world.len(), 2);
        let room = world.get(world.find("l1_r1").unwrap()).unwrap();
        assert_eq!(room.kind, 803.0, "OBJ_ROOM, out of the binary");
        assert_eq!(room.position, [1.0, 2.0, 3.0]);
        assert_eq!(room.bbox_min, Some([-1.0, -2.0, -3.0]));
        let door = world.get(world.find("dr2").unwrap()).unwrap();
        assert_eq!(door.kind, 800.0, "OBJ_SCENERY");
        assert_eq!(door.parent, Some(0), "the parent came in as a handle");
        assert_eq!(door.bbox_min, None);
    }

    /// Names repeat — 74 of the 5633 — and Lua's `_G[name] = gob` means the
    /// last one wins. The arena still keeps both.
    #[test]
    fn a_repeated_name_keeps_both_objects_and_the_last_global() {
        let mut world = World::new();
        world.register(Gob { name: "d".into(), position: [1.0, 0.0, 0.0], ..Gob::default() });
        world.register(Gob { name: "d".into(), position: [2.0, 0.0, 0.0], ..Gob::default() });
        assert_eq!(world.len(), 2);
        assert_eq!(world.get(world.find("d").unwrap()).unwrap().position[0], 2.0);
    }

    /// The scale is `2 * difficulty`, so the four menu settings are 0.4x,
    /// 0.7x, 1x and 2x — and a grunt's 40 is 40 only on Hard.
    #[test]
    fn the_difficulty_doubles_and_hard_leaves_the_base_alone() {
        let scaled: Vec<i32> = DIFFICULTY.iter().map(|(_, d)| diff_scale(*d, 40)).collect();
        assert_eq!(scaled, [16, 28, 40, 80]);
        assert_eq!(diff_scale(DIFFICULTY[3].1, 2000), 4000, "Zizzy on Jinkies!");
    }

    /// The half of 0x42d020 that has to be read rather than guessed: nothing
    /// with a base is left on zero hitpoints, in either direction.
    #[test]
    fn a_non_zero_base_never_scales_to_zero() {
        assert_eq!(diff_scale(0.0, 5000), 1, "a difficulty of zero still leaves one");
        assert_eq!(diff_scale(0.0, -5000), -1);
        assert_eq!(diff_scale(0.5, 0), 0, "but zero stays zero");
    }

    /// Registering an enemy fills the three fields the constructor fills.
    #[test]
    fn an_enemy_is_registered_whole() {
        let mut world = World::new();
        world.set_difficulty(DIFFICULTY[3].1); // Jinkies!
        let id = world.register(Gob { name: "z".into(), kind: 260.0, ..Gob::default() });
        let zizzy = world.get(id).unwrap();
        assert_eq!(zizzy.max_hitpoints, 4000);
        assert_eq!(zizzy.hitpoints, zizzy.max_hitpoints, "a thing starts whole");
        assert_eq!(zizzy.damage_filter, FILTER_GOODGUY_SNIPER);

        // and a door is not an enemy
        let d = world.register(Gob { name: "d".into(), kind: 800.0, ..Gob::default() });
        assert_eq!(world.get(d).unwrap().max_hitpoints, 0);
    }

    /// `mdkCreateDestructable(gob, 350)` — one of `boss.lua`'s sixteen.
    #[test]
    fn a_destructable_takes_the_base_a_script_gives_it() {
        let mut world = World::new(); // Hard by default, so 350 stays 350
        let id = world.register(Gob { name: "heart".into(), ..Gob::default() });
        world.make_destructable(id, 350);
        assert_eq!(world.get(id).unwrap().hitpoints, 350);
        assert!(world.hurt(id, 349) == false && world.hurt(id, 1), "and 350 hits kill it");
    }
}
