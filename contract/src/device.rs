use std::fmt;

use serde::{Deserialize, Serialize};

/// The name of a device (`office`): lowercase letters, digits and dashes, since it goes into the
/// bus subjects.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Device(String);

/// A name that cannot go into a subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidName(pub String);

impl fmt::Display for InvalidName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid name: {:?} (lowercase letters, digits and dashes)",
            self.0
        )
    }
}

impl std::error::Error for InvalidName {}

/// A piece of subject: not empty, made of lowercase letters, digits and dashes.
pub(crate) fn valid_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl Device {
    pub fn new(name: impl Into<String>) -> Result<Self, InvalidName> {
        let name = name.into();
        if valid_token(&name) {
            Ok(Self(name))
        } else {
            Err(InvalidName(name))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Device {
    type Error = InvalidName;
    fn try_from(name: String) -> Result<Self, Self::Error> {
        Self::new(name)
    }
}

impl From<Device> for String {
    fn from(d: Device) -> Self {
        d.0
    }
}

impl std::str::FromStr for Device {
    type Err = InvalidName;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
