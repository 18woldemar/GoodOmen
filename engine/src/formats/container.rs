//! The `data/*.zip` containers.
//!
//! They are ordinary ZIP archives whose members are stored (method 0) or
//! **DCL Imploded** (method 10, see [`super::blast`]). Reading the central
//! directory by hand rather than through a crate costs eighty lines and buys
//! two things: no dependency for the one job every other format needs first,
//! and a place to put what was learned about these particular files.
//!
//! Every member carries a CRC32, and [`Container::read`] checks it. The
//! project's rule is that a format counts as solved at 100%, and here the
//! format itself says whether it is: `../../tools/unpack.py` gets 4751 of
//! 4751 with every checksum matching, and so must this.

use super::blast::{blast, Error as BlastError};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const EOCD_SIG: u32 = 0x0605_4b50;
const CDIR_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;
const EOCD_LEN: usize = 22;
const LOCAL_LEN: usize = 30;

const STORED: u16 = 0;
const IMPLODED: u16 = 10;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    NotAZip,
    Zip64,
    Truncated,
    UnknownMethod(u16),
    Blast(BlastError),
    Checksum { name: String, want: u32, got: u32 },
    Missing(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::NotAZip => write!(f, "no end-of-central-directory record"),
            Error::Zip64 => write!(f, "zip64, which these containers are not"),
            Error::Truncated => write!(f, "the file ends inside a record"),
            Error::UnknownMethod(m) => write!(f, "compression method {m}"),
            Error::Blast(e) => write!(f, "{e}"),
            Error::Checksum { name, want, got } => {
                write!(f, "{name}: CRC32 {got:08x}, the directory says {want:08x}")
            }
            Error::Missing(n) => write!(f, "{n} is not in this container"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Io(e)
    }
}

/// One member, as the central directory describes it.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub method: u16,
    pub crc: u32,
    pub compressed: u64,
    pub uncompressed: u64,
    local_header: u64,
}

pub struct Container {
    path: PathBuf,
    file: File,
    entries: Vec<Entry>,
}

