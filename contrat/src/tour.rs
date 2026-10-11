use std::fmt;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// L'identifiant d'un tour, donné par le poste à l'appui. Il suit le tour d'un bout à l'autre :
/// le son, les phrases, le tour transcrit, les phrases à dire, la voix qui revient.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IdTour(Ulid);

impl IdTour {
    pub fn nouveau() -> Self {
        Self(Ulid::new())
    }
}

impl fmt::Display for IdTour {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for IdTour {
    type Err = ulid::DecodeError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}
