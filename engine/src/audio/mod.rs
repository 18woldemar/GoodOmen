//! Audio: OpenAL Soft, opened at run time.
//!
//! The original is DirectSound3D plus EAX 2.0 through `IKsPropertySet` — all
//! seven GUIDs are in `mdk2Main.exe`, found as bytes because the file
//! contains no "EAX" string. What is reproduced here is the **parameters**,
//! not the API: OpenAL's EFX ships the same reverb presets under the same
//! names, and DirectSound3D's attenuation is `AL_INVERSE_DISTANCE_CLAMPED`
//! exactly — below the reference distance there is no attenuation, above it
//! the gain is `reference / distance`, and past the maximum it clamps.
//!
//! **The library is loaded, not linked.** A machine with no OpenAL should run
//! the game silently rather than refuse to start, and the Windows build must
//! not need a DLL beside it. EFX is an extension fetched by name in any case,
//! so every entry point is taken the same way.
//!
//! [`selfcheck`] is the audio half of `render::triangle::selfcheck`: it
//! renders through OpenAL Soft's **loopback device**, so the mix comes back
//! as samples that can be asserted on, with nothing played on anyone's
//! speakers and no sound card required.

pub mod reverb;

use std::ffi::{c_char, c_void, CString};

/// The prefix an error carries when it means "there is no audio here", as
/// opposed to "the audio code is wrong". Matches [`crate::render::NO_VIDEO`].
pub const NO_AUDIO: &str = "no audio device";

type Device = *mut c_void;
type Context = *mut c_void;

// AL/al.h
const AL_SOURCE_RELATIVE: i32 = 0x202;
const AL_POSITION: i32 = 0x1004;
const AL_LOOPING: i32 = 0x1007;
const AL_BUFFER: i32 = 0x1009;
const AL_GAIN: i32 = 0x100A;
const AL_ORIENTATION: i32 = 0x100F;
const AL_SOURCE_STATE: i32 = 0x1010;
const AL_PLAYING: i32 = 0x1012;
const AL_BUFFERS_PROCESSED: i32 = 0x1016;
const AL_REFERENCE_DISTANCE: i32 = 0x1020;
const AL_MAX_DISTANCE: i32 = 0x1023;
const AL_FORMAT_MONO16: i32 = 0x1101;
const AL_FORMAT_STEREO16: i32 = 0x1103;
const AL_NO_ERROR: i32 = 0;
const AL_INVERSE_DISTANCE_CLAMPED: i32 = 0xD002;

// AL/efx.h. The effect functions are an extension and have to come through
// `alGetProcAddress`: OpenAL Soft exports them, but Creative's router does
// not, and the router is what a Windows player is likely to have.
const AL_EFFECT_TYPE: i32 = 0x8001;
const AL_EFFECT_REVERB: i32 = 0x0001;
const AL_EFFECTSLOT_EFFECT: i32 = 0x0001;
const AL_FILTER_NULL: i32 = 0;
const AL_AUXILIARY_SEND_FILTER: i32 = 0x20006;
const AL_REVERB_DENSITY: i32 = 0x0001;
const AL_REVERB_DIFFUSION: i32 = 0x0002;
const AL_REVERB_GAIN: i32 = 0x0003;
const AL_REVERB_GAINHF: i32 = 0x0004;
const AL_REVERB_DECAY_TIME: i32 = 0x0005;
const AL_REVERB_DECAY_HFRATIO: i32 = 0x0006;
const AL_REVERB_REFLECTIONS_GAIN: i32 = 0x0007;
const AL_REVERB_REFLECTIONS_DELAY: i32 = 0x0008;
const AL_REVERB_LATE_REVERB_GAIN: i32 = 0x0009;
const AL_REVERB_LATE_REVERB_DELAY: i32 = 0x000A;
const AL_REVERB_AIR_ABSORPTION_GAINHF: i32 = 0x000B;
const AL_REVERB_ROOM_ROLLOFF_FACTOR: i32 = 0x000C;
const AL_REVERB_DECAY_HFLIMIT: i32 = 0x000D;

// AL/alc.h and AL/alext.h, for the loopback device
const ALC_FREQUENCY: i32 = 0x1007;
const ALC_SHORT_SOFT: i32 = 0x1402;
const ALC_MONO_SOFT: i32 = 0x1500;

/// Every entry point the engine uses. Fetched by name so that a missing one
/// is a message rather than a failure to start.
#[allow(non_snake_case)]
struct Api {
    alcOpenDevice: unsafe extern "C" fn(*const c_char) -> Device,
    alcCloseDevice: unsafe extern "C" fn(Device) -> i8,
    alcCreateContext: unsafe extern "C" fn(Device, *const i32) -> Context,
    alcMakeContextCurrent: unsafe extern "C" fn(Context) -> i8,
    alcDestroyContext: unsafe extern "C" fn(Context),
    alcIsExtensionPresent: unsafe extern "C" fn(Device, *const c_char) -> i8,

