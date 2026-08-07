//! Turning a scene graph into something drawn.
//!
//! The graph is *run*, not parsed — see [`crate::game::world`] — and then
//! every object that names a model gets that model placed at it. The one
//! rule that is not obvious, and is silent when broken:
//!
//! **A static model is already in world coordinates.** Only an animated one
//! is in node-local space and has to be moved to its object's position and
//! rotation. The distinction is the same one `posed()` makes for vertices,
//! and getting it wrong scales `l3_maze` to 1.94x.
//!
//! The `resource` slot is polymorphic — a `.mod` usually, a `.wav` for
//! `OBJ_AMBIENTSOUND`, a `.tex` for `OBJ_STARS`, and a **waypoint name** for
//! every character type — so a name that does not resolve to a model is
//! counted, not complained about.

use crate::game::install::Install;
use crate::game::script::{Error, Scripts};
use crate::game::world;
pub use crate::game::api::Visibility;
use crate::render::camera::Mat4;
use crate::render::scene::Scene;

/// Everything a started level is: the Lua state it runs in, what was drawn
/// out of it, its rooms, its checkpoints and what it collides against.
///
/// The scripts come back with it because a level that is only *started* is
/// half of one — the driver has to keep ticking the same state.
pub struct Started {
    pub scripts: Scripts,
    pub loaded: Loaded,
    pub rooms: Visibility,
    pub checkpoints: Vec<crate::game::api::Checkpoint>,
    pub collision: crate::game::body::Collision,
}

pub struct Loaded {
    pub objects: usize,
    pub placed: usize,
    /// Objects whose `resource` names no model — characters, sounds, stars,
    /// and the objects that name nothing at all.
    pub without_a_model: usize,
    pub triangles: usize,
    /// Objects drawn because their **type** named a model, the characters
    /// among them.
    pub by_type: usize,
}

/// **Start** a level the way the game does, and fill `scene` from what it
/// created — objects, rooms and all.
///
/// This is the same path `--boot` takes, so the camera can stand at a real
/// checkpoint and the authored visibility is available: `ApplySceneGraph`
/// gives every room its `visible` list, which is the cull list the original
/// uses. Drawing only what the room the camera stands in names is a median
/// **11.3%** of a level's triangles at the game's own spawn points.
///
/// # Safety
/// A GL context must be current on this thread.
#[allow(clippy::type_complexity)]
pub unsafe fn start(
    gl: &glow::Context,
    install: &mut Install,
    scene: &mut Scene,
    sources: &std::collections::BTreeMap<String, String>,
    number: u32,
    checkpoint: u32,
) -> Result<Started, Error> {
    use crate::game::api;

    let scripts = Scripts::new()?;
    api::install(&scripts.lua, sources.clone())?;
    let mdk2 = sources
        .get("mdk2.lua")
        .ok_or_else(|| Error::Pragma("no mdk2.lua".into()))?;
    scripts.run("mdk2.lua", mdk2)?;
    api::level(&scripts, number, checkpoint, "sectionA")?;

    let boot = scripts
        .lua
        .app_data_ref::<api::Boot>()
        .ok_or_else(|| Error::Pragma("no boot state".into()))?;
    let w = world::world(&scripts.lua).ok_or_else(|| Error::Pragma("no world".into()))?;

    // a room *is* an object, so its box is that object's unless `bmin`
    // overrode it -- and the box is authored in the model's frame, which for
    // a static model is the world's
    let mut names = Vec::with_capacity(boot.rooms.len());
    let mut boxes = Vec::with_capacity(boot.rooms.len());
    for room in &boot.rooms {
        names.push(room.name.clone());
        boxes.push(room.bbox.or_else(|| {
            let gob = w.get(w.find(&room.name)?)?;
            let (lo, hi) = (gob.bbox_min?, gob.bbox_max?);
            Some([lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]])
        }));
    }
    // a room always draws itself, which `ApplySceneGraph` does not have to
    // say because the engine's own list starts with it
    let visible: Vec<std::collections::BTreeSet<usize>> = boot
        .rooms
        .iter()
        .enumerate()
        .map(|(i, r)| r.visible.iter().copied().chain(std::iter::once(i)).collect())
        .collect();
    let env: Vec<Option<f64>> = boot.rooms.iter().map(|r| r.env).collect();
    let index: std::collections::BTreeMap<&str, usize> =
        names.iter().enumerate().map(|(i, n)| (n.as_str(), i)).collect();

    // an object is in the first OBJ_ROOM up its parent chain
    let room_of = |mut id: world::Id| -> Option<usize> {
        for _ in 0..64 {
            let gob = w.get(id)?;
            if gob.kind == crate::game::api::OBJ_ROOM {
                return index.get(gob.name.as_str()).copied();
            }
            id = gob.parent?;
        }
        None
    };

    // the levels ship their lighting as objects
    scene.lights = w
        .iter()
        .filter(|(_, g)| g.kind == crate::game::api::OBJ_STATICLIGHT)
        .map(|(_, g)| crate::render::scene::Light::from_payload(g.position, g.payload))
        .collect();

    let placements: Vec<(String, Mat4, Option<usize>, world::Id, bool)> = w
        .iter()
        .filter_map(|(id, gob)| {
            // a character's `resource` slot holds a **waypoint name**, and a
            // sound's holds a `.wav`, so neither names a model; the model
            // comes from the object's type instead
            let named = match gob.resource.clone() {
                Some(r) if r.to_ascii_lowercase().ends_with(".wav") => None,
                other => other,
            };
            // The type-to-model convention holds for only 67 of the 149
            // `OBJ_*` types, so it is not applied to all of them: it would
            // drag in twenty-one guesses, and `cloak.mod`'s vertices are
            // uninitialised. It is applied to **the player**, which the
            // level named itself through `mdkSetPlayModeGobs`, and where
            // `OBJ_KURT` wearing `kurt.mod` is not in doubt.
            // The convention holds for 55 of the 149 `OBJ_*` types, and the
            // renderer refuses the two whose vertices are not sane, so it is
            // applied to everything now rather than to the player alone:
            // it is what puts the characters and the pickups in the world.
            let from_type = named.is_none();
            let resource = named.or_else(|| crate::game::api::model_for_type(gob.kind))?;
            Some((
                resource,
                Mat4::translation([
                    gob.position[0] as f32,
                    gob.position[1] as f32,
                    gob.position[2] as f32,
                ])
                .times(&Mat4::rotation([
                    gob.rotation[0] as f32,
                    gob.rotation[1] as f32,
                    gob.rotation[2] as f32,
                    gob.rotation[3] as f32,
                ])),
                room_of(id),
                id,
                from_type,
            ))
        })
        .collect();
    let objects = w.len();
    let checkpoints = boot.checkpoints.clone();
    let collision = crate::game::body::Collision::load(install, &w);
    drop(w);
    drop(boot);
    let visible: Vec<std::collections::BTreeSet<usize>> = visible;

    let mut placed = 0;
    let mut without_a_model = 0;
    let mut by_type = 0;
    for (resource, transform, room, id, from_type) in placements {
        if !scene.load(gl, install, &resource) {
            without_a_model += 1;
            continue;
        }
        let animated = scene.is_animated(&resource);
        scene.place(
            &resource,
            if animated { transform } else { Mat4::IDENTITY },
            room,
            Some(id),
        );
        placed += 1;
        if from_type {
            by_type += 1;
        }
    }

    Ok(Started {
        scripts,
        loaded: Loaded {
            objects,
            placed,
            without_a_model,
            by_type,
            triangles: scene.triangle_count(),
        },
        rooms: Visibility { names, boxes, visible, env },
        checkpoints,
        collision,
    })
}

