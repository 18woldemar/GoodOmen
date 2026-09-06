//! The 2-D layer: quads in normalised screen space, and the game's own font.
//!
//! Everything the game draws that is not the world goes through here — the
//! menu, the HUD, `mdkDialogPanel`'s subtitles, `mdkShowLoadingScreen` and
//! `omSceneFade`. Five consumers, one layer, which is why it is built before
//! any of them.
//!
//! **Coordinates are the game's own**: x and y in `[0, 1]` across the window
//! with the origin at the **top left**, because that is what the scripts
//! hand the engine -- `menu.defx 0.5, defy 0.15` puts a menu in the upper
//! middle. They are normalised in *both* axes, so a square in these
//! coordinates is not square in pixels. That is the original's own
//! behaviour: it ran at 4:3 and the menu geometry was authored for it.
//!
//! ## The font
//!
//! `font.tex` is 512x512 RGBA holding a **16 x 16 grid of 32-pixel cells**,
//! and the glyph for code `c` is the cell at column `c % 16`, row `c / 16`.
//! Beside it `font.lua` is a 16 x 16 table of per-glyph **advances**, as a
//! fraction of the cell.
//!
//! The table is **transposed**: `dim[column][row]`, so
//!
//! ```text
//! advance[c] = dim[c % 16 + 1][c / 16 + 1]
//! ```
//!
//! which is `flat[(c % 16) * 16 + c / 16]` once the file is read in order.
//! The binary is no help here -- 0x462cf0 reads a flat 256-entry array at
//! `font + 4` and says nothing about how it was filled -- so the reading was
//! settled against the pixels: measure the inked width of every cell and
//! compare. Over the 94 printable glyphs the transposed reading is off by a
//! mean of **0.0066** and the direct one by **0.0921**, and `W`, `M` and `.`
//! are exact. `tools/check.py` keeps that measurement.
//!
//! A glyph is drawn as the **whole cell**, and the pen then moves by the
//! advance -- so the blank right-hand part of a cell is overdrawn by the
//! next one, which is how the letters close up.

use super::{program, scene::GpuTexture};
use glow::HasContext;

/// The advances of one font, indexed by character code.
pub struct Font {
    pub texture: GpuTexture,
    pub advance: [f32; 256],
}

/// What 0x462cf0 answers when the font carries no width table: the float at
/// 0x48f2f4, which is **1.0** — a whole cell.
const DEFAULT_ADVANCE: f32 = 1.0;

impl Font {
    /// Read the 16 x 16 advance table out of a `font.lua`.
    ///
    /// The file is a single `dim = { { .. }, .. }` of 256 numbers and
    /// nothing else, so the numbers in order are the table in order.
    pub fn advances(source: &str) -> Result<[f32; 256], String> {
        let mut out = [DEFAULT_ADVANCE; 256];
        let mut seen = 0usize;
        for token in source.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
            if token.is_empty() {
                continue;
            }
            let v: f32 = token.parse().map_err(|_| format!("not a number: {token}"))?;
            if seen < 256 {
                // flat is row-major over the file; the code's low nibble
                // picks the *outer* table. See the module note.
                let (row, col) = (seen / 16, seen % 16);
                out[col * 16 + row] = v;
            }
            seen += 1;
        }
        if seen != 256 {
            return Err(format!("a font table is 256 numbers, this one has {seen}"));
        }
        Ok(out)
    }

    /// How wide a string is, in units of one cell — 0x462d10, which is the
    /// sum of the advances and nothing else.
    /// **Bytes, not characters.** A string out of `mdk2.str` is code-page
    /// bytes and the atlas is a code page; see [`crate::formats::strfile`].
    pub fn width(&self, text: &[u8]) -> f32 {
        text.iter().map(|&b| self.advance[b as usize]).sum()
    }
}

/// One vertex of the layer: place, texture coordinate, colour.
const FLOATS: usize = 8;

/// A run of quads sharing one texture.
struct Batch {
    texture: Option<glow::Texture>,
    first: i32,
    count: i32,
}

#[derive(Default)]
pub struct Overlay {
    shader: Option<glow::Program>,
    vao: Option<glow::VertexArray>,
    vbo: Option<glow::Buffer>,
    /// White, 1x1, for a quad that names no texture — the fade and the
    /// menu's own boxes.
    blank: Option<glow::Texture>,
    vertices: Vec<f32>,
    batches: Vec<Batch>,
}

const VERTEX: &str = r#"#version 330 core
layout (location = 0) in vec2 place;
layout (location = 1) in vec2 uv;
layout (location = 2) in vec4 colour;
out vec2 vary_uv;
out vec4 vary_colour;
void main() {
    vary_uv = uv;
    vary_colour = colour;
    // screen space, origin top left, into clip space
    gl_Position = vec4(place.x * 2.0 - 1.0, 1.0 - place.y * 2.0, 0.0, 1.0);
}
"#;