    alGetError: unsafe extern "C" fn() -> i32,
    alGenBuffers: unsafe extern "C" fn(i32, *mut u32),
    alDeleteBuffers: unsafe extern "C" fn(i32, *const u32),
    alBufferData: unsafe extern "C" fn(u32, i32, *const c_void, i32, i32),
    alGenSources: unsafe extern "C" fn(i32, *mut u32),
    alDeleteSources: unsafe extern "C" fn(i32, *const u32),
    alSourcei: unsafe extern "C" fn(u32, i32, i32),
    alSourcef: unsafe extern "C" fn(u32, i32, f32),
    alSource3f: unsafe extern "C" fn(u32, i32, f32, f32, f32),
    alGetSourcei: unsafe extern "C" fn(u32, i32, *mut i32),
    alSourcePlay: unsafe extern "C" fn(u32),
    alSourceStop: unsafe extern "C" fn(u32),
    alSource3i: unsafe extern "C" fn(u32, i32, i32, i32, i32),
    alSourceQueueBuffers: unsafe extern "C" fn(u32, i32, *const u32),
    alSourceUnqueueBuffers: unsafe extern "C" fn(u32, i32, *mut u32),
    alListener3f: unsafe extern "C" fn(i32, f32, f32, f32),
    alListenerfv: unsafe extern "C" fn(i32, *const f32),
    alDistanceModel: unsafe extern "C" fn(i32),
    alGetProcAddress: unsafe extern "C" fn(*const c_char) -> *mut c_void,

    /// `ALC_SOFT_loopback`, present only when the extension is. Used by the
    /// self-check and nothing else.
    alcLoopbackOpenDeviceSOFT: Option<unsafe extern "C" fn(*const c_char) -> Device>,
    alcRenderSamplesSOFT: Option<unsafe extern "C" fn(Device, *mut c_void, i32)>,

    /// **Last**, because fields drop in declaration order and every pointer
    /// above points into this.
    _lib: libloading::Library,
}

/// The names OpenAL Soft goes by, in the order worth trying.
const LIBRARIES: &[&str] = &[
    "libopenal.so.1",
    "libopenal.so",
    "OpenAL32.dll",
    "soft_oal.dll",
    "libopenal.1.dylib",
];

macro_rules! entry {
    ($lib:expr, $name:literal) => {
        unsafe {
            *$lib
                .get(concat!($name, "\0").as_bytes())
                .map_err(|e| format!("{NO_AUDIO}: OpenAL has no {}: {e}", $name))?
        }
    };
}

impl Api {
    fn load() -> Result<Api, String> {
        let mut last = String::new();
        for name in LIBRARIES {
            match unsafe { libloading::Library::new(*name) } {
                Ok(lib) => return Api::bind(lib),
                Err(e) => last = format!("{name}: {e}"),
            }
        }
        Err(format!("{NO_AUDIO}: no OpenAL to load ({last})"))
    }

    fn bind(lib: libloading::Library) -> Result<Api, String> {
        // the loopback entry points are absent on an OpenAL that is not
        // OpenAL Soft, which costs the self-check and nothing else
        let loopback = unsafe { lib.get(b"alcLoopbackOpenDeviceSOFT\0").ok().map(|s| *s) };
        let render = unsafe { lib.get(b"alcRenderSamplesSOFT\0").ok().map(|s| *s) };
        Ok(Api {
            alcOpenDevice: entry!(lib, "alcOpenDevice"),
            alcCloseDevice: entry!(lib, "alcCloseDevice"),
            alcCreateContext: entry!(lib, "alcCreateContext"),
            alcMakeContextCurrent: entry!(lib, "alcMakeContextCurrent"),
            alcDestroyContext: entry!(lib, "alcDestroyContext"),
            alcIsExtensionPresent: entry!(lib, "alcIsExtensionPresent"),
            alGetError: entry!(lib, "alGetError"),
            alGenBuffers: entry!(lib, "alGenBuffers"),
            alDeleteBuffers: entry!(lib, "alDeleteBuffers"),
            alBufferData: entry!(lib, "alBufferData"),
            alGenSources: entry!(lib, "alGenSources"),
            alDeleteSources: entry!(lib, "alDeleteSources"),
            alSourcei: entry!(lib, "alSourcei"),
            alSourcef: entry!(lib, "alSourcef"),
            alSource3f: entry!(lib, "alSource3f"),
            alGetSourcei: entry!(lib, "alGetSourcei"),
            alSourcePlay: entry!(lib, "alSourcePlay"),
            alSourceStop: entry!(lib, "alSourceStop"),
            alSource3i: entry!(lib, "alSource3i"),
            alSourceQueueBuffers: entry!(lib, "alSourceQueueBuffers"),
            alSourceUnqueueBuffers: entry!(lib, "alSourceUnqueueBuffers"),
            alListener3f: entry!(lib, "alListener3f"),
            alListenerfv: entry!(lib, "alListenerfv"),
            alDistanceModel: entry!(lib, "alDistanceModel"),
            alGetProcAddress: entry!(lib, "alGetProcAddress"),
            alcLoopbackOpenDeviceSOFT: loopback,
            alcRenderSamplesSOFT: render,
            _lib: lib,
        })
    }
}

