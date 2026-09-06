//! DirectInput scancodes, which are the ids the game binds keys by.
//!
//! `defaultkeys.lua` says `omBindCommandI(COM_MENUUP, 200)` and 200 is
//! `DIK_UP`; `menuinit.lua` says `omMakeCommand(COM_PAUSE, "ESC", ...)` and
//! the engine resolves the name to 1. Either way what reaches
//! [`crate::game::api::Input::bindings`] is a **DirectInput scancode**, and
//! SDL hands out its own — USB HID usage ids — so the two have to be put
//! side by side. There is no formula: DIK is the PS/2 set-1 make code.
//!
//! The table is only as long as it needs to be. A key that is not here is a
//! key nothing is bound to yet, and adding a row is the whole of adding one.

use sdl2::keyboard::Scancode;

/// `(the DirectInput scancode, the SDL one)`.
#[rustfmt::skip]
pub const KEYS: [(u32, Scancode); 63] = [
    (1, Scancode::Escape),
    (2, Scancode::Num1), (3, Scancode::Num2), (4, Scancode::Num3), (5, Scancode::Num4),
    (6, Scancode::Num5), (7, Scancode::Num6), (8, Scancode::Num7), (9, Scancode::Num8),
    (10, Scancode::Num9), (11, Scancode::Num0),
    (12, Scancode::Minus), (13, Scancode::Equals), (14, Scancode::Backspace),
    (15, Scancode::Tab),
    (16, Scancode::Q), (17, Scancode::W), (18, Scancode::E), (19, Scancode::R),
    (20, Scancode::T), (21, Scancode::Y), (22, Scancode::U), (23, Scancode::I),
    (24, Scancode::O), (25, Scancode::P),
    (26, Scancode::LeftBracket), (27, Scancode::RightBracket),
    (28, Scancode::Return),
    (29, Scancode::LCtrl),
    (30, Scancode::A), (31, Scancode::S), (32, Scancode::D), (33, Scancode::F),
    (34, Scancode::G), (35, Scancode::H), (36, Scancode::J), (37, Scancode::K),
    (38, Scancode::L),
    (39, Scancode::Semicolon), (40, Scancode::Apostrophe), (41, Scancode::Grave),
    (42, Scancode::LShift), (43, Scancode::Backslash),
    (44, Scancode::Z), (45, Scancode::X), (46, Scancode::C), (47, Scancode::V),
    (48, Scancode::B), (49, Scancode::N), (50, Scancode::M),
    (51, Scancode::Comma), (52, Scancode::Period), (53, Scancode::Slash),
    (54, Scancode::RShift),
    (56, Scancode::LAlt),
    (57, Scancode::Space),
    // the arrow cluster, which is where the menus live
    (200, Scancode::Up), (203, Scancode::Left), (205, Scancode::Right),
    (208, Scancode::Down),
    (156, Scancode::KpEnter), (157, Scancode::RCtrl), (184, Scancode::RAlt),
];

/// The DirectInput scancode SDL's key stands for, if the game can bind it.
pub fn dik(scancode: Scancode) -> Option<u32> {
    KEYS.iter().find(|(_, s)| *s == scancode).map(|(d, _)| *d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four the demo file itself names — `formats::omn` reads the same
    /// numbers out of a recording, which is the second witness for them.
    #[test]
    fn the_arrows_are_the_demos_own_numbers() {
        assert_eq!(dik(Scancode::Up), Some(crate::formats::omn::FORWARD));
        assert_eq!(dik(Scancode::Down), Some(crate::formats::omn::BACKWARD));
        assert_eq!(dik(Scancode::Left), Some(crate::formats::omn::LEFT));
        assert_eq!(dik(Scancode::Right), Some(crate::formats::omn::RIGHT));
    }

    #[test]
    fn nothing_is_listed_twice() {
        let mut ids: Vec<u32> = KEYS.iter().map(|(d, _)| *d).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before, "a scancode is in the table twice");
        let mut keys: Vec<Scancode> = KEYS.iter().map(|(_, s)| *s).collect();
        keys.sort_by_key(|s| *s as i32);
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before, "an SDL key is in the table twice");
    }
}
