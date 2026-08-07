//! Interplay ACM — the codec under every `.wav` in the game and under the
//! music, and the last format the engine could not read for itself.
//!
//! ```text
//! bits, least significant first, out of a byte stream
//!   24  0x032897, the magic -- never compared as four bytes, which is why
//!       searching the executable for `97 28 03 01` finds nothing
//!    8  1, the version
//!   32  samples, across all channels
//!   16  channels
//!   16  rate
//!    4  level
//!   12  rows
//! ```
//!
//! Then blocks of `rows * (1 << level)` values until the sample count is met.
//! Every stream MDK2 ships is level 7, rows 16 — 2048 values a block.
//!
//! A block is **columns of a transform**, not samples. Each of the `1 << level`
//! columns is filled by one of 32 routines chosen by a 5-bit code, and the
//! filled block is then run through an inverse transform (`juggle`) that
//! walks halving strides. What comes out is the audio, shifted right by
//! `level`.
//!
//! # Where this came from
//!
//! `mdk2Main.exe` carries the decoder whole, and the 32 fillers sit in a jump
//! table at `0x4b94c8` — six of its entries point at a routine that only
//! returns failure, which is what pinned the codes the encoder never emits.
//! Everything here was read out of that code and written fresh; see rule 2 in
//! `docs/rules.md`. `tools/acmcheck.py` holds it to `ffmpeg -f acm`, which is
//! a decoder nobody here wrote, over all 992 wrapped sounds and 27 music
//! tracks.
//!
//! Two details cost real time and are worth stating:
//!
//! * The amplitude table is **16-bit**, and the original stores `i * value`
//!   into it with the product wrapping. Widen it and loud blocks decode to
//!   different numbers.
//! * The wrap buffer carries state **between blocks**. It is allocated and
//!   zeroed once per stream, never per block, so decoding a block twice is
//!   not the same as decoding it once.

/// `97 28 03 01`, read as 24 bits of magic and 8 of version.
pub const MAGIC: u32 = 0x03_2897;
pub const VERSION: u32 = 1;
pub const HEADER: usize = 14;

#[derive(Debug, PartialEq)]
pub enum Error {
    NotAcm,
    Version(u32),
    Truncated,
    /// `level` and `rows` are 4 and 12 bits, so a corrupt header can ask for a
    /// half-terabyte block. Every stream the game ships wants 2048 values.
    Absurd { cols: usize, rows: usize },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotAcm => write!(f, "no Interplay ACM magic"),
            Error::Version(v) => write!(f, "ACM version {v}, not {VERSION}"),
            Error::Truncated => write!(f, "shorter than the {HEADER}-byte header"),
            Error::Absurd { cols, rows } => {
                write!(f, "a block of {rows} x {cols} values is not a sound")
            }
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Header {
    /// Across all channels, not frames.
    pub samples: u32,
    pub channels: u16,
    pub rate: u16,
    /// `1 << level` columns, and the shift applied to every output sample.
    pub level: u32,
    pub rows: u32,
}

/// The three tables that unpack several small values out of one code. Each is
/// a change of base: a code counted in base 3 or 5 or 11, re-spelled in base
/// 4 or 8 or 16 so the digits fall on bit boundaries.
///
/// They are sized to the code, not to the data — 5 bits index the first, 7 the
/// other two — so the entries past the last real one are read. The original
/// leaves them zero (they are `.bss`), and so does this.
struct Tables {
    base3: [u8; 32],
    base5: [u16; 128],
    base11: [u8; 128],
}

impl Tables {
    fn new() -> Self {
        let mut t = Tables { base3: [0; 32], base5: [0; 128], base11: [0; 128] };
        // the digit that counts ones in the code counts ones in the entry too:
        // transpose either of these and most columns still decode, which is
        // exactly what makes it expensive to find
        for i in 0..3 {
            for j in 0..3 {
                for k in 0..3 {
                    t.base3[i + 3 * j + 9 * k] = (i + 4 * j + 16 * k) as u8;
                }
            }
        }
        for i in 0..5 {
            for j in 0..5 {
                for k in 0..5 {
                    t.base5[i + 5 * j + 25 * k] = (i + 8 * j + 64 * k) as u16;
                }
            }
        }
        for i in 0..11 {
            for j in 0..11 {
                t.base11[i + 11 * j] = ((j << 4) + i) as u8;
            }
        }
        t
    }
}

/// The amplitude table is indexed from `-32768` to `32767`; this is where zero
/// sits in the backing array.
const MID: usize = 0x8000;

struct Decoder<'a> {
    src: &'a [u8],
    at: usize,
    acc: u32,
    have: u32,

