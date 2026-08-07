//! `.tex` — the Omen renderer's textures, container and block codec.
//!
//! The container is a 44-byte header, a `TEXC` chunk, then a mip chain from
//! the full size down to 1x1. Levels of 8x8 and up are coded at **4 bits per
//! pixel**; the 4x4, 2x2 and 1x1 levels are stored as raw BGRA, which is why
//! every compressed texture ends with 16 + 4 + 1 = 21 pixels. Rows are
//! **bottom-up** (the GL origin) and channel order is **BGRA**; both are left
//! as they lie here, because the upload path is where they get dealt with.
//!
//! The codec packs **8x4 pixels into 16 bytes**, and every block is really
//! two independent **4x4 sub-blocks**, left then right. The top bits of the
//! last dword choose one of four layouts — which is why fitting a single
//! model to it (DXT1, or two endpoints and 3-bit weights) never worked:
//!
//! ```text
//! bit 127          == 1   ->  A   a ramp per 4x4, two bits a pixel
//! bits 127,126     == 00  ->  B   one ramp for the block, three bits a pixel
//! bits 127,126,125 == 010 ->  C   four colours, stored outright
//! bits 127,126,125 == 011 ->  D   colours with alpha
//! ```
//!
//! Bit 124 then picks a sub-mode within A and within D; C ignores it (the one
//! spare bit in the format) and B has no room for it. So there are six
//! layouts, not four. Over all 4205514 blocks in the game: B 43.8%, A
//! four-colour 26.0%, A three-colour 13.9%, D ramp 7.5%, C 5.6%, D palette
//! 3.2%.
//!
//! `../../tools/texdec.py` is the reference and its docstring the long form.
//! It was written from scratch and checked block for block against the
//! original routine under emulation: **all 4205514 blocks byte-exact**. This
//! is a port of that, and `tools/texcheck.sh` holds the two to each other
//! over the whole corpus.

const TYPE_TEX: u32 = 2001;
/// `u32` at 0x24: 32 means a compressed `TEXC` chunk, 0 means raw BGRA. Only
/// the two fonts are raw.
const COMPRESSED: u32 = 32;
/// Level 0 of a compressed texture, past the header and the offset table.
const DATA_OFFSET: usize = 0x68;
/// The pixels of an uncompressed texture start right after the header.
const RAW_OFFSET: usize = 0x2c;
/// At and above this a level is 4 bpp; below it the level is raw BGRA.
const MIN_CODED_DIM: u32 = 8;

/// Where each sub-block's colour fields start, from the table at 0x4b8150.
const COLOUR_AT: [u32; 2] = [64, 94];
/// Where each sub-block's indices start — layouts A, C and D, two bits a
/// pixel. From the table at 0x4b8160.
const INDEX_AT: [u32; 2] = [0, 32];
/// Layout B, three bits a pixel, so sub-block 1 starts 48 bits in.
const INDEX_AT_B: [u32; 2] = [0, 48];

#[derive(Debug, PartialEq)]
pub enum Error {
    NotATexture(u32),
    Truncated,
    /// The level arithmetic and the file's own size disagree.
    Size { levels: usize, file: usize },
    /// The offset table at 0x3c disagrees with the level arithmetic.
    OffsetTable,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotATexture(t) => write!(f, "resource type {t}, not a texture (2001)"),
            Error::Truncated => write!(f, "the file ends inside the header"),
            Error::Size { levels, file } => {
                write!(f, "the levels total {levels} bytes, the file is {file}")
            }
            Error::OffsetTable => write!(f, "the offset table disagrees with the levels"),
        }
    }
}

impl std::error::Error for Error {}

/// One mip level, decoded to BGRA.
pub struct Level {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, BGRA, rows bottom-up.
    pub bgra: Vec<u8>,
}

pub struct Texture {
    pub width: u32,
    pub height: u32,
    /// 3 or 4. **This is what decides whether a surface is alpha-tested or
    /// blended**, and it is exact over the corpus: all 517 textures that say
    /// 3 are fully opaque, all 238 that say 4 carry alpha.
    pub channels: u32,
    pub levels: Vec<Level>,
}

