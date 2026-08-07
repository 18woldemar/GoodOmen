//! goodomen — an open reimplementation of the MDK2 engine, run on the
//! player's own copy of the game.
//!
//! The library is where everything lives so that the tests link the same
//! code the game runs, rather than a copy of it. `main.rs` is a thin shell
//! around it.

pub mod formats;
pub mod game;
pub mod render;
