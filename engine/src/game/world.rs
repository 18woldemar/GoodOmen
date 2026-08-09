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
    /// Whether `OnCreate` has already fired for it — bit **0x1000000** in the
    /// original's `omgob[0xb4]`, tested and set at 0x42e3e7 so that no object
    /// is created twice. See [`crate::game::api::create`].
    pub created: bool,
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


/// **The three heroes.** Each has a constructor in its own file that writes
/// its hitpoints and its damage filter as literals — and **not through
/// `mdkDiffScale`**, so the difficulty changes what you fight and never what
/// you are.
///
/// | who | at | hitpoints | filter |
/// |---|---|---|---|
/// | Kurt | 0x4168b8 | 100 | 0x9d6 |
/// | Hyde | 0x413f5a | 240 | 0x8d6 |
/// | Max | 0x420af8 | 200 | 0x8d6 |
///
/// The masks are `DAMAGE_BADGUY | LAVA | FALLING | LAVAFALL | LAVAFLOAT |
/// LAVADEATH` and the one bit that differs is **0x100, `DAMAGE_BLACKHOLE`**:
/// Kurt can be hurt by one and the other two cannot. None of the three has
/// `DAMAGE_GOODGUY`, which is why the player cannot shoot himself.
///
/// Kurt's constructor writes the **current** hitpoints and never the maximum,
/// so the 100 here stands for both and where the original's maximum comes
/// from is unread. Hyde and Max write both.
pub const PLAYER: [(f64, i16, i16); 3] = [
    (100.0, 100, 0x9d6), // OBJ_KURT
    (103.0, 240, 0x8d6), // OBJ_HYDE
    (101.0, 200, 0x8d6), // OBJ_MAX
];

/// **The shot table**, at 0x497388: 69 records of 0x58 bytes, in the same
/// shape as the enemy table and found the same way — a run of plausible
/// `OBJ_*` ids at a fixed stride. `mdkBullet.c` (the path is at 0x498f1c) is
/// where every one of these fields is read, and the columns kept here are the
/// ones it uses:
///
/// | offset | what |
/// |---|---|
/// | +0x00 | the `OBJ_*` type |
/// | +0x04 | the `DAMAGE_*` mask — 1 goodguy, 2 badguy, +1024 knockdown |
/// | +0x08 | the damage |
/// | +0x20 | the lifetime in seconds, -1 for none (0x403d94 reads it) |
/// | +0x2c | the speed (0x4039f4 divides a distance by it) |
/// | +0x30 | the model name |
/// | +0x4c | how much the shot leads a moving target |
/// | +0x54 | flags: 0x40 fixed heading, 0x400 aim at the gob, **0x800 at the player** |
///
/// The last of those is on almost every enemy shot — `gruntshot` 0x800,
/// `hansshot` 0x800, `hosershot` 0xc00, `dbgrenade` 0x2809 — and it is what
/// makes an enemy's bullet find a player who is not at its own height. See
/// [`AT_PLAYER`].
///
/// The numbers read like a weapon list should: `sniperbullet` is 25 damage at
/// 300 units a second and lives 6, `grenade` is 10 at 25 and lives 30,
/// `lasershot` 25 at 90 and lives **1**. `tools/health.py --bullets` reads
/// the same six columns out of the binary and `check.py` holds this literal
/// to them.
/// `+0x54` bit: the shot is launched **at the player**, in three dimensions,
/// rather than along the shooter's yaw. Without it a walker's bullet flies
/// flat out of its feet and a two-unit step is enough to miss with.
pub const AT_PLAYER: i32 = 0x800;

