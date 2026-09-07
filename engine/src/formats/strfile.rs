//! `.str` — the game's whole text, and the voice-over each line names.
//!
//! One file holds it: `override/<language>/mdk2.str`, with `local.zip`'s copy
//! underneath. The menu, the inventory, the hints and the subtitles all index
//! it by number -- `scripts/strings.lua` is the table of names for those
//! numbers, so `strings.continue` is an id and not a word.
//!
//! ```text
//! 0x00  u32   2003        resource type tag: .tex 2001, .mod 2002, .str 2003
//! 0x04  u32   2           version
//! 0x08  u32               entry count
//! 0x0c  u32   0x18        offset of the entry table, i.e. right here
//! 0x10  u32               offset of the character data
//! 0x14  u32               offset of the sound-name table
//!
//! entry, 12 bytes:  { u32 id; u32 text; u32 sound }
//! ```
//!
//! `text` is a byte offset into the character data and the string is a
//! NUL-terminated run of **16-bit units, each holding one code-page byte**.
//! `sound` is an offset into the sound-name table, whose entries are ASCII in
//! fixed 16-byte slots. Either may be `0xFFFFFFFF`: 47 entries have no text
//! and 338 no voice-over.
//!
//! **The text is not UTF-16**, though it reads as it. The engine widens a
//! code-page byte to 16 bits; what a byte above 0x7f *means* is decided by
//! the font that draws it, not by this file. Latin-1 is the first 256 code
//! points, so for the five Western languages the wrong reading gives the
//! right answer -- the 1C Russian edition, which stores cp1251 and ships its
//! own `font.tex` beside it, is what settles it. So a string comes out of
//! here as **bytes**, and those bytes are what
//! [`crate::render::overlay::Font`] indexes.
//!
//! `../../tools/strfile.py` is the reference and holds all six languages to
//! the 100% rule: strings tiling the character data with nothing left over,
//! and every sound offset landing on a slot that exists.

pub const TYPE_STR: u32 = 2003;
const HEADER: usize = 0x18;
const ENTRY: usize = 12;
const SLOT: usize = 16;
const NONE: u32 = 0xFFFF_FFFF;

#[derive(Debug, PartialEq)]
pub enum Error {
    NotAStringFile(u32),
    Truncated,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::NotAStringFile(t) => write!(f, "type tag {t}, expected {TYPE_STR}"),
            Error::Truncated => write!(f, "the file ends inside its own table"),
        }
    }
}

#[derive(Default)]
pub struct Strings {
    /// `id -> (the code-page bytes, the sound that speaks them)`. Ids are
    /// sparse and not in order, which is why this is a map.
    entries: std::collections::BTreeMap<u32, (Vec<u8>, Option<String>)>,
}

fn u32_at(data: &[u8], at: usize) -> Result<u32, Error> {
    data.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(Error::Truncated)
}

impl Strings {
    /// A table made here rather than read, for a test that needs text and
    /// not a file.
    pub fn synthetic(entries: &[(u32, &str)]) -> Strings {
        Strings {
            entries: entries
                .iter()
                .map(|(id, text)| (*id, (text.as_bytes().to_vec(), None)))
                .collect(),
        }
    }

    pub fn parse(data: &[u8]) -> Result<Strings, Error> {
        let tag = u32_at(data, 0)?;
        if tag != TYPE_STR {
            return Err(Error::NotAStringFile(tag));
        }
        let count = u32_at(data, 8)? as usize;
        let table = u32_at(data, 0x0c)? as usize;
        let text = u32_at(data, 0x10)? as usize;
        let sounds = u32_at(data, 0x14)? as usize;
        if table != HEADER {
            return Err(Error::Truncated);
        }
        let mut entries = std::collections::BTreeMap::new();
        for i in 0..count {
            let at = table + i * ENTRY;
            let (id, t, s) = (u32_at(data, at)?, u32_at(data, at + 4)?, u32_at(data, at + 8)?);
            let mut chars = Vec::new();
            if t != NONE {
                // 16-bit units, one code-page byte each, NUL terminated
                let mut p = text + t as usize;
                loop {
                    let unit = data.get(p..p + 2).ok_or(Error::Truncated)?;
                    let unit = u16::from_le_bytes([unit[0], unit[1]]);
                    if unit == 0 {
                        break;
                    }
                    chars.push(unit as u8);
                    p += 2;
                }
            }
            let sound = if s == NONE {
                None
            } else {
                let at = sounds + s as usize;
                let slot = data.get(at..at + SLOT).ok_or(Error::Truncated)?;
                let end = slot.iter().position(|&b| b == 0).unwrap_or(SLOT);
                Some(slot[..end].iter().map(|&b| b as char).collect())
            };
            entries.insert(id, (chars, sound));
        }
        Ok(Strings { entries })
    }

    /// The line for an id, as the code-page bytes the font is indexed by.
    pub fn text(&self, id: u32) -> Option<&[u8]> {
        self.entries.get(&id).map(|(t, _)| &t[..])
    }

    /// The `.wav` that speaks that line, for the 348 that have one.
    pub fn sound(&self, id: u32) -> Option<&str> {
        self.entries.get(&id).and_then(|(_, s)| s.as_deref())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file built by hand, so the offsets are known rather than trusted:
    /// two entries, the second with no sound.
    fn built() -> Vec<u8> {
        let mut out = Vec::new();
        for v in [TYPE_STR, 2, 2, HEADER as u32, 0, 0] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        let table = out.len();
        out.extend_from_slice(&[0u8; 2 * ENTRY]);
        let text = out.len();
        for c in b"Door's broken. " {
            out.extend_from_slice(&(*c as u16).to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
        let second = out.len() - text;
        for c in [0xC4u8, b'a'] {
            out.extend_from_slice(&(c as u16).to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
        let sounds = out.len();
        let mut slot = [0u8; SLOT];
        slot[..8].copy_from_slice(b"jd_doors");
        out.extend_from_slice(&slot);

        out[0x10..0x14].copy_from_slice(&(text as u32).to_le_bytes());
        out[0x14..0x18].copy_from_slice(&(sounds as u32).to_le_bytes());
        for (i, (id, t, s)) in [(1u32, 0u32, 0u32), (7, second as u32, NONE)].iter().enumerate() {
            let at = table + i * ENTRY;
            out[at..at + 4].copy_from_slice(&id.to_le_bytes());
            out[at + 4..at + 8].copy_from_slice(&t.to_le_bytes());
            out[at + 8..at + 12].copy_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    fn reads_the_text_and_the_voice() {
        let s = Strings::parse(&built()).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s.text(1), Some(&b"Door's broken. "[..]));
        assert_eq!(s.sound(1), Some("jd_doors"));
        assert_eq!(s.sound(7), None);
    }

    /// The byte above 0x7f stays one byte: it is a code-page index and not a
    /// code point, and widening it to UTF-8 would be the refuted reading.
    #[test]
    fn a_high_byte_is_one_byte() {
        let s = Strings::parse(&built()).unwrap();
        assert_eq!(s.text(7), Some(&[0xC4u8, b'a'][..]));
    }

    #[test]
    fn a_wrong_tag_is_an_error() {
        assert_eq!(
            Strings::parse(&2001u32.to_le_bytes()).err(),
            Some(Error::NotAStringFile(2001))
        );
    }
}
