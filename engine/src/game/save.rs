//! `save\N.sav` — the file the pause menu writes, and where each field is.
//!
//! Four functions in the original touch this file and between them they name
//! every offset, so the header is read rather than chosen:
//!
//! ```text
//! 0x00000  u32      a version, and the only thing the load checks
//! 0x00004  0x80     the description, `strncpy`ed -- 0x42a220 seeks to 4 and
//!                   writes exactly 0x80 bytes, 0x42a2a0 seeks to 4 and
//!                   reads exactly 0x80 back, which is `mdkGetSaveFileDesc`
//! 0x00084  u32      the thumbnail's side, a power of two: 0x4012e0 starts
//!                   at 256 and halves while it is wider than the window
//! 0x00088  0x30000  the thumbnail, 256*256*3 bytes written whole whatever
//!                   the side is -- 0x42a2f0 seeks to 0x84 and `fwrite`s the
//!                   side and then 0x30000 bytes
//! 0x30088  u8       the level        } 0x42a140 seeks to 0x30088 and reads
//! 0x30089  u8       the checkpoint   } one byte into each
//! 0x3008a  ...      the world
//! ```
//!
//! **The world is where this engine stops.** 0x428e60 walks every object and
//! writes its state; reproducing that is a second engine's worth of work and
//! it buys nothing here, because nothing ships a save file — the only reader
//! of ours is us. So the level and the checkpoint go where the original puts
//! them, the difficulty follows them, and loading is `mdkNewGame` at the
//! checkpoint that was saved.
//!
//! ponytail: the game resumes at the checkpoint, not at the step. Write the
//! world block at 0x3008a when a save has to survive mid-level.

use std::path::{Path, PathBuf};

/// The description's room in the file, and so the longest one a player can
/// type — 0x42a220's `strncpy(local_80, desc, 0x80)`.
pub const DESCRIPTION: usize = 0x80;
/// What 0x42a2f0 writes for the thumbnail however small the side is:
/// 256 * 256 * 3.
const THUMBNAIL: usize = 0x30000;
/// Where 0x42a140 seeks for the level and the checkpoint.
const WHERE: usize = 0x84 + 4 + THUMBNAIL;
/// The version 0x42a140 compares against 0x48ff2c before it reads anything,
/// and 0x48ff2c is **7**. Ours says the same, because everything this engine
/// writes before 0x3008a *is* the original's layout and everything an
/// original file holds before 0x3008a is one we can read: the description
/// and the thumbnail come out of a retail save correctly. What differs is
/// only what follows, where the original starts its world and this writes
/// the difficulty — so a retail save loads at the right checkpoint and its
/// four bytes there are read as a difficulty and thrown away for being out
/// of range.
const VERSION: u32 = 7;
/// What a difficulty can be: `menu.diff` offers 0.2, 0.35, 0.5 and 0.65, and
/// `mdkDiffScale` divides by it. Anything outside this is not one.
const DIFFICULTY: std::ops::RangeInclusive<f32> = 0.05..=1.0;

/// A script's `save\3.sav` as a path under the installation. The scripts are
/// Windows' and say `\`; nothing else in them names a file to write.
pub fn beside(root: &Path, name: &str) -> PathBuf {
    root.join(name.replace('\\', "/"))
}

/// `mdkGetSaveFileDesc` — 0x42a2a0, which is `fseek(f, 4, SEEK_SET)` and
/// `fread(buf, 0x80, 1, f)` and nothing else. The bytes are the code page's,
/// the way the font is indexed, so they come back as bytes.
pub fn description(at: &Path) -> Option<Vec<u8>> {
    let file = std::fs::read(at).ok()?;
    let text = file.get(4..4 + DESCRIPTION)?;
    Some(text.split(|&b| b == 0).next().unwrap_or_default().to_vec())
}

/// The level and the checkpoint a save was taken at, and the difficulty it
/// was being fought at — `None` for the last when the file is a retail one
/// and those four bytes are the head of its world instead.
pub fn resume(at: &Path) -> Option<(u32, u32, Option<f32>)> {
    let file = std::fs::read(at).ok()?;
    if u32::from_le_bytes(file.get(..4)?.try_into().ok()?) != VERSION {
        return None;
    }
    let level = *file.get(WHERE)? as u32;
    let checkpoint = *file.get(WHERE + 1)? as u32;
    let difficulty = file
        .get(WHERE + 2..WHERE + 6)
        .and_then(|b| b.try_into().ok())
        .map(f32::from_le_bytes)
        .filter(|d| DIFFICULTY.contains(d));
    Some((level, checkpoint, difficulty))
}

/// `mdkStartInstantSave`, once the main loop has picked the request up.
///
/// The original writes it in two passes -- 0x428e60 for the world and then
/// 0x42a220 for the description, with the thumbnail patched in a frame later
/// by 0x42a2f0 -- because it renders the thumbnail after the save. One pass
/// is the same file.
pub fn write(
    at: &Path,
    description: &[u8],
    level: u32,
    checkpoint: u32,
    difficulty: f32,
    thumbnail: Option<(u32, &[u8])>,
) -> std::io::Result<()> {
    if let Some(dir) = at.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut out = Vec::with_capacity(WHERE + 6);
    out.extend_from_slice(&VERSION.to_le_bytes());
    let mut desc = [0u8; DESCRIPTION];
    let n = description.len().min(DESCRIPTION - 1);
    desc[..n].copy_from_slice(&description[..n]);
    out.extend_from_slice(&desc);
    let (side, pixels) = thumbnail.unwrap_or((0, &[]));
    out.extend_from_slice(&side.to_le_bytes());
    out.extend_from_slice(&pixels[..pixels.len().min(THUMBNAIL)]);
    out.resize(WHERE, 0);
    out.push(level.min(255) as u8);
    out.push(checkpoint.min(255) as u8);
    out.extend_from_slice(&difficulty.to_le_bytes());
    std::fs::write(at, out)
}

/// `save/auto.sav`, which is twenty bytes and its own format: 0x42d2e0 reads
/// a dword that has to be zero, skips a second, then takes the level, the
/// checkpoint and the difficulty. `mdkGetAutoSave` is this.
pub fn autosave(root: &Path) -> Option<(u32, u32, f32)> {
    let file = std::fs::read(root.join("save/auto.sav")).ok()?;
    let word = |i: usize| -> Option<[u8; 4]> { file.get(i..i + 4)?.try_into().ok() };
    if u32::from_le_bytes(word(0)?) != 0 {
        return None;
    }
    Some((
        u32::from_le_bytes(word(8)?),
        u32::from_le_bytes(word(12)?),
        f32::from_le_bytes(word(16)?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_save_says_where_it_was_taken_and_what_it_was_called() {
        let dir = std::env::temp_dir().join(format!("goodomen-save-{}", std::process::id()));
        let at = beside(&dir, "save\\7.sav");
        write(&at, b"halfway up the spire", 4, 11, 0.35, None).unwrap();
        // the description sits at 4 and is padded, whatever it says
        assert_eq!(description(&at).unwrap(), b"halfway up the spire");
        assert_eq!(resume(&at), Some((4, 11, Some(0.35))));
        // and the level is at the offset the original seeks to, not wherever
        // it happened to land
        assert_eq!(std::fs::read(&at).unwrap()[WHERE], 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