pub const BULLET: [(f64, &str, i16, i16, f64, f64, i32); 69] = [
    (400.0, "bifshot", 1026, 5, 5.0, 60.0, 0x802),
    (404.0, "birdshot", 2, 3, 4.0, 60.0, 0x800),
    (420.0, "rpmissl", 2, 5, 30.0, 20.0, 0x803),
    (417.0, "dbgrenade", 1026, 10, 30.0, 20.0, 0x2809),
    (403.0, "dboy1shot", 2, 2, 4.0, 40.0, 0x0),
    (423.0, "pdboy1shot", 2, 4, 4.0, 40.0, 0x0),
    (412.0, "powergenshot", 1026, 5, 5.0, 60.0, 0x802),
    (421.0, "hansshot", 1026, 3, 4.0, 60.0, 0x800),
    (435.0, "badmaxshot1", 2, 8, 4.0, 70.0, 0x801),
    (441.0, "badmaxshot2", 3, 5, 5.0, 60.0, 0x1802),
    (427.0, "gruntshot", 2, 2, 4.0, 40.0, 0x800),
    (425.0, "udboy1shot", 1026, 10, 4.0, 100.0, 0x2800),
    (432.0, "udboy2shot", 1027, 5, 5.0, 60.0, 0x1802),
    (428.0, "hosershot", 2, 2, 4.0, 40.0, 0xc00),
    (413.0, "turshot01", 2, 2, 4.0, 60.0, 0x0),
    (419.0, "turshot02", 2, 1, 4.0, 60.0, 0x0),
    (452.0, "turshot03", 2, 2, 4.0, 60.0, 0x0),
    (453.0, "turshot04", 2, 2, 4.0, 500.0, 0x800),
    (454.0, "turshot04", 2, 2, 4.0, 500.0, 0x800),
    (455.0, "turshot06", 2, 10, 10.0, 90.0, 0x20801),
    (456.0, "grmissl", 0, 0, 5.0, 35.0, 0x2),
    (457.0, "turshot01", 2, 2, 4.0, 60.0, 0x0),
    (458.0, "turshot09", 2, 2, 4.0, 70.0, 0x0),
    (459.0, "turshot10", 2, 10, 5.0, 80.0, 0x801),
    (460.0, "turshot11", 2, 10, 5.0, 80.0, 0x801),
    (461.0, "turshot12", 2, 10, 5.0, 80.0, 0x801),
    (462.0, "turshot13", 2, 5, 5.0, 160.0, 0x800),
    (463.0, "turshot14", 2, 10, 5.0, 80.0, 0x801),
    (464.0, "turshot15", 2, 10, 5.0, 80.0, 0x801),
    (465.0, "turshot16", 2, 10, 5.0, 80.0, 0x801),
    (466.0, "turshot17", 2, 10, 5.0, 80.0, 0x801),
    (467.0, "turshot18", 2, 10, 5.0, 80.0, 0x801),
    (468.0, "turshot19", 2, 10, 5.0, 80.0, 0x801),
    (469.0, "turshot20", 2, 10, 5.0, 80.0, 0x801),
    (416.0, "toast_missle", 1, 15, 10.0, 50.0, 0x831),
    (422.0, "blackhole", 1, 0, 30.0, 25.0, 0x28c8),
    (405.0, "grenade", 1027, 10, 30.0, 25.0, 0x2c09),
    (424.0, "decoygrenade", 0, 0, 30.0, 25.0, 0x28c8),
    (401.0, "grmissl", 1027, 12, 30.0, 20.0, 0xa03),
    (418.0, "moltov", 1025, 12, 30.0, 25.0, 0x809),
    (414.0, "rpmissl", 3, 6, 10.0, 20.0, 0xa03),
    (415.0, "toast_flop", 1, 0, 30.0, 25.0, 0x831),
    (406.0, "sniperbullet", 1, 25, 6.0, 300.0, 0x804),
    (407.0, "snipergrenade", 1027, 40, 6.0, 200.0, 0x805),
    (408.0, "sniperhoming", 1025, 60, 30.0, 100.0, 0xa06),
    (411.0, "snipermortar", 1027, 20, 30.0, 50.0, 0xc0d),
    (410.0, "sniperbounce", 1027, 20, 30.0, 60.0, 0x915),
    (430.0, "lasershot", 1, 25, 1.0, 90.0, 0x4),
    (431.0, "lasershot2", 1, 40, 1.0, 90.0, 0x4),
    (433.0, "toastbaguette", 1, 50, 30.0, 50.0, 0xa33),
    (436.0, "toastpumper", 1027, 20, 30.0, 50.0, 0xc0d),
    (434.0, "bfbshot", 2, 4, 4.0, 40.0, 0x0),
    (437.0, "sniperlock", 0, 0, -1.0, 20.0, 0x1a008),
    (451.0, "sniperlock", 0, 0, -1.0, 25.0, 0x1a008),
    (438.0, "bfbshotseek", 0, 0, 30.0, 0.0, 0x2c08),
    (439.0, "bfbshotseek2", 1026, 5, 30.0, 30.0, 0x5802),
    (444.0, "bfbshotseekb", 0, 0, 30.0, 0.0, 0x2c08),
    (445.0, "bfbshotseek2b", 1026, 5, 30.0, 30.0, 0x1802),
    (440.0, "bfbshotpsi", 0, 0, 30.0, 0.0, 0x2c08),
    (446.0, "bfbbomb", 1026, 20, 30.0, 20.0, 0x2809),
    (442.0, "turshot11", 2, 2, 4.0, 120.0, 0x800),
    (443.0, "turshot10", 2, 3, 30.0, 80.0, 0x801),
    (447.0, "zizshot01", 1026, 15, 30.0, 70.0, 0x140810),
    (448.0, "zizbub", 0, 0, 600.0, 40.0, 0x2008),
    (449.0, "zizshot03", 2, 5, 4.0, 200.0, 0x160800),
    (450.0, "bilebelch", 1026, 5, 10.0, 25.0, 0x183802),
    (480.0, "zizeye", 0, 0, -1.0, 1.0, 0x1a008),
    (481.0, "gastricjuice", 2, 4, 4.0, 30.0, 0x140012),
    (482.0, "zizbrain", 1026, 5, 30.0, 30.0, 0x1802),
];

