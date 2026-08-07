//! Finding the game and reading it.
//!
//! An installation is a directory holding `mdk2Main.exe`, a `data/`
//! directory of containers and — this is the part that is easy to miss —
//! an `override/` directory, which is a **shipped patch the engine reads
//! before the containers**. Anything that consults only `data/*.zip` is
//! reading the game as it was before its own patch: `override/level1.lua`
//! differs by sixty lines and adds two checkpoints.

use crate::formats::container::{Container, Error as ContainerError};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum Error {
    NoGameHere(PathBuf),
    Container(PathBuf, ContainerError),
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoGameHere(p) => write!(
                f,
                "no MDK2 in {} -- expected a data/ directory of .zip containers",
                p.display()
            ),
            Error::Container(p, e) => write!(f, "{}: {e}", p.display()),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Io(e)
    }
}

pub struct Install {
    pub root: PathBuf,
    pub containers: Vec<Container>,
    /// The shipped patch, when the installation has one.
    pub override_dir: Option<PathBuf>,
}

impl Install {
    /// Open the installation rooted at `root`.
    pub fn open(root: &Path) -> Result<Install, Error> {
        let data = root.join("data");
        if !data.is_dir() {
            return Err(Error::NoGameHere(root.to_path_buf()));
        }
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&data)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
            })
            .collect();
        paths.sort();
        if paths.is_empty() {
            return Err(Error::NoGameHere(root.to_path_buf()));
        }

        let mut containers = Vec::new();
        for p in paths {
            let c = Container::open(&p).map_err(|e| Error::Container(p.clone(), e))?;
            containers.push(c);
        }

        let over = root.join("override");
        Ok(Install {
            root: root.to_path_buf(),
            containers,
            override_dir: over.is_dir().then_some(over),
        })
    }

    /// Where the binary is, which is where the game is expected to be.
    pub fn beside_the_binary() -> PathBuf {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn entry_count(&self) -> usize {
        self.containers.iter().map(|c| c.entries().len()).sum()
    }

    /// Read a named resource. **`override/` wins over the containers**, the
    /// way the engine's own loader does.
    pub fn read(&mut self, name: &str) -> Result<Vec<u8>, Error> {
        if let Some(dir) = &self.override_dir {
            if let Some(p) = find_case_insensitively(dir, name) {
                return Ok(std::fs::read(p)?);
            }
        }
        let mut last = None;
        for c in self.containers.iter_mut() {
            match c.read(name) {
                Ok(bytes) => return Ok(bytes),
                Err(ContainerError::Missing(_)) => continue,
                Err(e) => last = Some(Error::Container(c.path().to_path_buf(), e)),
            }
        }
        Err(last.unwrap_or_else(|| {
            Error::Container(
                self.root.clone(),
                ContainerError::Missing(name.to_string()),
            )
        }))
    }
}

/// The game spells its own filenames inconsistently -- `track01a.acm` sits
/// beside `Track18a.acm` in the same directory -- so nothing may match by
/// exact case.
fn find_case_insensitively(dir: &Path, name: &str) -> Option<PathBuf> {
    let want = Path::new(name).file_name()?.to_str()?.to_ascii_lowercase();
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().to_ascii_lowercase() == want)
        .map(|e| e.path())
}
