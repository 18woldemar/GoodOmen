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
    let audio = args.iter().any(|a| a == "--wav");
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
    // `--boot [N [CP]]` starts a level the way the game does. No GL.
    if let Some(i) = args.iter().position(|a| a == "--boot") {
        // the level and checkpoint are positional and both optional, so a
        // flag right after `--boot` must not be read as one
        let n: u32 = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0);
        let cp: u32 = if n > 0 {
            args.get(i + 2).and_then(|v| v.parse().ok()).unwrap_or(0)
        } else {
            0
        };
        let root = args
            .iter()
            .zip(std::iter::once(&String::new()).chain(args.iter()))
            .find(|(a, before)| {
                !a.starts_with("--")
                    && a.parse::<u32>().is_err()
                    && *before != "--expect"
            })
            .map(|(a, _)| std::path::PathBuf::from(a))
            .unwrap_or_else(Install::beside_the_binary);
        match boot(
            &root,
            n,
            cp,
            args.iter().any(|a| a == "--work-list"),
            args.iter().any(|a| a == "--events"),
        ) {
            Ok(line) => println!("{line}"),
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

    // `--run N [CP] [SECONDS]` starts a level and *runs* it: the controller
    // walks, the driver ticks, the handlers fire. No GL.
    if let Some(i) = args.iter().position(|a| a == "--run") {
        let n: u32 = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(1);
        let cp: u32 = args.get(i + 2).and_then(|v| v.parse().ok()).unwrap_or(1);
        let seconds: f64 = args.get(i + 3).and_then(|v| v.parse().ok()).unwrap_or(10.0);
        let root = args
            .iter()
            .zip(std::iter::once(&String::new()).chain(args.iter()))
            .find(|(a, before)| {
                !a.starts_with("--") && a.parse::<f64>().is_err() && *before != "--expect"
            })
            .map(|(a, _)| std::path::PathBuf::from(a))
            .unwrap_or_else(Install::beside_the_binary);
        match run(&root, n, cp, seconds) {
            Ok(line) => println!("{line}"),
            Err(e) => {
                eprintln!("goodomen: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    // `--play N [CP]` starts level N at checkpoint CP and draws it from
    // there, culled by the room the camera stands in.
    if let Some(i) = args.iter().position(|a| a == "--play") {
        let n: u32 = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(1);
        let cp: u32 = args.get(i + 2).and_then(|v| v.parse().ok()).unwrap_or(1);
        let root = args
            .iter()
            .zip(std::iter::once(&String::new()).chain(args.iter()))
            .find(|(a, before)| {
                !a.starts_with("--") && a.parse::<u32>().is_err() && *before != "--expect"
            })
            .map(|(a, _)| std::path::PathBuf::from(a))
            .unwrap_or_else(Install::beside_the_binary);
        match play(&root, n, cp, args.iter().any(|a| a == "--window")) {
            Ok(line) => println!("{line}"),
            Err(e) if e.starts_with(goodomen::render::NO_VIDEO) => println!("skip: {e}"),
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
    // the mixer needs no game files either, and answers the same way the
    // renderer does: nothing audible, nothing on screen, and a skip rather
    // than a failure where there is no device
    if args.iter().any(|a| a == "--sound") {
        match goodomen::audio::selfcheck() {
            Ok(line) => println!("{line}"),
            Err(e) if e.starts_with(goodomen::audio::NO_AUDIO) => println!("skip: {e}"),
            Err(e) => {
                eprintln!("goodomen: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    // `--rand SEED N` prints the game's own random sequence, which
    // `tools/rand.py` holds against the original's under emulation.
    if let Some(i) = args.iter().position(|a| a == "--rand") {
        let seed: u32 = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(127);
        let count: usize = args.get(i + 2).and_then(|v| v.parse().ok()).unwrap_or(8);
        let mut r = goodomen::game::rand::Random::default();
        r.seed(seed);
        for _ in 0..count {
            println!("{}", r.next_u32());
        }
        return;
    }

    // `--health` prints the enemy table at each of the four difficulties, so
    // `tools/health.py` can hold it against the binary it came out of.
    // Any bases after it are scaled too, which is how the rule that keeps a
    // non-zero base off zero gets checked: the table has no record small
    // enough to reach it.
    if let Some(i) = args.iter().position(|a| a == "--health") {
        use goodomen::game::world::{diff_scale, BASE_HITPOINTS, DIFFICULTY};
        let scaled = |base: i32| -> String {
            DIFFICULTY
                .iter()
                .map(|(_, d)| diff_scale(*d, base).to_string())
                .collect::<Vec<_>>()
                .join(" ")
        };
        for (kind, name, base) in BASE_HITPOINTS {
            println!("{} {} {} {}", kind as i64, name, base, scaled(base));
        }
        for base in args[i + 1..].iter().filter_map(|a| a.parse::<i32>().ok()) {
            println!("- - {} {}", base, scaled(base));
        }
        return;
    }

    // `--music DIR` reads the 27 bare ACM streams under `Music/` and checks
    // the two things streaming them adds: that rewinding really starts over,
    // and that a queued source produces sound.
    if let Some(i) = args.iter().position(|a| a == "--music") {
        let dir = args
            .get(i + 1)
            .filter(|a| !a.starts_with("--"))
            .map(std::path::PathBuf::from)
            .unwrap_or_else(Install::beside_the_binary);
        let expect = args
            .iter()
            .position(|a| a == "--expect")
            .and_then(|i| args.get(i + 1))
            .and_then(|n| n.parse::<usize>().ok());
        match music(&dir, expect) {
            Ok(line) => println!("{line}"),
            Err(e) => {
                eprintln!("goodomen: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    // `--pcm file.wav` writes one sound's samples to stdout as s16le, which
    // is what `ffmpeg -f acm` writes too, so the two can be diffed directly.
    if let Some(i) = args.iter().position(|a| a == "--pcm") {
        let Some(path) = args.get(i + 1) else {
            eprintln!("goodomen: --pcm needs a file");
            std::process::exit(1);
        };
        let data = std::fs::read(path).unwrap_or_else(|e| {
            eprintln!("goodomen: {path}: {e}");
            std::process::exit(1);
        });
        // the music is a bare stream; everything else carries the wrapper
        let acm = match goodomen::formats::wavc::parse(&data) {
            Ok(s) => s.acm.to_vec(),
            Err(_) => data.clone(),
        };
        match goodomen::formats::acm::decode(&acm) {
            Ok(pcm) => {
                use std::io::Write;
                let bytes: Vec<u8> = pcm.iter().flat_map(|v| v.to_le_bytes()).collect();
                std::io::stdout().write_all(&bytes).ok();
            }
            Err(e) => {
                eprintln!("goodomen: {path}: {e}");
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

    if tex || models || trees || scripts || graphs || audio {
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
        } else if graphs {
            ("objects", scene_graphs(&mut install, expect.is_none()))
        } else {
            ("sounds", sounds(&mut install, expect.is_none()))
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

/// Where a track number's stream is. `mdkRoomSetMusic(room, N)` means
/// `Music/TrackNN`, identity and not an offset, and the `.mus` beside it names
/// one segment `A` looping back to itself — so the file is
/// `Music/TrackNN/tracknna.acm`. **The case on disk is inconsistent**
/// (`track01a.acm` sits beside `Track18a.acm`), so it is matched folded.
fn music_track(root: &std::path::Path, n: u32) -> Option<std::path::PathBuf> {
    let want = format!("track{n:02}a.acm");
    let mut stack = vec![root.join("Music")];
    // the directory itself may be `music/`
    if !stack[0].is_dir() {
        stack[0] = root.join("music");
    }
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p
                .file_name()
                .is_some_and(|f| f.to_string_lossy().to_ascii_lowercase() == want)
            {
                return Some(p);
            }
        }
    }
    None
}

/// The music: every `.acm` under a directory, checked for the two things
/// streaming adds to decoding.
///
/// **Rewinding has to be a real restart.** The decoder carries state between
/// blocks — a wrap buffer the inverse transform reads either side of its
/// window — so going back to the first block without clearing it would give
/// a second time round that is not the first. That is what makes a looping
/// track sound wrong on the seam and nowhere else, which is the worst kind of
/// bug to be told about.
fn music(root: &std::path::Path, expect: Option<usize>) -> Result<String, String> {
    use goodomen::formats::acm;

    let dir = [root.join("Music"), root.join("music"), root.to_path_buf()]
        .into_iter()
        .find(|p| p.is_dir())
        .ok_or_else(|| format!("no Music under {}", root.display()))?;
    let mut tracks: Vec<std::path::PathBuf> = Vec::new();
    let mut stack = vec![dir.clone()];
    while let Some(d) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&d) else { continue };
        for e in read.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("acm")) {
                tracks.push(p);
            }
        }
    }
    tracks.sort();
    if tracks.is_empty() {
        return Err(format!("no .acm streams under {}", dir.display()));
    }

    // four blocks is enough to have filled the wrap buffer and used it
    const BLOCKS: usize = 4;
    let (mut seconds, mut stereo) = (0.0f64, 0usize);
    for path in &tracks {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut s = acm::Stream::open(bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let h = s.header;
        if h.channels >= 2 {
            stereo += 1;
        }
        seconds += h.samples as f64 / (h.rate as f64 * h.channels.max(1) as f64);
        let mut first = Vec::new();
        for _ in 0..BLOCKS {
            s.block(&mut first);
        }
        s.rewind();
        let mut again = Vec::new();
        for _ in 0..BLOCKS {
            s.block(&mut again);
        }
        if first != again {
            let at = first.iter().zip(&again).position(|(a, b)| a != b);
            return Err(format!(
                "{}: rewinding did not start over — {} samples, first differs at {at:?}",
                path.file_name().unwrap_or_default().to_string_lossy(),
                first.len()
            ));
        }
    }
    if let Some(want) = expect {
        if tracks.len() != want {
            return Err(format!("{} streams, expected {want}", tracks.len()));
        }
    }

    // and the naming rule the rooms rely on: `mdkRoomSetMusic(room, N)` is
    // `Music/TrackNN/tracknna.acm`, identity and not an offset. Every index a
    // room names is in 1..=27, so every one of those must resolve.
    let unresolved: Vec<u32> =
        (1..=tracks.len() as u32).filter(|&n| music_track(root, n).is_none()).collect();
    if !unresolved.is_empty() {
        return Err(format!(
            "no file for track {unresolved:?} — the Music/TrackNN naming does not hold"
        ));
    }

    // and one of them through the mixer, on the loopback device so that
    // nothing is played at anyone
    let played = match goodomen::audio::Audio::loopback(44100) {
        Ok(mut audio) => {
            let bytes = std::fs::read(&tracks[0]).map_err(|e| e.to_string())?;
            let mut track = audio.music(bytes)?;
            let queued = audio.queued(&track);
            let mut peak = 0f32;
            // a second of it, a frame at a time, pumping as a game would
            for _ in 0..60 {
                let block = audio.render(735)?;
                peak = peak.max(
                    block.iter().map(|s| s.unsigned_abs() as f32).fold(0.0, f32::max) / 32767.0,
                );
                audio.pump(&mut track)?;
            }
            if peak < 0.01 {
                return Err(format!("the queued track rendered silence (peak {peak:.4})"));
            }
            format!(", {queued} buffers ahead, peak {peak:.2} through the mixer")
        }
        Err(e) if e.starts_with(goodomen::audio::NO_AUDIO) => String::new(),
        Err(e) => return Err(e),
    };

    Ok(format!(
        "{} music streams, {stereo} stereo, {:.0}h{:02.0}m in all, \
         every TrackNN resolves, all rewind to the sample{played}",
        tracks.len(),
        (seconds / 3600.0).floor(),
        (seconds % 3600.0) / 60.0,
    ))
}

/// Start levels the way the game does, and report what they asked for.
///
/// With `n` and `cp` zero it starts **every** level at **every** checkpoint,
/// which is `tools/boot.py`'s measure and the one that says the sequence not
/// only runs but can be satisfied.
fn boot(
    root: &std::path::Path,
    n: u32,
    cp: u32,
    work_list: bool,
    events: bool,
) -> Result<String, String> {
    let mut install = Install::open(root).map_err(|e| e.to_string())?;
    // every script, in one map, so the engine's `dofile` can resolve a bare
    // resource name the way the original's does
    let mut sources = std::collections::BTreeMap::new();
    for (name, i, j) in entries_named(&install, ".lua") {
        if let Ok(bytes) = install.containers[i].read_at(j) {
            sources.insert(name, bytes.iter().map(|&b| b as char).collect::<String>());
        }
    }
    if let Some(dir) = install.override_dir.clone() {
        if let Ok(read) = std::fs::read_dir(&dir) {
            for e in read.flatten() {
                let name = e.file_name().to_string_lossy().to_ascii_lowercase();
                if name.ends_with(".lua") {
                    if let Ok(bytes) = std::fs::read(e.path()) {
                        sources.insert(name, bytes.iter().map(|&b| b as char).collect());
                    }
                }
            }
        }
    }

    // which checkpoints each level has is what the previous boot found, so
    // the sweep asks each level once and then starts each checkpoint it named
    let levels: Vec<u32> = if n > 0 { vec![n] } else { (1..=10).collect() };
    let (mut started, mut rooms, mut checkpoints) = (0usize, 0usize, 0usize);
    let (mut commands, mut bindings, mut stasis, mut timers) = (0usize, 0usize, 0usize, 0usize);
    let (mut fired, mut survived) = (0usize, 0usize);
    let (mut moves, mut playing) = (0u64, 0usize);
    let (mut spawners, mut spawned, mut armed, mut died) = (0usize, 0usize, 0usize, 0usize);
    let (mut queued, mut destroyed, mut homeless) = (0usize, 0usize, 0usize);
    let mut alerted = 0usize;
    let mut why: std::collections::BTreeMap<String, usize> = Default::default();
    let mut faults: std::collections::BTreeSet<u32> = Default::default();
    let mut resources = std::collections::BTreeSet::new();
    let mut sounds = std::collections::BTreeSet::new();
    let mut attached = std::collections::BTreeSet::new();
    let mut plays = 0usize;
    let mut work: std::collections::BTreeMap<String, usize> = Default::default();
    let mut failures = Vec::new();

    for level in levels {
        // one boot to learn the checkpoints, then one per checkpoint
        let first = match run_one(&sources, level, cp.max(1), "sectionA") {
            Ok(b) => b,
            Err(e) => {
                failures.push(format!("l{level} cp{}: {e}", cp.max(1)));
                continue;
            }
        };
        let numbers: Vec<u32> = if cp > 0 {
            vec![cp]
        } else {
            let mut v: Vec<u32> = first.checkpoints.iter().map(|c| c.index as u32).collect();
            v.sort_unstable();
            v.dedup();
            if v.is_empty() { vec![1] } else { v }
        };
        // a level's rooms and checkpoints are the same at every checkpoint,
        // so they are counted once per level rather than once per boot
        rooms += first.rooms.len();
        checkpoints += first.checkpoints.len();
        commands = commands.max(first.input.commands.len());
        bindings = bindings.max(first.input.bindings.len());
        faults.extend(first.input.faults());
        stasis += first.stasis.len();
        timers += first.timers.len();
        for number in numbers {
            match run_one_with(&sources, level, number, "sectionA", events) {
                Ok((b, f, s, r)) => {
                    fired += f;
                    survived += s;
                    for (kind, count) in r {
                        *why.entry(kind).or_insert(0) += count;
                    }
                    started += 1;
                    resources.extend(b.resources.iter().cloned());
                    sounds.extend(b.sounds.iter().cloned());
                    // the sounds the handlers hang on objects, which are a
                    // different loader from the hard-coded ones and are named
                    // without an extension like every other resource slot
                    attached.extend(b.gob_sounds.iter().map(|g| g.sound.clone()));
                    plays += b.gob_sounds.iter().map(|g| g.played).sum::<usize>();
                    moves += b.moves;
                    playing += b.playing.len();
                    spawners += b.spawners.len();
                    spawned += b.spawned.len();
                    armed += b.spawned.iter().filter(|(_, hp)| *hp > 0).count();
                    died += b.died.len();
                    destroyed += b.destroyed.len();
                    homeless += b.homeless;
                    alerted += b.alerted.len();
                    // spawners a handler has filled but nothing has
                    // drained: a boot fires handlers and never ticks
                    queued += b.spawners.values().filter(|s| s.queue > 0).count();
                    for (name, count) in &b.unimplemented {
                        *work.entry(name.clone()).or_insert(0) += count;
                    }
                }
                Err(e) => failures.push(format!("l{level} cp{number}: {e}")),
            }
        }
    }

    let mut missing = 0usize;
    for name in resources.iter().chain(sounds.iter()).chain(attached.iter()) {
        if install.read(name).is_err()
            && install.read(&format!("{name}.mod")).is_err()
            && install.read(&format!("{name}.tex")).is_err()
            && install.read(&format!("{name}.wav")).is_err()
        {
            missing += 1;
        }
    }

    if work_list {
        let mut ranked: Vec<(&String, &usize)> = work.iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (name, count) in ranked {
            println!("{name} {count}");
        }
        // and why the handlers that stopped, stopped: each kind names state
        // the engine does not hold yet
        let mut stopped: Vec<(&String, &usize)> = why.iter().collect();
        stopped.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (kind, count) in stopped.iter().take(20) {
            println!("stopped {count}\t{kind}");
        }
    }
    for f in failures.iter().take(10) {
        eprintln!("goodomen: {f}");
    }
    if !failures.is_empty() {
        return Err(format!("{} of {} boots failed", failures.len(), failures.len() + started));
    }
    for (what, got, want) in [
        ("boots", started, args_expect()),
        ("resources", resources.len(), expect_flag("--expect-resources")),
        ("rooms", rooms, expect_flag("--expect-rooms")),
        ("bindings", bindings, expect_flag("--expect-bindings")),
        ("handler calls", fired, expect_flag("--expect-events")),
        ("handlers ran to the end", survived, expect_flag("--expect-survived")),
        ("sounds played", plays, expect_flag("--expect-plays")),
        ("objects spawned", spawned, expect_flag("--expect-spawned")),
        // every spawned object is an enemy, so every one should arrive with
        // hitpoints; a gap here means a type the table at 0x4ab2e8 misses
        ("of them with hitpoints", armed, expect_flag("--expect-armed")),
        ("objects killed", died, expect_flag("--expect-died")),
        ("objects streamed out", destroyed, expect_flag("--expect-destroyed")),
        // the one that decides whether the delete lists are applied at the
        // right moment: a checkpoint must never lose its own room
        ("checkpoints left roomless", homeless, expect_flag("--expect-roomless")),
        ("walkers alerted", alerted, expect_flag("--expect-alerted")),
    ] {
        if let Some(want) = want {
            if got != want {
                return Err(format!("{got} {what}, expected {want}"));
            }
        }
    }
    if missing > 0 {
        return Err(format!("{missing} named resources are not in the game"));
    }
    // every bound id must be a DirectInput scancode or one of the six mouse
    // ids, which is the same check `tools/walksim.py --keys` makes
    if !faults.is_empty() {
        return Err(format!("{} bound ids are neither: {faults:?}", faults.len()));
    }
    Ok(format!(
        "{started} boots, {} resources and {} hard-coded sounds named \
         ({missing} missing), {rooms} rooms, {checkpoints} checkpoints, \
         {commands} commands over {bindings} bindings (0 faults), \
         {stasis} objects in stasis, {timers} timers, \
         {spawners} spawners set up ({queued} left queued), {alerted} walkers alerted, \
         {destroyed} objects streamed out ({homeless} checkpoints left roomless), \
         {} functions called with no behaviour yet{}",
        resources.len(),
        sounds.len(),
        work.len(),
        if events {
            format!(
                ", {survived} of {fired} handler calls ran to the end, \
                 {moves} object moves, {playing} animations chosen, \
                 {spawned} objects spawned ({armed} with hitpoints), {died} killed and \
                 {} sounds hung on objects and played {plays} times",
                attached.len()
            )
        } else {
            String::new()
        }
    ))
}

/// `--expect N`, read here rather than threaded through, because the boot is
/// the only caller.
fn args_expect() -> Option<usize> {
    expect_flag("--expect")
}

fn expect_flag(flag: &str) -> Option<usize> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
}

/// One level start in a state of its own — the gobs are destroyed between
/// levels, and a fresh state is the honest way to say so.
fn run_one(
    sources: &std::collections::BTreeMap<String, String>,
    level: u32,
    checkpoint: u32,
    section: &str,
) -> Result<goodomen::game::api::Boot, String> {
    run_one_with(sources, level, checkpoint, section, false).map(|(b, _, _, _)| b)
}

/// The same, optionally firing every handler afterwards.
fn run_one_with(
    sources: &std::collections::BTreeMap<String, String>,
    level: u32,
    checkpoint: u32,
    section: &str,
    events: bool,
) -> Result<
    (
        goodomen::game::api::Boot,
        usize,
        usize,
        std::collections::BTreeMap<String, usize>,
    ),
    String,
> {
    use goodomen::game::api;
    use goodomen::game::script::Scripts;

    let scripts = Scripts::new().map_err(|e| e.to_string())?;
    api::install(&scripts.lua, sources.clone()).map_err(|e| e.to_string())?;
    let boot = sources
        .get("mdk2.lua")
        .ok_or_else(|| "no mdk2.lua".to_string())?;
    scripts.run("mdk2.lua", boot).map_err(|e| e.to_string())?;
    api::level(&scripts, level, checkpoint, section).map_err(|e| e.to_string())?;
    let (fired, survived, reasons) = if events {
        api::fire_events(&scripts).map_err(|e| e.to_string())?
    } else {
        (0, 0, Default::default())
    };
    let moved = goodomen::game::world::world(&scripts.lua)
        .map(|w| w.generation())
        .unwrap_or(0);
    let mut boot = scripts
        .lua
        .remove_app_data::<api::Boot>()
        .ok_or_else(|| "no boot state".to_string())?;
    boot.moves = moved;
    Ok((boot, fired, survived, reasons))
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

/// Start a level and **run** it: the body walks from the checkpoint, the
/// driver ticks, and the handlers fire in the order the scripts expect.
///
/// The input is the game's own recording where there is one — `demoN_CP.omn`
/// — and forwards otherwise, which is `tools/walksim.py`'s blunt measure and
/// good enough to cross rooms.
fn run(root: &std::path::Path, number: u32, checkpoint: u32, seconds: f64) -> Result<String, String> {
    use goodomen::formats::omn;
    use goodomen::game::api;
    use goodomen::game::body::{Body, Collision, EYE, WALK};
    use goodomen::game::script::Scripts;
    use goodomen::game::world;

    let mut install = Install::open(root).map_err(|e| e.to_string())?;
    let mut sources = std::collections::BTreeMap::new();
    for (name, i, j) in entries_named(&install, ".lua") {
        if let Ok(bytes) = install.containers[i].read_at(j) {
            sources.insert(name, bytes.iter().map(|&b| b as char).collect::<String>());
        }
    }
    if let Some(dir) = install.override_dir.clone() {
        if let Ok(read) = std::fs::read_dir(&dir) {
            for e in read.flatten() {
                let name = e.file_name().to_string_lossy().to_ascii_lowercase();
                if name.ends_with(".lua") {
                    if let Ok(bytes) = std::fs::read(e.path()) {
                        sources.insert(name, bytes.iter().map(|&b| b as char).collect());
                    }
                }
            }
        }
    }

    let scripts = Scripts::new().map_err(|e| e.to_string())?;
    api::install(&scripts.lua, sources.clone()).map_err(|e| e.to_string())?;
    let mdk2 = sources.get("mdk2.lua").ok_or("no mdk2.lua")?;
    scripts.run("mdk2.lua", mdk2).map_err(|e| e.to_string())?;
    api::level(&scripts, number, checkpoint, "sectionA").map_err(|e| e.to_string())?;

    let (rooms, collision, spawn) = {
        let boot = scripts.lua.app_data_ref::<api::Boot>().ok_or("no boot state")?;
        let w = world::world(&scripts.lua).ok_or("no world")?;
        let mut names = Vec::new();
        let mut boxes = Vec::new();
        for room in &boot.rooms {
            names.push(room.name.clone());
            boxes.push(room.bbox.or_else(|| {
                let gob = w.get(w.find(&room.name)?)?;
                let (lo, hi) = (gob.bbox_min?, gob.bbox_max?);
                Some([lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]])
            }));
        }
        let visible = boot
            .rooms
            .iter()
            .enumerate()
            .map(|(i, r)| r.visible.iter().copied().chain(std::iter::once(i)).collect())
            .collect();
        let spawn = boot
            .checkpoints
            .iter()
            .find(|c| c.index as u32 == checkpoint)
            .cloned()
            .ok_or_else(|| format!("level {number} has no checkpoint {checkpoint}"))?;
        let collision = Collision::load(&mut install, &w);
        let env = boot.rooms.iter().map(|r| r.env).collect();
        let music = boot.rooms.iter().map(|r| r.music).collect();
        (api::Visibility { names, boxes, visible, env, music }, collision, spawn)
    };

    let frames = install
        .read(&format!("demo{number}_{checkpoint}.omn"))
        .ok()
        .and_then(|b| omn::parse(&b).ok())
        .map(|f| f[1.min(f.len())..].to_vec());
    let recorded = frames.is_some();

    let mut body = Body::new(
        [spawn.position[0], spawn.position[1], spawn.position[2] + EYE],
        spawn.facing,
    );
    let mut state = api::Ticking::default();
    let dt = 1.0 / 30.0;
    let steps = (seconds / dt) as usize;

    for step in 0..steps {
        let was = body.position;
        match &frames {
            Some(f) if step < f.len() => body.replay(&collision, &f[step..step + 1], 1.0, WALK),
            // forwards, which is enough to cross a room
            _ => {
                let d = [body.yaw.cos(), body.yaw.sin()];
                body.step(&collision, d, false, WALK, dt);
            }
        }
        // the same locomotion the window drives, so it can be checked
        // without one
        if let Some(name) = scripts
            .lua
            .app_data_ref::<api::Boot>()
            .and_then(|b| b.player.clone())
        {
            let (fx, fy) = (body.yaw.cos(), body.yaw.sin());
            let moved = [
                body.position[0] - was[0],
                body.position[1] - was[1],
            ];
            let _ = api::play_named(
                &scripts,
                &name,
                api::walk_animation(moved[0] * fx + moved[1] * fy, moved[0] * -fy + moved[1] * fx),
            );
        }
        // what the body is against this frame, by name, so `OnCollision`
        // has an object to be about
        let touching = body
            .touching
            .iter()
            .filter_map(|&t| collision.owner(t).map(str::to_string))
            .collect();
        api::tick_touching(
            &scripts,
            &rooms,
            body.position,
            body.yaw,
            dt,
            &mut state,
            &touching,
        )
        .map_err(|e| e.to_string())?;
    }

    // what the scripts actually did to the world while it ran
    let (moved, playing, what, fired_sounds, spawned) = {
        let w = world::world(&scripts.lua).expect("a world");
        let boot = scripts.lua.app_data_ref::<api::Boot>().expect("boot state");
        let what: Vec<String> = boot
            .playing
            .iter()
            .map(|(n, id)| format!("{n}:{id}"))
            .collect();
        // what the run asked to be heard: `omGobGSPlay` calls that reached a
        // sound the scripts had hung on an object
        let sounds: usize = boot.gob_sounds.iter().map(|g| g.played).sum();
        (w.generation(), boot.playing.len(), what, sounds, boot.spawned.len())
    };
    let (fired, survived) = state.total();
    let mut stopped: Vec<(&String, &usize)> = state.why.iter().collect();
    stopped.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let mut slots: Vec<String> = state
        .fired
        .iter()
        .map(|(s, (f, r))| format!("{s} {r}/{f}"))
        .collect();
    slots.sort();
    for (what, got, want) in [
        ("rooms entered", state.rooms_entered, expect_flag("--expect-rooms")),
        ("handler calls", fired, expect_flag("--expect-events")),
        ("handlers ran to the end", survived, expect_flag("--expect-survived")),
        ("animations chosen", playing, expect_flag("--expect-playing")),
        ("object moves", moved as usize, expect_flag("--expect-moves")),
        ("sounds fired", fired_sounds, expect_flag("--expect-sounds")),
        ("collisions", state.collisions, expect_flag("--expect-collisions")),
        ("objects touched", state.touched.len(), expect_flag("--expect-touched")),
        ("objects spawned", spawned, expect_flag("--expect-spawned")),
    ] {
        if let Some(want) = want {
            if got != want {
                return Err(format!("{got} {what}, expected {want}"));
            }
        }
    }
    Ok(format!(
        "l{number} cp{checkpoint}: {steps} ticks of {seconds:.0}s on {} input, \
         travelled {:.0} units, met a wall on {} frames, inside geometry on {}, \
         {} rooms entered, {survived} of {fired} handler calls ran to the end, \
         {moved} object moves, {playing} animations chosen, \
         {fired_sounds} sounds fired, {spawned} objects spawned, \
         {} objects touched and {} of them \
         scripted{}{} [{}]{}",
        if recorded { "the game's own recorded" } else { "held-forwards" },
        body.travelled,
        body.hits,
        body.inside,
        state.rooms_entered,
        state.touched.len(),
        state.collisions,
        if state.touched.is_empty() {
            String::new()
        } else {
            format!(
                " <{}>",
                state.touched.iter().take(6).cloned().collect::<Vec<_>>().join(", ")
            )
        },
        if what.is_empty() { String::new() } else { format!(" ({})", what.join(", ")) },
        slots.join(", "),
        // and the commonest reasons handlers stopped, which name the state
        // the driver does not hold yet
        if stopped.is_empty() {
            String::new()
        } else {
            format!(
                " stopped: {}",
                stopped
                    .iter()
                    .take(3)
                    .map(|(k, n)| format!("{n}x {k}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        }
    ))
}

/// Start a level at a checkpoint and draw it from there, with the authored
/// room visibility doing the culling.
///
/// The camera stands where the game would put the player, which is what
/// makes the culling figure mean anything: a room's `visible` list is
/// authored for the places a player can be.
fn play(root: &std::path::Path, number: u32, checkpoint: u32, show: bool) -> Result<String, String> {
    use goodomen::game::body::EYE;
    use goodomen::render::{scene::Scene, Offscreen};

    let mut install = Install::open(root).map_err(|e| e.to_string())?;
    let mut sources = std::collections::BTreeMap::new();
    for (name, i, j) in entries_named(&install, ".lua") {
        if let Ok(bytes) = install.containers[i].read_at(j) {
            sources.insert(name, bytes.iter().map(|&b| b as char).collect::<String>());
        }
    }
    if let Some(dir) = install.override_dir.clone() {
        if let Ok(read) = std::fs::read_dir(&dir) {
            for e in read.flatten() {
                let name = e.file_name().to_string_lossy().to_ascii_lowercase();
                if name.ends_with(".lua") {
                    if let Ok(bytes) = std::fs::read(e.path()) {
                        sources.insert(name, bytes.iter().map(|&b| b as char).collect());
                    }
                }
            }
        }
    }

    let mut video = Video::open("goodomen", 1024, 768, show)?;
    let version = video.version();
    let mut scene = Scene::default();
    // SAFETY: the context Video::open made is current on this thread.
    let started_level = unsafe {
        goodomen::game::level::start(
            &video.gl,
            &mut install,
            &mut scene,
            &sources,
            number,
            checkpoint,
        )
        .map_err(|e| e.to_string())?
    };
    let goodomen::game::level::Started {
        scripts: level_scripts,
        loaded,
        rooms,
        checkpoints,
        collision,
    } = started_level;

    let spawn = checkpoints
        .iter()
        .find(|c| c.index as u32 == checkpoint)
        .ok_or_else(|| format!("level {number} has no checkpoint {checkpoint}"))?;
    let eye = [
        spawn.position[0] as f32,
        spawn.position[1] as f32,
        (spawn.position[2] + EYE) as f32,
    ];
    let here = rooms.at([eye[0] as f64, eye[1] as f64, eye[2] as f64]);
    let visible = here.first().map(|&i| rooms.visible[i].clone());
    let where_ = here
        .iter()
        .map(|&i| rooms.names[i].as_str())
        .collect::<Vec<_>>()
        .join(", ");

    // one tick before anything is drawn: it is the driver's first tick that
    // puts the player's own object where the body is, and without it the
    // player stands at the origin -- which is where Kurt was, invisible.
    let mut ticking = goodomen::game::api::Ticking::default();
    let _ = goodomen::game::api::tick(
        &level_scripts,
        &rooms,
        [eye[0] as f64, eye[1] as f64, eye[2] as f64],
        spawn.facing,
        1.0 / 30.0,
        &mut ticking,
    );
    if let (Some(w), Some(boot)) = (
        goodomen::game::world::world(&level_scripts.lua),
        level_scripts.lua.app_data_ref::<goodomen::game::api::Boot>(),
    ) {
        scene.follow(&w, &boot.playing, &boot.hidden);
        // and the fog the level start asked for. `Level.Init` sets it on
        // nine of the levels; the rest do it from an `OnEnterRoom`, so it
        // arrives once the player is in the room that wants it.
        scene.fog = boot.fog;
    }

    // the level's own ambient sounds, placed where its objects stand. No
    // audio device is a quieter game, never a failed level.
    let ambient: Vec<goodomen::audio::Ambient> =
        match goodomen::game::world::world(&level_scripts.lua) {
            Some(w) => w
                .iter()
                .filter(|(_, g)| g.kind == goodomen::game::api::OBJ_AMBIENTSOUND)
                .filter_map(|(_, g)| {
                    g.resource.as_ref().map(|r| {
                        goodomen::audio::Ambient::from_payload(r, g.position, g.payload)
                    })
                })
                .collect(),
            None => Vec::new(),
        };
    // A device is opened only for a window. Offscreen this runs as a check
    // and a check must not play sound at anybody, so the sounds are decoded
    // and counted and nothing is opened.
    let mut read = |name: &str| install.read(name).ok();
    let (mut heard, sounding) = if show {
        match goodomen::audio::Audio::open()
            .and_then(|d| goodomen::audio::Ambience::open(d, &ambient, &mut read))
        {
            Ok(a) => {
                let line = format!("{} of {} ambient sounds", a.voices.len(), ambient.len());
                (Some(a), line)
            }
            Err(e) => {
                eprintln!("goodomen: {e}");
                (None, format!("{} ambient sounds, none playing", ambient.len()))
            }
        }
    } else {
        let decoded = goodomen::audio::decodable(&ambient, &mut read);
        (None, format!("{decoded} of {} ambient sounds decode", ambient.len()))
    };
    // the room's EAX 2.0 environment, named rather than numbered: a wrong
    // table shows up here as a wrong room
    let standing_in = here
        .first()
        .and_then(|&i| rooms.env[i])
        .map(|e| goodomen::audio::reverb::NAMES[(e as usize).min(25)])
        .unwrap_or("GENERIC");
    // and the track it asks for, resolved rather than only named: a room
    // naming a track no file answers to would otherwise be silent for good
    let track_here = match here.first().and_then(|&i| rooms.music[i]).unwrap_or(0.0) as i32 {
        n if n > 0 => match music_track(root, n as u32) {
            Some(_) => format!("Track{n:02}"),
            None => format!("Track{n:02} MISSING"),
        },
        _ => "no music".to_string(),
    };

    let mut yaw = spawn.facing;
    let mut pitch = 0.0f64;
    let summary = format!(
        "l{number} cp{checkpoint}: {} objects, {} placed ({} by their type), {} triangles, \
         {} posed, {} lights, {} refused, in {} \
         ({} rooms visible from it, {standing_in}, {track_here}), {sounding}, {}",
        loaded.objects,
        loaded.placed,
        loaded.by_type,
        loaded.triangles,
        scene.posed_draws(),
        scene.lights.len(),
        scene.refused,
        if where_.is_empty() { "no room".into() } else { where_ },
        visible.as_ref().map(|v| v.len()).unwrap_or(0),
        // the level's own fog, which is per room and set by an OnEnterRoom
        // handler; level 1 sets none at all
        if scene.fog.on {
            format!(
                "fog {:.0}-{:.0} at {:.2},{:.2},{:.2}",
                scene.fog.near,
                scene.fog.far,
                scene.fog.colour[0],
                scene.fog.colour[1],
                scene.fog.colour[2]
            )
        } else {
            "no fog".to_string()
        }
    );

    if show {
        use sdl2::event::Event;
        use sdl2::keyboard::{Keycode, Scancode};
        println!("OpenGL {version}\n{summary}");
        println!(
            "{} -- W A S D, mouse to look, shift to run, C to turn culling off, escape to leave",
            if std::env::args().any(|a| a == "--walk") {
                format!("walking, {} trees under foot", collision.len())
            } else {
                "flying".to_string()
            }
        );
        video.sdl.mouse().set_relative_mouse_mode(true);
        // `--walk` puts the body on the ground with the controller the demo
        // replay checks; without it the camera flies.
        let walk = std::env::args().any(|a| a == "--walk");
        let mut body = goodomen::game::body::Body::new(
            [eye[0] as f64, eye[1] as f64, eye[2] as f64],
            yaw,
        );
        let mut at = body.position;
        let mut cull = true;
        // -1 is not an environment, so the first frame always sets one; the
        // same trick starts the music, since -1 is also "stop"
        let mut listening = -1i32;
        let mut playing_track = -1i32;
        let mut track: Option<goodomen::audio::Track> = None;
        let started = std::time::Instant::now();
        let mut last = std::time::Instant::now();
        loop {
            for event in video.events.poll_iter() {
                match event {
                    Event::Quit { .. }
                    | Event::KeyDown { keycode: Some(Keycode::Escape), .. } => {
                        unsafe { scene.delete(&video.gl) };
                        let (fired, survived) = ticking.total();
                        return Ok(format!(
                            "{summary}, ran {:.0}s: {} rooms entered, \
                             {survived} of {fired} handler calls ran to the end",
                            ticking.clock, ticking.rooms_entered
                        ));
                    }
                    Event::KeyDown { keycode: Some(Keycode::C), .. } => cull = !cull,
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
            let fast = held(Scancode::LShift);
            let (fx, fy) = (yaw.cos(), yaw.sin());
            let mut d = [0.0f64, 0.0];
            if held(Scancode::W) { d = [d[0] + fx, d[1] + fy]; }
            if held(Scancode::S) { d = [d[0] - fx, d[1] - fy]; }
            if held(Scancode::D) { d = [d[0] - fy, d[1] + fx]; }
            if held(Scancode::A) { d = [d[0] + fy, d[1] - fx]; }
            if walk {
                body.step(
                    &collision,
                    d,
                    held(Scancode::Space),
                    if fast { goodomen::game::body::SPRINT } else { goodomen::game::body::WALK },
                    dt,
                );
                at = body.position;
                // the engine drives the player's locomotion, which is what
                // the original calls `mdkWalkerAnimUpdate`
                if let Some(player) = level_scripts
                    .lua
                    .app_data_ref::<goodomen::game::api::Boot>()
                    .and_then(|b| b.player.clone())
                {
                    let (fx, fy) = (yaw.cos(), yaw.sin());
                    let forward = d[0] * fx + d[1] * fy;
                    let right = d[0] * -fy + d[1] * fx;
                    let _ = goodomen::game::api::play_named(
                        &level_scripts,
                        &player,
                        goodomen::game::api::walk_animation(forward, right),
                    );
                }
            } else {
                let speed = if fast { 40.0 } else { 12.0 };
                let length = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1.0);
                at = [at[0] + d[0] / length * speed * dt, at[1] + d[1] / length * speed * dt, at[2]];
                if held(Scancode::Space) { at[2] += speed * dt; }
                if held(Scancode::LCtrl) { at[2] -= speed * dt; }
            }

            // the scripts get their tick before the frame is drawn, so what
            // is drawn is the state they just left behind
            if let Err(e) =
                goodomen::game::api::tick_touching(
                    &level_scripts,
                    &rooms,
                    at,
                    yaw,
                    dt,
                    &mut ticking,
                    &body
                        .touching
                        .iter()
                        .filter_map(|&t| collision.owner(t).map(str::to_string))
                        .collect(),
                )
            {
                eprintln!("goodomen: {e}");
            }

            // the drawn world follows the arena: an object the scripts moved
            // moves on screen, and one told to play an animation plays it
            if let (Some(w), Some(boot)) = (
                goodomen::game::world::world(&level_scripts.lua),
                level_scripts.lua.app_data_ref::<goodomen::game::api::Boot>(),
            ) {
                scene.follow(&w, &boot.playing, &boot.hidden);
                // the game's own draw distance, once it has named one
                scene.fog = boot.fog;
            }

            // the room under the camera decides what is drawn, every frame
            let here = rooms.at(at);
            let visible = here.first().map(|&i| rooms.visible[i].clone());
            // and what it sounds like: EAX 2.0 has one listener environment,
            // and the original swaps it on entry rather than blending
            let env = here.first().and_then(|&i| rooms.env[i]).unwrap_or(0.0) as i32;
            if let (Some(a), true) = (&heard, env != listening) {
                a.audio.environment(env);
                listening = env;
            }
            // and its music. 0 and -1 stop it; anything else is Music/TrackNN.
            // `chSndSwitchMusic` wins over the room, since that is the whole
            // point of a script calling it — 148 times over the corpus.
            let want = level_scripts
                .lua
                .app_data_ref::<goodomen::game::api::Boot>()
                .and_then(|b| b.music)
                .or_else(|| here.first().and_then(|&i| rooms.music[i]))
                .unwrap_or(0.0) as i32;
            if let (Some(a), true) = (&mut heard, want != playing_track) {
                playing_track = want;
                if let Some(t) = track.take() {
                    a.audio.stop_music(&t);
                }
                track = (want > 0)
                    .then(|| music_track(&root, want as u32))
                    .flatten()
                    .and_then(|p| std::fs::read(p).ok())
                    .and_then(|bytes| match a.audio.music(bytes) {
                        Ok(t) => Some(t),
                        Err(e) => {
                            eprintln!("goodomen: {e}");
                            None
                        }
                    });
            }
            if let (Some(a), Some(t)) = (&mut heard, &mut track) {
                if let Err(e) = a.audio.pump(t) {
                    eprintln!("goodomen: {e}");
                }
            }
            // the sounds the scripts asked for this tick, at the objects they
            // were hung on. The queue is drained whether or not there is a
            // device, so a silent run does not grow it without bound.
            let asked: Vec<(String, [f32; 3])> = {
                let mut out = Vec::new();
                if let Some(mut boot) =
                    level_scripts.lua.app_data_mut::<goodomen::game::api::Boot>()
                {
                    let queue = std::mem::take(&mut boot.to_play);
                    if let Some(w) = goodomen::game::world::world(&level_scripts.lua) {
                        for i in queue {
                            let Some(s) = boot.gob_sounds.get(i) else { continue };
                            let p = w
                                .find(&s.gob)
                                .and_then(|id| w.get(id))
                                .map(|g| g.position)
                                .unwrap_or(at);
                            out.push((
                                s.sound.clone(),
                                [p[0] as f32, p[1] as f32, p[2] as f32],
                            ));
                        }
                    }
                }
                out
            };
            if let Some(a) = &mut heard {
                for (name, place) in asked {
                    let mut read = |n: &str| install.read(n).ok();
                    a.fire(&name, place, &mut read);
                }
            }
            // Third person when walking: the player has a body now, so the
            // camera stands behind it. `BEHIND` and `ABOVE` are chosen here
            // and not taken from the game -- the original's camera is an
            // object (`OBJ_DEFAULTCAMERA`) with its own script, and that is
            // not read yet.
            const BEHIND: f64 = 4.5;
            const ABOVE: f64 = 1.6;
            let look = [
                (yaw.cos() * pitch.cos()) as f32,
                (yaw.sin() * pitch.cos()) as f32,
                pitch.sin() as f32,
            ];
            let from = if walk {
                [
                    (at[0] - look[0] as f64 * BEHIND) as f32,
                    (at[1] - look[1] as f64 * BEHIND) as f32,
                    (at[2] - look[2] as f64 * BEHIND + ABOVE) as f32,
                ]
            } else {
                [at[0] as f32, at[1] as f32, at[2] as f32]
            };
            let ahead = [
                from[0] + look[0],
                from[1] + look[1],
                from[2] + look[2],
            ];
            // the ears go with the camera and not with the body: what is
            // heard has to agree with what is seen
            if let Some(a) = &heard {
                a.listen(from, yaw, pitch);
            }
            let view = Mat4::look_at(from, ahead, [0.0, 0.0, 1.0]);
            let (w, h) = video.window.drawable_size();
            let projection = Mat4::perspective(1.1, w as f32 / h.max(1) as f32, 0.05, 4000.0);
            unsafe {
                video.gl.viewport(0, 0, w as i32, h as i32);
                video.gl.clear_color(0.05, 0.06, 0.09, 1.0);
                video.gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
                scene.draw(
                    &video.gl,
                    &projection.times(&view),
                    scene.fog,
                    if cull { visible.as_ref() } else { None },
                    started.elapsed().as_secs_f64(),
                    from,
                )?;
            }
            video.window.gl_swap_window();
        }
    }

    // offscreen: draw once culled and once not, and report the difference,
    // which is what the authored visibility is worth. `--walk` puts the
    // camera behind the body here too, so the same picture can be looked at.
    let (eye, yaw) = if std::env::args().any(|a| a == "--walk") {
        const BEHIND: f64 = 4.5;
        const ABOVE: f64 = 1.6;
        (
            [
                (eye[0] as f64 - yaw.cos() * BEHIND) as f32,
                (eye[1] as f64 - yaw.sin() * BEHIND) as f32,
                eye[2] + ABOVE as f32,
            ],
            yaw,
        )
    } else {
        (eye, yaw)
    };
    let (width, height) = (512i32, 512i32);
    unsafe {
        let target = Offscreen::new(&video.gl, width, height)?;
        let ahead = [
            eye[0] + yaw.cos() as f32,
            eye[1] + yaw.sin() as f32,
            eye[2] + pitch.sin() as f32,
        ];
        let view = Mat4::look_at(eye, ahead, [0.0, 0.0, 1.0]);
        let projection = Mat4::perspective(1.1, 1.0, 0.05, 4000.0);
        video.gl.clear_color(0.05, 0.06, 0.09, 1.0);
        video.gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
        // a fixed clock, so the frame the check looks at is the same one
        // every run: the animations are deterministic in time
        let culled = scene.draw(&video.gl, &projection.times(&view), scene.fog, visible.as_ref(), 0.0, eye)?;
        let all = scene.draw(&video.gl, &projection.times(&view), scene.fog, None, 0.0, eye)?;
        video.gl.finish();

        if let Some(i) = std::env::args().position(|a| a == "--save") {
            if let Some(path) = std::env::args().nth(i + 1) {
                let mut pixels = vec![0u8; (width * height * 4) as usize];
                video.gl.read_pixels(
                    0, 0, width, height,
                    glow::RGBA, glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(&mut pixels)),
                );
                let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
                for row in (0..height).rev() {
                    for col in 0..width {
                        let o = ((row * width + col) * 4) as usize;
                        ppm.extend_from_slice(&pixels[o..o + 3]);
                    }
                }
                std::fs::write(&path, ppm).map_err(|e| e.to_string())?;
            }
        }
        target.delete(&video.gl);
        scene.delete(&video.gl);
        if let Some(want) = expect_flag("--expect-drawn") {
            if culled != want {
                return Err(format!("drew {culled} triangles, expected {want}"));
            }
        }
        if let Some(want) = expect_flag("--expect-sounds") {
            if ambient.len() != want {
                return Err(format!(
                    "{} ambient sounds placed, expected {want}",
                    ambient.len()
                ));
            }
        }
        Ok(format!(
            "OpenGL {version}: {summary}, drew {culled} of {all} triangles ({:.1}%)",
            100.0 * culled as f64 / all.max(1) as f64
        ))
    }
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
         {} draws ({} posed), {span:.0} units across",
        loaded.objects,
        loaded.placed,
        loaded.without_a_model,
        loaded.triangles,
        scene.draw_count(),
        scene.posed_draws()
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
                scene.draw(
                    &video.gl,
                    &projection.times(&view),
                    // this mode frames one model, so the level's fog does not apply
                    goodomen::render::scene::Fog { far: span * 2.0, ..Default::default() },
                    None,
                    0.0,
                    eye,
                )?;
            }
            video.window.gl_swap_window();
            let _ = EYE;
        }
    }

    // offscreen, and then three questions of the pixels
    let (width, height) = (512i32, 512i32);
    unsafe {
        // bound for as long as it is alive, which is this block
        let _target = Offscreen::new(&video.gl, width, height)?;
        let projection = Mat4::perspective(1.1, 1.0, span * 0.002, span * 4.0);
        video.gl.clear_color(0.05, 0.06, 0.09, 1.0);
        video.gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
        scene.draw(
                    &video.gl,
                    &projection.times(&view),
                    // this mode frames one model, so the level's fog does not apply
                    goodomen::render::scene::Fog { far: span * 2.0, ..Default::default() },
                    None,
                    0.0,
                    eye,
                )?;
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

/// Parse every `.wav` wrapper and say what is inside it.
///
/// The ACM payload is not decoded yet — that is the engine's one remaining
/// format debt — but everything around it checks: the magic, the size
/// identity `28 + compressed == filesize`, the ACM magic at offset 28, and
/// the two codec parameters, which are **the same pair for every stream the
/// game ships**.
fn sounds(install: &mut Install, list: bool) -> usize {
    use goodomen::formats::wavc;
    let found = entries_named(install, ".wav");
    let (mut wrapped, mut riff, mut pcm_bytes) = (0usize, 0usize, 0u64);
    let mut parameters: std::collections::BTreeSet<(u32, u32)> = Default::default();
    let mut rates: std::collections::BTreeMap<u16, usize> = Default::default();

    for (name, i, j) in &found {
        let data = match install.containers[*i].read_at(*j) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("goodomen: {name}: {e}");
                std::process::exit(1);
            }
        };
        // the six genuine RIFF files are footsteps, short enough that
        // compressing them was not worth it
        if data.starts_with(b"RIFF") {
            riff += 1;
            continue;
        }
        match wavc::parse(&data) {
            Ok(s) => {
                wrapped += 1;
                pcm_bytes += s.decompressed as u64;
                *rates.entry(s.rate).or_insert(0) += 1;
                let (h, pcm) = match goodomen::formats::acm::header(s.acm)
                    .and_then(|h| goodomen::formats::acm::decode(s.acm).map(|p| (h, p)))
                {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("goodomen: {name}: {e}");
                        std::process::exit(1);
                    }
                };
                parameters.insert((h.level, h.rows));
                // the wrapper states the decoded size, so the decoder is held
                // to it here rather than only against the oracle
                if pcm.len() * 2 != s.decompressed as usize {
                    eprintln!(
                        "goodomen: {name}: decoded {} bytes, the header says {}",
                        pcm.len() * 2,
                        s.decompressed
                    );
                    std::process::exit(1);
                }
                if list {
                    let bytes: Vec<u8> =
                        pcm.iter().flat_map(|v| v.to_le_bytes()).collect();
                    println!(
                        "{name} {} {} {} {} {} {:08x}",
                        s.decompressed,
                        s.channels,
                        s.rate,
                        h.level,
                        h.rows,
                        crc32(&bytes)
                    );
                }
            }
            Err(e) => {
                eprintln!("goodomen: {name}: {e}");
                std::process::exit(1);
            }
        }
    }
    eprintln!(
        "{} sounds: {wrapped} WAVC over Interplay ACM, {riff} RIFF, \
         {:.1} MiB of PCM behind them, codec parameters {parameters:?}, rates {rates:?}",
        found.len(),
        pcm_bytes as f64 / (1024.0 * 1024.0)
    );
    found.len()
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
    let (mut posed, mut moving, mut posing) = (0usize, 0usize, 0usize);

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
        // The shader poses a model from its **node-local** vertices and one
        // transform per node. That is only right if it agrees with
        // `animate()`, which does the same arithmetic on the CPU -- so ask,
        // for every animated model, over every vertex.
        if let Some(anim) = model.animations.first() {
            let local = model.local();
            let animated = model.animate(anim, 0.5);
            let world = model.node_world(anim, 0.5);
            for k in 0..local.positions.len() {
                let (q, o) = world[local.node[k] as usize];
                let r = goodomen::formats::model::rotate(q, local.positions[k]);
                let want = animated.positions[k];
                if (0..3).any(|c| (r[c] + o[c] - want[c]).abs() > 1e-6) {
                    eprintln!("goodomen: {name}: the shader's pose and animate() disagree");
                    std::process::exit(1);
                }
            }
            posed += 1;

            // **Animation 0 never moves.** Every one of its channels holds a
            // single key, in all 1146 animated models -- so playing it is a
            // still pose, and a level only comes alive when `omAnimPlay`
            // chooses another. Anything that claims otherwise is measuring
            // something else.
            for (i, a) in model.animations.iter().enumerate() {
                // a channel with more than one key **that drives a
                // transform**: kinds 32..36 drive a sound's volume and
                // distances, and moving those moves no node
                let varies = a.channels.iter().any(|c| {
                    c.keys.len() > 1
                        && matches!(c.kind, goodomen::formats::model::KIND_TRANSLATION
                                          | goodomen::formats::model::KIND_ROTATION)
                });
                if varies {
                    moving += 1;
                    // animation 0 never moves, in any of the 1146. That is
                    // the assertion; the rest is counted, because a
                    // multi-key channel can still pose the same at two times
                    // -- its keys can hold the same value, or sit at one end
                    // of the timeline, or name a node the model does not have.
                    if i == 0 {
                        eprintln!("goodomen: {name}: animation 0 moves, and none does");
                        std::process::exit(1);
                    }
                    if model.node_world(a, 0.0) != model.node_world(a, 0.5) {
                        posing += 1;
                    }
                }
            }
        }

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
        "{} models, {animated} animated, {nodes} nodes, {tris} triangles, \
         {posed} checked against the shader's own posing, \
         {moving} animations with a moving transform channel, none of them \
         animation 0, and {posing} that pose differently at two times",
        found.len()
    );
    found.len()
}
