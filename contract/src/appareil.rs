use std::fmt;

use serde::{Deserialize, Serialize};

/// Le nom d'un appareil (`bureau`) : des minuscules, des chiffres et des tirets, puisqu'il entre
/// dans les sujets du bus.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Appareil(String);

/// Un nom qui ne peut pas entrer dans un sujet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NomInvalide(pub String);

impl fmt::Display for NomInvalide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "nom invalide : {:?} (minuscules, chiffres et tirets)",
            self.0
        )
    }
}

impl std::error::Error for NomInvalide {}

/// Un morceau de sujet : non vide, fait de minuscules, de chiffres et de tirets.
pub(crate) fn morceau_valide(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl Appareil {
    pub fn new(nom: impl Into<String>) -> Result<Self, NomInvalide> {
        let nom = nom.into();
        if morceau_valide(&nom) {
            Ok(Self(nom))
        } else {
            Err(NomInvalide(nom))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Appareil {
    type Error = NomInvalide;
    fn try_from(nom: String) -> Result<Self, Self::Error> {
        Self::new(nom)
    }
}

impl From<Appareil> for String {
    fn from(a: Appareil) -> Self {
        a.0
    }
}

impl std::str::FromStr for Appareil {
    type Err = NomInvalide;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl fmt::Display for Appareil {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
