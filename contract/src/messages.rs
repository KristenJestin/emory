//! Les messages en JSON. Les champs facultatifs ont une valeur par défaut, pour qu'un message
//! d'une version plus ancienne se lise encore.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Appareil, IdTour, Quand};

/// Le début ou la fin de quelque chose : un appui, la parole d'Emory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Etat {
    Debut,
    Fin,
}

/// Un appui sur le bouton d'un poste (`poste.<nom>.bouton`). Le son du tour part entre le début
/// et la fin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bouton {
    pub tour: IdTour,
    pub etat: Etat,
    pub quand: Quand,
}

/// Une phrase fermée par le VAD et transcrite pendant que Kris parle (`oreilles.phrase`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Phrase {
    pub tour: IdTour,
    pub poste: Appareil,
    /// Le numéro de la phrase dans le tour, à partir de 0.
    pub numero: u32,
    pub texte: String,
    pub debut: Quand,
    pub fin: Quand,
}

/// Le tour entier, transcrit au second appui (`oreilles.tour`). `texte` est vide quand rien n'a
/// été entendu : l'esprit sait quand même que le tour est fini.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tour {
    pub tour: IdTour,
    pub poste: Appareil,
    pub debut: Quand,
    pub fin: Quand,
    pub texte: String,
    pub reveil: Reveil,
    /// `None` tant que les voix ne sont pas reconnues.
    #[serde(default)]
    pub locuteur: Option<Locuteur>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confiance_transcription: Option<f32>,
    /// Les phrases de la mémoire tampon des oreilles, quand l'esprit les demande.
    #[serde(default)]
    pub contexte: Vec<PhraseDeContexte>,
}

/// Ce qui a réveillé Emory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reveil {
    /// L'oreille à bouton.
    Bouton,
    /// Le mot de réveil.
    Mot,
    /// Dans la fenêtre de suite.
    Suite,
}

/// Qui a parlé, quand les voix sont reconnues.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Locuteur {
    /// L'étiquette stable de l'empreinte (`kris`, `voix-7`).
    pub voix: String,
    /// Le slug de la personne dans Hippocampe, quand la voix a un nom.
    #[serde(default)]
    pub personne: Option<String>,
    pub origine: Origine,
    pub confiance: f32,
}

/// D'où vient le nom d'une voix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origine {
    /// Kris a donné le nom.
    Connue,
    /// Le système l'a déduit : ce que l'esprit en écrit est marqué `inferred`.
    Supposee,
}

/// Une phrase entendue avant le tour.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PhraseDeContexte {
    pub debut: Quand,
    pub voix: String,
    #[serde(default)]
    pub personne: Option<String>,
    pub texte: String,
}

/// Une phrase de la réponse à dire sur un poste (`bouche.dire`), numérotée dans le tour ; la
/// bouche les dit dans l'ordre.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dire {
    pub tour: IdTour,
    pub poste: Appareil,
    pub numero: u32,
    pub texte: String,
}

/// Jette tout ce que la bouche n'a pas encore dit pour ce tour (`bouche.stop`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stop {
    pub tour: IdTour,
    pub poste: Appareil,
}

/// La bouche commence ou finit de parler sur un appareil (`bouche.<nom>.parle`) : la main de
/// l'appareil baisse la musique, puis la remonte.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Parle {
    pub etat: Etat,
}

/// Ce qu'un poste ou une main sait faire (`poste.<nom>.annonce`, `main.<nom>.annonce`), publié au
/// démarrage et en réponse à `annonces.demande`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Annonce {
    pub appareil: Appareil,
    pub programme: Programme,
    pub version: String,
    pub modules: Vec<String>,
    /// Les actions d'une main ; un poste n'en a pas.
    #[serde(default)]
    pub actions: Vec<Action>,
}

/// Le programme qui s'annonce.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Programme {
    Poste,
    Main,
}

/// Une action d'une main, dont l'esprit fait un outil du LLM.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    /// `mpris.pause` : le sujet est `main.<nom>.mpris.pause`.
    pub nom: String,
    pub description: String,
    /// Le schéma JSON des arguments.
    pub arguments: Value,
}

/// La réponse d'une main à une action : `ok` avec un résultat, ou une erreur.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReponseAction {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resultat: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub erreur: Option<String>,
}

impl ReponseAction {
    pub fn reussie(resultat: Value) -> Self {
        Self {
            ok: true,
            resultat: Some(resultat),
            erreur: None,
        }
    }

    pub fn ratee(erreur: impl Into<String>) -> Self {
        Self {
            ok: false,
            resultat: None,
            erreur: Some(erreur.into()),
        }
    }

    pub fn en_result(self) -> Result<Value, String> {
        if self.ok {
            Ok(self.resultat.unwrap_or(Value::Null))
        } else {
            Err(self
                .erreur
                .unwrap_or_else(|| "erreur sans message".to_owned()))
        }
    }
}