/// The four EFX entry points a single listener reverb needs, and the effect
/// and slot they made. Absent on a device without `ALC_EXT_EFX`, which is a
/// game without reverb and not a broken one.
#[allow(non_snake_case)]
struct Efx {
    alEffecti: unsafe extern "C" fn(u32, i32, i32),
    alEffectf: unsafe extern "C" fn(u32, i32, f32),
    alAuxiliaryEffectSloti: unsafe extern "C" fn(u32, i32, i32),
    alDeleteEffects: unsafe extern "C" fn(i32, *const u32),
    alDeleteAuxiliaryEffectSlots: unsafe extern "C" fn(i32, *const u32),
    effect: u32,
    slot: u32,
}

/// An open device with a current context.
pub struct Audio {
    api: Api,
    device: Device,
    context: Context,
    buffers: Vec<u32>,
    sources: Vec<u32>,
    /// Set for a loopback device; the mix has to be pulled by hand.
    loopback: Option<i32>,
    efx: Option<Efx>,
}

/// A decoded sound, uploaded once and shared by every source that plays it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sound(u32);

/// A voice: one sound, one place in the world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Voice(u32);

impl Audio {
    /// Open the system's default device.
    pub fn open() -> Result<Audio, String> {
        let api = Api::load()?;
        let device = unsafe { (api.alcOpenDevice)(std::ptr::null()) };
        if device.is_null() {
            return Err(format!("{NO_AUDIO}: no default device"));
        }
        Audio::start(api, device, None)
    }

    /// Open OpenAL Soft's loopback device: nothing is played, and the mix is
    /// pulled out as samples by [`Audio::render`]. Mono 16-bit, so the
    /// numbers a check asserts on are the gain and nothing else — no
    /// panning, no HRTF.
    pub fn loopback(rate: i32) -> Result<Audio, String> {
        let api = Api::load()?;
        let open = api
            .alcLoopbackOpenDeviceSOFT
            .ok_or_else(|| format!("{NO_AUDIO}: no ALC_SOFT_loopback"))?;
        let device = unsafe { open(std::ptr::null()) };
        if device.is_null() {
            return Err(format!("{NO_AUDIO}: the loopback device did not open"));
        }
        Audio::start(api, device, Some(rate))
    }

    fn start(api: Api, device: Device, loopback: Option<i32>) -> Result<Audio, String> {
        let attributes: Vec<i32> = match loopback {
            // ALC_SOFT_loopback wants the format stated up front; the two
            // enum names are the extension's own
            Some(rate) => vec![
                ALC_FREQUENCY,
                rate,
                0x1990, // ALC_FORMAT_CHANNELS_SOFT
                ALC_MONO_SOFT,
                0x1991, // ALC_FORMAT_TYPE_SOFT
                ALC_SHORT_SOFT,
                0,
            ],
            None => vec![0],
        };
        let context = unsafe { (api.alcCreateContext)(device, attributes.as_ptr()) };
        if context.is_null() {
            unsafe { (api.alcCloseDevice)(device) };
            return Err(format!("{NO_AUDIO}: no context"));
        }
        if unsafe { (api.alcMakeContextCurrent)(context) } == 0 {
            unsafe {
                (api.alcDestroyContext)(context);
                (api.alcCloseDevice)(device);
            }
            return Err(format!("{NO_AUDIO}: the context would not go current"));
        }
        // DirectSound3D's model, which is what the payload's near and far
        // distances mean
        unsafe { (api.alDistanceModel)(AL_INVERSE_DISTANCE_CLAMPED) };
        let efx = Efx::open(&api);
        Ok(Audio {
            api,
            device,
            context,
            buffers: Vec::new(),
            sources: Vec::new(),
            loopback,
            efx,
        })
    }

    /// Set the one listener reverb, the way `chSndSetEnvironment` does: a
    /// room's `env` and nothing else. False when the device has no EFX.
    ///
    /// **A single slot for the whole listener**, not one per room — EAX 2.0
    /// has exactly one listener environment, and the original swaps it on
    /// room entry rather than blending between them.
    pub fn environment(&self, index: i32) -> bool {
        let Some(e) = &self.efx else { return false };
        let r = reverb::environment(index);
        unsafe {
            (e.alEffectf)(e.effect, AL_REVERB_DENSITY, r.density);
            (e.alEffectf)(e.effect, AL_REVERB_DIFFUSION, r.diffusion);
            (e.alEffectf)(e.effect, AL_REVERB_GAIN, r.gain);
            (e.alEffectf)(e.effect, AL_REVERB_GAINHF, r.gain_hf);
            (e.alEffectf)(e.effect, AL_REVERB_DECAY_TIME, r.decay);
            (e.alEffectf)(e.effect, AL_REVERB_DECAY_HFRATIO, r.decay_hf);
            (e.alEffectf)(e.effect, AL_REVERB_REFLECTIONS_GAIN, r.reflections);
            (e.alEffectf)(e.effect, AL_REVERB_REFLECTIONS_DELAY, r.reflections_delay);
            (e.alEffectf)(e.effect, AL_REVERB_LATE_REVERB_GAIN, r.late);
            (e.alEffectf)(e.effect, AL_REVERB_LATE_REVERB_DELAY, r.late_delay);
            (e.alEffectf)(e.effect, AL_REVERB_AIR_ABSORPTION_GAINHF, r.air);
            (e.alEffectf)(e.effect, AL_REVERB_ROOM_ROLLOFF_FACTOR, r.rolloff);
            (e.alEffecti)(e.effect, AL_REVERB_DECAY_HFLIMIT, r.hf_limit as i32);
            // the slot only picks the effect up when it is handed it again
            (e.alAuxiliaryEffectSloti)(e.slot, AL_EFFECTSLOT_EFFECT, e.effect as i32);
        }
        true
    }