    cols: usize,
    rows: usize,
    level: u32,
    /// Rows the transform does at a time: `max(2048 / cols - 2, 1)`.
    chunk: usize,

    block: Vec<i32>,
    wrap: Vec<i32>,
    amp: Vec<i16>,
    tables: Tables,
}

impl<'a> Decoder<'a> {
    /// Past the end reads as zero. The original refills through a callback
    /// that returns nothing at EOF, and eleven of the game's streams rely on
    /// it: they stop within a block of the end and the last block is only
    /// finished because the reader keeps handing out zeros.
    fn byte(&mut self) -> u32 {
        let b = self.src.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        b as u32
    }

    fn bits(&mut self, n: u32) -> u32 {
        while self.have < n {
            let b = self.byte();
            self.acc |= b << self.have;
            self.have += 8;
        }
        let v = self.acc & ((1u32 << n) - 1);
        self.acc >>= n;
        self.have -= n;
        v
    }

    /// Read without consuming. Every filler peeks, branches on the low bits,
    /// then consumes only as many as the branch it took needs.
    fn peek(&mut self, n: u32) -> u32 {
        while self.have < n {
            let b = self.byte();
            self.acc |= b << self.have;
            self.have += 8;
        }
        self.acc
    }

    fn skip(&mut self, n: u32) {
        self.acc >>= n;
        self.have -= n;
    }

    fn amp(&self, i: i32) -> i32 {
        self.amp[(MID as i32 + i) as usize] as i32
    }
}

/// One column of the block: `rows` values, `cols` apart.
struct Column {
    at: usize,
    left: usize,
}

impl Column {
    fn put(&mut self, block: &mut [i32], cols: usize, v: i32) -> bool {
        block[self.at] = v;
        self.at += cols;
        self.left -= 1;
        self.left > 0
    }
}

