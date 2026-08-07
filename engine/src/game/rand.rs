//! The game's random numbers, which are **MT19937** — and the 1998 edition of
//! it, not the 2002 one.
//!
//! This matters more than a random number generator usually would.
//! `mdk2.lua` calls `chSeedRand(127)` on every level start, so the original's
//! encounters are reproducible: the same checkpoint gives the same spawns and
//! the same taunts every time. Reproducing the *sequence* is therefore part of
//! reproducing the game, not a detail.
//!
//! Read out of `mdk2Main.exe` rather than guessed. `chRand` is at 0x41ccb0 and
//! hands its work to 0x452a80, whose tempering is unmistakable once the
//! compiler's rewriting is undone: `and eax, 0xff3a58ad; shl eax, 7` is
//! `(y << 7) & 0x9d2c5680` with the low seven bits masked early, and
//! `and eax, 0xffffdf8c; shl eax, 15` is `(y << 15) & 0xefc60000`. Those are
//! MT19937's two tempering constants exactly.
//!
//! The seeding at 0x452920 settles the edition: `mt[0] = seed | 1` and
//! `mt[i] = 69069 * mt[i-1]`, which is Matsumoto and Nishimura's 1998
//! `sgenrand`. The 2002 replacement uses 1812433253 and does not force the
//! seed odd. **4357** is the seed the original falls back to when nothing has
//! seeded it, which is the reference implementation's own default.
//!
//! `chRand()` returns the 32-bit output divided by **4294967295**, not by
//! 2^32 — so its range is `[0, 1]` closed, and 1.0 is attainable. A script
//! doing `chRand() * n` can therefore return `n`, and an index computed that
//! way needs a bound check that a half-open generator would not.

/// The multiplier the 1998 `sgenrand` uses, out of 0x452920.
const SEED_MULTIPLIER: u32 = 69069;
/// The seed 0x452960 falls back to, and the reference implementation's.
pub const DEFAULT_SEED: u32 = 4357;

const N: usize = 624;
const M: usize = 397;
const MATRIX_A: u32 = 0x9908_b0df;
const UPPER: u32 = 0x8000_0000;
const LOWER: u32 = 0x7fff_ffff;

pub struct Random {
    mt: [u32; N],
    /// `N` means "nothing left, twist first", which is the state seeding
    /// leaves behind: the original's counter is zero and its first call
    /// decrements past it.
    at: usize,
}

impl Default for Random {
    fn default() -> Random {
        let mut r = Random { mt: [0; N], at: N };
        r.seed(DEFAULT_SEED);
        r
    }
}

impl Random {
    pub fn seed(&mut self, seed: u32) {
        // the odd-forcing is the 1998 edition's, and dropping it changes
        // every sequence the game produces
        self.mt[0] = seed | 1;
        for i in 1..N {
            self.mt[i] = SEED_MULTIPLIER.wrapping_mul(self.mt[i - 1]);
        }
        self.at = N;
    }

    fn twist(&mut self) {
        for i in 0..N {
            let y = (self.mt[i] & UPPER) | (self.mt[(i + 1) % N] & LOWER);
            self.mt[i] = self.mt[(i + M) % N] ^ (y >> 1) ^ if y & 1 != 0 { MATRIX_A } else { 0 };
        }
        self.at = 0;
    }

    pub fn next_u32(&mut self) -> u32 {
        if self.at >= N {
            self.twist();
        }
        let mut y = self.mt[self.at];
        self.at += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^= y >> 18;
        y
    }

    /// What `chRand` hands the scripts: **`[0, 1]` closed**, since the
    /// original divides by 4294967295 rather than by 2^32.
    pub fn next(&mut self) -> f64 {
        self.next_u32() as f64 / 4_294_967_295.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first outputs after `chSeedRand(127)`, which is what every level
    /// start does. Taken from the original's own generator run under
    /// emulation — see `tools/rand.py` — so this test fails if any of the
    /// seeding, the twist or the tempering is off by anything at all.
    #[test]
    fn the_sequence_is_the_original_s() {
        let mut r = Random::default();
        r.seed(127);
        let got: Vec<u32> = (0..8).map(|_| r.next_u32()).collect();
        assert_eq!(
            got,
            [
                3447130821, 3921056250, 1835579211, 3772296893, 231824126, 2956236577,
                1185653816, 2379096476
            ]
        );
    }

    /// Nothing seeded it, so it seeds itself the way the original does.
    #[test]
    fn the_default_seed_is_the_reference_implementation_s() {
        let mut named = Random { mt: [0; N], at: N };
        named.seed(DEFAULT_SEED);
        let mut default = Random::default();
        assert_eq!(named.next_u32(), default.next_u32());
    }

    /// `[0, 1]` **closed**, and the closed end is the point: the original
    /// divides by 4294967295, so the largest 32-bit output lands exactly on
    /// 1.0. Dividing by 2^32 instead would never reach it, and every
    /// `chRand() * n` in the scripts would lose its top value.
    #[test]
    fn the_range_is_closed_at_both_ends() {
        assert_eq!(u32::MAX as f64 / 4_294_967_295.0, 1.0);
        let mut r = Random::default();
        r.seed(1);
        for _ in 0..10_000 {
            assert!((0.0..=1.0).contains(&r.next()));
        }
    }
}