const FRAGMENT: &str = r#"#version 330 core
in vec2 vary_uv;
in vec4 vary_colour;
uniform sampler2D albedo;
out vec4 fragment;
void main() {
    fragment = vary_colour * texture(albedo, vary_uv);
}
"#;

impl Overlay {
    /// Everything queued since the last [`Overlay::draw`] is dropped.
    pub fn clear(&mut self) {
        self.vertices.clear();
        self.batches.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    /// One quad. `uv` is `[u, v, width, height]` **in picture
    /// coordinates**, origin top left, like the screen ones beside them.
    ///
    /// A `.tex` stores its rows bottom-up because that is what
    /// `glTexImage2D` wants, so v counts up from the bottom in the sampler
    /// and this is where the two conventions are reconciled — once, rather
    /// than in each of the five things that draw here. The world renderer
    /// needs no such flip: model UVs are authored in the sampler's own
    /// convention. See [`crate::render::scene`].
    pub fn quad(
        &mut self,
        texture: Option<glow::Texture>,
        [x, y, w, h]: [f32; 4],
        [u, v, uw, vh]: [f32; 4],
        colour: [f32; 4],
    ) {
        let (top, bottom) = (1.0 - v, 1.0 - (v + vh));
        self.emit(texture, [x, y, w, h], [(u, top), (u, bottom), (u + uw, bottom), (u + uw, top)], colour);
    }

    /// The same quad with its texture **turned a quarter**: the strip's own
    /// long axis runs across the quad rather than down it. 0x462ff0 needs
    /// it for the top and bottom of a frame, where one edge texture is
    /// reused sideways -- the tile count arrives in the `v` slot and has to
    /// end up along the width.
    pub fn quad_turned(
        &mut self,
        texture: Option<glow::Texture>,
        place: [f32; 4],
        [u, v, uw, vh]: [f32; 4],
        colour: [f32; 4],
    ) {
        let (top, bottom) = (1.0 - v, 1.0 - (v + vh));
        self.emit(
            texture,
            place,
            [(top, u), (bottom, u), (bottom, u + uw), (top, u + uw)],
            colour,
        );
    }

    /// Two triangles, with the texture coordinates of the four corners given
    /// clockwise from the top left.
    fn emit(
        &mut self,
        texture: Option<glow::Texture>,
        [x, y, w, h]: [f32; 4],
        [tl, bl, br, tr]: [(f32, f32); 4],
        colour: [f32; 4],
    ) {
        let first = (self.vertices.len() / FLOATS) as i32;
        for (px, py, (pu, pv)) in [
            (x, y, tl),
            (x, y + h, bl),
            (x + w, y + h, br),
            (x, y, tl),
            (x + w, y + h, br),
            (x + w, y, tr),
        ] {
            self.vertices.extend_from_slice(&[px, py, pu, pv]);
            self.vertices.extend_from_slice(&colour);
        }
        match self.batches.last_mut() {
            Some(b) if b.texture == texture => b.count += 6,
            _ => self.batches.push(Batch { texture, first, count: 6 }),
        }
    }

    /// A string, its top-left corner at `(x, y)`, each cell `w` by `h`.
    /// Answers where the pen ended, which is what a caret and a right-hand
    /// column both need.
    pub fn text(
        &mut self,
        font: &Font,
        s: &[u8],
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        colour: [f32; 4],
    ) -> f32 {
        let mut pen = x;
        for &b in s {
            let (col, row) = ((b % 16) as f32, (b / 16) as f32);
            // a space is blank in the texture as well, so it costs a quad
            // for nothing; skipping it is the one special case worth having
            if b != b' ' {
                self.quad(
                    Some(font.texture.texture),
                    [pen, y, w, h],
                    [col / 16.0, row / 16.0, 1.0 / 16.0, 1.0 / 16.0],
                    colour,
                );
            }
            pen += font.advance[b as usize] * w;
        }
        pen
    }

    /// The rounded metal frame the game puts behind a menu, a dialogue
    /// panel and the title screen's caption -- **0x462ff0**, nine quads out
    /// of two textures.
    ///
    /// `textbox2` is the corner, mirrored into all four; `textbox1` is the
    /// edge, and it repeats: the tile count is `floor(2 * length) + 1`, in
    /// screen widths, so a menu half the screen across gets one length of it
    /// and a wide one gets two. The middle is filled from a single texel of
    /// the corner texture at `(0.8, 0.8)`, which is how one texture does
    /// both the border and the backing. The `0.004` inset on every edge is
    /// the float at 0x490340, and it is there so the repeat does not sample
    /// across the seam.
    ///
    /// `corner` is the corner's size on screen, which the menu passes as
    /// **0.04**; the frame is drawn *outside* the box it is given.
    pub fn frame(
        &mut self,
        corners: &GpuTexture,
        edges: &GpuTexture,
        [x, y, w, h]: [f32; 4],
        corner: f32,
        alpha: f32,
    ) {
        let (c, white) = (corner, [1.0, 1.0, 1.0, alpha]);
        const IN: f32 = 0.004;
        const OUT: f32 = 1.0 - IN;
        let tiles = |length: f32| (2.0 * length).floor() + 1.0 - IN;
        let (across, down) = (tiles(w), tiles(h));
        let (co, ed) = (Some(corners.texture), Some(edges.texture));
        // the binary's texture coordinates count v **up** from the bottom,
        // the way the sampler does; [`Overlay::quad`] takes them counting
        // down from the top like everything else on this layer. So every v
        // out of 0x462ff0 is turned over on the way in, and only here.
        let mut piece = |o: &mut Self, texture, place: [f32; 4], [u0, v0, u1, v1]: [f32; 4]| {
            o.quad(texture, place, [u0, 1.0 - v0, u1 - u0, v0 - v1], white);
        };
        let mut turned = |o: &mut Self, texture, place: [f32; 4], [u0, v0, u1, v1]: [f32; 4]| {
            o.quad_turned(texture, place, [u0, 1.0 - v0, u1 - u0, v0 - v1], white);
        };

        // the middle, from one texel of the corner sheet
        piece(self, co, [x, y, w, h], [0.8, 0.8, 0.8, 0.8]);
        // the four corners, the same picture turned over into each quadrant
        piece(self, co, [x - c, y - c, c, c], [IN, IN, OUT, OUT]);
        piece(self, co, [x + w, y - c, c, c], [OUT, IN, IN, OUT]);
        piece(self, co, [x - c, y + h, c, c], [IN, OUT, OUT, IN]);
        piece(self, co, [x + w, y + h, c, c], [OUT, OUT, IN, IN]);
        // and the four edges, each a run of the same strip
        turned(self, ed, [x, y - c, w, c], [IN, IN, OUT, across]);
        turned(self, ed, [x, y + h, w, c], [OUT, across, IN, IN]);
        piece(self, ed, [x - c, y, c, h], [IN, IN, OUT, down]);
        piece(self, ed, [x + w, y, c, h], [OUT, down, IN, IN]);
    }

    /// The whole screen in one colour — `omSceneFade`, and the backdrop a
    /// menu draws behind itself.
    pub fn fill(&mut self, colour: [f32; 4]) {
        let blank = self.blank;
        self.quad(blank, [0.0, 0.0, 1.0, 1.0], [0.0, 0.0, 1.0, 1.0], colour);
    }

    /// # Safety
    /// A GL context must be current on this thread.
    pub unsafe fn draw(&mut self, gl: &glow::Context) -> Result<(), String> {
        if self.shader.is_none() {
            self.shader = Some(program(gl, VERTEX, FRAGMENT)?);
            self.vao = Some(gl.create_vertex_array()?);
            self.vbo = Some(gl.create_buffer()?);
            // a 1x1 white pixel: an untextured quad is a textured one that
            // multiplies by 1, which keeps the shader down to one branchless
            // line
            let white = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(white));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                1,
                1,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&[255u8, 255, 255, 255])),
            );
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            self.blank = Some(white);
        }
        if self.vertices.is_empty() {
            return Ok(());
        }
        gl.bind_vertex_array(self.vao);
        gl.bind_buffer(glow::ARRAY_BUFFER, self.vbo);
        gl.buffer_data_u8_slice(
            glow::ARRAY_BUFFER,
            std::slice::from_raw_parts(
                self.vertices.as_ptr() as *const u8,
                std::mem::size_of_val(&self.vertices[..]),
            ),
            glow::STREAM_DRAW,
        );
        let stride = (FLOATS * std::mem::size_of::<f32>()) as i32;
        for (index, size, offset) in [(0, 2, 0), (1, 2, 8), (2, 4, 16)] {
            gl.enable_vertex_attrib_array(index);
            gl.vertex_attrib_pointer_f32(index, size, glow::FLOAT, false, stride, offset);
        }
        gl.use_program(self.shader);
        // the layer sits on top of the world: no depth, and blended
        gl.disable(glow::DEPTH_TEST);
        gl.enable(glow::BLEND);
        gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
        if let Some(u) = gl.get_uniform_location(self.shader.unwrap(), "albedo") {
            gl.uniform_1_i32(Some(&u), 0);
        }
        gl.active_texture(glow::TEXTURE0);
        for b in &self.batches {
            gl.bind_texture(glow::TEXTURE_2D, b.texture.or(self.blank));
            gl.draw_arrays(glow::TRIANGLES, b.first, b.count);
        }
        gl.disable(glow::BLEND);
        gl.enable(glow::DEPTH_TEST);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Font;

    /// The transposition, with a table that names its own cells: entry
    /// `[i][j]` is `i + j / 100`, so a wrong index shows up as a swapped
    /// pair rather than as a plausible number.
    #[test]
    fn advances_are_transposed() {
        let mut src = String::from("dim = {\n");
        for i in 0..16 {
            src.push('{');
            for j in 0..16 {
                src.push_str(&format!("{}.{:02}, ", i, j));
            }
            src.push_str("}\n");
        }
        src.push('}');
        let a = Font::advances(&src).unwrap();
        for code in 0..256usize {
            // the file's outer index is the code's *low* nibble
            let want = (code % 16) as f32 + (code / 16) as f32 / 100.0;
            assert!((a[code] - want).abs() < 1e-4, "{code}: {} not {want}", a[code]);
        }
    }

    #[test]
    fn a_short_table_is_an_error() {
        assert!(Font::advances("dim = { { 1.0 } }").is_err());
    }
}

