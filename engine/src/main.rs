//! goodomen — drop it into your MDK2 directory and run it.
//!
//! With no arguments it reads the installation beside the binary and says
//! what it found. `--tex` decodes every texture and prints one CRC32 each,
//! in the form `tools/texdec.py --digest` prints, so that
//! `tools/texcheck.sh` can hold the two implementations to each other.

use goodomen::formats::bsp::Bsp;
use goodomen::formats::container::crc32;
use goodomen::formats::model::Model;
use goodomen::formats::tex::Texture;
use goodomen::game::install::Install;
use goodomen::game::script::Scripts;
use goodomen::render::{triangle, Video};
use glow::HasContext;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tex = args.iter().any(|a| a == "--tex");
    let models = args.iter().any(|a| a == "--mod");
    let trees = args.iter().any(|a| a == "--bsp");
    let scripts = args.iter().any(|a| a == "--lua");
    let graphs = args.iter().any(|a| a == "--scene");

    // the renderer needs no game files, so it is answered before the
    // installation is looked for
    if args.iter().any(|a| a == "--triangle") {
        match goodomen::render::triangle::selfcheck() {
            Ok(line) => println!("{line}"),
            // no display is not a failed renderer, and a check that cannot
            // run must say so rather than pass or fail
            Err(e) if e.starts_with(goodomen::render::NO_VIDEO) => {
                println!("skip: {e}");
            }
            Err(e) => {
                eprintln!("goodomen: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if args.iter().any(|a| a == "--window") {
        if let Err(e) = window() {
            eprintln!("goodomen: {e}");
            std::process::exit(1);
        }
        return;
    }

    // the only positional argument is the game directory; `--expect N` takes
    // a value, so the token after it is not one
    let root = args
        .iter()
        .zip(std::iter::once(&String::new()).chain(args.iter()))
        .find(|(a, before)| !a.starts_with("--") && *before != "--expect")
        .map(|(a, _)| std::path::PathBuf::from(a))
        .unwrap_or_else(Install::beside_the_binary);

    let mut install = match Install::open(&root) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("goodomen: {e}");
            std::process::exit(1);
        }
    };

    if tex || models || trees || scripts || graphs {
        // `--expect N`, the same convention the Python tools use: a check
        // that silently found nothing is the failure mode worth guarding
        let expect = args
            .iter()
            .position(|a| a == "--expect")
            .and_then(|i| args.get(i + 1))
            .and_then(|n| n.parse::<usize>().ok());
        // the per-resource lines exist for the cross-checks to diff; when
        // this is being run as a check, only the verdict is wanted
        let (what, found) = if tex {
            ("textures", textures(&mut install, expect.is_none()))
        } else if models {
            ("models", meshes(&mut install, expect.is_none()))
        } else if trees {
            ("trees", collision(&mut install, expect.is_none()))
        } else if scripts {
            ("scripts", compile_scripts(&mut install, expect.is_none()))
        } else {
            ("objects", scene_graphs(&mut install, expect.is_none()))
        };
        if let Some(n) = expect {
            if found != n {
                eprintln!("goodomen: {found} {what}, expected {n}");
                std::process::exit(1);
            }
        }
        return;
    }

    println!("MDK2 in {}", install.root.display());
    for c in &install.containers {
        println!(
            "  {:12} {} files",
            c.path().file_name().unwrap_or_default().to_string_lossy(),
            c.entries().len()
        );
    }
    match &install.override_dir {
        Some(p) => println!("  override/    a shipped patch, read first ({})", p.display()),
        None => println!("  override/    absent"),
    }

    // Reading everything is the only honest way to say the containers are
    // understood: every member carries a CRC32 and Container::read checks it.
    let mut read = 0usize;
    let mut bytes = 0usize;
    for i in 0..install.containers.len() {
        for j in 0..install.containers[i].entries().len() {
            match install.containers[i].read_at(j) {
                Ok(b) => {
                    read += 1;
                    bytes += b.len();
                }
                Err(e) => {
                    eprintln!("goodomen: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
    println!(
        "{read}/{} files read, every checksum matching, {:.1} MiB",
        install.entry_count(),
        bytes as f64 / (1024.0 * 1024.0)
    );
}

/// A window with the triangle in it, until it is closed or Escape is pressed.
fn window() -> Result<(), String> {
    use sdl2::event::Event;
    use sdl2::keyboard::Keycode;

    let mut video = Video::open("goodomen", 1024, 768, true)?;
    println!("OpenGL {}", video.version());
    loop {
        for event in video.events.poll_iter() {
            match event {
                Event::Quit { .. }
                | Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => return Ok(()),
                _ => {}
            }
        }
        let (w, h) = video.window.drawable_size();
        // SAFETY: the context is current on this thread for the whole loop.
        unsafe {
            video.gl.viewport(0, 0, w as i32, h as i32);
            triangle::draw(&video.gl)?;
        }
        video.window.gl_swap_window();
    }
}

/// Decode every `.tex` in the containers and print `name crc32` a line, sorted
/// by name. The CRC32 covers **every level**, not just the largest, so a mip
/// chain that decodes right at the top and wrong further down still shows.
fn textures(install: &mut Install, list: bool) -> usize {
    let found = entries_named(install, ".tex");

    let mut levels = 0usize;
    let mut pixels = 0usize;
    for (name, i, j) in &found {
        let data = match install.containers[*i].read_at(*j) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("goodomen: {name}: {e}");
                std::process::exit(1);
            }
        };
        let texture = match Texture::parse(&data) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("goodomen: {name}: {e}");
                std::process::exit(1);
            }
        };
        // crc32 is not incremental here, so the levels are concatenated
        // first; the corpus is 200 MiB decoded and this runs once.
        let mut all = Vec::new();
        for level in &texture.levels {
            assert_eq!(level.bgra.len(), (level.width * level.height * 4) as usize);
            if list {
                all.extend_from_slice(&level.bgra);
            }
            pixels += (level.width * level.height) as usize;
        }
        levels += texture.levels.len();
        if list {
            println!("{name} {:08x}", crc32(&all));
        }
    }
    eprintln!(
        "{} textures, {levels} levels, {pixels} pixels",
        found.len()
    );
    found.len()
}

/// Parse and validate every `.bsp`, then ask each tree the same deterministic
/// questions, so that a disagreement about *inside* shows and not just a
/// disagreement about parsing. The points come from the tree's own planes —
/// see `probe_points`.
fn collision(install: &mut Install, list: bool) -> usize {
    let found = entries_named(install, ".bsp");
    let (mut nodes, mut deepest) = (0usize, 0usize);

    for (name, i, j) in &found {
        let data = match install.containers[*i].read_at(*j) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("goodomen: {name}: {e}");
                std::process::exit(1);
            }
        };
        let bsp = match Bsp::parse(&data) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("goodomen: {name}: {e}");
                std::process::exit(1);
            }
        };
        if let Err(e) = bsp.validate() {
            eprintln!("goodomen: {name}: {e}");
            std::process::exit(1);
        }
        let answers: Vec<u8> = probe_points(&bsp)
            .into_iter()
            .map(|p| bsp.contains(p) as u8)
            .collect();
        nodes += bsp.nodes.len();
        deepest = deepest.max(bsp.depth());
        if list {
            println!(
                "{name} {} {} {} {:08x}",
                bsp.nodes.len(),
                bsp.depth(),
                answers.iter().map(|&a| a as usize).sum::<usize>(),
                crc32(&answers)
            );
        }
    }
    eprintln!(
        "{} trees, {nodes} nodes, deepest {deepest}",
        found.len()
    );
    found.len()
}

