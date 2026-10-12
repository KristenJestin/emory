//! The hearing of Emory, version 1: the button ear's server side (`specs/hearing.md`).
//!
//! From the audio of the ear (`ear.<name>.audio`) to the sentences (`hearing.sentence`) and the
//! whole turn (`hearing.turn`), with Silero VAD and Parakeet.

pub mod audio;
pub mod bus;
pub mod config;
pub mod corrections;
pub mod engine;
pub mod hotwords;
pub mod level;
pub mod service;
pub mod turns;