/// Draw a string offscreen with the real font and look at the pixels that
/// come back: the smallest thing that fails if any part of the 2-D path is
/// wrong -- the shader, the coordinate flip, the atlas cell, or the advance.
///
/// The assertion is the **pen**: a string's ink must start at the place it
/// was put and end no further right than the sum of its advances says. That
/// is the one number the whole layer is built on, and a transposed table or
/// a misread cell breaks it by a wide margin.
pub fn selfcheck(font_lua: &str, font_tex: &[u8]) -> Result<String, String> {
    use super::{Offscreen, Video};

    const SIZE: i32 = 256;
    const TEXT: &str = "MDK2";
    const X: f32 = 0.1;
    const Y: f32 = 0.35;
    const W: f32 = 0.1;
    const H: f32 = 0.2;

    let advance = Font::advances(font_lua)?;
    let tex = crate::formats::tex::Texture::parse(font_tex).map_err(|e| e.to_string())?;
    let video = Video::open("goodomen", SIZE as u32, SIZE as u32, false)?;
    let gl = &video.gl;

    // SAFETY: the context Video::open made is current on this thread.
    unsafe {
        let target = Offscreen::new(gl, SIZE, SIZE)?;
        let texture =
            super::scene::upload(gl, &tex).ok_or_else(|| "no texture".to_string())?;
        let font = Font { texture, advance };
        let mut overlay = Overlay::default();
        let pen = overlay.text(&font, TEXT.as_bytes(), X, Y, W, H, [1.0, 1.0, 1.0, 1.0]);
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
        overlay.draw(gl)?;
        gl.finish();

        let mut pixels = vec![0u8; (SIZE * SIZE * 4) as usize];
        gl.read_pixels(
            0,
            0,
            SIZE,
            SIZE,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut pixels)),
        );
        target.delete(gl);
        // `--save PATH` writes the frame out for looking at, the same way
        // `--play` does
        if let Some(i) = std::env::args().position(|a| a == "--save") {
            if let Some(path) = std::env::args().nth(i + 1) {
                let _ = super::write_ppm(&path, &pixels, SIZE, SIZE);
            }
        }

        // GL reads back bottom-up; the layer counts from the top
        let (mut left, mut right, mut top, mut bottom, mut lit) = (SIZE, -1, SIZE, -1, 0);
        for row in 0..SIZE {
            for column in 0..SIZE {
                if pixels[((row * SIZE + column) * 4) as usize] > 32 {
                    let y = SIZE - 1 - row;
                    left = left.min(column);
                    right = right.max(column);
                    top = top.min(y);
                    bottom = bottom.max(y);
                    lit += 1;
                }
            }
        }
        if lit < 100 {
            return Err(format!("the font drew {lit} lit pixels, which is nothing"));
        }
        let want_left = (X * SIZE as f32).round() as i32;
        let want_right = (pen * SIZE as f32).round() as i32;
        // the left bearing of `M` is small but not zero, and the last glyph
        // never fills its cell to the advance, so the ink sits *inside* the
        // pen on both sides -- what must not happen is ink outside it
        if left < want_left || left > want_left + 8 {
            return Err(format!("the ink starts at {left}, not at {want_left}"));
        }
        if right > want_right {
            return Err(format!("the ink ends at {right}, past the pen at {want_right}"));
        }
        if right < want_right - 12 {
            return Err(format!("the ink ends at {right}, well short of the pen at {want_right}"));
        }
        Ok(format!(
            "{TEXT:?} drawn offscreen: {lit} lit pixels, ink x {left}..{right} against a pen at \
             {want_right}, y {top}..{bottom}, width {:.4} cells",
            font.width(TEXT.as_bytes())
        ))
    }
}