/// Query points derived from the tree itself, so no other file is needed and
/// two implementations can be asked exactly the same thing.
///
/// The first are the feet of the planes, negated — `contains` negates again,
/// so each lands **exactly on** a plane, which is where two implementations
/// would differ if either got the `>= 0` boundary wrong. The rest are a 4x4x4
/// grid over the box those feet span.
fn probe_points(bsp: &Bsp) -> Vec<[f64; 3]> {
    let mut out: Vec<[f64; 3]> = bsp
        .nodes
        .iter()
        .take(256)
        .map(|n| {
            [
                -(n.normal[0] as f64 * n.dist as f64),
                -(n.normal[1] as f64 * n.dist as f64),
                -(n.normal[2] as f64 * n.dist as f64),
            ]
        })
        .collect();
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in &out {
        for c in 0..3 {
            lo[c] = lo[c].min(p[c]);
            hi[c] = hi[c].max(p[c]);
        }
    }
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                let t = [a, b, c];
                out.push(std::array::from_fn(|k| {
                    lo[k] + (hi[k] - lo[k]) * t[k] as f64 / 3.0
                }));
            }
        }
    }
    out
}

/// Preprocess and compile every shipped `.lua`, the way the engine will have
/// to at run time: through [`Install::read`], so the `override/` copy of a
/// script wins over the container's.
///
/// Compiling is not running. Running them needs the 133 engine functions a
/// boot touches, which is the next milestone; this says the Lua 3 source is
/// accepted by a Lua 5.1 the engine carries itself.
fn compile_scripts(install: &mut Install, list: bool) -> usize {
    let mut names: Vec<String> = entries_named(install, ".lua")
        .into_iter()
        .map(|(n, _, _)| n)
        .collect();
    if let Some(dir) = install.override_dir.clone() {
        if let Ok(read) = std::fs::read_dir(&dir) {
            for e in read.flatten() {
                let n = e.file_name().to_string_lossy().to_ascii_lowercase();
                if n.ends_with(".lua") && !names.contains(&n) {
                    names.push(n);
                }
            }
        }
    }
    names.sort();
    names.dedup();

    let engine = match Scripts::new() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("goodomen: lua: {e}");
            std::process::exit(1);
        }
    };
    let (mut upvalues, mut breaks) = (0usize, 0usize);
    for name in &names {
        let data = match install.read(name) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("goodomen: {name}: {e}");
                std::process::exit(1);
            }
        };
        // the scripts are Latin-1 in places, and Lua does not care
        let source: String = data.iter().map(|&b| b as char).collect();
        match engine.compile(name, &source) {
            Ok((u, b)) => {
                upvalues += u;
                breaks += b;
                if list {
                    println!("{name} {u} {b}");
                }
            }
            Err(e) => {
                eprintln!("goodomen: {name}: {e}");
                std::process::exit(1);
            }
        }
    }
    eprintln!(
        "{} scripts compile, {upvalues} upvalue references and {breaks} \
         `break` variables rewritten",
        names.len()
    );
    names.len()
}

