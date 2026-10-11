//! `emory-mouth`: plays the voice that comes back from the server on a speaker, and falls silent
//! as soon as the button is pressed (see `specs/voice.md`).
//!
//! The pieces, from the bus to the speaker:
//! - [`jitter`]: orders the chunks of a turn, decides when to start and what to drop (pure logic,
//!   no clock of its own, no audio hardware);
//! - [`convert`]: 16-bit PCM to what the output accepts (sample rate, channels);
//! - [`ring`]: the samples waiting for the output, a lock-free ring shared with its audio thread;
//! - [`sink`]: the outputs, a real speaker ([`sink::SpeakerSink`]) or a WAV file ([`sink::WavSink`]);
//! - [`bus`]: the NATS side.

pub mod bus;
pub mod config;
pub mod convert;
pub mod jitter;
pub mod ring;
pub mod sink;
