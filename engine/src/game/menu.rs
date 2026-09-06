//! The menus the scripts build, and where each item lands on screen.
//!
//! The whole of the menu *logic* is already in the game's own Lua --
//! `menu.lua` builds 41 items and `options.lua` 43, each with its own
//! `Select`, and `menuinit.lua` binds the keys. What the engine owes them is
//! three things: a record per menu, the geometry of it, and the drawing.
//!
//! **The geometry is 0x411aa0 (`mdkCreateMenu`) and 0x411be0
//! (`mdkCreateMenuItem`), both in `mdkGui.c`.** Everything below is read off
//! those two and the draw at 0x412090:
//!
//! ```text
//! item i sits at  y + (gap + h) * i
//! and at          x + justify(text)
//! the title at    0.5 - width(title) * w * 1.2 * 0.5,  y - h * 1.8
//! ```
//!
//! `JUSTIFY_LEFT` 0 adds nothing, `JUSTIFY_RIGHT` 1 subtracts the whole
//! width and `JUSTIFY_CENTER` 2 half of it -- the -0.5 is the float at
//! 0x48f69c. The title's own 1.2 (0x48f698) is the scale it is drawn at, and
//! the 1.8 (0x48f694) is how far above the first item it stands.

/// The colours 0x412090 picks between, per item, before it draws: warm for
/// the one under the cursor and cool for the rest.
pub const SELECTED: [f32; 3] = [1.0, 0.7, 0.3];
pub const PLAIN: [f32; 3] = [0.25, 0.45, 0.65];
/// What a disabled item is multiplied by, colour and alpha both — the float
/// at 0x48f6bc, reached when `item + 0x3c & 2` is clear.
pub const DISABLED: f32 = 0.7;
/// The title is drawn this much larger than an item (0x48f698) and this far
/// above the first one, in item heights (0x48f694).
pub const TITLE_SCALE: f32 = 1.2;
pub const TITLE_ABOVE: f32 = 1.8;
/// Everything the menu centres, it centres on the middle of the screen and
/// not on its own x — the 0.5 at 0x48f2fc.
const MIDDLE: f32 = 0.5;

/// What an item carries to the right of its own words.
///
/// The kind is `(item + 0x3c >> 3) & 0xf`, and 0x413650 reads it to size the
/// menu's frame: each widget adds its own width to the item's, and all but
/// the checkbox add the float at 0x48f6c8 -- **0.04** -- on top.
#[derive(Default, Clone)]
pub enum Widget {
    #[default]
    None,
    /// Kind 1. A fixed **0.21** wide, the float at 0x48f6cc, and the only
    /// one whose width does not depend on what is in it.
    CheckBox,
    /// Kind 2, and one menu height wide. `mdkMenuItemAddSlider(item, step,
    /// 1)` -- `options.lua` passes `1/(n-1)` for a slider of n stops.
    Slider { step: f64 },
    /// Kind 3, as wide as its widest string. Filled by
    /// `mdkMenuItemAddComboString` one at a time.
    Combo(Vec<Vec<u8>>),
    /// Kind 4, as wide as the one string it holds.
    TextBox(Vec<u8>),
}

pub struct Item {
    /// Code-page bytes, the way [`crate::formats::strfile`] hands them over
    /// and the way the font is indexed.
    pub text: Vec<u8>,
    /// Where the text starts, absolute, both already justified.
    pub x: f32,
    pub y: f32,
    /// How wide the text is on screen, which is what the frame around the
    /// menu is measured from.
    pub width: f32,
    /// `item + 0x3c & 2`, which `mdkMenuItemEnable` writes.
    pub enabled: bool,
    /// Whatever the caller uses to find the script's own object for this
    /// item again — the scripts hang `Select` on it, and something has to
    /// call that. The model here does not care what it means.
    pub handle: usize,
    pub widget: Widget,
    /// How much room [`Menu::widget_width`] said the widget wants, kept
    /// beside the label's own width so the frame can be measured without a
    /// font.
    pub extra: f32,
    /// `mdkMenuItemGetWidgitValue` and its setter: a checkbox's 0 or 1, a
    /// slider's 0..1, a combo's index.
    pub value: f64,
}

pub struct Menu {
    pub x: f32,
    pub y: f32,
    /// One cell of the font: an item's width is its advances times this.
    pub w: f32,
    pub h: f32,
    pub gap: f32,
    pub justify: i64,
    pub title: Option<Vec<u8>>,
    pub title_at: [f32; 2],
    /// The title's width on screen at one cell, before the 1.2.
    pub title_width: f32,
    pub items: Vec<Item>,
    /// The record's `[2]`, which the constructor sets to 0 and the draw
    /// compares each item against.
    pub selected: usize,
}