/// **Run** every scene graph, rather than parse it, and print each object the
/// way `tools/scene.py` describes it.
///
/// This is the check that the whole Lua side works: the file is Lua 3, the
/// preprocessor and the prelude have to carry it, `mdkRegisterObject` has to
/// take twenty arguments in the right order, the `OBJ_*` constants have to
/// have the values the binary gives them, and each object's parent has to be
/// reachable as the global the previous registration made. Any one of those
/// wrong and the objects come out different.
///
/// -> the number of objects registered, not of files.
fn scene_graphs(install: &mut Install, list: bool) -> usize {
    let mut total = 0usize;
    let mut files = 0usize;
    for (name, i, j) in entries_named(install, ".lua") {
        let data = match install.containers[i].read_at(j) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("goodomen: {name}: {e}");
                std::process::exit(1);
            }
        };
        let source: String = data.iter().map(|&b| b as char).collect();
        if !source.contains("mdkRegisterObject(") {
            continue;
        }
        files += 1;

        // a fresh state each time: a scene graph names its parents as
        // globals, and two levels reuse names
        let engine = Scripts::new().and_then(|s| {
            goodomen::game::world::install(&s.lua)?;
            Ok(s)
        });
        let engine = match engine {
            Ok(s) => s,
            Err(e) => {
                eprintln!("goodomen: {name}: {e}");
                std::process::exit(1);
            }
        };
        if let Err(e) = engine.run(&name, &source) {
            eprintln!("goodomen: {name}: {e}");
            std::process::exit(1);
        }
        let world = goodomen::game::world::world(&engine.lua).expect("a world");
        for (_id, gob) in world.iter() {
            total += 1;
            if list {
                let parent = gob
                    .parent
                    .and_then(|p| world.get(p))
                    .map(|g| g.name.as_str())
                    .unwrap_or("");
                println!(
                    "{name}\t{}\t{}\t{parent}\t{} {} {}\t{} {} {} {}\t{}",
                    gob.name,
                    gob.kind,
                    gob.position[0], gob.position[1], gob.position[2],
                    gob.rotation[0], gob.rotation[1], gob.rotation[2], gob.rotation[3],
                    gob.resource.as_deref().unwrap_or("")
                );
            }
        }
    }
    eprintln!("{files} scene graphs run, {total} objects registered");
    total
}