    /// True when the device carries an extension, by its ALC name.
    pub fn has(&self, extension: &str) -> bool {
        let name = CString::new(extension).unwrap_or_default();
        unsafe { (self.api.alcIsExtensionPresent)(self.device, name.as_ptr()) != 0 }
    }

    fn error(&self, what: &str) -> Result<(), String> {
        match unsafe { (self.api.alGetError)() } {
            AL_NO_ERROR => Ok(()),
            e => Err(format!("OpenAL error {e:#x} on {what}")),
        }
    }

    /// Upload 16-bit PCM — what `formats::acm::decode` returns.
    pub fn sound(&mut self, pcm: &[i16], channels: u16, rate: u32) -> Result<Sound, String> {
        let mut id = 0u32;
        unsafe { (self.api.alGenBuffers)(1, &mut id) };
        self.error("alGenBuffers")?;
        let format = if channels >= 2 { AL_FORMAT_STEREO16 } else { AL_FORMAT_MONO16 };
        unsafe {
            (self.api.alBufferData)(
                id,
                format,
                pcm.as_ptr() as *const c_void,
                std::mem::size_of_val(pcm) as i32,
                rate as i32,
            )
        };
        self.error("alBufferData")?;
        self.buffers.push(id);
        Ok(Sound(id))
    }

    /// A voice for a sound, silent and stopped until it is placed and played.
    pub fn voice(&mut self, sound: Sound) -> Result<Voice, String> {
        let mut id = 0u32;
        unsafe { (self.api.alGenSources)(1, &mut id) };
        self.error("alGenSources")?;
        unsafe { (self.api.alSourcei)(id, AL_BUFFER, sound.0 as i32) };
        self.error("alSourcei(AL_BUFFER)")?;
        // every voice feeds the one listener reverb, since that is what an
        // EAX 2.0 environment is
        if let Some(e) = &self.efx {
            unsafe {
                (self.api.alSource3i)(
                    id,
                    AL_AUXILIARY_SEND_FILTER,
                    e.slot as i32,
                    0,
                    AL_FILTER_NULL,
                )
            };
            self.error("alSource3i(AL_AUXILIARY_SEND_FILTER)")?;
        }
        self.sources.push(id);
        Ok(Voice(id))
    }

    /// Place a voice, in the units the game uses.
    ///
    /// `near` and `far` are the first two numbers of an `OBJ_AMBIENTSOUND`
    /// payload: no attenuation closer than `near`, `near / d` beyond it, and
    /// clamped past `far`.
    pub fn place(&self, v: Voice, at: [f32; 3], near: f32, far: f32) {
        unsafe {
            (self.api.alSource3f)(v.0, AL_POSITION, at[0], at[1], at[2]);
            (self.api.alSourcef)(v.0, AL_REFERENCE_DISTANCE, near.max(0.01));
            (self.api.alSourcef)(v.0, AL_MAX_DISTANCE, far.max(near).max(0.01));
        }
    }

    pub fn gain(&self, v: Voice, gain: f32) {
        unsafe { (self.api.alSourcef)(v.0, AL_GAIN, gain.max(0.0)) };
    }

    pub fn looping(&self, v: Voice, on: bool) {
        unsafe { (self.api.alSourcei)(v.0, AL_LOOPING, on as i32) };
    }

    /// Follow the listener instead of standing in the world. Music and the
    /// interface are the two that should.
    pub fn head_relative(&self, v: Voice, on: bool) {
        unsafe { (self.api.alSourcei)(v.0, AL_SOURCE_RELATIVE, on as i32) };
    }

    pub fn play(&self, v: Voice) {
        unsafe { (self.api.alSourcePlay)(v.0) };
    }

    pub fn stop(&self, v: Voice) {
        unsafe { (self.api.alSourceStop)(v.0) };
    }

    pub fn playing(&self, v: Voice) -> bool {
        let mut state = 0i32;
        unsafe { (self.api.alGetSourcei)(v.0, AL_SOURCE_STATE, &mut state) };
        state == AL_PLAYING
    }