impl Menu {
    /// `width` answers how wide a string is in cells — the font's business,
    /// which is why it arrives as a closure rather than as a dependency.
    pub fn new(
        [x, y, w, h, gap]: [f32; 5],
        justify: i64,
        title: Option<Vec<u8>>,
        width: impl Fn(&[u8]) -> f32,
    ) -> Menu {
        let title_width = title.as_deref().map(&width).unwrap_or(0.0) * w;
        let title_at = match &title {
            Some(_) => [MIDDLE - title_width * TITLE_SCALE * MIDDLE, y - h * TITLE_ABOVE],
            None => [x, y],
        };
        Menu {
            x, y, w, h, gap, justify, title, title_at, title_width,
            items: Vec::new(),
            selected: 0,
        }
    }

    /// `mdkSetMenuTitle` — the title again, which has to be placed again
    /// because the placing is of the title's own width.
    pub fn retitle(&mut self, title: Option<Vec<u8>>, width: impl Fn(&[u8]) -> f32) {
        self.title_width = title.as_deref().map(&width).unwrap_or(0.0) * self.w;
        self.title_at = match &title {
            Some(_) => [
                MIDDLE - self.title_width * TITLE_SCALE * MIDDLE,
                self.y - self.h * TITLE_ABOVE,
            ],
            None => [self.x, self.y],
        };
        self.title = title;
    }

    pub fn add(&mut self, text: Vec<u8>, handle: usize, width: impl Fn(&[u8]) -> f32) {
        let offset = match self.justify {
            1 => -width(&text) * self.w,
            2 => -width(&text) * self.w * MIDDLE,
            _ => 0.0,
        };
        let y = self.y + (self.gap + self.h) * self.items.len() as f32;
        let width = width(&text) * self.w;
        self.items.push(Item {
            text,
            x: self.x + offset,
            y,
            width,
            enabled: true,
            handle,
            widget: Widget::None,
            extra: 0.0,
            value: 0.0,
        });
    }

    /// How much room an item's widget wants beside its words — the switch
    /// in 0x413650, with the **0.04** at 0x48f6c8 and the **0.21** at
    /// 0x48f6cc.
    pub fn widget_width(&self, widget: &Widget, width: impl Fn(&[u8]) -> f32) -> f32 {
        const PAD: f32 = 0.04;
        match widget {
            Widget::None => 0.0,
            Widget::CheckBox => 0.21,
            Widget::Slider { .. } => self.h + PAD,
            Widget::Combo(strings) => {
                strings.iter().map(|s| width(s) * self.w).fold(0.0, f32::max) + PAD
            }
            Widget::TextBox(text) => width(text) * self.w + PAD,
        }
    }

    /// Left and right on a menu — the engine's own half, the way up and down
    /// are: nothing in Lua handles `COM_MENULEFT` outside the title screen.
    ///
    /// **Not read out of the binary.** A checkbox toggles, a slider steps by
    /// its own step and stops at the ends, a combo walks its strings and
    /// stops at the ends; the shapes come from what `options.lua` builds and
    /// from `mdkMenuItemSetWidgitValue`'s own arguments -- an index for a
    /// combo, 0 or 1 for a checkbox, a fraction for a slider.
    pub fn nudge(&mut self, by: f64) {
        let (h, w) = (self.h, self.w);
        let _ = (h, w);
        let Some(item) = self.items.get_mut(self.selected) else { return };
        if !item.enabled {
            return;
        }
        match &item.widget {
            Widget::None | Widget::TextBox(_) => {}
            Widget::CheckBox => item.value = if item.value == 0.0 { 1.0 } else { 0.0 },
            Widget::Slider { step } => {
                item.value = (item.value + by * step.abs().max(1e-6)).clamp(0.0, 1.0)
            }
            Widget::Combo(strings) => {
                let last = strings.len().saturating_sub(1) as f64;
                item.value = (item.value + by).clamp(0.0, last);
            }
        }
    }

