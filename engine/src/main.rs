//! goodomen — drop it into your MDK2 directory and run it.
//!
//! With no arguments it reads the installation beside the binary and says
//! what it found. `--tex` decodes every texture and prints one CRC32 each,
//! in the form `tools/texdec.py --digest` prints, so that
//! `tools/texcheck.sh` can hold the two implementations to each other.

use goodomen::formats::container::crc32;
use goodomen::formats::model::Model;
use goodomen::formats::tex::Texture;
use goodomen::game::install::Install;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tex = args.iter().any(|a| a == "--tex");
    let models = args.iter().any(|a| a == "--mod");
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

    if tex || models {
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
        } else {
            ("models", meshes(&mut install, expect.is_none()))
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
