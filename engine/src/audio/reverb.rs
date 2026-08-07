//! The 26 EAX 2.0 environments, as EFX reverb settings.
//!
//! A room's `env` in `scripts/levelN.lua` is one of these by number, and that
//! is the whole of what the game says about reverb. `chSndSetEnvironment`
//! (0x451600) is eleven instructions: it clamps its argument and calls
//! `IKsPropertySet::Set` on `DSPROPSETID_EAX20_ListenerProperties` with
//! **property 11**, `DSPROPERTY_EAXLISTENER_ENVIRONMENT`. No parameters of
//! its own are ever sent — the driver's own preset is what a player heard.
//!
//! So reproducing it means reproducing EAX 2.0's preset set. EFX defines the
//! same 26 by the same names, and the numbers below are those, taken from
//! `AL/efx-presets.h` as OpenAL Soft ships it (LGPL, and this project is
//! GPL-3.0-or-later). Only the thirteen fields **standard** `AL_EFFECT_REVERB`
//! has are kept: EAX 2.0 has no echo, no modulation and no reverb panning, so
//! `AL_EFFECT_EAXREVERB`'s extra ten would be inventing settings the game
//! never had.
//!
//! Over the ten levels 17 of the 26 are used. 12 `HALLWAY` is much the
//! commonest at 56 rooms, then 5 `STONEROOM` at 24 and 21 `SEWERPIPE` at 23 —
//! which is a reasonable description of MDK2.

/// One environment, in EFX's units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reverb {
    pub density: f32,
    pub diffusion: f32,
    pub gain: f32,
    pub gain_hf: f32,
    pub decay: f32,
    pub decay_hf: f32,
    pub reflections: f32,
    pub reflections_delay: f32,
    pub late: f32,
    pub late_delay: f32,
    pub air: f32,
    pub rolloff: f32,
    pub hf_limit: bool,
}

