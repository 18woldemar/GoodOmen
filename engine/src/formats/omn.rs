//! `.omn` — the one recorded demo the game ships.
//!
//! A flat list of `{u32 command; f32 value}`. Command `0xFFFFFFFF` ends a
//! frame and **its value is that frame's delta time**; everything before it
//! is the input held during the frame. A record is present in every frame its
//! input is held, not only when it changes.
//!
//! **The command ids are DirectInput scancodes**, not the `COM_*` constants —
//! `scripts/defaultkeys.lua` is the key, and `omBindCommandI(COM_FORWARD,
//! 200)` against DIK_UP = 0xC8 is what settles it. 1000..1007 are not
//! scancodes at all: the two mouse buttons and the four half-axes.
//!
//! The file is records **from byte zero** — the 1005 that reads like a type
//! tag is the first record's command, so it belongs to frame 0, and frame 0
//! is the load and carries no real input. `../../tools/omn.py` is the
//! reference and drops it the same way.
//!
//! `demo%d_%d.omn` is level and checkpoint, so `demo1_5` starts at level 1
//! checkpoint 5.

pub const TYPE_OMN: u32 = 1005;
const END_OF_FRAME: u32 = 0xFFFF_FFFF;
const RECORD: usize = 8;

/// The scancodes and pseudo-ids the controller reads. Named here rather than
/// where they are used, because the names are the evidence.
pub const FORWARD: u32 = 200; // DIK_UP
pub const BACKWARD: u32 = 208; // DIK_DOWN
pub const LEFT: u32 = 203; // DIK_LEFT
pub const RIGHT: u32 = 205; // DIK_RIGHT
pub const JUMP: u32 = 1001; // the second mouse button
/// The first mouse button — `COM_SHOOT` and `COM_MELEE` are both bound to it
/// by `defaultkeys.lua`. Held on 161 of `demo1_5.omn`'s 1348 frames.
pub const SHOOT: u32 = 1000;
pub const TURN_RIGHT: u32 = 1004;
pub const TURN_LEFT: u32 = 1005;

#[derive(Debug, PartialEq)]
pub enum Error {
    NotADemo(u32),
    Truncated,
    /// Input records after the last end-of-frame: the file is not whole.
    Dangling(usize),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotADemo(t) => write!(f, "resource type {t}, not a demo (1005)"),
            Error::Truncated => write!(f, "the file ends inside a record"),
            Error::Dangling(n) => write!(f, "{n} input records after the last frame"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Debug, Default)]
pub struct Frame {
    /// Seconds. The recording runs at 30 fps.
    pub dt: f32,
    /// `(command, value)` for everything held during the frame.
    pub input: Vec<(u32, f32)>,
}

impl Frame {
    pub fn held(&self, command: u32) -> Option<f32> {
        self.input
            .iter()
            .rev()
            .find(|(c, _)| *c == command)
            .map(|(_, v)| *v)
    }
}

pub fn parse(data: &[u8]) -> Result<Vec<Frame>, Error> {
    let tag = u32::from_le_bytes(data.get(..4).ok_or(Error::Truncated)?.try_into().unwrap());
    if tag != TYPE_OMN {
        return Err(Error::NotADemo(tag));
    }
    if data.len() % RECORD != 0 {
        return Err(Error::Truncated);
    }
    let mut frames = Vec::new();
    let mut current = Frame::default();
    let mut o = 0usize;
    while o + RECORD <= data.len() {
        let command = u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
        let value = f32::from_bits(u32::from_le_bytes(data[o + 4..o + 8].try_into().unwrap()));
        o += RECORD;
        if command == END_OF_FRAME {
            current.dt = value;
            frames.push(std::mem::take(&mut current));
        } else {
            current.input.push((command, value));
        }
    }
    if !current.input.is_empty() {
        return Err(Error::Dangling(current.input.len()));
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(command: u32, value: f32) -> Vec<u8> {
        let mut b = command.to_le_bytes().to_vec();
        b.extend_from_slice(&value.to_le_bytes());
        b
    }

    #[test]
    fn a_frame_ends_at_the_terminator_and_carries_its_delta_time() {
        let mut d = record(TYPE_OMN, 0.0);
        d.extend(record(END_OF_FRAME, 0.0)); // frame 0, the load
        d.extend(record(FORWARD, 1.0));
        d.extend(record(TURN_RIGHT, 0.5));
        d.extend(record(END_OF_FRAME, 1.0 / 30.0));
        d.extend(record(END_OF_FRAME, 1.0 / 30.0));

        let frames = parse(&d).unwrap();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].input.len(), 1, "frame 0 holds only the 1005 record");
        assert_eq!(frames[1].input.len(), 2);
        assert_eq!(frames[1].held(FORWARD), Some(1.0));
        assert_eq!(frames[1].held(TURN_RIGHT), Some(0.5));
        assert_eq!(frames[1].held(JUMP), None);
        assert!((frames[1].dt - 1.0 / 30.0).abs() < 1e-7);
        assert!(frames[2].input.is_empty(), "the last frame holds nothing");
    }

    #[test]
    fn refuses_what_is_not_a_demo() {
        assert!(matches!(parse(&record(2002, 0.0)), Err(Error::NotADemo(2002))));
        assert!(matches!(parse(&[0u8; 3]), Err(Error::Truncated)));
        let dangling = [record(TYPE_OMN, 0.0), record(FORWARD, 1.0)].concat();
        assert!(matches!(parse(&dangling), Err(Error::Dangling(2))));
    }
}