    /// Where the ears are, and which way they face. `forward` and `up` are
    /// the camera's, so the mix turns with the view.
    pub fn listener(&self, at: [f32; 3], forward: [f32; 3], up: [f32; 3]) {
        let orientation = [forward[0], forward[1], forward[2], up[0], up[1], up[2]];
        unsafe {
            (self.api.alListener3f)(AL_POSITION, at[0], at[1], at[2]);
            (self.api.alListenerfv)(AL_ORIENTATION, orientation.as_ptr());
        }
    }

    /// Pull `samples` frames of the mix out of a loopback device. Errors on
    /// a real one, where the samples go to the speakers instead.
    pub fn render(&self, samples: usize) -> Result<Vec<i16>, String> {
        if self.loopback.is_none() {
            return Err("not a loopback device".into());
        }
        let render = self
            .api
            .alcRenderSamplesSOFT
            .ok_or_else(|| format!("{NO_AUDIO}: no alcRenderSamplesSOFT"))?;
        let mut out = vec![0i16; samples];
        unsafe { render(self.device, out.as_mut_ptr() as *mut c_void, samples as i32) };
        Ok(out)
    }
}

impl Efx {
    /// Fetch the extension and make the one effect and slot, or `None`.
    ///
    /// A context has to be current before this: `alGetProcAddress` answers
    /// out of the current device's extension list.
    fn open(api: &Api) -> Option<Efx> {
        let address = |name: &str| -> Option<*mut c_void> {
            let c = CString::new(name).ok()?;
            let p = unsafe { (api.alGetProcAddress)(c.as_ptr()) };
            (!p.is_null()).then_some(p)
        };
        // SAFETY: each name is fetched with the signature EFX gives it.
        unsafe {
            let gen_effects: unsafe extern "C" fn(i32, *mut u32) =
                std::mem::transmute(address("alGenEffects")?);
            let gen_slots: unsafe extern "C" fn(i32, *mut u32) =
                std::mem::transmute(address("alGenAuxiliaryEffectSlots")?);
            let efx = Efx {
                alEffecti: std::mem::transmute(address("alEffecti")?),
                alEffectf: std::mem::transmute(address("alEffectf")?),
                alAuxiliaryEffectSloti: std::mem::transmute(
                    address("alAuxiliaryEffectSloti")?,
                ),
                alDeleteEffects: std::mem::transmute(address("alDeleteEffects")?),
                alDeleteAuxiliaryEffectSlots: std::mem::transmute(
                    address("alDeleteAuxiliaryEffectSlots")?,
                ),
                effect: {
                    let mut id = 0u32;
                    gen_effects(1, &mut id);
                    id
                },
                slot: {
                    let mut id = 0u32;
                    gen_slots(1, &mut id);
                    id
                },
            };
            if efx.effect == 0 || efx.slot == 0 {
                return None;
            }
            // EAX 2.0's listener properties are the **standard** reverb's
            // thirteen; EAXREVERB's extra ten are EAX 3.0 and later, which
            // this game never had
            (efx.alEffecti)(efx.effect, AL_EFFECT_TYPE, AL_EFFECT_REVERB);
            if (api.alGetError)() != AL_NO_ERROR {
                return None;
            }
            Some(efx)
        }
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        unsafe {
            if let Some(e) = &self.efx {
                (e.alAuxiliaryEffectSloti)(e.slot, AL_EFFECTSLOT_EFFECT, 0);
                (e.alDeleteAuxiliaryEffectSlots)(1, &e.slot);
                (e.alDeleteEffects)(1, &e.effect);
            }
            for &s in &self.sources {
                (self.api.alSourceStop)(s);
            }
            if !self.sources.is_empty() {
                (self.api.alDeleteSources)(self.sources.len() as i32, self.sources.as_ptr());
            }
            if !self.buffers.is_empty() {
                (self.api.alDeleteBuffers)(self.buffers.len() as i32, self.buffers.as_ptr());
            }
            (self.api.alcMakeContextCurrent)(std::ptr::null_mut());
            (self.api.alcDestroyContext)(self.context);
            (self.api.alcCloseDevice)(self.device);
        }
    }
}

/// A music track, played out of its compressed bytes rather than decoded
/// whole: 25 MiB of PCM a track, and 27 of them.
///
/// **It loops, because every one of the 27 `.mus` playlists says so** — one
/// segment, looping back to itself. Looping a queue means rewinding the
/// decoder, not setting `AL_LOOPING`, which only repeats a single buffer.
pub struct Track {
    voice: Voice,
    /// Every buffer made for this track. Some are queued at any moment.
    buffers: Vec<u32>,
    free: Vec<u32>,
    stream: crate::formats::acm::Stream,
    format: i32,
    rate: i32,
    /// Reused between fills so a track costs one allocation, not one a
    /// second.
    scratch: Vec<i16>,
}

/// How much audio is kept ahead of the mixer. Four of these is about a
/// second and a half, which survives a frame that takes far too long.
const TRACK_BUFFERS: usize = 4;
/// Samples per buffer, across all channels. A stereo 22050 Hz block is 2048
/// values, so this is eight blocks — about a third of a second.
const TRACK_SAMPLES: usize = 16384;

