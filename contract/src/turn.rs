use std::fmt;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// The identifier of a turn, given by the ear at the button press. It follows the turn from one
/// end to the other: the audio, the sentences, the transcribed turn, the sentences to say, the
/// voice coming back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TurnId(Ulid);

impl TurnId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for TurnId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for TurnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for TurnId {
    type Err = ulid::DecodeError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}
