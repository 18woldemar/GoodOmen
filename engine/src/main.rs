//! goodomen — drop it into your MDK2 directory and run it.
//!
//! With no arguments it reads the installation beside the binary and says
//! what it found. `--tex` decodes every texture and prints one CRC32 each,
//! in the form `tools/texdec.py --digest` prints, so that
//! `tools/texcheck.sh` can hold the two implementations to each other.

use goodomen::formats::container::crc32;
use goodomen::formats::tex::Texture;
use goodomen::game::install::Install;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tex = args.iter().any(|a| a == "--tex");
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

    if tex {
        // `--expect N`, the same convention the Python tools use: a check
        // that silently found nothing is the failure mode worth guarding
        let expect = args
            .iter()
            .position(|a| a == "--expect")
            .and_then(|i| args.get(i + 1))
            .and_then(|n| n.parse::<usize>().ok());
        // the per-texture lines exist for tools/texcheck.sh to diff; when
        // this is being run as a check, only the verdict is wanted
        let found = textures(&mut install, expect.is_none());
        if let Some(n) = expect {
            if found != n {
                eprintln!("goodomen: {found} textures, expected {n}");
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
    let mut found: Vec<(String, usize, usize)> = Vec::new();
    for i in 0..install.containers.len() {
        for j in 0..install.containers[i].entries().len() {
            let name = install.containers[i].entries()[j].name.to_ascii_lowercase();
            if name.ends_with(".tex") {
                found.push((name, i, j));
            }
        }
    }
    found.sort();

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