impl Audio {
    /// Start a track. The bytes are the bare ACM stream `Music/` holds.
    pub fn music(&mut self, acm: Vec<u8>) -> Result<Track, String> {
        let stream =
            crate::formats::acm::Stream::open(acm).map_err(|e| e.to_string())?;
        let format = if stream.header.channels >= 2 {
            AL_FORMAT_STEREO16
        } else {
            AL_FORMAT_MONO16
        };
        let rate = stream.header.rate as i32;
        let mut buffers = vec![0u32; TRACK_BUFFERS];
        unsafe { (self.api.alGenBuffers)(TRACK_BUFFERS as i32, buffers.as_mut_ptr()) };
        self.error("alGenBuffers for music")?;
        self.buffers.extend_from_slice(&buffers);

        let mut id = 0u32;
        unsafe { (self.api.alGenSources)(1, &mut id) };
        self.error("alGenSources for music")?;
        self.sources.push(id);
        // music is not in the world: it does not move with the listener and
        // it is not fed through the room's reverb
        unsafe {
            (self.api.alSourcei)(id, AL_SOURCE_RELATIVE, 1);
            (self.api.alSource3f)(id, AL_POSITION, 0.0, 0.0, 0.0);
        }

        let mut track = Track {
            voice: Voice(id),
            free: buffers.clone(),
            buffers,
            stream,
            format,
            rate,
            scratch: Vec::with_capacity(TRACK_SAMPLES + 4096),
        };
        self.fill(&mut track)?;
        self.play(track.voice);
        Ok(track)
    }

    /// Hand the mixer whatever it has finished with, refilled. Call it once a
    /// frame; it does nothing when nothing has been consumed.
    pub fn pump(&mut self, track: &mut Track) -> Result<(), String> {
        let mut processed = 0i32;
        unsafe {
            (self.api.alGetSourcei)(track.voice.0, AL_BUFFERS_PROCESSED, &mut processed)
        };
        if processed > 0 {
            let mut done = vec![0u32; processed as usize];
            unsafe {
                (self.api.alSourceUnqueueBuffers)(track.voice.0, processed, done.as_mut_ptr())
            };
            self.error("alSourceUnqueueBuffers")?;
            track.free.extend(done);
        }
        self.fill(track)?;
        // a frame that took far too long can drain the queue and stop the
        // source; starting it again is the whole of the recovery
        if !self.playing(track.voice) {
            self.play(track.voice);
        }
        Ok(())
    }

    /// Fill and queue every free buffer.
    fn fill(&mut self, track: &mut Track) -> Result<(), String> {
        while let Some(id) = track.free.pop() {
            track.scratch.clear();
            while track.scratch.len() < TRACK_SAMPLES {
                if !track.stream.block(&mut track.scratch) {
                    // the end, and every track loops back to itself
                    track.stream.rewind();
                    if !track.stream.block(&mut track.scratch) {
                        break;
                    }
                }
            }
            if track.scratch.is_empty() {
                track.free.push(id);
                break;
            }
            unsafe {
                (self.api.alBufferData)(
                    id,
                    track.format,
                    track.scratch.as_ptr() as *const c_void,
                    std::mem::size_of_val(track.scratch.as_slice()) as i32,
                    track.rate,
                );
                (self.api.alSourceQueueBuffers)(track.voice.0, 1, &id);
            }
            self.error("queueing music")?;
        }
        Ok(())
    }

    pub fn stop_music(&self, track: &Track) {
        self.stop(track.voice);
    }

    pub fn music_gain(&self, track: &Track, gain: f32) {
        self.gain(track.voice, gain);
    }

    /// How many buffers this track owns, for a check to assert on.
    pub fn queued(&self, track: &Track) -> usize {
        track.buffers.len() - track.free.len()
    }
}

/// 16-bit PCM out of whatever the game calls a sound file, with the channel
/// count and the rate.
///
/// Three things go by `.wav` here and only one of them is a `.wav`: 992 are
/// [`crate::formats::wavc`] wrappers over Interplay ACM, six really are RIFF,
/// and `Music/` is bare ACM with no wrapper at all. This is the one place
/// that has to know which is which.
pub fn pcm(data: &[u8]) -> Result<(Vec<i16>, u16, u32), String> {
    use crate::formats::{acm, wavc};
    if data.starts_with(b"RIFF") {
        return riff(data);
    }
    // the wrapper repeats the rate, but the ACM header carries its own and
    // that is the one the samples were made at
    let stream = match wavc::parse(data) {
        Ok(s) => s.acm,
        Err(_) => data,
    };
    let head = acm::header(stream).map_err(|e| e.to_string())?;
    let samples = acm::decode(stream).map_err(|e| e.to_string())?;
    Ok((samples, head.channels.max(1), head.rate as u32))
}

