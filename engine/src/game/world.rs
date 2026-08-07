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
    pub bbox_min: Option<[f64; 3]>,
    pub bbox_max: Option<[f64; 3]>,
    pub flag: f64,
}

#[derive(Default)]
pub struct World {
    gobs: Vec<Gob>,
    /// Name to id, **last registration winning**, because that is what
    /// `_G[name] = gob` does.
    by_name: std::collections::HashMap<String, Id>,
    /// Bumped by every move, so nothing has to diff the world to notice one.
    generation: u64,
}

impl World {
    pub fn new() -> World {
        World::default()
    }

    pub fn register(&mut self, gob: Gob) -> Id {
        let id = self.gobs.len() as Id;
        self.by_name.insert(gob.name.clone(), id);
        self.gobs.push(gob);
        id
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

        // the Lua side is a table the scripts can hang handlers and fields
        // on, carrying its id in the arena
        let handle = lua.create_table()?;
        handle.set("name", name.clone())?;
        handle.set("__gob", id)?;
        let position = lua.create_table()?;
        position.set("x", number(&arg(5)))?;
        position.set("y", number(&arg(6)))?;
        position.set("z", number(&arg(7)))?;
        handle.set("position", position)?;
        lua.globals().set(name, &handle)?;
        Ok(handle)
    })?;
    globals.set("mdkRegisterObject", register)?;
    Ok(())
}

/// Read the world back out of a Lua state.
pub fn world(lua: &Lua) -> Option<mlua::AppDataRef<'_, World>> {
    lua.app_data_ref::<World>()
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
}