fn u32le(b: &[u8], at: usize) -> Result<u32, Error> {
    let s = b.get(at..at + 4).ok_or(Error::Truncated)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

impl Texture {
    /// Parse a `.tex` and decode every level.
    pub fn parse(data: &[u8]) -> Result<Texture, Error> {
        let tag = u32le(data, 0)?;
        if tag != TYPE_TEX {
            return Err(Error::NotATexture(tag));
        }
        let width = u32le(data, 8)?;
        let height = u32le(data, 0x0c)?;
        let channels = u32le(data, 0x10)?;

        if u32le(data, 0x24)? != COMPRESSED {
            // the two fonts: one level, raw BGRA, no chain and no table
            let n = (width * height * 4) as usize;
            let bgra = data
                .get(RAW_OFFSET..RAW_OFFSET + n)
                .ok_or(Error::Truncated)?
                .to_vec();
            return Ok(Texture {
                width,
                height,
                channels,
                levels: vec![Level { width, height, bgra }],
            });
        }

        let mut levels = Vec::new();
        for (off, size, w, h) in level_chain(data, width, height)? {
            let raw = data.get(off..off + size).ok_or(Error::Truncated)?;
            let bgra = if w >= MIN_CODED_DIM && h >= MIN_CODED_DIM {
                decode_level(w, h, raw)?
            } else {
                raw.to_vec()
            };
            levels.push(Level { width: w, height: h, bgra });
        }
        Ok(Texture { width, height, channels, levels })
    }
}

/// `-> [(offset, size, width, height)]`, largest level first.
///
/// Derived arithmetically rather than from the table at 0x3c: levels of 8x8
/// and up take `w*h/2` bytes, the rest are raw BGRA, and they follow one
/// another from 0x68. The table only holds the **last nine** levels — a
/// 1024x1024 texture has eleven, so its two largest are missing from it —
/// which makes the arithmetic the more reliable source. The table is then
/// checked against it, and the file's own length checks both.
fn level_chain(
    data: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<(usize, usize, u32, u32)>, Error> {
    let (mut w, mut h) = (width, height);
    let mut out = Vec::new();
    let mut off = DATA_OFFSET;
    loop {
        let size = if w >= MIN_CODED_DIM && h >= MIN_CODED_DIM {
            (w * h / 2) as usize
        } else {
            (w * h * 4) as usize
        };
        out.push((off, size, w, h));
        off += size;
        if w == 1 && h == 1 {
            break;
        }
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    if off != data.len() {
        return Err(Error::Size { levels: off, file: data.len() });
    }

    let mut table = Vec::new();
    for k in 0..9 {
        let v = u32le(data, 0x3c + 4 * k)? as usize;
        if v != 0 {
            table.push(v);
        }
    }
    table.sort_unstable();
    let starts: Vec<usize> = out.iter().map(|&(o, _, _, _)| o).collect();
    if table != starts[starts.len() - table.len()..] {
        return Err(Error::OffsetTable);
    }
    Ok(out)
}

fn bits(block: u128, start: u32, width: u32) -> u32 {
    ((block >> start) & ((1u128 << width) - 1)) as u32
}

/// 5 bits to 8, by replication. Exact at both ends: 0 -> 0 and 31 -> 255.
fn e5(v: u32) -> u32 {
    (v << 3) | (v >> 2)
}

fn e6(v: u32) -> u32 {
    (v << 2) | (v >> 4)
}

/// BGR555 at a bit offset, opaque.
fn bgr555(block: u128, at: u32) -> [u32; 4] {
    [
        e5(bits(block, at, 5)),
        e5(bits(block, at + 5, 5)),
        e5(bits(block, at + 10, 5)),
        255,
    ]
}

/// BGR555 with a separate 5-bit alpha, which layout D keeps elsewhere in the
/// block.
fn bgra555(block: u128, at: u32, alpha_at: u32) -> [u32; 4] {
    let mut c = bgr555(block, at);
    c[3] = e5(bits(block, alpha_at, 5));
    c
}

fn lerp(c0: [u32; 4], c1: [u32; 4], num: u32, den: u32, round: u32, alpha: bool) -> [u8; 4] {
    let mut out = [255u8; 4];
    let channels = if alpha { 4 } else { 3 };
    for c in 0..channels {
        out[c] = (((den - num) * c0[c] + num * c1[c] + round) / den) as u8;
    }
    out
}

const TRANSPARENT: [u8; 4] = [0, 0, 0, 0];

/// **A** — each 4x4 gets its own pair of endpoints, 15 bits each: c1 at
/// `64 + 30*sub` and c0 fifteen bits after it.
///
/// Green is six bits rather than five, but only sometimes and never
/// symmetrically. c1's green LSB is bit `125 + sub`. c0's is that bit
/// **XOR** bit `32*sub + 1`, so it shares a bit with pixel 0's high index
/// bit; and in the three-colour ramp c0 does not get a sixth bit at all. The
/// original reaches that by jumping over one fix-up, and the asymmetry is
/// real enough to show up as 994 wrong blocks in twelve textures if you
/// assume otherwise.
fn palette_a(block: u128, sub: usize) -> [[u8; 4]; 4] {
    let at = COLOUR_AT[sub];
    let mut c1 = bgr555(block, at);
    let mut c0 = bgr555(block, at + 15);
    let high = bits(block, 125 + sub as u32, 1);
    c1[1] = e6((bits(block, at + 5, 5) << 1) | high);

    if bits(block, 124, 1) == 1 {
        // three colours, and c0 keeps its plain five-bit green
        return [
            lerp(c0, c1, 0, 2, 0, false),
            lerp(c0, c1, 1, 2, 0, false),
            lerp(c0, c1, 2, 2, 0, false),
            TRANSPARENT,
        ];
    }
    c0[1] = e6((bits(block, at + 20, 5) << 1) | (high ^ bits(block, INDEX_AT[sub] + 1, 1)));
    [
        lerp(c0, c1, 0, 3, 1, false),
        lerp(c0, c1, 1, 3, 1, false),
        lerp(c0, c1, 2, 3, 1, false),
        lerp(c0, c1, 3, 3, 1, false),
    ]
}

/// **B** — one pair of endpoints for all 32 pixels, c0 at bit 96 and c1 at
/// 111, with three-bit indices: seven steps and index 7 transparent.
fn palette_b(block: u128) -> [[u8; 4]; 8] {
    let c0 = bgr555(block, 96);
    let c1 = bgr555(block, 111);
    let mut pal = [TRANSPARENT; 8];
    for (k, slot) in pal.iter_mut().take(7).enumerate() {
        *slot = lerp(c0, c1, k as u32, 6, 2, false);
    }
    pal
}

/// **C** — four BGR555 stored outright, at bits 64, 79, 94 and 109. No
/// interpolation at all, and bit 124 is ignored.
fn palette_c(block: u128) -> [[u8; 4]; 4] {
    let mut pal = [TRANSPARENT; 4];
    for (k, slot) in pal.iter_mut().enumerate() {
        let c = bgr555(block, 64 + 15 * k as u32);
        *slot = [c[0] as u8, c[1] as u8, c[2] as u8, c[3] as u8];
    }
    pal
}

/// **D** — the alpha mode, and its two sub-modes are the least guessable part
/// of the format. Bit 124 clear: three BGRA colours plus a transparent
/// fourth. Bit 124 set: a ramp again, but each 4x4 keeps its own first colour
/// while **both share the last**, so 45 bits of colour buy two four-step
/// ramps instead of one.
fn palette_d(block: u128, sub: usize) -> [[u8; 4]; 4] {
    if bits(block, 124, 1) == 0 {
        let mut pal = [TRANSPARENT; 4];
        for (k, slot) in pal.iter_mut().take(3).enumerate() {
            let k = k as u32;
            let c = bgra555(block, 64 + 15 * k, 109 + 5 * k);
            *slot = [c[0] as u8, c[1] as u8, c[2] as u8, c[3] as u8];
        }
        return pal;
    }
    let c0 = if sub == 0 {
        bgra555(block, 64, 109)
    } else {
        bgra555(block, 94, 119)
    };
    let c1 = bgra555(block, 79, 114);
    [
        lerp(c0, c1, 0, 3, 1, true),
        lerp(c0, c1, 1, 3, 1, true),
        lerp(c0, c1, 2, 3, 1, true),
        lerp(c0, c1, 3, 3, 1, true),
    ]
}

/// Decode one block to 32 BGRA pixels, row-major over the 8x4 it covers.
///
/// Indices are stored per sub-block and run row by row within it, which is
/// why the destination index has to be rebuilt rather than just counted up.
pub fn decode_block(block: u128) -> [[u8; 4]; 32] {
    let mut out = [TRANSPARENT; 32];

    if block >> 127 == 1 {
        for sub in 0..2 {
            let pal = palette_a(block, sub);
            let base = INDEX_AT[sub];
            for i in 0..16u32 {
                out[((i / 4) * 8 + i % 4) as usize + 4 * sub] =
                    pal[bits(block, base + 2 * i, 2) as usize];
            }
        }
        return out;
    }
    if bits(block, 126, 1) == 0 {
        let pal = palette_b(block);
        for sub in 0..2 {
            let base = INDEX_AT_B[sub];
            for i in 0..16u32 {
                out[((i / 4) * 8 + i % 4) as usize + 4 * sub] =
                    pal[bits(block, base + 3 * i, 3) as usize];
            }
        }
        return out;
    }
    let is_c = bits(block, 125, 1) == 0;
    for sub in 0..2 {
        let pal = if is_c { palette_c(block) } else { palette_d(block, sub) };
        let base = INDEX_AT[sub];
        for i in 0..16u32 {
            out[((i / 4) * 8 + i % 4) as usize + 4 * sub] =
                pal[bits(block, base + 2 * i, 2) as usize];
        }
    }
    out
}

/// Decode one coded level to `width * height * 4` bytes of BGRA.
pub fn decode_level(width: u32, height: u32, data: &[u8]) -> Result<Vec<u8>, Error> {
    if (width * height / 2) as usize != data.len() {
        return Err(Error::Size {
            levels: (width * height / 2) as usize,
            file: data.len(),
        });
    }
    let mut out = vec![0u8; (width * height * 4) as usize];
    let mut n = 0usize;
    for by in (0..height).step_by(4) {
        for bx in (0..width).step_by(8) {
            let mut raw = [0u8; 16];
            raw.copy_from_slice(&data[n..n + 16]);
            n += 16;
            for (i, p) in decode_block(u128::from_le_bytes(raw)).iter().enumerate() {
                let i = i as u32;
                let o = (((by + i / 8) * width + bx + i % 8) * 4) as usize;
                out[o..o + 4].copy_from_slice(p);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One block per layout with the answer the original gives, recorded from
    /// `tools/refdec.py` — the original routine under emulation — so the test
    /// needs no game files and is not a recording of what this code happens
    /// to produce.
    const VECTORS: [(&str, u128, &str); 6] = [
        (
            "A four-colour",
            0x8abcdef013579bdf02468acefedcba98,
            "7baa4affd3db39ffa7c242ffd3db39ff10ccdeff00f3f7ff317dadff00f3f7ff\
             d3db39ffd3db39fffff331ffd3db39ff10ccdeff10ccdeff317dadff10ccdeff\
             7baa4afffff331ffa7c242fffff331ff10ccdeff21a4c6ff317dadff21a4c6ff\
             d3db39fffff331fffff331fffff331ff10ccdeff317dadff317dadff317dadff",
        ),
        (
            "A three-colour",
            0x9abcdef013579bdf02468acefedcba98,
            "7bad4afffff331ffbdd03dfffff331ff00f3f7ff00000000317badff00000000\
             fff331fffff331ff00000000fff331ff00f3f7ff00f3f7ff317badff00f3f7ff\
             7bad4aff00000000bdd03dff0000000000f3f7ff18b7d2ff317badff18b7d2ff\
             fff331ff00000000000000000000000000f3f7ff317badff317badff317badff",
        ),
        (
            "B",
            0x3abcdef013579bdf02468acefedcba98,
            "84bdbdffa98bd6ff9d9cceffc26ae7ffce5aefff84bdbdff90acc5ff90acc5ff\
             a98bd6ff90acc5ff00000000ce5aefff84bdbdffce5aefff00000000ce5aefff\
             ce5aefff00000000a98bd6ff00000000a98bd6ffa98bd6ffce5aefffa98bd6ff\
             b57bdeffc26ae7ff9d9cceffb57bdeffc26ae7ffce5aefffb57bdeff84bdbdff",
        ),
        (
            "C",
            0x4abcdef013579bdf02468acefedcba98,
            "fff731ff00f7f7ff7bad4aff00f7f7ff00f7f7ff317badfffff731ff317badff\
             00f7f7ff00f7f7ff317badff00f7f7ff00f7f7ff00f7f7fffff731ff00f7f7ff\
             fff731ff317badff7bad4aff317badff00f7f7ff7bad4afffff731ff7bad4aff\
             00f7f7ff317badff317badff317badff00f7f7fffff731fffff731fffff731ff",
        ),
        (
            "D palette",
            0x6abcdef013579bdf02468acefedcba98,
            "fff7313100f7f7ad7bad4a7b00f7f7ad00f7f7ad00000000fff7313100000000\
             00f7f7ad00f7f7ad0000000000f7f7ad00f7f7ad00f7f7adfff7313100f7f7ad\
             fff73131000000007bad4a7b0000000000f7f7ad7bad4a7bfff731317bad4a7b\
             00f7f7ad00000000000000000000000000f7f7adfff73131fff73131fff73131",
        ),
        (
            "D ramp",
            0x7abcdef013579bdf02468acefedcba98,
            "fff73131a7c64262d3de394aa7c6426252c6848c7bad4a7b00f7f7ad7bad4a7b\
             a7c64262a7c642627bad4a7ba7c6426252c6848c52c6848c00f7f7ad52c6848c\
             fff731317bad4a7bd3de394a7bad4a7b52c6848c29debd9c00f7f7ad29debd9c\
             a7c642627bad4a7b7bad4a7b7bad4a7b52c6848c00f7f7ad00f7f7ad00f7f7ad",
        ),
    ];

    fn unhex(s: &str) -> Vec<u8> {
        let d: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        d.chunks(2)
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn decodes_all_six_layouts() {
        for (name, block, want) in VECTORS {
            let got: Vec<u8> = decode_block(block).concat();
            assert_eq!(got, unhex(want), "{name}");
        }
    }

    /// C ignores bit 124; it is the one spare bit in the whole format, and
    /// reading it as a sub-mode selector there would be silent.
    #[test]
    fn bit_124_is_spare_in_layout_c() {
        let b = VECTORS[3].1;
        assert_eq!(decode_block(b), decode_block(b | (1 << 124)));
    }

    /// Replication has to be exact at both ends or every texture is dark by
    /// one part in thirty-two.
    #[test]
    fn five_bit_replication_reaches_both_ends() {
        assert_eq!(e5(0), 0);
        assert_eq!(e5(31), 255);
        assert_eq!(e6(0), 0);
        assert_eq!(e6(63), 255);
    }

    #[test]
    fn refuses_what_is_not_a_texture() {
        let mut data = vec![0u8; 0x68];
        data[..4].copy_from_slice(&2002u32.to_le_bytes()); // a model
        assert!(matches!(
            Texture::parse(&data),
            Err(Error::NotATexture(2002))
        ));
        assert!(matches!(Texture::parse(&[]), Err(Error::Truncated)));
    }
}