/// What a shot of this type does, if the table names it.
pub fn bullet(kind: f64) -> Option<(&'static str, i16, i16, f64, f64, i32)> {
    BULLET
        .iter()
        .find(|(k, ..)| *k == kind)
        .map(|(_, m, f, d, l, s, g)| (*m, *f, *d, *l, *s, *g))
}


/// **The item table**, at 0x49f2c0: 49 records of 0x34 bytes — the third of
/// the three the stride scan found, and the inventory. +0x00 is the type,
/// **+0x04 an inline name of up to 16 bytes which is the model** (the
/// constructor at 0x416310 walks the table for a matching type and hands
/// `record + 4` straight to the loader at 0x4609a0), and **+0x14 an index
/// into `mdk2.str`** for what the inventory calls it.
///
/// That last one is checked against the real world rather than asserted: 18
/// is "Magnum", 21 "Uzi", 16 "Gatling Gun", 20 "Shotgun", **567 "Raygun"**
/// for the thing the table calls `lasergatgun`, 2 "+25 Health" for the apple
/// and 7 "+50 Health" for the ham. A column that lands on seven sensible
/// names in a row is the column.
///
/// The rest of the record is not kept here because it is not read yet: +0x1c
/// and +0x20 look like a category and an owner, +0x2c is a float that reads
/// like a fire interval (0.2 magnum, 0.6 shotgun, 1.5 guided rockets) and
/// +0x30 an int that reads like a magazine (50, 200, 15). Guesses, so out.
pub const ITEM: [(f64, &str, i32); 49] = [
    (300.0, "magnum", 18),
    (301.0, "uzi", 21),
    (304.0, "magnum", -1),
    (305.0, "gatgun", 16),
    (306.0, "shotgun", 20),
    (307.0, "lasergatgun", 567),
    (309.0, "guidedrocket", 17),
    (311.0, "doublea", 23),
    (312.0, "carbattery", 22),
    (318.0, "jetpack", 24),
    (352.0, "jetpackatm", 212),
    (313.0, "apple", 2),
    (314.0, "ham", 7),
    (316.0, "blackhole", 3),
    (317.0, "grenade", 6),
    (319.0, "decoygrenade", 5),
    (320.0, "cloak", 4),
    (347.0, "snipershield", 14),
    (321.0, "sniperbullet", 189),
    (322.0, "snipergrenade", 12),
    (323.0, "sniperhoming", 11),
    (325.0, "snipermortar", 10),
    (326.0, "sniperbounce", 13),
    (327.0, "lighter", 33),
    (328.0, "loaf", 34),
    (329.0, "toaster", 41),
    (330.0, "booze", 25),
    (332.0, "ducttape", 27),
    (333.0, "fishbowl", 30),
    (336.0, "plutonium", 37),
    (340.0, "magnet", 35),
    (341.0, "pop", 38),
    (343.0, "leafer", 42),
    (344.0, "atomictoaster", 43),
    (345.0, "moltov", 44),
    (346.0, "towels", 45),
    (349.0, "ladder", 174),
    (351.0, "pipes", 178),
    (350.0, "cord", 177),
    (353.0, "dimdes", 213),
    (354.0, "kurtcoord", 214),
    (355.0, "posdoo", 215),
    (356.0, "schaingun", 225),
    (357.0, "fballgun", 226),
    (358.0, "toast", 227),
    (359.0, "handdryer", 239),
    (360.0, "loafbaguette", 324),
    (361.0, "loafpumper", 374),
    (362.0, "fishbowle", 29),
];