/// EAX 2.0's environments, in the order the property numbers them.
pub const EAX: [Reverb; 26] = [
    //  0 GENERIC
    Reverb { density: 1.0000, diffusion: 1.0000, gain: 0.3162, gain_hf: 0.8913,
        decay: 1.4900, decay_hf: 0.8300, reflections: 0.0500, reflections_delay: 0.0070,
        late: 1.2589, late_delay: 0.0110, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    //  1 PADDEDCELL
    Reverb { density: 0.1715, diffusion: 1.0000, gain: 0.3162, gain_hf: 0.0010,
        decay: 0.1700, decay_hf: 0.1000, reflections: 0.2500, reflections_delay: 0.0010,
        late: 1.2691, late_delay: 0.0020, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    //  2 ROOM
    Reverb { density: 0.4287, diffusion: 1.0000, gain: 0.3162, gain_hf: 0.5929,
        decay: 0.4000, decay_hf: 0.8300, reflections: 0.1503, reflections_delay: 0.0020,
        late: 1.0629, late_delay: 0.0030, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    //  3 BATHROOM
    Reverb { density: 0.1715, diffusion: 1.0000, gain: 0.3162, gain_hf: 0.2512,
        decay: 1.4900, decay_hf: 0.5400, reflections: 0.6531, reflections_delay: 0.0070,
        late: 3.2734, late_delay: 0.0110, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    //  4 LIVINGROOM
    Reverb { density: 0.9766, diffusion: 1.0000, gain: 0.3162, gain_hf: 0.0010,
        decay: 0.5000, decay_hf: 0.1000, reflections: 0.2051, reflections_delay: 0.0030,
        late: 0.2805, late_delay: 0.0040, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    //  5 STONEROOM
    Reverb { density: 1.0000, diffusion: 1.0000, gain: 0.3162, gain_hf: 0.7079,
        decay: 2.3100, decay_hf: 0.6400, reflections: 0.4411, reflections_delay: 0.0120,
        late: 1.1003, late_delay: 0.0170, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    //  6 AUDITORIUM
    Reverb { density: 1.0000, diffusion: 1.0000, gain: 0.3162, gain_hf: 0.5781,
        decay: 4.3200, decay_hf: 0.5900, reflections: 0.4032, reflections_delay: 0.0200,
        late: 0.7170, late_delay: 0.0300, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    //  7 CONCERTHALL
    Reverb { density: 1.0000, diffusion: 1.0000, gain: 0.3162, gain_hf: 0.5623,
        decay: 3.9200, decay_hf: 0.7000, reflections: 0.2427, reflections_delay: 0.0200,
        late: 0.9977, late_delay: 0.0290, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    //  8 CAVE
    Reverb { density: 1.0000, diffusion: 1.0000, gain: 0.3162, gain_hf: 1.0000,
        decay: 2.9100, decay_hf: 1.3000, reflections: 0.5000, reflections_delay: 0.0150,
        late: 0.7063, late_delay: 0.0220, air: 0.9943, rolloff: 0.0000, hf_limit: false },
    //  9 ARENA
    Reverb { density: 1.0000, diffusion: 1.0000, gain: 0.3162, gain_hf: 0.4477,
        decay: 7.2400, decay_hf: 0.3300, reflections: 0.2612, reflections_delay: 0.0200,
        late: 1.0186, late_delay: 0.0300, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    // 10 HANGAR
    Reverb { density: 1.0000, diffusion: 1.0000, gain: 0.3162, gain_hf: 0.3162,
        decay: 10.0500, decay_hf: 0.2300, reflections: 0.5000, reflections_delay: 0.0200,
        late: 1.2560, late_delay: 0.0300, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    // 11 CARPETEDHALLWAY
    Reverb { density: 0.4287, diffusion: 1.0000, gain: 0.3162, gain_hf: 0.0100,
        decay: 0.3000, decay_hf: 0.1000, reflections: 0.1215, reflections_delay: 0.0020,
        late: 0.1531, late_delay: 0.0300, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    // 12 HALLWAY
    Reverb { density: 0.3645, diffusion: 1.0000, gain: 0.3162, gain_hf: 0.7079,
        decay: 1.4900, decay_hf: 0.5900, reflections: 0.2458, reflections_delay: 0.0070,
        late: 1.6615, late_delay: 0.0110, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    // 13 STONECORRIDOR
    Reverb { density: 1.0000, diffusion: 1.0000, gain: 0.3162, gain_hf: 0.7612,
        decay: 2.7000, decay_hf: 0.7900, reflections: 0.2472, reflections_delay: 0.0130,
        late: 1.5758, late_delay: 0.0200, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    // 14 ALLEY
    Reverb { density: 1.0000, diffusion: 0.3000, gain: 0.3162, gain_hf: 0.7328,
        decay: 1.4900, decay_hf: 0.8600, reflections: 0.2500, reflections_delay: 0.0070,
        late: 0.9954, late_delay: 0.0110, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    // 15 FOREST
    Reverb { density: 1.0000, diffusion: 0.3000, gain: 0.3162, gain_hf: 0.0224,
        decay: 1.4900, decay_hf: 0.5400, reflections: 0.0525, reflections_delay: 0.1620,
        late: 0.7682, late_delay: 0.0880, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    // 16 CITY
    Reverb { density: 1.0000, diffusion: 0.5000, gain: 0.3162, gain_hf: 0.3981,
        decay: 1.4900, decay_hf: 0.6700, reflections: 0.0730, reflections_delay: 0.0070,
        late: 0.1427, late_delay: 0.0110, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    // 17 MOUNTAINS
    Reverb { density: 1.0000, diffusion: 0.2700, gain: 0.3162, gain_hf: 0.0562,
        decay: 1.4900, decay_hf: 0.2100, reflections: 0.0407, reflections_delay: 0.3000,
        late: 0.1919, late_delay: 0.1000, air: 0.9943, rolloff: 0.0000, hf_limit: false },
    // 18 QUARRY
    Reverb { density: 1.0000, diffusion: 1.0000, gain: 0.3162, gain_hf: 0.3162,
        decay: 1.4900, decay_hf: 0.8300, reflections: 0.0000, reflections_delay: 0.0610,
        late: 1.7783, late_delay: 0.0250, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    // 19 PLAIN
    Reverb { density: 1.0000, diffusion: 0.2100, gain: 0.3162, gain_hf: 0.1000,
        decay: 1.4900, decay_hf: 0.5000, reflections: 0.0585, reflections_delay: 0.1790,
        late: 0.1089, late_delay: 0.1000, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    // 20 PARKINGLOT
    Reverb { density: 1.0000, diffusion: 1.0000, gain: 0.3162, gain_hf: 1.0000,
        decay: 1.6500, decay_hf: 1.5000, reflections: 0.2082, reflections_delay: 0.0080,
        late: 0.2652, late_delay: 0.0120, air: 0.9943, rolloff: 0.0000, hf_limit: false },
    // 21 SEWERPIPE
    Reverb { density: 0.3071, diffusion: 0.8000, gain: 0.3162, gain_hf: 0.3162,
        decay: 2.8100, decay_hf: 0.1400, reflections: 1.6387, reflections_delay: 0.0140,
        late: 3.2471, late_delay: 0.0210, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    // 22 UNDERWATER
    Reverb { density: 0.3645, diffusion: 1.0000, gain: 0.3162, gain_hf: 0.0100,
        decay: 1.4900, decay_hf: 0.1000, reflections: 0.5963, reflections_delay: 0.0070,
        late: 7.0795, late_delay: 0.0110, air: 0.9943, rolloff: 0.0000, hf_limit: true },
    // 23 DRUGGED
    Reverb { density: 0.4287, diffusion: 0.5000, gain: 0.3162, gain_hf: 1.0000,
        decay: 8.3900, decay_hf: 1.3900, reflections: 0.8760, reflections_delay: 0.0020,
        late: 3.1081, late_delay: 0.0300, air: 0.9943, rolloff: 0.0000, hf_limit: false },
    // 24 DIZZY
    Reverb { density: 0.3645, diffusion: 0.6000, gain: 0.3162, gain_hf: 0.6310,
        decay: 17.2300, decay_hf: 0.5600, reflections: 0.1392, reflections_delay: 0.0200,
        late: 0.4937, late_delay: 0.0300, air: 0.9943, rolloff: 0.0000, hf_limit: false },
    // 25 PSYCHOTIC
    Reverb { density: 0.0625, diffusion: 0.5000, gain: 0.3162, gain_hf: 0.8404,
        decay: 7.5600, decay_hf: 0.9100, reflections: 0.4864, reflections_delay: 0.0200,
        late: 2.4378, late_delay: 0.0300, air: 0.9943, rolloff: 0.0000, hf_limit: false },
];

/// The environment a number names, clamped **the way the original clamps it**:
/// anything negative is 25 (`PSYCHOTIC`) and anything past the end is 0
/// (`GENERIC`). Both branches are in `chSndSetEnvironment`, and `-1` meaning
/// psychotic rather than "none" is the sort of thing that is free to copy now
/// and baffling later.
pub fn environment(index: i32) -> &'static Reverb {
    let i = if index < 0 {
        25
    } else if index >= EAX.len() as i32 {
        0
    } else {
        index as usize
    };
    &EAX[i]
}

/// The names, for messages. Not used to look anything up.
pub const NAMES: [&str; 26] = [
    "GENERIC", "PADDEDCELL", "ROOM", "BATHROOM", "LIVINGROOM", "STONEROOM",
    "AUDITORIUM", "CONCERTHALL", "CAVE", "ARENA", "HANGAR", "CARPETEDHALLWAY",
    "HALLWAY", "STONECORRIDOR", "ALLEY", "FOREST", "CITY", "MOUNTAINS",
    "QUARRY", "PLAIN", "PARKINGLOT", "SEWERPIPE", "UNDERWATER", "DRUGGED",
    "DIZZY", "PSYCHOTIC",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The clamp is behaviour, not defensive coding: it is what the original
    /// does with an index the level tables never contain.
    #[test]
    fn the_index_is_clamped_the_way_the_original_clamps_it() {
        assert_eq!(environment(-1), &EAX[25], "negative is PSYCHOTIC");
        assert_eq!(environment(-1000), &EAX[25]);
        assert_eq!(environment(26), &EAX[0], "past the end is GENERIC");
        assert_eq!(environment(12).decay, 1.49, "12 is HALLWAY");
        assert_eq!(NAMES[12], "HALLWAY");
    }

    /// Every preset has to be usable: EFX rejects a decay outside 0.1..20 s
    /// and a gain outside 0..1, and a table typed in by hand gets that wrong.
    #[test]
    fn every_environment_is_within_what_efx_accepts() {
        for (i, r) in EAX.iter().enumerate() {
            let name = NAMES[i];
            assert!((0.0..=1.0).contains(&r.density), "{name} density");
            assert!((0.0..=1.0).contains(&r.diffusion), "{name} diffusion");
            assert!((0.0..=1.0).contains(&r.gain), "{name} gain");
            assert!((0.0..=1.0).contains(&r.gain_hf), "{name} gain_hf");
            assert!((0.1..=20.0).contains(&r.decay), "{name} decay");
            assert!((0.1..=2.0).contains(&r.decay_hf), "{name} decay_hf");
            assert!((0.0..=3.16).contains(&r.reflections), "{name} reflections");
            assert!((0.0..=0.3).contains(&r.reflections_delay), "{name} delay");
            assert!((0.0..=10.0).contains(&r.late), "{name} late");
            assert!((0.0..=0.1).contains(&r.late_delay), "{name} late delay");
            assert!((0.892..=1.0).contains(&r.air), "{name} air");
            assert!((0.0..=10.0).contains(&r.rolloff), "{name} rolloff");
        }
    }
}
