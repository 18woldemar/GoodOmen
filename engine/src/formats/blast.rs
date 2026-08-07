//! PKWARE DCL Implode, the compression the `data/*.zip` containers use.
//!
//! Method 10 in the ZIP header, and no ordinary tool reads it: not
//! `python-zipfile`, not `unzip`, not libarchive, and `7z` is worse than
//! useless because it writes zero-length files while returning non-zero, so
//! the output directory looks populated when nothing was extracted.
//!
//! The algorithm is the one Mark Adler documented as *blast*. Its Huffman
//! tables are fixed and part of the format rather than carried in the
//! stream, which is why they sit here as constants with no way to derive
//! them.
//!
//! `../../tools/unpack.py` does the same job in Python and stays the
//! reference. The format checks itself: every member of a container carries
//! a CRC32 in the ZIP central directory, so agreeing on all 4751 files is
//! not an opinion.

/// Run-length coded code lengths, `(repeat - 1) << 4 | length`. Part of the
/// format's description, not a choice made here.
const LITLEN: [u8; 98] = [
    11, 124, 8, 7, 28, 7, 188, 13, 76, 4, 10, 8, 12, 10, 12, 10, 8, 23, 8, 9,
    7, 6, 7, 8, 7, 6, 55, 8, 23, 24, 12, 11, 7, 9, 11, 12, 6, 7, 22, 5, 7, 24,
    6, 11, 9, 6, 7, 22, 7, 11, 38, 7, 9, 8, 25, 11, 8, 11, 9, 12, 8, 12, 5,
    38, 5, 38, 5, 11, 7, 5, 6, 21, 6, 10, 53, 8, 7, 24, 10, 27, 44, 253, 253,
    253, 252, 252, 252, 13, 12, 45, 12, 45, 12, 61, 12, 45, 44, 173,
];
const LENLEN: [u8; 6] = [2, 35, 36, 53, 38, 23];
const DISTLEN: [u8; 7] = [2, 20, 53, 230, 247, 151, 248];

const BASE: [u32; 16] = [3, 2, 4, 5, 6, 7, 8, 9, 10, 12, 16, 24, 40, 72, 136, 264];
const EXTRA: [u32; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8];

const MAXBITS: usize = 13;
/// A decoded length of 519 ends the stream. There is no other terminator.
const END_LEN: u32 = 519;

#[derive(Debug, PartialEq)]
pub enum Error {
    LiteralFlag(u8),
    DictionarySize(u8),
    BadCode,
    DistanceBeforeStart,
    Truncated,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::LiteralFlag(v) => write!(f, "unknown literal flag {v}"),
            Error::DictionarySize(v) => write!(f, "invalid dictionary size {v}"),
            Error::BadCode => write!(f, "invalid Huffman code"),
            Error::DistanceBeforeStart => write!(f, "back-reference before the start"),
            Error::Truncated => write!(f, "stream ends mid-symbol"),
        }
    }
}

impl std::error::Error for Error {}

/// A canonical Huffman code: how many codes of each bit length, and the
/// symbols in canonical order.
struct Code {
    count: [u32; MAXBITS + 2],
    symbol: Vec<u16>,
}

impl Code {
    fn new(rep: &[u8]) -> Code {
        let mut lengths = Vec::new();
        for &b in rep {
            for _ in 0..=(b >> 4) {
                lengths.push(b & 15);
            }
        }
        let mut count = [0u32; MAXBITS + 2];
        for &l in &lengths {
            count[l as usize] += 1;
        }
        let mut offs = [0u32; MAXBITS + 3];
        for l in 1..=MAXBITS {
            offs[l + 1] = offs[l] + count[l];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbol[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        Code { count, symbol }
    }
}

/// A bit stream, least significant bit first.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u32,
    cnt: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Bits<'a> {
        Bits { data, pos: 0, buf: 0, cnt: 0 }
    }

    fn take(&mut self) -> Result<u32, Error> {
        let b = *self.data.get(self.pos).ok_or(Error::Truncated)?;
        self.pos += 1;
        Ok(b as u32)
    }

