//! `.wav` — which they are not. 992 of the game's 998 are **WAVC over
//! Interplay ACM**.
//!
//! ```text
//! 0x00  char[8]  "WAVCV1.0"
//! 0x08  u32      decompressed size, in bytes
//! 0x0c  u32      compressed size -- and 28 + this is always the file length
//! 0x10  u32      28, the header size
//! 0x14  u16      channels, always 1
//! 0x16  u16      bits per sample, always 16
//! 0x18  u16      sample rate: 22050 for 653, 11025 for 337, 44100 for two
//! 0x1a  u16      0x77ED, constant
//! ```
//!
//! What follows is an Interplay ACM stream, and its own magic at offset 28
//! says so. That is the codec Interplay used across the Infinity Engine, and
//! this is the same `Chitin/` platform the executable's debug paths name:
//! BioWare carried its sound layer over from Baldur's Gate and never renamed
//! the files.
//!
//! The other six really are RIFF — footsteps, short enough that compressing
//! them was not worth it — and `Music/` holds 27 **bare** ACM streams with no
//! WAVC wrapper at all.
//!
//! **The ACM payload is not decoded here yet**, and that is the engine's one
//! remaining format debt. `../../tools/wavc.py` hands it to `ffmpeg`, which
//! the engine cannot do. What is settled is everything around it, including
//! one thing worth having before writing a decoder: **every stream the game
//! ships uses the same two codec parameters**, 7 and 1 — all 992 wrapped and
//! all 27 music tracks — so a decoder needs one configuration, not a family
//! of them.

pub const WAVC_MAGIC: &[u8; 8] = b"WAVCV1.0";
pub const HEADER: usize = 28;
/// `97 28 03 01` at the start of the payload.
pub const ACM_MAGIC: u32 = 0x0103_2897;
/// The `u16` at 0x1a. Constant across all 992, and unexplained.
pub const TRAILING: u16 = 0x77ED;

#[derive(Debug, PartialEq)]
pub enum Error {
    NotWavc,
    Truncated,
    /// `28 + compressed` must be the file's length, which it is for all 992.
    Size { header: usize, file: usize },
    NotAcm(u32),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotWavc => write!(f, "no WAVCV1.0 magic"),
            Error::Truncated => write!(f, "shorter than the header"),
            Error::Size { header, file } => {
                write!(f, "the header says {header} bytes, the file is {file}")
            }
            Error::NotAcm(m) => write!(f, "payload magic {m:#x}, not Interplay ACM"),
        }
    }
}

impl std::error::Error for Error {}

pub struct Sound<'a> {
    /// Bytes of 16-bit PCM once decoded.
    pub decompressed: u32,
    pub channels: u16,
    pub bits: u16,
    pub rate: u16,
    /// The two ACM codec parameters. Every stream the game ships has (7, 1).
    pub levels: u8,
    pub rows: u8,
    /// The undecoded ACM stream.
    pub acm: &'a [u8],
}

fn u32le(b: &[u8], at: usize) -> Result<u32, Error> {
    let s = b.get(at..at + 4).ok_or(Error::Truncated)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u16le(b: &[u8], at: usize) -> Result<u16, Error> {
    let s = b.get(at..at + 2).ok_or(Error::Truncated)?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

/// Parse the wrapper and check everything it can be checked against.
pub fn parse(data: &[u8]) -> Result<Sound<'_>, Error> {
    if data.len() < HEADER {
        return Err(Error::Truncated);
    }
    if &data[..8] != WAVC_MAGIC {
        return Err(Error::NotWavc);
    }
    let compressed = u32le(data, 0x0c)? as usize;
    if HEADER + compressed != data.len() {
        return Err(Error::Size { header: HEADER + compressed, file: data.len() });
    }
    let acm = &data[HEADER..];
    let magic = u32le(acm, 0)?;
    if magic != ACM_MAGIC {
        return Err(Error::NotAcm(magic));
    }
    Ok(Sound {
        decompressed: u32le(data, 8)?,
        channels: u16le(data, 0x14)?,
        bits: u16le(data, 0x16)?,
        rate: u16le(data, 0x18)?,
        // the two bytes after the ACM header's magic, samples, channels
        // and rate
        levels: *acm.get(12).ok_or(Error::Truncated)?,
        rows: *acm.get(13).ok_or(Error::Truncated)?,
        acm,
    })
}

/// The sample count an ACM stream declares, **across all channels** — not
/// frames. Getting that wrong halves or doubles a stereo track's length.
pub fn acm_samples(acm: &[u8]) -> Result<u32, Error> {
    u32le(acm, 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrapped(payload: &[u8]) -> Vec<u8> {
        let mut d = WAVC_MAGIC.to_vec();
        d.extend_from_slice(&(payload.len() as u32 * 4).to_le_bytes()); // decompressed
        d.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // compressed
        d.extend_from_slice(&(HEADER as u32).to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes()); // channels
        d.extend_from_slice(&16u16.to_le_bytes()); // bits
        d.extend_from_slice(&22050u16.to_le_bytes());
        d.extend_from_slice(&TRAILING.to_le_bytes());
        d.extend_from_slice(payload);
        d
    }

    fn acm_stream() -> Vec<u8> {
        let mut a = ACM_MAGIC.to_le_bytes().to_vec();
        a.extend_from_slice(&34801u32.to_le_bytes()); // samples
        a.extend_from_slice(&1u16.to_le_bytes()); // channels
        a.extend_from_slice(&22050u16.to_le_bytes()); // rate
        a.push(7); // levels
        a.push(1); // rows
        a.extend_from_slice(&[0xaa; 16]);
        a
    }

    #[test]
    fn the_wrapper_parses_and_the_payload_is_acm() {
        let d = wrapped(&acm_stream());
        let s = parse(&d).unwrap();
        assert_eq!((s.channels, s.bits, s.rate), (1, 16, 22050));
        assert_eq!((s.levels, s.rows), (7, 1), "the only pair the game ships");
        assert_eq!(acm_samples(s.acm).unwrap(), 34801);
    }

    /// `28 + compressed == filesize` holds for all 992, so a file where it
    /// does not is not one of them.
    #[test]
    fn the_size_identity_is_enforced() {
        let mut d = wrapped(&acm_stream());
        d.push(0);
        assert!(matches!(parse(&d), Err(Error::Size { .. })));

        let mut d = wrapped(&acm_stream());
        d[HEADER..HEADER + 4].copy_from_slice(&0u32.to_le_bytes());
        assert!(matches!(parse(&d), Err(Error::NotAcm(0))));

        assert!(matches!(parse(b"RIFF...."), Err(Error::Truncated)));
        assert!(matches!(parse(&[0u8; 64]), Err(Error::NotWavc)));
    }
}