fn u16le(b: &[u8], at: usize) -> Result<u16, Error> {
    let s = b.get(at..at + 2).ok_or(Error::Truncated)?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

fn u32le(b: &[u8], at: usize) -> Result<u32, Error> {
    let s = b.get(at..at + 4).ok_or(Error::Truncated)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

impl Container {
    pub fn open(path: &Path) -> Result<Container, Error> {
        let mut file = File::open(path)?;
        let size = file.metadata()?.len();

        // The end-of-central-directory record is last, but a trailing
        // comment can push it back by up to 64 KiB, so it has to be scanned
        // for rather than seeked to.
        let tail_len = size.min(EOCD_LEN as u64 + 0xffff) as usize;
        file.seek(SeekFrom::End(-(tail_len as i64)))?;
        let mut tail = vec![0u8; tail_len];
        file.read_exact(&mut tail)?;
        let eocd = (0..=tail_len.saturating_sub(EOCD_LEN))
            .rev()
            .find(|&i| u32le(&tail, i).ok() == Some(EOCD_SIG))
            .ok_or(Error::NotAZip)?;

        let count = u16le(&tail, eocd + 10)? as usize;
        let cd_size = u32le(&tail, eocd + 12)? as u64;
        let cd_at = u32le(&tail, eocd + 16)? as u64;
        if cd_at == u32::MAX as u64 || count == 0xffff {
            return Err(Error::Zip64);
        }

        file.seek(SeekFrom::Start(cd_at))?;
        let mut cd = vec![0u8; cd_size as usize];
        file.read_exact(&mut cd)?;

        let mut entries = Vec::with_capacity(count);
        let mut at = 0usize;
        while at + 46 <= cd.len() && u32le(&cd, at)? == CDIR_SIG {
            let name_len = u16le(&cd, at + 28)? as usize;
            let extra_len = u16le(&cd, at + 30)? as usize;
            let comment_len = u16le(&cd, at + 32)? as usize;
            let name = cd
                .get(at + 46..at + 46 + name_len)
                .ok_or(Error::Truncated)?;
            entries.push(Entry {
                name: String::from_utf8_lossy(name).into_owned(),
                method: u16le(&cd, at + 10)?,
                crc: u32le(&cd, at + 16)?,
                compressed: u32le(&cd, at + 20)? as u64,
                uncompressed: u32le(&cd, at + 24)? as u64,
                local_header: u32le(&cd, at + 42)? as u64,
            });
            at += 46 + name_len + extra_len + comment_len;
        }

        Ok(Container { path: path.to_path_buf(), file, entries })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Read one member and check its CRC32. The name is matched without
    /// regard to case or slash direction, because the game's own scripts
    /// spell resources in a case of their own.
    pub fn read(&mut self, name: &str) -> Result<Vec<u8>, Error> {
        let want = name.to_ascii_lowercase().replace('\\', "/");
        let i = self
            .entries
            .iter()
            .position(|e| e.name.to_ascii_lowercase().replace('\\', "/") == want)
            .ok_or_else(|| Error::Missing(name.to_string()))?;
        self.read_at(i)
    }

    /// Read the member at an index into [`Container::entries`].
    pub fn read_at(&mut self, index: usize) -> Result<Vec<u8>, Error> {
        let e = self.entries.get(index).ok_or(Error::Truncated)?.clone();

        // The central directory's name and extra lengths are not necessarily
        // the local header's, so the data offset has to come from the local
        // header itself.
        self.file.seek(SeekFrom::Start(e.local_header))?;
        let mut head = [0u8; LOCAL_LEN];
        self.file.read_exact(&mut head)?;
        if u32le(&head, 0)? != LOCAL_SIG {
            return Err(Error::NotAZip);
        }
        let skip = u16le(&head, 26)? as u64 + u16le(&head, 28)? as u64;
        self.file
            .seek(SeekFrom::Start(e.local_header + LOCAL_LEN as u64 + skip))?;
        let mut raw = vec![0u8; e.compressed as usize];
        self.file.read_exact(&mut raw)?;

        let out = match e.method {
            STORED => raw,
            IMPLODED => blast(&raw).map_err(Error::Blast)?,
            m => return Err(Error::UnknownMethod(m)),
        };

        let got = crc32(&out);
        if got != e.crc {
            return Err(Error::Checksum { name: e.name, want: e.crc, got });
        }
        Ok(out)
    }
}

/// CRC32, the ordinary one, reflected with polynomial 0xedb88320. Twenty
/// lines rather than a dependency, and the format is what checks it.
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        t
    });
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c = table[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
    }
    c ^ 0xffff_ffff
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published check value for CRC-32: "123456789" is 0xcbf43926.
    /// Every implementation of this agrees on it, so it is a real check and
    /// not a recording of what this code happened to produce.
    #[test]
    fn crc32_matches_the_published_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
    }

    /// A stored member, built by hand, read back through the whole path:
    /// end-of-directory scan, central directory, local header, checksum.
    #[test]
    fn reads_a_hand_built_archive() {
        let body = b"hello";
        let crc = crc32(body);
        let name = b"a.txt";
        let mut z = Vec::new();

        let local_at = z.len() as u32;
        z.extend_from_slice(&LOCAL_SIG.to_le_bytes());
        z.extend_from_slice(&[0u8; 4]); // version, flags
        z.extend_from_slice(&STORED.to_le_bytes());
        z.extend_from_slice(&[0u8; 4]); // time, date
        z.extend_from_slice(&crc.to_le_bytes());
        z.extend_from_slice(&(body.len() as u32).to_le_bytes());
        z.extend_from_slice(&(body.len() as u32).to_le_bytes());
        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(name);
        z.extend_from_slice(body);

        let cd_at = z.len() as u32;
        z.extend_from_slice(&CDIR_SIG.to_le_bytes());
        z.extend_from_slice(&[0u8; 6]); // versions, flags
        z.extend_from_slice(&STORED.to_le_bytes());
        z.extend_from_slice(&[0u8; 4]); // time, date
        z.extend_from_slice(&crc.to_le_bytes());
        z.extend_from_slice(&(body.len() as u32).to_le_bytes());
        z.extend_from_slice(&(body.len() as u32).to_le_bytes());
        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
        z.extend_from_slice(&[0u8; 12]); // extra, comment, disk, attrs
        z.extend_from_slice(&local_at.to_le_bytes());
        z.extend_from_slice(name);
        let cd_size = z.len() as u32 - cd_at;

        z.extend_from_slice(&EOCD_SIG.to_le_bytes());
        z.extend_from_slice(&[0u8; 4]); // disks
        z.extend_from_slice(&1u16.to_le_bytes());
        z.extend_from_slice(&1u16.to_le_bytes());
        z.extend_from_slice(&cd_size.to_le_bytes());
        z.extend_from_slice(&cd_at.to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());

        let path = std::env::temp_dir().join("goodomen-test.zip");
        std::fs::write(&path, &z).unwrap();
        let mut c = Container::open(&path).unwrap();
        assert_eq!(c.entries().len(), 1);
        assert_eq!(c.entries()[0].name, "a.txt");
        assert_eq!(c.read("A.TXT").unwrap(), body);
        assert!(matches!(c.read("nope"), Err(Error::Missing(_))));
        std::fs::remove_file(&path).ok();
    }
}