/// A plain RIFF WAVE: walk the chunks rather than assume `fmt ` and `data`
/// sit where they do in these six files.
fn riff(data: &[u8]) -> Result<(Vec<i16>, u16, u32), String> {
    let word = |at: usize| -> u32 {
        data.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .unwrap_or(0)
    };
    let half = |at: usize| -> u16 {
        data.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0)
    };
    if data.len() < 12 || &data[8..12] != b"WAVE" {
        return Err("not a RIFF WAVE".into());
    }
    let (mut channels, mut rate, mut bits) = (0u16, 0u32, 0u16);
    let mut body: Option<&[u8]> = None;
    let mut at = 12usize;
    while at + 8 <= data.len() {
        let size = word(at + 4) as usize;
        let start = at + 8;
        match &data[at..at + 4] {
            b"fmt " => {
                channels = half(start + 2);
                rate = word(start + 4);
                bits = half(start + 14);
            }
            b"data" => body = data.get(start..(start + size).min(data.len())),
            _ => {}
        }
        at = start + size + (size & 1);
    }
    let body = body.ok_or("no data chunk")?;
    if bits != 16 {
        return Err(format!("{bits}-bit RIFF, and the game ships only 16"));
    }
    let samples = body.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
    Ok((samples, channels.max(1), rate.max(1)))
}

/// One `OBJ_AMBIENTSOUND`: a sound standing in the world.
///
/// The payload is (near distance, far distance, unexplained, volume) — near
/// is below far in all 80 of them, and slot 2 takes only 0.0, 0.1, 0.2 or
/// 0.3 and is still unaccounted for.
#[derive(Clone, Debug)]
pub struct Ambient {
    /// The `.wav` the object's `resource` slot names.
    pub sound: String,
    pub at: [f32; 3],
    pub near: f32,
    pub far: f32,
    pub gain: f32,
}

impl Ambient {
    pub fn from_payload(sound: &str, position: [f64; 3], payload: [f64; 4]) -> Ambient {
        // the scene graph names resources without an extension -- `amb_alien2`,
        // the way it writes `kurt` for a model -- and the type says which one
        let mut sound = sound.to_ascii_lowercase();
        if !sound.contains('.') {
            sound.push_str(".wav");
        }
        Ambient {
            sound,
            at: [position[0] as f32, position[1] as f32, position[2] as f32],
            near: payload[0].max(0.01) as f32,
            far: payload[1].max(payload[0]).max(0.01) as f32,
            gain: payload[3].clamp(0.0, 1.0) as f32,
        }
    }
}

/// How many of these sounds can be read and decoded — the whole of
/// [`Ambience::open`] except for the device, so that it can be checked on a
/// machine with no sound card and without playing anything at anyone.
pub fn decodable(sounds: &[Ambient], read: &mut dyn FnMut(&str) -> Option<Vec<u8>>) -> usize {
    let mut known: std::collections::HashMap<String, bool> = Default::default();
    sounds
        .iter()
        .filter(|a| {
            *known.entry(a.sound.clone()).or_insert_with(|| {
                read(&a.sound).is_some_and(|bytes| pcm(&bytes).is_ok())
            })
        })
        .count()
}

/// The ambient sounds of a level, playing.
pub struct Ambience {
    pub audio: Audio,
    pub voices: Vec<Voice>,
    /// Named a sound that could not be read or decoded.
    pub silent: usize,
}

impl Ambience {
    /// Decode every distinct sound once, then give each object a looping
    /// voice at its own place. `read` fetches a resource by name.
    pub fn open(
        mut audio: Audio,
        sounds: &[Ambient],
        read: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
    ) -> Result<Ambience, String> {
        let mut uploaded: std::collections::HashMap<String, Sound> = Default::default();
        let mut voices = Vec::new();
        let mut silent = 0usize;
        for a in sounds {
            let sound = match uploaded.get(&a.sound) {
                Some(&s) => s,
                None => {
                    let Some(bytes) = read(&a.sound) else {
                        silent += 1;
                        continue;
                    };
                    let Ok((samples, channels, rate)) = pcm(&bytes) else {
                        silent += 1;
                        continue;
                    };
                    // a stereo buffer is not positioned by OpenAL at all, and
                    // every ambient sound the game places is mono
                    let s = audio.sound(&samples, channels, rate)?;
                    uploaded.insert(a.sound.clone(), s);
                    s
                }
            };
            let v = audio.voice(sound)?;
            audio.place(v, a.at, a.near, a.far);
            audio.gain(v, a.gain);
            audio.looping(v, true);
            audio.play(v);
            voices.push(v);
        }
        Ok(Ambience { audio, voices, silent })
    }

    /// Where the ears are. `yaw` and `pitch` are the camera's, in the same
    /// frame the game uses: x forward at yaw 0, **z up**.
    pub fn listen(&self, at: [f32; 3], yaw: f64, pitch: f64) {
        let (cy, sy) = (yaw.cos() as f32, yaw.sin() as f32);
        let (cp, sp) = (pitch.cos() as f32, pitch.sin() as f32);
        self.audio
            .listener(at, [cy * cp, sy * cp, sp], [0.0, 0.0, 1.0]);
    }
}

