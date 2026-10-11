//! La configuration : un fichier TOML donné en argument.

use std::{path::Path, path::PathBuf, time::Duration};

use anyhow::Context;
use serde::Deserialize;

/// ```toml
/// nats_url = "nats://127.0.0.1:4222"
/// # nats_ca = "/etc/emory/ca.pem"                 # active TLS avec cette autorité
/// # nats_nkey_seed_file = "/etc/emory/bouche.nk"  # la graine de la clé (nkey) de la bouche
/// engine_url = "http://127.0.0.1:8000"
/// voice = "estelle"
/// ```
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// L'adresse du serveur NATS.
    pub nats_url: String,
    /// L'autorité de certification du bus (TLS) ; sans elle, la connexion est en clair.
    pub nats_ca: Option<PathBuf>,
    /// Le fichier qui contient la graine de la nkey de la bouche.
    pub nats_nkey_seed_file: Option<PathBuf>,
    /// L'adresse du moteur de voix (`pocket-tts serve`).
    pub engine_url: String,
    /// La voix, par son nom chez le moteur.
    #[serde(default = "default_voice")]
    pub voice: String,
    /// La taille visée d'un morceau de son envoyé au poste, en millisecondes.
    #[serde(default = "default_chunk_ms")]
    pub chunk_ms: u32,
    /// Combien de temps après la fin du son la bouche attend une phrase de plus avant de dire
    /// qu'elle a fini de parler (`Parle` `fin`), en millisecondes.
    #[serde(default = "default_end_grace_ms")]
    pub end_grace_ms: u64,
}

fn default_voice() -> String {
    "estelle".to_owned()
}

fn default_chunk_ms() -> u32 {
    60
}

fn default_end_grace_ms() -> u64 {
    700
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("lecture de {}", path.display()))?;
        text.parse()
            .with_context(|| format!("configuration {}", path.display()))
    }

    pub fn end_grace(&self) -> Duration {
        Duration::from_millis(self.end_grace_ms)
    }
}

impl std::str::FromStr for Config {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        let config: Config = toml::from_str(s)?;
        anyhow::ensure!(
            (10..=500).contains(&config.chunk_ms),
            "chunk_ms doit être entre 10 et 500"
        );
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn minimal_file_gets_the_defaults() {
        let c: Config = "nats_url = \"nats://x:4222\"\nengine_url = \"http://e:8000\"\n"
            .parse()
            .unwrap();
        assert_eq!(c.voice, "estelle");
        assert_eq!(c.chunk_ms, 60);
        assert!(c.nats_ca.is_none() && c.nats_nkey_seed_file.is_none());
    }

    #[test]
    fn tls_and_key_paths_are_read() {
        let c: Config = "nats_url = \"tls://x\"\nengine_url = \"http://e\"\nnats_ca = \"/ca.pem\"\nnats_nkey_seed_file = \"/s.nk\"\n"
            .parse()
            .unwrap();
        assert_eq!(c.nats_ca.unwrap().to_str(), Some("/ca.pem"));
        assert_eq!(c.nats_nkey_seed_file.unwrap().to_str(), Some("/s.nk"));
    }

    #[test]
    fn unknown_keys_and_absurd_chunks_are_refused() {
        assert!(
            "nats_url = \"a\"\nengine_url = \"b\"\nnope = 1\n"
                .parse::<Config>()
                .is_err()
        );
        assert!(
            "nats_url = \"a\"\nengine_url = \"b\"\nchunk_ms = 5\n"
                .parse::<Config>()
                .is_err()
        );
    }
}