impl<'a> Decoder<'a> {
    /// Fill column `ind` by routine `code`. False means the code is one the
    /// encoder never emits, which the original treats as the end of the
    /// stream.
    fn fill(&mut self, code: u32, ind: usize) -> bool {
        let (cols, rows) = (self.cols, self.rows);
        if rows == 0 {
            return true;
        }
        let mut c = Column { at: ind, left: rows };
        match code {
            // nothing was coded for this column
            0 => {
                for _ in 0..rows {
                    self.block[c.at] = 0;
                    c.at += cols;
                }
                true
            }
            // one value per row, `code` bits wide, straight into the table
            3..=16 => {
                let middle = 1i32 << (code - 1);
                loop {
                    let b = self.bits(code) as i32;
                    let v = self.amp(b - middle);
                    if !c.put(&mut self.block, cols, v) {
                        return true;
                    }
                }
            }
            // 1 bit: two zeros. 2: one zero. 3: one +-1.
            17 => loop {
                let p = self.peek(3);
                if p & 1 == 0 {
                    self.skip(1);
                    if !c.put(&mut self.block, cols, 0) {
                        return true;
                    }
                    if !c.put(&mut self.block, cols, 0) {
                        return true;
                    }
                } else if p & 2 == 0 {
                    self.skip(2);
                    if !c.put(&mut self.block, cols, 0) {
                        return true;
                    }
                } else {
                    self.skip(3);
                    let v = self.amp(if p & 4 != 0 { 1 } else { -1 });
                    if !c.put(&mut self.block, cols, v) {
                        return true;
                    }
                }
            },
            // 1 bit: one zero. 2: one +-1.
            18 => loop {
                let p = self.peek(2);
                if p & 1 == 0 {
                    self.skip(1);
                    if !c.put(&mut self.block, cols, 0) {
                        return true;
                    }
                } else {
                    self.skip(2);
                    let v = self.amp(if p & 2 != 0 { 1 } else { -1 });
                    if !c.put(&mut self.block, cols, v) {
                        return true;
                    }
                }
            },
            // three of {-1,0,+1} in five bits
            19 => loop {
                let b = self.bits(5) as usize;
                let v = self.tables.base3[b] as i32;
                for shift in [0, 2, 4] {
                    let a = self.amp(((v >> shift) & 3) - 1);
                    if !c.put(&mut self.block, cols, a) {
                        return true;
                    }
                }
            },
            // 1 bit: two zeros. 2: one zero. 4: one of {-2,-1,+1,+2}.
            20 => loop {
                let p = self.peek(4);
                if p & 1 == 0 {
                    self.skip(1);
                    if !c.put(&mut self.block, cols, 0) {
                        return true;
                    }
                    if !c.put(&mut self.block, cols, 0) {
                        return true;
                    }
                } else if p & 2 == 0 {
                    self.skip(2);
                    if !c.put(&mut self.block, cols, 0) {
                        return true;
                    }
                } else {
                    self.skip(4);
                    let v = self.amp(match (p & 8 != 0, p & 4 != 0) {
                        (true, true) => 2,
                        (true, false) => 1,
                        (false, true) => -1,
                        (false, false) => -2,
                    });
                    if !c.put(&mut self.block, cols, v) {
                        return true;
                    }
                }
            },
            // 1 bit: one zero. 3: one of {-2,-1,+1,+2}.
            21 => loop {
                let p = self.peek(3);
                if p & 1 == 0 {
                    self.skip(1);
                    if !c.put(&mut self.block, cols, 0) {
                        return true;
                    }
                } else {
                    self.skip(3);
                    let v = self.amp(match (p & 4 != 0, p & 2 != 0) {
                        (true, true) => 2,
                        (true, false) => 1,
                        (false, true) => -1,
                        (false, false) => -2,
                    });
                    if !c.put(&mut self.block, cols, v) {
                        return true;
                    }
                }
            },
            // three of {-2..+2} in seven bits
            22 => loop {
                let b = (self.bits(7) & 0x7f) as usize;
                let v = self.tables.base5[b] as i32;
                for d in [v & 7, (v >> 3) & 7, v >> 6] {
                    let a = self.amp(d - 2);
                    if !c.put(&mut self.block, cols, a) {
                        return true;
                    }
                }
            },
            // 1 bit: two zeros. 2: one zero. 4: one +-1. 5: one of {-3,-2,+2,+3}.
            23 => loop {
                let p = self.peek(5);
                if p & 1 == 0 {
                    self.skip(1);
                    if !c.put(&mut self.block, cols, 0) {
                        return true;
                    }
                    if !c.put(&mut self.block, cols, 0) {
                        return true;
                    }
                } else if p & 2 == 0 {
                    self.skip(2);
                    if !c.put(&mut self.block, cols, 0) {
                        return true;
                    }
                } else if p & 4 == 0 {
                    self.skip(4);
                    let v = self.amp(if p & 8 != 0 { 1 } else { -1 });
                    if !c.put(&mut self.block, cols, v) {
                        return true;
                    }
                } else {
                    self.skip(5);
                    let v = self.amp(far((p >> 3) & 3));
                    if !c.put(&mut self.block, cols, v) {
                        return true;
                    }
                }
            },
            // 1 bit: one zero. 3: one +-1. 4: one of {-3,-2,+2,+3}.
            24 => loop {
                let p = self.peek(4);
                if p & 1 == 0 {
                    self.skip(1);
                    if !c.put(&mut self.block, cols, 0) {
                        return true;
                    }
                } else if p & 2 == 0 {
                    self.skip(3);
                    let v = self.amp(if p & 4 != 0 { 1 } else { -1 });
                    if !c.put(&mut self.block, cols, v) {
                        return true;
                    }
                } else {
                    self.skip(4);
                    let v = self.amp(far((p >> 2) & 3));
                    if !c.put(&mut self.block, cols, v) {
                        return true;
                    }
                }
            },
            // 1 bit: two zeros. 2: one zero. 5: one of {-4..-1,+1..+4}.
            26 => loop {
                let p = self.peek(5);
                if p & 1 == 0 {
                    self.skip(1);
                    if !c.put(&mut self.block, cols, 0) {
                        return true;
                    }
                    if !c.put(&mut self.block, cols, 0) {
                        return true;
                    }
                } else if p & 2 == 0 {
                    self.skip(2);
                    if !c.put(&mut self.block, cols, 0) {
                        return true;
                    }
                } else {
                    self.skip(5);
                    let v = self.amp(skip_zero((p & 0x1c) >> 2));
                    if !c.put(&mut self.block, cols, v) {
                        return true;
                    }
                }
            },
            // 1 bit: one zero. 4: one of {-4..-1,+1..+4}.
            27 => loop {
                let p = self.peek(4);
                if p & 1 == 0 {
                    self.skip(1);
                    if !c.put(&mut self.block, cols, 0) {
                        return true;
                    }
                } else {
                    self.skip(4);
                    let v = self.amp(skip_zero((p & 0xe) >> 1));
                    if !c.put(&mut self.block, cols, v) {
                        return true;
                    }
                }
            },
            // two of {-5..+5} in seven bits
            29 => loop {
                let b = (self.bits(7) & 0x7f) as usize;
                let v = self.tables.base11[b] as i32;
                for d in [v & 0xf, v >> 4] {
                    let a = self.amp(d - 5);
                    if !c.put(&mut self.block, cols, a) {
                        return true;
                    }
                }
            },
            // 1, 2, 25, 28, 30, 31 -- the table points these at a routine
            // that does nothing but fail
            _ => false,
        }
    }
}