/// The gain OpenAL's clamped inverse model gives at a distance — the same
/// arithmetic DirectSound3D does, written out so a check can predict it.
pub fn attenuation(distance: f32, near: f32, far: f32) -> f32 {
    near / distance.clamp(near, far.max(near))
}

/// Render a sound at four distances through the loopback device and check
/// that what comes back falls off the way the model says.
///
/// This is the audio counterpart of drawing the first triangle offscreen: it
/// answers "is anything actually being mixed, and by the right law" with no
/// speakers, no sound card and nothing audible.
pub fn selfcheck() -> Result<String, String> {
    const RATE: u32 = 44100;
    const AMPLITUDE: i16 = 12000;
    // a quarter second of steady tone: long enough that the fade OpenAL puts
    // on the first samples of a source is well behind the window measured
    let tone: Vec<i16> = (0..RATE as usize / 4)
        .map(|i| if (i / 50) % 2 == 0 { AMPLITUDE } else { -AMPLITUDE })
        .collect();

    let mut audio = Audio::loopback(RATE as i32)?;
    let sound = audio.sound(&tone, 1, RATE)?;
    audio.listener([0.0; 3], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]);

    let (near, far) = (10.0f32, 40.0f32);
    let mut measured = Vec::new();
    for &d in &[5.0f32, 10.0, 20.0, 80.0] {
        let v = audio.voice(sound)?;
        // straight ahead, so the only thing between the buffer and the mix
        // is the distance model
        audio.place(v, [0.0, 0.0, -d], near, far);
        audio.play(v);
        // skip the first block, then measure the next
        audio.render(512)?;
        let block = audio.render(2048)?;
        audio.stop(v);
        let peak = block.iter().map(|s| s.unsigned_abs() as f32).fold(0.0, f32::max);
        measured.push((d, peak / AMPLITUDE as f32));
    }

    let mut worst = 0.0f32;
    for &(d, got) in &measured {
        let want = attenuation(d, near, far);
        worst = worst.max((got - want).abs());
    }
    if worst > 0.02 {
        return Err(format!(
            "the distance model is off by {worst:.3}: {}",
            measured
                .iter()
                .map(|&(d, g)| format!("{d:.0}u {g:.3} against {:.3}", attenuation(d, near, far)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // and the reverb, by the one thing a reverb does: sound after the sound
    // has stopped. A short preset and a long one on the same tone must leave
    // very different tails, which checks the effect slot, the send, and that
    // the preset table's decay times actually arrive.
    let mut tails = Vec::new();
    for &env in &[1usize, 9] {
        if !audio.environment(env as i32) {
            tails.clear();
            break;
        }
        let v = audio.voice(sound)?;
        // at the reference distance, so nothing but the reverb is in play
        audio.place(v, [0.0, 0.0, -near], near, far);
        audio.play(v);
        // Past the end of the quarter-second tone, then a further quarter
        // second, and only then listen. Measuring right after the tone stops
        // would find both presets loud: PADDEDCELL decays in 0.17 s, so the
        // window has to start after that or the two look alike.
        audio.render(RATE as usize / 2)?;
        let tail = audio.render(RATE as usize / 4)?;
        audio.stop(v);
        let peak = tail.iter().map(|s| s.unsigned_abs() as f32).fold(0.0, f32::max);
        tails.push((reverb::NAMES[env], peak / AMPLITUDE as f32));
    }
    let reverberation = match tails.as_slice() {
        [] => ", no EFX".to_string(),
        [(short, quiet), (long, loud)] => {
            // ARENA decays for 7.24 s and PADDEDCELL for 0.17; a third of a
            // second after the sound stops they cannot be alike
            if *loud < 0.01 || *loud < quiet * 3.0 {
                return Err(format!(
                    "the reverb tails do not differ: {short} {quiet:.4}, {long} {loud:.4}"
                ));
            }
            format!(", tail {quiet:.3} {short} against {loud:.3} {long}")
        }
        _ => ", the reverb answered once".to_string(),
    };

    Ok(format!(
        "OpenAL loopback: {} rendered at {RATE} Hz, gain {} — clamped inverse to {worst:.3}{}",
        measured.len(),
        measured
            .iter()
            .map(|&(d, g)| format!("{g:.2}@{d:.0}u"))
            .collect::<Vec<_>>()
            .join(" "),
        reverberation
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The model, without a device: flat inside `near`, `near/d` between,
    /// and flat again past `far`.
    #[test]
    fn the_distance_model_is_inverse_and_clamped_at_both_ends() {
        assert_eq!(attenuation(1.0, 10.0, 40.0), 1.0);
        assert_eq!(attenuation(10.0, 10.0, 40.0), 1.0);
        assert_eq!(attenuation(20.0, 10.0, 40.0), 0.5);
        assert_eq!(attenuation(40.0, 10.0, 40.0), 0.25);
        assert_eq!(attenuation(400.0, 10.0, 40.0), 0.25);
        // far below near is not a silent divide by zero
        assert_eq!(attenuation(5.0, 10.0, 1.0), 1.0);
    }
}