/// The model a type wears, if one of the three tables names it — and they
/// name 137 types between them, where guessing from the `OBJ_*` name covers
/// 67 of 149. See [`crate::game::api::model_for_type`], which asks this first.
pub fn table_model(kind: f64) -> Option<&'static str> {
    ITEM.iter()
        .find(|(k, ..)| *k == kind)
        .map(|(_, m, _)| *m)
        .or_else(|| BULLET.iter().find(|(k, ..)| *k == kind).map(|(_, m, ..)| *m))
        .or_else(|| BASE_HITPOINTS.iter().find(|(k, ..)| *k == kind).map(|(_, m, _)| *m))
}


/// **The AI behaviour table**, at 0x48ff78: 9 records of 0x34, indexed by
/// `def + 0x84` — which is 0xffffffff for the nine enemy types that have no
/// AI at all. The columns kept here are the ones 0x4324f0 reads:
///
/// | offset | what |
/// |---|---|
/// | +0x00 | **the burst**: how many shots in a row (0x4328e6 loads it into
/// `walker + 0x9c` and state 0 counts it down, one a second) |
/// | +0x04 | **the seconds between rounds of a burst** — 0x433219 writes it
/// straight into `walker + 0x64` after each shot |
/// | +0x08 | the melee distance |
/// | +0x1c | the near distance: inside it the thing backs away |
/// | +0x20 | the far distance |
/// | +0x24 | the reach |
/// | +0x28 | the field of view — PI/3, PI/2, PI/4, PI/8 |
/// | +0x30 | whether it leads a moving target |
///
/// The three probabilities at +0x0c, +0x10 and +0x14 are left out: they gate
/// branches of the state machine that are not built, and a number nothing
/// reads is a number that rots. `tools/health.py --ai` prints the whole
/// record.
/// The last column is `record + 0x18`, and it is **the one that decides
/// whether an enemy comes at you**: 0x432c56 rolls `chRand()` against it and
/// takes state 5, *advance*, when the roll is the higher — so a doganboy at
/// 0.3 closes the distance seven times in ten and fires the other three.
///
/// It was missed for a whole session because `health.py --ai` printed eleven
/// of the record's thirteen columns and skipped this one.
pub const AI: [(f64, f64, f64, f64, f64, f64, f64, bool, f64); 9] = [
    // burst, interval, melee, near, far, reach, fov, lead, advance
    (3.0, 0.5, 0.9, 10.0, 15.0, 100.0, 1.047198, true, 0.3),
    (5.0, 0.8, 0.9, 10.0, 15.0, 75.0, 1.570796, true, 0.3),
    (3.0, 2.0, 0.15, 15.0, 25.0, 150.0, 0.785398, false, 0.0),
    (3.0, 0.5, 0.9, 8.0, 13.0, 50.0, 1.047198, false, 0.2),
    (1.0, 2.0, 0.2, 25.0, 35.0, 150.0, 1.047198, false, 0.2),
    (1.0, 2.0, 0.2, 25.0, 35.0, 200.0, 1.047198, true, 0.4),
    (3.0, 0.0, 1.0, 20.0, 30.0, 50.0, 1.570796, false, 0.5),
    (0.0, 0.5, 1.0, 5.0, 5.0, 6.0, 0.392699, false, 0.2),
    (1.0, 3.0, 0.0, 15.0, 25.0, 150.0, 0.785398, false, 0.3),
];

