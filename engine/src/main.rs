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
use goodomen::render::camera::Mat4;
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
    // `--demo l1.lua demo1_5.omn --from x,y,z --yaw a` replays the game's
    // own recorded input through the controller. No GL, so it runs anywhere.
    if let Some(i) = args.iter().position(|a| a == "--demo") {
        let value = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .and_then(|k| args.get(k + 1))
                .cloned()
        };
        let graph = args.get(i + 1).cloned().unwrap_or_else(|| "l1.lua".into());
        let demo = args.get(i + 2).cloned().unwrap_or_else(|| "demo1_5.omn".into());
        let start: Vec<f64> = value("--from")
            .unwrap_or_default()
            .split(',')
            .filter_map(|v| v.trim().parse().ok())
            .collect();
        if start.len() != 3 {
            eprintln!("goodomen: --demo needs --from x,y,z");
            std::process::exit(1);
        }
        let yaw = value("--yaw").and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let mouse = value("--mouse").and_then(|v| v.parse().ok()).unwrap_or(1.0);
        let root = args
            .iter()
            .zip(std::iter::once(&String::new()).chain(args.iter()))
            .find(|(a, before)| {
                !a.starts_with("--")
                    && !["--demo", "--from", "--yaw", "--mouse", "--expect"]
                        .contains(&before.as_str())
                    && *before != &graph
            })
            .map(|(a, _)| std::path::PathBuf::from(a))
            .unwrap_or_else(Install::beside_the_binary);
        match replay(&root, &graph, &demo, [start[0], start[1], start[2]], yaw, mouse) {
            Ok(line) => println!("{line}"),
            Err(e) => {
                eprintln!("goodomen: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    // `--level l1` renders a level; with `--window` it is shown, without it
    // the pixels are checked offscreen the way `--triangle` is
    if let Some(i) = args.iter().position(|a| a == "--level") {
        let graph = args.get(i + 1).cloned().unwrap_or_else(|| "l1.lua".into());
        let show = args.iter().any(|a| a == "--window");
        let root = args
            .iter()
            .zip(std::iter::once(&String::new()).chain(args.iter()))
            .find(|(a, before)| {
                !a.starts_with("--") && *before != "--level" && *before != "--expect"
            })
            .map(|(a, _)| std::path::PathBuf::from(a))
            .unwrap_or_else(Install::beside_the_binary);
        let start: Option<[f64; 3]> = args
            .iter()
            .position(|a| a == "--from")
            .and_then(|k| args.get(k + 1))
            .and_then(|v| {
                let c: Vec<f64> = v.split(',').filter_map(|n| n.trim().parse().ok()).collect();
                (c.len() == 3).then(|| [c[0], c[1], c[2] + goodomen::game::body::EYE])
            });
        match level(&root, &graph, show, start) {
            Ok(line) => println!("{line}"),
            Err(e) if e.starts_with(goodomen::render::NO_VIDEO) => println!("skip: {e}"),
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

/// Replay a recorded demo through the controller and say where the body went.
///
/// The invariant is the one that can be asserted without the original: the
/// game's own input, run through this controller, must never put the body
/// inside the world.
fn replay(
    root: &std::path::Path,
    graph: &str,
    demo: &str,
    start: [f64; 3],
    yaw: f64,
    mouse: f64,
) -> Result<String, String> {
    use goodomen::formats::omn;
    use goodomen::game::body::{Body, Collision, EYE, WALK};
    use goodomen::game::{script::Scripts, world};

    let mut install = Install::open(root).map_err(|e| e.to_string())?;
    let source: String = install
        .read(graph)
        .map_err(|e| e.to_string())?
        .iter()
        .map(|&b| b as char)
        .collect();
    let scripts = Scripts::new().map_err(|e| e.to_string())?;
    world::install(&scripts.lua).map_err(|e| e.to_string())?;
    scripts.run(graph, &source).map_err(|e| e.to_string())?;

    let collision = {
        let w = world::world(&scripts.lua).expect("a world");
        Collision::load(&mut install, &w)
    };
    let bytes = install.read(demo).map_err(|e| e.to_string())?;
    // frame 0 is the load and carries no input
    let frames = omn::parse(&bytes).map_err(|e| e.to_string())?;
    let frames = &frames[1.min(frames.len())..];

    let mut body = Body::new([start[0], start[1], start[2] + EYE], yaw);
    body.replay(&collision, frames, mouse, WALK);

    let seconds: f32 = frames.iter().map(|f| f.dt).sum();
    let drift = (0..3)
        .map(|c| (body.position[c] - [start[0], start[1], start[2] + EYE][c]).powi(2))
        .sum::<f64>()
        .sqrt();
    Ok(format!(
        "{demo}: {} frames, {seconds:.1}s, {} trees and {} nodes; travelled {:.0} units, \
         {drift:.0} from where it started, {} at the end, met a wall on {} frames, \
         inside geometry on {}",
        frames.len(),
        collision.len(),
        collision.nodes,
        body.travelled,
        if body.on_ground { "standing" } else { "in the air" },
        body.hits,
        body.inside
    ))
}

/// Load a level and draw it — into a window if `show`, otherwise offscreen
/// with the pixels checked.
///
/// The camera has nothing to go on yet, so it frames the level's own extent
/// from a fixed direction: deterministic, and the same picture every run.
fn level(
    root: &std::path::Path,
    graph: &str,
    show: bool,
    start: Option<[f64; 3]>,
) -> Result<String, String> {
    use goodomen::render::{scene::Scene, Offscreen};

    let mut install = Install::open(root).map_err(|e| e.to_string())?;
    let mut video = Video::open("goodomen", 1024, 768, show)?;
    let version = video.version();
    let mut scene = Scene::default();

    // SAFETY: the context is current on this thread from here on.
    let loaded = unsafe {
        goodomen::game::level::load(&video.gl, &mut install, &mut scene, graph)
            .map_err(|e| e.to_string())?
    };
    let (lo, hi) = scene.bounds();
    let centre = [
        (lo[0] + hi[0]) / 2.0,
        (lo[1] + hi[1]) / 2.0,
        (lo[2] + hi[2]) / 2.0,
    ];
    let span = (0..3).map(|c| hi[c] - lo[c]).fold(1.0f32, f32::max);
    let eye = [
        centre[0] + span * 0.7,
        centre[1] - span * 0.7,
        centre[2] + span * 0.4,
    ];
    let view = Mat4::look_at(eye, centre, [0.0, 0.0, 1.0]);

    let summary = format!(
        "{graph}: {} objects, {} placed ({} name no model), {} triangles, \
         {} draws, {span:.0} units across",
        loaded.objects,
        loaded.placed,
        loaded.without_a_model,
        loaded.triangles,
        scene.draw_count()
    );

    if show {
        use goodomen::game::body::{Body, Collision, EYE, SPRINT, WALK};
        use sdl2::event::Event;
        use sdl2::keyboard::{Keycode, Scancode};

        // walking needs the collision world, and a place to stand: the
        // checkpoints live in the level *script*, which wants the boot the
        // engine does not have yet, so `--from` supplies one
        let walk = std::env::args().any(|a| a == "--walk");
        let mut body = Body::new(start.unwrap_or([eye[0] as f64, eye[1] as f64, eye[2] as f64]), 0.0);
        let collision = if walk {
            let scripts = goodomen::game::script::Scripts::new().map_err(|e| e.to_string())?;
            goodomen::game::world::install(&scripts.lua).map_err(|e| e.to_string())?;
            let source: String = install
                .read(graph)
                .map_err(|e| e.to_string())?
                .iter()
                .map(|&b| b as char)
                .collect();
            scripts.run(graph, &source).map_err(|e| e.to_string())?;
            let w = goodomen::game::world::world(&scripts.lua).expect("a world");
            Collision::load(&mut install, &w)
        } else {
            Collision::default()
        };

        println!("OpenGL {version}\n{summary}");
        println!(
            "{}  --  W A S D to move, mouse to look, shift to run, escape to leave",
            if walk {
                format!("walking, {} trees under foot", collision.len())
            } else {
                "flying".to_string()
            }
        );
        video.sdl.mouse().set_relative_mouse_mode(true);
        let (mut yaw, mut pitch) = (0.0f64, -0.2f64);
        let mut last = std::time::Instant::now();
        loop {
            for event in video.events.poll_iter() {
                match event {
                    Event::Quit { .. }
                    | Event::KeyDown { keycode: Some(Keycode::Escape), .. } => {
                        unsafe { scene.delete(&video.gl) };
                        return Ok(format!("{summary}, left at {:?}", body.position));
                    }
                    Event::MouseMotion { xrel, yrel, .. } => {
                        yaw -= xrel as f64 * 0.0025;
                        pitch = (pitch - yrel as f64 * 0.0025).clamp(-1.5, 1.5);
                    }
                    _ => {}
                }
            }
            let dt = last.elapsed().as_secs_f64().min(0.1);
            last = std::time::Instant::now();

            let keys: std::collections::HashSet<Scancode> =
                video.events.keyboard_state().pressed_scancodes().collect();
            let held = |s: Scancode| keys.contains(&s);
            let (fx, fy) = (yaw.cos(), yaw.sin());
            let mut d = [0.0f64, 0.0];
            if held(Scancode::W) || held(Scancode::Up) {
                d = [d[0] + fx, d[1] + fy];
            }
            if held(Scancode::S) || held(Scancode::Down) {
                d = [d[0] - fx, d[1] - fy];
            }
            if held(Scancode::D) {
                d = [d[0] - fy, d[1] + fx];
            }
            if held(Scancode::A) {
                d = [d[0] + fy, d[1] - fx];
            }
            let fast = held(Scancode::LShift) || held(Scancode::RShift);

            if walk {
                body.step(
                    &collision,
                    d,
                    held(Scancode::Space),
                    if fast { SPRINT } else { WALK },
                    dt,
                );
            } else {
                // flying: no collision, and fast enough to cross a level
                let speed = span as f64 * if fast { 0.8 } else { 0.25 };
                let length = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1.0);
                body.position[0] += d[0] / length * speed * dt * pitch.cos();
                body.position[1] += d[1] / length * speed * dt * pitch.cos();
                if d != [0.0, 0.0] {
                    body.position[2] += pitch.sin() * speed * dt;
                }
                if held(Scancode::Space) {
                    body.position[2] += speed * dt;
                }
                if held(Scancode::LCtrl) {
                    body.position[2] -= speed * dt;
                }
            }

            let from = [
                body.position[0] as f32,
                body.position[1] as f32,
                body.position[2] as f32,
            ];
            let ahead = [
                from[0] + (yaw.cos() * pitch.cos()) as f32,
                from[1] + (yaw.sin() * pitch.cos()) as f32,
                from[2] + pitch.sin() as f32,
            ];
            let view = Mat4::look_at(from, ahead, [0.0, 0.0, 1.0]);
            let (w, h) = video.window.drawable_size();
            let projection =
                Mat4::perspective(1.1, w as f32 / h.max(1) as f32, 0.05, span * 4.0);
            unsafe {
                video.gl.viewport(0, 0, w as i32, h as i32);
                video.gl.clear_color(0.05, 0.06, 0.09, 1.0);
                video.gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
                scene.draw(&video.gl, &projection.times(&view), span * 2.0)?;
            }
            video.window.gl_swap_window();
            let _ = EYE;
        }
    }

    // offscreen, and then three questions of the pixels
    let (width, height) = (512i32, 512i32);
    unsafe {
        let target = Offscreen::new(&video.gl, width, height)?;
        let projection = Mat4::perspective(1.1, 1.0, span * 0.002, span * 4.0);
        video.gl.clear_color(0.05, 0.06, 0.09, 1.0);
        video.gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
        scene.draw(&video.gl, &projection.times(&view), span * 2.0)?;
        video.gl.finish();

        let mut pixels = vec![0u8; (width * height * 4) as usize];
        video.gl.read_pixels(
            0,
            0,
            width,
            height,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut pixels)),
        );
        target.delete(&video.gl);
        scene.delete(&video.gl);

        let clear = [13u8, 15, 23];
        let mut drawn = 0usize;
        let mut colours = std::collections::HashSet::new();
        for p in pixels.chunks_exact(4) {
            if (0..3).any(|c| (p[c] as i32 - clear[c] as i32).abs() > 1) {
                drawn += 1;
                colours.insert((p[0], p[1], p[2]));
            }
        }
        // `--save PATH` writes the frame as a plain PPM, which is ten lines
        // and needs no encoder. It is for looking at while working; nothing
        // in the checks reads it.
        if let Some(i) = std::env::args().position(|a| a == "--save") {
            if let Some(path) = std::env::args().nth(i + 1) {
                let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
                // GL counts rows from the bottom, PPM from the top
                for row in (0..height).rev() {
                    for col in 0..width {
                        let o = ((row * width + col) * 4) as usize;
                        ppm.extend_from_slice(&pixels[o..o + 3]);
                    }
                }
                std::fs::write(&path, ppm).map_err(|e| e.to_string())?;
            }
        }

        let coverage = drawn as f64 / (width * height) as f64;
        if coverage < 0.02 {
            return Err(format!("almost nothing drawn: {:.1}% of the frame", coverage * 100.0));
        }
        // a level drawn with the wrong UVs, or with the textures not arriving,
        // comes out in a handful of flat colours
        if colours.len() < 256 {
            return Err(format!("only {} distinct colours -- the textures did not arrive", colours.len()));
        }
        Ok(format!(
            "OpenGL {version}: {summary}, {:.0}% of the frame in {} colours",
            coverage * 100.0,
            colours.len()
        ))
    }
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