/// Run a scene graph and fill `scene` with what it places.
///
/// # Safety
/// A GL context must be current on this thread.
pub unsafe fn load(
    gl: &glow::Context,
    install: &mut Install,
    scene: &mut Scene,
    graph: &str,
) -> Result<Loaded, Error> {
    let bytes = install
        .read(graph)
        .map_err(|e| Error::Pragma(format!("{graph}: {e}")))?;
    let source: String = bytes.iter().map(|&b| b as char).collect();

    let scripts = Scripts::new()?;
    world::install(&scripts.lua)?;
    scripts.run(graph, &source)?;

    // the world is borrowed for the walk, so the models are gathered first
    // and loaded after: `Scene::load` needs the installation mutably
    let placements: Vec<(String, Mat4)> = {
        let world = world::world(&scripts.lua).expect("a world");
        world
            .iter()
            .filter_map(|(_, gob)| {
                let resource = gob.resource.clone()?;
                Some((
                    resource,
                    Mat4::translation([
                        gob.position[0] as f32,
                        gob.position[1] as f32,
                        gob.position[2] as f32,
                    ])
                    .times(&Mat4::rotation([
                        gob.rotation[0] as f32,
                        gob.rotation[1] as f32,
                        gob.rotation[2] as f32,
                        gob.rotation[3] as f32,
                    ])),
                ))
            })
            .collect()
    };
    let objects = world::world(&scripts.lua).expect("a world").len();

    let mut placed = 0;
    let mut without_a_model = 0;
    let before = scene.missing;
    for (resource, transform) in placements {
        if !scene.load(gl, install, &resource) {
            without_a_model += 1;
            continue;
        }
        // static models are already in the world; only animated ones move
        let animated = scene.is_animated(&resource);
        scene.place(&resource, if animated { transform } else { Mat4::IDENTITY }, None, None);
        placed += 1;
    }
    let _ = before;

    Ok(Loaded {
        objects,
        placed,
        without_a_model,
        by_type: 0,
        triangles: scene.triangle_count(),
    })
}