/// Which behaviour an enemy type uses, out of `def + 0x84`. Nine of the 19
/// have none at all and simply stand there.
pub const AI_OF: [(f64, usize); 10] = [
    (203.0, 6), (200.0, 2), (204.0, 5), (205.0, 1), (202.0, 3),
    (219.0, 7), (207.0, 0), (214.0, 4), (217.0, 1), (260.0, 8),
];

/// The behaviour a type fights with, if it has one.
pub fn ai(kind: f64) -> Option<(f64, f64, f64, f64, f64, f64, f64, bool, f64)> {
    let i = AI_OF.iter().find(|(k, _)| *k == kind)?.1;
    Some(AI[i])
}

/// The base hitpoints for a type, if it is one the table names.
pub fn base_hitpoints(kind: f64) -> Option<i32> {
    BASE_HITPOINTS.iter().find(|(k, _, _)| *k == kind).map(|(_, _, hp)| *hp)
}

/// **The same 19 records, read for how the thing moves.** The table at
/// 0x4ab2e8 is not a health table at all — it is the walker's whole
/// definition, and `walker[0]` points into it. 0x42fd0d indexes it by the
/// gait: **`def[0x18 + gait * 4]`**, a float, so the four speeds below are
/// slots +0x18, +0x1c, +0x20 and +0x24 in order — **still, walk, run and
/// back** — and the back one is *negative* so a walk clip plays in reverse.
/// Beside them are +0x28, the turn rate in radians a second (0x42fc5d
/// multiplies it by the frame time), and +0x2c, the strafe speed, which is
/// multiplied by `walker + 0x10`.
///
/// **A walker at or below `def[0x40]` hitpoints moves at half speed** — the
/// branch at 0x42fd4c takes the same slot and multiplies by the 0.5 at
/// 0x48f2fc. See `LIMP_AT` for who that is; the same threshold also changes
/// which animations it plays.
///
/// `tools/health.py --walk` reads the same six columns out of the binary and
/// `check.py` holds this literal to them.
pub const LOCOMOTION: [(f64, [f64; 4], f64, f64); 19] = [
    //  type    still  walk   run   back        turn  strafe
    (201.0, [0.0, 7.0, 16.0, 0.0], 3.0, 0.0),
    (215.0, [0.0, 7.0, 14.0, 0.0], 3.0, 0.0),
    (216.0, [0.0, 9.0, 23.0, 0.0], 3.5, 0.0),
    (203.0, [0.0, 4.0, 7.0, -4.0], 5.0, 4.0),
    (250.0, [0.0, 4.0, 7.0, -4.0], 5.0, 4.0),
    (200.0, [0.0, 4.0, 10.0, -4.0], 2.5, 0.0),
    (204.0, [0.0, 12.0, 20.0, -12.0], 4.0, 8.0),
    (205.0, [0.0, 2.0, 5.0, -2.0], 4.0, 3.0),
    (202.0, [0.0, 6.0, 20.0, -6.0], 4.0, 15.7),
    (219.0, [0.0, 6.0, 12.0, -6.0], 4.0, 11.3),
    (207.0, [0.0, 6.0, 12.0, -6.0], 4.0, 10.0),
    (214.0, [0.0, 8.0, 15.0, -8.0], 4.0, 13.0),
    (211.0, [0.0, 5.0, 9.0, -4.0], 2.0, 4.0),
    (220.0, [0.0, 16.0, 32.0, -16.0], 3.0, 16.0),
    (210.0, [0.0, 12.0, 16.0, -7.0], 4.0, 5.0),
    (217.0, [0.0, 4.0, 8.0, -4.0], 4.0, 5.0),
    (206.0, [0.0, 12.0, 24.0, -8.0], 2.0, 8.0),
    (208.0, [0.0, 8.0, 16.0, -8.0], 4.0, 12.0),
    (260.0, [0.0, 12.0, 36.0, -12.0], 3.5, 12.0),
];

/// How fast a type moves at a gait, and how fast it turns, if the table names
/// it. A gait outside 0..3 is not one the original can produce — the only
/// writers are the input block and the two goto functions — so it reads as
/// standing still.
pub fn locomotion(kind: f64, gait: i64) -> Option<(f64, f64)> {
    let (_, speeds, turn, _) = LOCOMOTION.iter().find(|(k, ..)| *k == kind)?;
    Some((*speeds.get(gait.max(0) as usize).unwrap_or(&0.0), *turn))
}

