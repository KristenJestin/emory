//! Le contrat d'Emory : les sujets du bus et le format de chaque message.
//!
//! La seule chose que les programmes partagent (voir `specs/contrat.md`). Pas de réseau ici :
//! des types, des constantes et les fonctions qui fabriquent les sujets.
//!
//! Le contrat grandit sans casser : un champ nouveau est facultatif, et les champs inconnus sont
//! ignorés à la lecture. Retirer un champ ou changer son sens change la version majeure.

mod appareil;
mod messages;
pub mod son;
pub mod sujets;
mod tour;

pub use appareil::{Appareil, NomInvalide};
pub use messages::*;
pub use tour::IdTour;

/// Un instant, avec son décalage horaire (RFC 3339).
pub type Quand = chrono::DateTime<chrono::FixedOffset>;

/// L'instant présent, à l'heure locale.
pub fn maintenant() -> Quand {
    chrono::Local::now().fixed_offset()
}

/// Ce que l'esprit attend au plus la réponse d'une main.
pub const DELAI_ACTION: std::time::Duration = std::time::Duration::from_secs(2);