/// `{0,1,2,3}` -> `{-3,-2,+2,+3}`: two bits, no zero, no ones.
fn far(d: u32) -> i32 {
    let d = if d >= 2 { d + 3 } else { d } as i32;
    d - 3
}

/// `{0..7}` -> `{-4..-1,+1..+4}`: three bits with zero left out.
fn skip_zero(d: u32) -> i32 {
    let d = if d >= 4 { d + 1 } else { d } as i32;
    d - 4
}

/// The inverse transform, one stride at a time.
///
/// `wrap` carries the two values either side of the window, so the next call
/// continues where this one stopped — which is why it must not be cleared
/// between blocks.
fn juggle(wrap: &mut [i32], block: &mut [i32], sub_len: usize, sub_count: usize) {
    for i in 0..sub_len {
        let mut p = i;
        let mut r0 = wrap[2 * i];
        let mut r1 = wrap[2 * i + 1];
        for _ in 0..sub_count / 2 {
            let r2 = block[p];
            block[p] = r1.wrapping_mul(2).wrapping_add(r0).wrapping_add(r2);
            p += sub_len;
            let r3 = block[p];
            block[p] = r2.wrapping_mul(2).wrapping_sub(r1).wrapping_sub(r3);
            p += sub_len;
            r0 = r2;
            r1 = r3;
        }
        wrap[2 * i] = r0;
        wrap[2 * i + 1] = r1;
    }
}