/// The four animations a walker plays to move, indexed by its gait: the table
/// at **0x48ff58**, which `mdkWalkerAnimUpdate` reads with `walker + 0x0c` at
/// 0x42fde9 and hands straight to the animation player. The engine drives a
/// character's legs, in other words, and no script ever asks it to.
pub const GAIT_ANIM: [f64; 4] = [
    107.0, // ANIM_READY0   — standing
    6.0,   // ANIM_WALK
    7.0,   // ANIM_RUN
    8.0,   // ANIM_WALKBACK — the back speed is negative, so the clip reverses
];

/// The set a limping walker plays instead, at **0x48ff68**. 0x42fdd0 clamps
/// the gait from 2 to 1 before indexing it, so a limping walker cannot run:
/// asking it to gives `ANIM_ACTION00`, the same clip its walk uses.
pub const GAIT_ANIM_HURT: [f64; 4] = [
    82.0, // ANIM_ACTION05
    77.0, // ANIM_ACTION00
    7.0,  // unreachable — the run is clamped to the walk above
    8.0,  // ANIM_WALKBACK
];

/// `def + 0x40`: the hitpoints at or below which a walker limps — half speed
/// (0x42fd4c) and the second animation set (0x42fdc6), off the one threshold.
///
/// Eighteen of the nineteen records hold 0xffffffff and never limp. **Only the
/// doganboy does**, at 20 of its 100, so the game has exactly one enemy that
/// visibly slows down when you have nearly killed it.
pub const LIMP_AT: [(f64, i16); 1] = [(207.0, 20)];

/// Whether a walker of this type is hurt enough to limp.
pub fn limping(kind: f64, hitpoints: i16) -> bool {
    LIMP_AT
        .iter()
        .any(|&(k, at)| k == kind && hitpoints <= at)
}

/// **How big a walker is**: `def + 0x78` and `def + 0x7c`, which the
/// constructor at 0x42f539 copies into the gob's collision block at
/// `gob + 0x68`, fields +0xec and +0xf0. omCollision reads them at 0x47361b
/// and 0x47365c and **halves both**, so they are the full height and the full
/// width and not half-extents.
///
/// The pair is **-1 on the shwang and the angel** and 0x4311a0 refuses to
/// switch the block on unless both are above zero, so those two have no body
/// at all. What settles the reading is Kurt: mdkKurt.c writes 2.0 and 0.8 into
/// the same two fields at 0x416863, which is a person two units tall and
/// eight tenths wide — so `EYE`'s 1.7 and these numbers are in one scale.
pub const SIZE: [(f64, f64, f64); 19] = [
    //  type    tall  wide
    (201.0, 2.0, 2.0),   // samsmite
    (215.0, 2.0, 2.0),   // samfire
    (216.0, 2.0, 2.0),   // samrock
    (203.0, 3.5, 1.0),   // conehead
    (250.0, 3.1, 1.0),   // coneciv
    (200.0, 6.0, 4.7),   // bif
    (204.0, 16.0, 10.0), // hans
    (205.0, 2.0, 1.0),   // hoser
    (202.0, 4.4, 3.8),   // grunt
    (219.0, 4.4, 3.8),   // grunt
    (207.0, 5.0, 5.0),   // doganboy
    (214.0, 11.0, 10.0), // ultradogan
    (211.0, 4.7, 4.0),   // bfb
    (220.0, -1.0, -1.0), // shwang — no body
    (210.0, 4.0, 4.0),   // badmax
    (217.0, 2.5, 2.0),   // poopsy
    (206.0, -1.0, -1.0), // angel — no body
    (208.0, 11.0, 10.0), // birdbrain1
    (260.0, 24.0, 12.0), // zizzy
];

/// How tall and how wide a type is, if the table names it and it has a body
/// at all. The shwang and the angel hold -1 and get none.
pub fn size(kind: f64) -> Option<(f64, f64)> {
    SIZE.iter()
        .find(|(k, ..)| *k == kind)
        .map(|&(_, tall, wide)| (tall, wide))
        .filter(|&(tall, wide)| tall > 0.0 && wide > 0.0)
}

