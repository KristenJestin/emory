//! La configuration : un fichier TOML donné en argument au programme.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer};

/// Un chemin dans le fichier de configuration ; `~/` désigne le dossier personnel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chemin(pub PathBuf);

impl<'de> Deserialize<'de> for Chemin {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let brut = String::deserialize(d)?;
        Ok(Self(developper(&brut, std::env::var_os("HOME"))))
    }
}

impl AsRef<Path> for Chemin {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

fn developper(brut: &str, maison: Option<std::ffi::OsString>) -> PathBuf {
    match (brut.strip_prefix("~/"), maison) {
        (Some(reste), Some(maison)) => PathBuf::from(maison).join(reste),
        _ => PathBuf::from(brut),
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub nats: ConfigNats,
    pub modeles: ConfigModeles,
    #[serde(default)]
    pub transcription: ConfigTranscription,
    #[serde(default)]
    pub vad: ConfigVad,
}

/// La connexion au bus. Le développement n'utilise ni `ca` ni `nkey` ; le serveur verrouillé
/// utilise les deux.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigNats {
    #[serde(default = "url_par_defaut")]
    pub url: String,
    /// Le certificat de l'autorité qui a signé celui du serveur NATS (TLS exigé s'il est donné).
    pub ca: Option<Chemin>,
    /// Le fichier de la graine de la clé nkey de ce programme.
    pub nkey: Option<Chemin>,
}

fn url_par_defaut() -> String {
    "nats://127.0.0.1:4222".to_owned()
}

impl Default for ConfigNats {
    fn default() -> Self {
        Self {
            url: url_par_defaut(),
            ca: None,
            nkey: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigModeles {
    /// Le fichier `silero_vad.onnx`.
    pub vad: Chemin,
    /// Le dossier de Parakeet (`encoder.int8.onnx`, `decoder.int8.onnx`, `joiner.int8.onnx`,
    /// `tokens.txt`).
    pub parakeet: Chemin,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigTranscription {
    /// Le nombre de fils de calcul de Parakeet.
    #[serde(default = "fils_par_defaut")]
    pub fils: i32,
    /// La table de corrections (TSV), appliquée après la transcription.
    pub corrections: Option<Chemin>,
}

fn fils_par_defaut() -> i32 {
    4
}

impl Default for ConfigTranscription {
    fn default() -> Self {
        Self {
            fils: fils_par_defaut(),
            corrections: None,
        }
    }
}

/// Les réglages de Silero VAD ; les valeurs par défaut sont celles du banc d'essai.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ConfigVad {
    pub seuil: f32,
    /// Le silence qui ferme une phrase, en secondes.
    pub silence_min: f32,
    /// En dessous, un bruit n'est pas une phrase, en secondes.
    pub parole_min: f32,
    /// Au-delà, une phrase est coupée de force, en secondes.
    pub parole_max: f32,
}

impl Default for ConfigVad {
    fn default() -> Self {
        Self {
            seuil: 0.5,
            silence_min: 0.5,
            parole_min: 0.25,
            parole_max: 20.0,
        }
    }
}

impl Config {
    pub fn lire(chemin: &Path) -> Result<Self> {
        let texte = std::fs::read_to_string(chemin)
            .with_context(|| format!("lecture de {}", chemin.display()))?;
        toml::from_str(&texte).with_context(|| format!("configuration {}", chemin.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_configuration_minimale_prend_les_valeurs_par_defaut() {
        let config: Config = toml::from_str(
            r#"
            [modeles]
            vad = "/m/silero_vad.onnx"
            parakeet = "/m/parakeet"
            "#,
        )
        .unwrap();
        assert_eq!(config.nats.url, "nats://127.0.0.1:4222");
        assert!(config.nats.ca.is_none() && config.nats.nkey.is_none());
        assert_eq!(config.transcription.fils, 4);
        assert!(config.transcription.corrections.is_none());
        assert_eq!(config.vad.silence_min, 0.5);
    }

    #[test]
    fn une_cle_inconnue_est_refusee() {
        let erreur = toml::from_str::<Config>(
            r#"
            [modeles]
            vad = "/m/v.onnx"
            parakeet = "/m/p"
            [transcription]
            filz = 2
            "#,
        );
        assert!(erreur.is_err());
    }

    #[test]
    fn le_tilde_designe_le_dossier_personnel() {
        let maison = Some("/home/kris".into());
        assert_eq!(
            developper("~/.cache/emory", maison.clone()),
            PathBuf::from("/home/kris/.cache/emory")
        );
        assert_eq!(developper("/abs/x", maison), PathBuf::from("/abs/x"));
        assert_eq!(developper("~/x", None), PathBuf::from("~/x"));
    }
}