/// Every container member with this extension, lowercased and sorted, so that
/// two implementations walking different copies of the game agree on order.
fn entries_named(install: &Install, extension: &str) -> Vec<(String, usize, usize)> {
    let mut found = Vec::new();
    for (i, c) in install.containers.iter().enumerate() {
        for (j, e) in c.entries().iter().enumerate() {
            let name = e.name.to_ascii_lowercase();
            if name.ends_with(extension) {
                found.push((name, i, j));
            }
        }
    }
    found.sort();
    found
}

/// Parse every `.mod`, pose it, and print what `tools/modcheck.py` compares.
///
/// Sums rather than checksums, because the arithmetic crosses a slerp and two
/// implementations of `acos` need not agree in the last bit. The sums are
/// over every vertex and every node, so a systematic error — a missed
/// hierarchy, a transposed quaternion, a strip wound the wrong way — moves
/// them far more than rounding ever could.
fn meshes(install: &mut Install, list: bool) -> usize {
    let found = entries_named(install, ".mod");
    let (mut nodes, mut tris, mut animated) = (0usize, 0usize, 0usize);

    for (name, i, j) in &found {
        let data = match install.containers[*i].read_at(*j) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("goodomen: {name}: {e}");
                std::process::exit(1);
            }
        };
        let model = match Model::parse(&data) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("goodomen: {name}: {e}");
                std::process::exit(1);
            }
        };
        let mesh = model.posed();
        let mut p = [0.0f64; 3];
        for v in &mesh.positions {
            for c in 0..3 {
                p[c] += v[c];
            }
        }
        // animation 0 at the middle of its loop: t = 0 would sample only the
        // first key of every channel and never exercise the interpolation
        let (mut q, mut o) = (0.0f64, 0.0f64);
        if let Some(anim) = model.animations.first() {
            for (quat, off) in model.node_world(anim, 0.5) {
                q += quat.iter().sum::<f64>();
                o += off.iter().sum::<f64>();
            }
        }

        nodes += model.nodes.len();
        tris += mesh.triangles.len();
        animated += model.animated() as usize;
        if list {
            println!(
                "{name} {} {} {} {} {} {} {:.6} {:.6} {:.6} {:.6} {:.6}",
                model.nodes.len(),
                model.groups.len(),
                model.vertices.len(),
                model.refs.len(),
                model.animations.len(),
                mesh.triangles.len(),
                p[0],
                p[1],
                p[2],
                q,
                o
            );
        }
    }
    eprintln!(
        "{} models, {animated} animated, {nodes} nodes, {tris} triangles",
        found.len()
    );
    found.len()
}