impl<'a> Decoder<'a> {
    fn juggle_block(&mut self) {
        if self.level == 0 {
            return;
        }
        let mut todo = self.rows as isize;
        let mut blk = 0usize;
        while todo > 0 {
            let mut wrap = 0usize;
            let mut sub_len = self.cols / 2;
            let mut sub_count = 2 * todo.min(self.chunk as isize) as usize;
            juggle(&mut self.wrap[wrap..], &mut self.block[blk..], sub_len, sub_count);
            wrap += 2 * sub_len;
            // the rounding term the shift at the end expects
            for k in 0..sub_count {
                let i = blk + k * sub_len;
                self.block[i] = self.block[i].wrapping_add(1);
            }
            sub_len /= 2;
            sub_count *= 2;
            while sub_len != 0 {
                juggle(&mut self.wrap[wrap..], &mut self.block[blk..], sub_len, sub_count);
                wrap += 2 * sub_len;
                sub_len /= 2;
                sub_count *= 2;
            }
            todo -= self.chunk as isize;
            blk += self.chunk * self.cols;
        }
    }

    /// One block, or false at the end of the stream.
    fn block(&mut self) -> bool {
        let power = self.bits(4);
        let value = self.bits(16) as u16 as i16 as i32;
        let count = 1i32 << power;
        // amp[i] = i * value, and the product is stored 16 bits wide: the
        // original's table is an array of shorts and loud blocks do wrap
        for i in 0..count {
            self.amp[(MID as i32 + i) as usize] = i.wrapping_mul(value) as i16;
            self.amp[(MID as i32 - 1 - i) as usize] = (-1 - i).wrapping_mul(value) as i16;
        }
        for ind in 0..self.cols {
            let code = self.bits(5);
            if !self.fill(code, ind) {
                return false;
            }
        }
        self.juggle_block();
        true
    }
}

/// Read the 14-byte header. Nothing after it is touched.
pub fn header(acm: &[u8]) -> Result<Header, Error> {
    if acm.len() < HEADER {
        return Err(Error::Truncated);
    }
    let mut d = Decoder {
        src: acm,
        at: 0,
        acc: 0,
        have: 0,
        cols: 0,
        rows: 0,
        level: 0,
        chunk: 0,
        block: Vec::new(),
        wrap: Vec::new(),
        amp: Vec::new(),
        tables: Tables { base3: [0; 32], base5: [0; 128], base11: [0; 128] },
    };
    if d.bits(24) != MAGIC {
        return Err(Error::NotAcm);
    }
    let version = d.bits(8);
    if version != VERSION {
        return Err(Error::Version(version));
    }
    // the sample count is 32 bits and the accumulator holds 31, so it comes
    // in halves -- which is how the original reads it too
    let (low, high) = (d.bits(16), d.bits(16));
    Ok(Header {
        samples: low | (high << 16),
        channels: d.bits(16) as u16,
        rate: d.bits(16) as u16,
        level: d.bits(4),
        rows: d.bits(12),
    })
}