/// Which animation a walker plays for a gait, given how hurt it is.
pub fn gait_animation(kind: f64, gait: i64, hitpoints: i16) -> Option<f64> {
    let i = usize::try_from(gait).ok()?;
    if limping(kind, hitpoints) {
        GAIT_ANIM_HURT.get(if i == 2 { 1 } else { i }).copied()
    } else {
        GAIT_ANIM.get(i).copied()
    }
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

/// What [`World::take_damage`] did, which is what decides whether `OnDie`
/// fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// Invulnerable, or already dead. Nothing happened at all.
    Ignored,
    Hurt,
    Died,
}

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
        if let Some(&(_, hp, filter)) = PLAYER.iter().find(|(k, ..)| *k == gob.kind) {
            gob.max_hitpoints = hp;
            gob.hitpoints = hp;
            gob.damage_filter = filter;
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

    /// What a hit did.
    ///
    /// Read off the class damage handler at **0x424f60** — the shortest of
    /// the eleven the class table at 0x49bc58 holds, and the shape they
    /// share. Two guards come before the arithmetic and both matter: a
    /// **filter of zero is invulnerable** and something **already at zero
    /// hitpoints is not hit again**, so a corpse cannot be killed twice and
    /// `OnDie` fires once.
    pub fn take_damage(&mut self, id: Id, amount: i16) -> Hit {
        let Some(gob) = self.gobs.get(id as usize) else { return Hit::Ignored };
        if gob.damage_filter == 0 || gob.hitpoints <= 0 {
            return Hit::Ignored;
        }
        if self.hurt(id, amount) { Hit::Died } else { Hit::Hurt }
    }

    /// Take an object and everything under it out of the world, and say what
    /// went — the names, so the caller can clear the Lua globals too.
    ///
    /// `mdkDestroyRoom` is the only caller and a room's contents are its
    /// children, so this is a tree. The arena entries stay (ids are handed
    /// out by position and must not move), but they are emptied and their
    /// names unindexed: a destroyed room cannot be found, drawn or hit, and
    /// nothing that held an id gets a different object back.
    pub fn destroy(&mut self, id: Id) -> Vec<String> {
        let mut doomed = vec![id];
        let mut i = 0;
        while i < doomed.len() {
            let parent = doomed[i];
            for (child, gob) in self.gobs.iter().enumerate() {
                if gob.parent == Some(parent) && !gob.name.is_empty() {
                    doomed.push(child as Id);
                }
            }
            i += 1;
        }
        let mut gone = Vec::with_capacity(doomed.len());
        for id in doomed {
            let Some(gob) = self.gobs.get_mut(id as usize) else { continue };
            let name = std::mem::take(&mut gob.name);
            if name.is_empty() {
                continue;
            }
            *gob = Gob::default();
            if self.by_name.get(&name) == Some(&id) {
                self.by_name.remove(&name);
            }
            gone.push(name);
        }
        self.generation += 1;
        gone
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
            created: false,
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

    // **And a walker arms itself.** The last thing the walker constructor
    // does, at 0x42f65d..0x42f688, is call the Lua global
    // `SetDefaultWalkerEvents(gob, type)` — which is in `script.lua`, hangs
    // `DefaultWalkerOnDamage` on the object and starts
    // `DefaultAIScripts[type].script`, and that script is `{{
    // mdkDoganboyAttack }}` for eleven of the thirteen types it names.
    //
    // So **the engine arms every enemy, not the level**: only one line in all
    // ten level scripts calls it (level 2, for `l2rbif`), and without this an
    // enemy never fights no matter how far the player walks. It is guarded by
    // the global existing, because a boot with no `script.lua` has no such
    // function and the original would have raised.
    let kind = lua
        .app_data_ref::<World>()
        .and_then(|w| w.get(id).map(|g| g.kind));
    if let Some(kind) = kind.filter(|k| base_hitpoints(*k).is_some()) {
        if let Ok(arm) = lua.globals().get::<mlua::Function>("SetDefaultWalkerEvents") {
            let _ = arm.call::<Value>((&handle, kind));
        }
    }
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