    fn bits(&mut self, need: u32) -> Result<u32, Error> {
        while self.cnt < need {
            let b = self.take()?;
            self.buf |= b << self.cnt;
            self.cnt += 8;
        }
        let val = self.buf & ((1u32 << need) - 1);
        self.buf >>= need;
        self.cnt -= need;
        Ok(val)
    }

    /// Decode one symbol. **The codes are inverted** — a DCL quirk, and the
    /// single thing most likely to be got wrong when porting this.
    fn decode(&mut self, code: &Code) -> Result<u16, Error> {
        let (mut val, mut first, mut index) = (0u32, 0u32, 0u32);
        for length in 1..=MAXBITS {
            if self.cnt == 0 {
                self.buf = self.take()?;
                self.cnt = 8;
            }
            val |= (self.buf & 1) ^ 1;
            self.buf >>= 1;
            self.cnt -= 1;
            let c = code.count[length];
            if val < first + c {
                return Ok(code.symbol[(index + (val - first)) as usize]);
            }
            index += c;
            first = (first + c) << 1;
            val <<= 1;
        }
        Err(Error::BadCode)
    }
}

/// Decompress one DCL Implode stream.
pub fn blast(data: &[u8]) -> Result<Vec<u8>, Error> {
    let litcode = Code::new(&LITLEN);
    let lencode = Code::new(&LENLEN);
    let distcode = Code::new(&DISTLEN);

    let mut b = Bits::new(data);
    let coded_literals = b.bits(8)? as u8;
    if coded_literals > 1 {
        return Err(Error::LiteralFlag(coded_literals));
    }
    let dict_bits = b.bits(8)?;
    if !(4..=6).contains(&dict_bits) {
        return Err(Error::DictionarySize(dict_bits as u8));
    }

    let mut out: Vec<u8> = Vec::new();
    loop {
        if b.bits(1)? == 1 {
            let sym = b.decode(&lencode)? as usize;
            let length = BASE[sym] + b.bits(EXTRA[sym])?;
            if length == END_LEN {
                return Ok(out);
            }
            // for a length of 2 the distance is two bits wide, not the
            // dictionary's — the one place the format stops being uniform
            let dbits = if length == 2 { 2 } else { dict_bits };
            let dist = ((b.decode(&distcode)? as u32) << dbits) + b.bits(dbits)? + 1;
            if dist as usize > out.len() {
                return Err(Error::DistanceBeforeStart);
            }
            let start = out.len() - dist as usize;
            // byte by byte, because the copy may overlap its own output:
            // that is how the format encodes a run
            for k in 0..length as usize {
                let byte = out[start + k];
                out.push(byte);
            }
        } else if coded_literals == 1 {
            out.push(b.decode(&litcode)? as u8);
        } else {
            out.push(b.bits(8)? as u8);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tables must expand to the sizes the format defines: 256 literals,
    /// 16 lengths, 64 distances. Getting the run-length expansion wrong is
    /// otherwise silent — every symbol simply decodes to the wrong thing.
    #[test]
    fn tables_expand_to_the_sizes_the_format_defines() {
        assert_eq!(Code::new(&LITLEN).symbol.len(), 256);
        assert_eq!(Code::new(&LENLEN).symbol.len(), 16);
        assert_eq!(Code::new(&DISTLEN).symbol.len(), 64);
    }

    /// Mark Adler's own test vector for blast. It exercises the overlapping
    /// copy: "AIAIAIAIAIAIA" is thirteen bytes from two literals and a
    /// back-reference longer than its own distance.
    #[test]
    fn decodes_the_reference_stream() {
        let data = [0x00u8, 0x04, 0x82, 0x24, 0x25, 0x8f, 0x80, 0x7f];
        assert_eq!(blast(&data).unwrap(), b"AIAIAIAIAIAIA");
    }

    #[test]
    fn rejects_a_header_it_does_not_understand() {
        assert_eq!(blast(&[0x02, 0x04]), Err(Error::LiteralFlag(2)));
        assert_eq!(blast(&[0x00, 0x09]), Err(Error::DictionarySize(9)));
    }

    #[test]
    fn refuses_to_run_off_the_end() {
        assert_eq!(blast(&[0x00]), Err(Error::Truncated));
    }
}