/// Decode a whole stream to interleaved 16-bit PCM.
///
/// Short output is not an error: eleven of the game's streams stop inside
/// their last block and the original stops with them.
pub fn decode(acm: &[u8]) -> Result<Vec<i16>, Error> {
    let h = header(acm)?;
    let cols = 1usize << h.level;
    let rows = h.rows as usize;
    if cols * rows > 1 << 20 {
        return Err(Error::Absurd { cols, rows });
    }
    let mut d = Decoder {
        src: acm,
        at: HEADER,
        acc: 0,
        have: 0,
        cols,
        rows,
        level: h.level,
        // 2048 values at a time, less the two the transform reads either side
        chunk: (2048 / cols).saturating_sub(2).max(1),
        block: vec![0; rows * cols],
        // enough for every stride: cols + cols/2 + ... + 2
        wrap: vec![0; if h.level == 0 { 0 } else { 2 * cols - 2 }],
        amp: vec![0; 0x10000],
        tables: Tables::new(),
    };
    let mut out: Vec<i16> = Vec::with_capacity(h.samples as usize);
    while out.len() < h.samples as usize {
        if !d.block() {
            break;
        }
        let want = (h.samples as usize - out.len()).min(d.block.len());
        for i in 0..want {
            out.push((d.block[i] >> h.level) as i16);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(samples: u32, level: u32, rows: u32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&[0x97, 0x28, 0x03, 0x01]);
        v.extend_from_slice(&samples.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&22050u16.to_le_bytes());
        v.extend_from_slice(&(((rows as u16) << 4) | level as u16).to_le_bytes());
        v
    }

    #[test]
    fn the_header_is_read_bitwise() {
        let h = header(&head(34801, 7, 16)).unwrap();
        assert_eq!(h, Header { samples: 34801, channels: 1, rate: 22050, level: 7, rows: 16 });
        assert_eq!(header(&[0u8; 14]), Err(Error::NotAcm));
        assert_eq!(header(&[0u8; 8]), Err(Error::Truncated));
    }

    /// Level 0 means one column, no transform and no shift, so a stream of
    /// `linear` codes decodes to exactly the amplitudes it names. This is the
    /// one shape whose output can be predicted by hand.
    #[test]
    fn a_flat_stream_decodes_to_its_amplitudes() {
        let mut bits: Vec<(u32, u32)> = vec![
            (4, 1),   // power: two amplitudes each way
            (16, 100), // value: amp[i] = 100 i
            (5, 4),   // the column routine: four bits a value
        ];
        // four bits, middle 8: 8 -> amp[0], 9 -> amp[1], 7 -> amp[-1]
        bits.extend([(4, 8), (4, 9), (4, 7)]);
        let mut d = head(3, 0, 3);
        let (mut acc, mut have) = (0u32, 0u32);
        for (n, v) in bits {
            acc |= v << have;
            have += n;
            while have >= 8 {
                d.push(acc as u8);
                acc >>= 8;
                have -= 8;
            }
        }
        if have > 0 {
            d.push(acc as u8);
        }
        assert_eq!(decode(&d).unwrap(), vec![0, 100, -100]);
    }

    /// The tables are a change of base and nothing else; a wrong stride shows
    /// up here rather than as noise in one sound out of a thousand.
    #[test]
    fn the_unpacking_tables_are_a_change_of_base() {
        let t = Tables::new();
        for i in 0..3u32 {
            for j in 0..3u32 {
                for k in 0..3u32 {
                    let v = t.base3[(i + 3 * j + 9 * k) as usize] as u32;
                    assert_eq!((v & 3, (v >> 2) & 3, (v >> 4) & 3), (i, j, k));
                }
            }
        }
        for i in 0..5u32 {
            for j in 0..5u32 {
                for k in 0..5u32 {
                    let v = t.base5[(i + 5 * j + 25 * k) as usize] as u32;
                    assert_eq!((v & 7, (v >> 3) & 7, v >> 6), (i, j, k));
                }
            }
        }
        for i in 0..11u32 {
            for j in 0..11u32 {
                let v = t.base11[(i + 11 * j) as usize] as u32;
                assert_eq!((v & 0xf, v >> 4), (i, j));
            }
        }
        // the entries past the last real one are read, and must be zero
        assert_eq!(t.base3[27..], [0; 5]);
        assert_eq!(t.base5[125..], [0; 3]);
        assert_eq!(t.base11[121..], [0; 7]);
    }

    #[test]
    fn the_two_sign_maps_leave_out_what_they_should() {
        assert_eq!([far(0), far(1), far(2), far(3)], [-3, -2, 2, 3]);
        assert_eq!(
            (0..8).map(skip_zero).collect::<Vec<_>>(),
            [-4, -3, -2, -1, 1, 2, 3, 4]
        );
    }
}
