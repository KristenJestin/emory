//! Le son sur le bus : la charge d'un message est le son brut (PCM 16 bits signé, petit-boutiste),
//! ses informations sont dans les en-têtes NATS.

use std::{fmt, str::FromStr, time::Duration};

/// Les en-têtes d'un message de son.
pub mod entetes {
    /// L'identifiant du tour.
    pub const TOUR: &str = "Emory-Tour";
    /// Le numéro du morceau dans le flux, à partir de 0 : un trou se voit.
    pub const SEQ: &str = "Emory-Seq";
    /// Le format du son ([`super::Format`]).
    pub const FORMAT: &str = "Emory-Format";
    /// Au retour seulement : le numéro de la phrase dite.
    pub const PHRASE: &str = "Emory-Phrase";
    /// Au retour seulement : `1` sur le dernier morceau d'une phrase.
    pub const FIN: &str = "Emory-Fin";
}

/// Le format d'un son : PCM 16 bits signé, petit-boutiste, à une fréquence et un nombre de
/// canaux. Écrit `s16le;16000;1` dans l'en-tête [`entetes::FORMAT`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub frequence: u32,
    pub canaux: u16,
}

/// Le son du poste aux oreilles : 16 kHz mono, ce que demandent le VAD et Parakeet.
pub const MICRO: Format = Format {
    frequence: 16_000,
    canaux: 1,
};

/// La durée d'un morceau de son du micro.
pub const MORCEAU_MICRO: Duration = Duration::from_millis(20);

/// La taille d'un morceau de son du micro, en octets (20 ms à 16 kHz mono, 2 octets par
/// échantillon).
pub const OCTETS_MORCEAU_MICRO: usize = 640;

/// Les oreilles ferment d'elles-mêmes un tour qui ne reçoit plus de son depuis ce temps : un
/// poste qui meurt entre les deux appuis n'enverra jamais la fin.
pub const SILENCE_QUI_FERME_LE_TOUR: Duration = Duration::from_secs(3);

const ENCODAGE: &str = "s16le";

impl Format {
    /// Le nombre d'octets d'une durée de son dans ce format.
    pub fn octets(&self, duree: Duration) -> usize {
        let echantillons = (duree.as_micros() * self.frequence as u128 / 1_000_000) as usize;
        echantillons * self.canaux as usize * 2
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{ENCODAGE};{};{}", self.frequence, self.canaux)
    }
}

/// Un format que le contrat ne connaît pas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatInconnu(pub String);

impl fmt::Display for FormatInconnu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "format de son inconnu : {:?}", self.0)
    }
}

impl std::error::Error for FormatInconnu {}

impl FromStr for Format {
    type Err = FormatInconnu;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let inconnu = || FormatInconnu(s.to_owned());
        let mut morceaux = s.split(';');
        if morceaux.next() != Some(ENCODAGE) {
            return Err(inconnu());
        }
        let frequence = morceaux
            .next()
            .and_then(|m| m.parse().ok())
            .ok_or_else(inconnu)?;
        let canaux = morceaux
            .next()
            .and_then(|m| m.parse().ok())
            .ok_or_else(inconnu)?;
        if morceaux.next().is_some() || frequence == 0 || canaux == 0 {
            return Err(inconnu());
        }
        Ok(Self { frequence, canaux })
    }
}