    /// The box the frame is drawn around — **0x413650**, which walks the
    /// items and the title and keeps the extremes. The original stores it in
    /// the record at `[0x11]`..`[0x14]` and recomputes it whenever the menu
    /// changes; here it is cheap enough to ask for.
    ///
    /// `None` for a menu with nothing in it, which is what a freshly cleared
    /// one is.
    pub fn frame(&self) -> Option<[f32; 4]> {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        let mut seen = false;
        let mut take = |x: f32, y: f32, w: f32, h: f32| {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + w);
            y1 = y1.max(y + h);
            seen = true;
        };
        for item in &self.items {
            take(item.x, item.y, item.width + item.extra, self.h);
        }
        if let Some(t) = &self.title {
            // the title is drawn 1.2 cells tall and wide, so its box is too
            let width = self.title_width * TITLE_SCALE;
            take(self.title_at[0], self.title_at[1], width, self.h * TITLE_SCALE);
            let _ = t;
        }
        seen.then_some([x0, y0, x1 - x0, y1 - y0])
    }

    /// Move the cursor, skipping anything disabled and wrapping at the ends.
    ///
    /// **Read off the shape of the menus rather than off the code**: the
    /// pause menu disables its own Save item when the level says you may not
    /// save, and an item that cannot be chosen but can be landed on would
    /// leave Enter doing nothing. Not confirmed against 0x413230's family.
    pub fn step(&mut self, by: i64) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        for _ in 0..n {
            self.selected = (self.selected as i64 + by).rem_euclid(n as i64) as usize;
            if self.items[self.selected].enabled {
                return;
            }
        }
    }

    /// The colour an item is drawn in, alpha included.
    pub fn colour(&self, index: usize) -> [f32; 4] {
        let base = if index == self.selected { SELECTED } else { PLAIN };
        match self.items.get(index).map(|i| i.enabled) {
            Some(false) => [
                base[0] * DISABLED,
                base[1] * DISABLED,
                base[2] * DISABLED,
                DISABLED,
            ],
            _ => [base[0], base[1], base[2], 1.0],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every glyph one cell wide, so a width is a length and the arithmetic
    /// is visible.
    fn wide(s: &[u8]) -> f32 {
        s.len() as f32
    }

    #[test]
    fn items_stack_by_gap_plus_height() {
        // the pause menu's own numbers
        let mut m = Menu::new([0.5, 0.26, 0.06, 0.06, 0.0], 2, None, wide);
        m.add(b"ab".to_vec(), 0, wide);
        m.add(b"cd".to_vec(), 1, wide);
        assert!((m.items[0].y - 0.26).abs() < 1e-6);
        assert!((m.items[1].y - 0.32).abs() < 1e-6);
        // centred: half of two cells of 0.06 is 0.06 to the left
        assert!((m.items[0].x - 0.44).abs() < 1e-6);
    }

    #[test]
    fn justify_left_and_right() {
        let mut left = Menu::new([0.1, 0.0, 0.05, 0.05, 0.0], 0, None, wide);
        left.add(b"abcd".to_vec(), 0, wide);
        assert!((left.items[0].x - 0.1).abs() < 1e-6);
        let mut right = Menu::new([0.9, 0.0, 0.05, 0.05, 0.0], 1, None, wide);
        right.add(b"abcd".to_vec(), 0, wide);
        assert!((right.items[0].x - 0.7).abs() < 1e-6);
    }

    #[test]
    fn the_title_centres_on_the_screen_not_on_the_menu() {
        let m = Menu::new([0.065, 0.2, 0.04, 0.04, 0.0], 0, Some(b"abcde".to_vec()), wide);
        // 0.5 - 5 * 0.04 * 1.2 * 0.5
        assert!((m.title_at[0] - 0.38).abs() < 1e-6);
        assert!((m.title_at[1] - (0.2 - 0.04 * 1.8)).abs() < 1e-6);
    }

    #[test]
    fn the_cursor_steps_over_a_disabled_item() {
        let mut m = Menu::new([0.0, 0.0, 0.05, 0.05, 0.0], 0, None, wide);
        for _ in 0..3 {
            m.add(b"x".to_vec(), 0, wide);
        }
        m.items[1].enabled = false;
        m.step(1);
        assert_eq!(m.selected, 2);
        m.step(1);
        assert_eq!(m.selected, 0);
        m.step(-1);
        assert_eq!(m.selected, 2);
    }

    #[test]
    fn a_menu_of_nothing_but_disabled_items_does_not_spin() {
        let mut m = Menu::new([0.0, 0.0, 0.05, 0.05, 0.0], 0, None, wide);
        m.add(b"x".to_vec(), 0, wide);
        m.items[0].enabled = false;
        m.step(1);
        assert_eq!(m.selected, 0);
    }
}
