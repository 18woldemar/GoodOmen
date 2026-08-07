//! The renderer: an SDL2 window, an OpenGL 3.3 core context, and shaders.
//!
//! GL 3.3 core is the right level for Linux and Windows — old enough that
//! every driver has it, new enough for the shader-side node posing the
//! models need. It is written in the subset **GLES 3.0 also has**, which
//! costs nothing today and is what would carry macOS or Android if either is
//! ever wanted. In practice that means: `#version 330 core` on the desktop
//! and `#version 300 es` elsewhere, no `gl_FragColor`, no fixed-function
//! anything, and no feature that GLES 3.0 lacks.

use glow::HasContext;

pub mod camera;
pub mod scene;
pub mod triangle;

/// The prefix [`Video::open`] puts on an error that means "there is no
/// display here", as opposed to "the renderer is wrong".
pub const NO_VIDEO: &str = "no video device";

pub struct Video {
    /// Dropped last: the GL context must outlive everything made with it,
    /// and the SDL context must outlive the window.
    pub gl: glow::Context,
    pub window: sdl2::video::Window,
    pub events: sdl2::EventPump,
    _gl_context: sdl2::video::GLContext,
    _video: sdl2::VideoSubsystem,
    /// Public for the mouse: relative mode is asked of the context. **Last**,
    /// because fields drop in declaration order and SDL must outlive the GL
    /// context that was made from it.
    pub sdl: sdl2::Sdl,
}

impl Video {
    /// Open a window with a GL 3.3 core context. `visible` is false for the
    /// self-check, which renders offscreen and should not put a window on
    /// anyone's screen.
    pub fn open(title: &str, width: u32, height: u32, visible: bool) -> Result<Video, String> {
        // A machine with no display at all is not a broken renderer, and the
        // two must not be reported as the same thing: `NO_VIDEO` is what
        // lets `--triangle` skip rather than fail on a headless box.
        let sdl = sdl2::init().map_err(|e| format!("{NO_VIDEO}: {e}"))?;
        let video = sdl.video().map_err(|e| format!("{NO_VIDEO}: {e}"))?;

        let attr = video.gl_attr();
        attr.set_context_profile(sdl2::video::GLProfile::Core);
        attr.set_context_version(3, 3);
        attr.set_double_buffer(true);
        attr.set_depth_size(24);

        let mut builder = video.window(title, width, height);
        builder.opengl();
        if visible {
            builder.position_centered().resizable();
        } else {
            builder.hidden();
        }
        let window = builder.build().map_err(|e| e.to_string())?;

        let gl_context = window.gl_create_context()?;
        window.gl_make_current(&gl_context)?;
        // SAFETY: the loader is SDL's own, and the context was just made
        // current on this thread.
        let gl = unsafe {
            glow::Context::from_loader_function(|name| {
                video.gl_get_proc_address(name) as *const _
            })
        };

        let events = sdl.event_pump()?;
        Ok(Video {
            gl,
            window,
            events,
            _gl_context: gl_context,
            _video: video,
            sdl,
        })
    }

    /// What the driver says it gave us. Asked for rather than assumed: a
    /// request for 3.3 core can be answered with more, and one day the
    /// difference will matter.
    pub fn version(&self) -> String {
        unsafe { self.gl.get_parameter_string(glow::VERSION) }
    }
}

/// Compile and link a program, reporting what the driver said rather than
/// "shader failed".
///
/// # Safety
/// A GL context must be current on this thread.
pub unsafe fn program(
    gl: &glow::Context,
    vertex: &str,
    fragment: &str,
) -> Result<glow::Program, String> {
    let program = gl.create_program()?;
    let mut shaders = Vec::new();
    for (kind, source) in [
        (glow::VERTEX_SHADER, vertex),
        (glow::FRAGMENT_SHADER, fragment),
    ] {
        let shader = gl.create_shader(kind)?;
        gl.shader_source(shader, source);
        gl.compile_shader(shader);
        if !gl.get_shader_compile_status(shader) {
            let log = gl.get_shader_info_log(shader);
            gl.delete_shader(shader);
            gl.delete_program(program);
            return Err(format!("{} shader: {log}", kind_name(kind)));
        }
        gl.attach_shader(program, shader);
        shaders.push(shader);
    }
    gl.link_program(program);
    let linked = gl.get_program_link_status(program);
    let log = gl.get_program_info_log(program);
    for shader in shaders {
        gl.detach_shader(program, shader);
        gl.delete_shader(shader);
    }
    if !linked {
        gl.delete_program(program);
        return Err(format!("link: {log}"));
    }
    Ok(program)
}

fn kind_name(kind: u32) -> &'static str {
    match kind {
        glow::VERTEX_SHADER => "vertex",
        glow::FRAGMENT_SHADER => "fragment",
        _ => "unknown",
    }
}

/// A framebuffer that is not a window: the only way to check a renderer
/// without one, and the only way to read back pixels the compositor has not
/// had its hands on.
pub struct Offscreen {
    pub framebuffer: glow::Framebuffer,
    colour: glow::Renderbuffer,
    depth: glow::Renderbuffer,
    pub width: i32,
    pub height: i32,
}

impl Offscreen {
    /// # Safety
    /// A GL context must be current on this thread.
    pub unsafe fn new(gl: &glow::Context, width: i32, height: i32) -> Result<Offscreen, String> {
        let framebuffer = gl.create_framebuffer()?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));

        let colour = gl.create_renderbuffer()?;
        gl.bind_renderbuffer(glow::RENDERBUFFER, Some(colour));
        gl.renderbuffer_storage(glow::RENDERBUFFER, glow::RGBA8, width, height);
        gl.framebuffer_renderbuffer(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::RENDERBUFFER,
            Some(colour),
        );

        let depth = gl.create_renderbuffer()?;
        gl.bind_renderbuffer(glow::RENDERBUFFER, Some(depth));
        gl.renderbuffer_storage(glow::RENDERBUFFER, glow::DEPTH_COMPONENT24, width, height);
        gl.framebuffer_renderbuffer(
            glow::FRAMEBUFFER,
            glow::DEPTH_ATTACHMENT,
            glow::RENDERBUFFER,
            Some(depth),
        );

        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        if status != glow::FRAMEBUFFER_COMPLETE {
            return Err(format!("framebuffer incomplete: 0x{status:x}"));
        }
        gl.viewport(0, 0, width, height);
        Ok(Offscreen { framebuffer, colour, depth, width, height })
    }

    /// One pixel, RGBA, in window coordinates — origin at the **bottom
    /// left**, as GL counts.
    ///
    /// # Safety
    /// A GL context must be current and this framebuffer bound.
    pub unsafe fn pixel(&self, gl: &glow::Context, x: i32, y: i32) -> [u8; 4] {
        let mut out = [0u8; 4];
        gl.read_pixels(
            x,
            y,
            1,
            1,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut out)),
        );
        out
    }

    /// # Safety
    /// A GL context must be current on this thread.
    pub unsafe fn delete(self, gl: &glow::Context) {
        gl.delete_renderbuffer(self.colour);
        gl.delete_renderbuffer(self.depth);
        gl.delete_framebuffer(self.framebuffer);
    }
}
