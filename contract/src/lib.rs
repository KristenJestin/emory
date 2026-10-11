//! The Emory contract: the bus subjects and the format of every message.
//!
//! The only thing the programs share (see `specs/contract.md`). No network here: types,
//! constants and the functions that build the subjects.
//!
//! The contract grows without breaking: a new field is optional, and unknown fields are ignored
//! on reading. Removing a field or changing its meaning changes the major version.

pub mod audio;
mod device;
mod messages;
pub mod subjects;
mod turn;

pub use device::{Device, InvalidName};
pub use messages::*;
pub use turn::TurnId;

/// A moment, with its time zone offset (RFC 3339).
pub type Timestamp = chrono::DateTime<chrono::FixedOffset>;

/// The present moment, in local time.
pub fn now() -> Timestamp {
    chrono::Local::now().fixed_offset()
}

/// How long the mind waits at most for the answer of a hand.
pub const ACTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
