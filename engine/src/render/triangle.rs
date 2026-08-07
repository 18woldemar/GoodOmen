//! The first triangle: the smallest thing that proves the whole path works —
//! window, context, shader compiler, buffer, draw call, and pixels coming
//! back out the other end.
//!
//! It is kept because it is also the check. `goodomen --triangle` renders it
//! into an offscreen framebuffer and asserts on three pixels, so the
//! renderer can be verified with no window on anyone's screen and no display
//! server that happens to be running.

use super::{program, Offscreen, Video};
use glow::HasContext;

/// `#version 330 core` on the desktop; the body is the GLES 3.0 subset.
const VERTEX: &str = r#"#version 330 core
layout (location = 0) in vec2 position;
layout (location = 1) in vec3 colour;
out vec3 vary_colour;
void main() {
    vary_colour = colour;
    gl_Position = vec4(position, 0.0, 1.0);
}
"#;

const FRAGMENT: &str = r#"#version 330 core
in vec3 vary_colour;
out vec4 fragment;
void main() {
    fragment = vec4(vary_colour, 1.0);
}
"#;

/// Clockwise from the top, in clip space, each corner a primary — so a wrong
/// attribute stride shows as a wrong colour rather than as nothing at all.
#[rustfmt::skip]
const VERTICES: [f32; 15] = [
     0.0,  0.8,   1.0, 0.0, 0.0,
    -0.8, -0.8,   0.0, 1.0, 0.0,
     0.8, -0.8,   0.0, 0.0, 1.0,
];

pub const CLEAR: [f32; 4] = [0.05, 0.06, 0.09, 1.0];

/// Draw the triangle into whatever framebuffer is bound.
///
/// # Safety
/// A GL context must be current on this thread.
pub unsafe fn draw(gl: &glow::Context) -> Result<(), String> {
    let shader = program(gl, VERTEX, FRAGMENT)?;

    // GL 3.3 core has no default vertex array object; without one every draw
    // call is an INVALID_OPERATION and nothing at all appears.
    let vao = gl.create_vertex_array()?;
    gl.bind_vertex_array(Some(vao));

    let vbo = gl.create_buffer()?;
    gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
    gl.buffer_data_u8_slice(
        glow::ARRAY_BUFFER,
        std::slice::from_raw_parts(
            VERTICES.as_ptr() as *const u8,
            std::mem::size_of_val(&VERTICES),
        ),
        glow::STATIC_DRAW,
    );

    let stride = 5 * std::mem::size_of::<f32>() as i32;
    gl.enable_vertex_attrib_array(0);
    gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, stride, 0);
    gl.enable_vertex_attrib_array(1);
    gl.vertex_attrib_pointer_f32(1, 3, glow::FLOAT, false, stride, 8);

    gl.clear_color(CLEAR[0], CLEAR[1], CLEAR[2], CLEAR[3]);
    gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
    gl.use_program(Some(shader));
    gl.draw_arrays(glow::TRIANGLES, 0, 3);

    gl.delete_buffer(vbo);
    gl.delete_vertex_array(vao);
    gl.delete_program(shader);
    Ok(())
}

/// Render the triangle offscreen and check the pixels that came back.
///
/// Three of them, and each one fails for a different reason: the centre says
/// the draw call happened at all, the bottom-left corner says the clear
/// happened and the triangle did not cover everything, and the bottom-left
/// *vertex* says the colour attribute arrived — a wrong stride puts red
/// there instead of green.
pub fn selfcheck() -> Result<String, String> {
    let video = Video::open("goodomen", 256, 256, false)?;
    let gl = &video.gl;
    let version = video.version();

    // SAFETY: the context Video::open made is current on this thread, and
    // every object below is created and deleted inside this block.
    unsafe {
        let target = Offscreen::new(gl, 256, 256)?;
        draw(gl)?;
        gl.finish();

        let centre = target.pixel(gl, 128, 128);
        let corner = target.pixel(gl, 4, 4);
        let green = target.pixel(gl, 40, 40);
        target.delete(gl);

        let clear = [
            (CLEAR[0] * 255.0).round() as u8,
            (CLEAR[1] * 255.0).round() as u8,
            (CLEAR[2] * 255.0).round() as u8,
        ];
        // an 8-bit renderbuffer and a float clear colour will not round
        // identically on every driver, so allow a level either way
        let near = |got: [u8; 4], want: [u8; 3]| {
            (0..3).all(|c| (got[c] as i32 - want[c] as i32).abs() <= 1)
        };
        if near(centre, clear) {
            return Err(format!("nothing was drawn: the centre pixel is {centre:?}"));
        }
        if !near(corner, clear) {
            return Err(format!("the clear did not happen: the corner is {corner:?}"));
        }
        if green[1] <= green[0] || green[1] <= green[2] {
            return Err(format!(
                "the colour attribute did not arrive: the green vertex is {green:?}"
            ));
        }
        Ok(format!(
            "OpenGL {version}: triangle drawn offscreen, centre {centre:?}, \
             clear {corner:?}, green vertex {green:?}"
        ))
    }
}
