//! The configuration: a TOML file given as argument.

use std::{path::Path, path::PathBuf, time::Duration};

use anyhow::Context;
use serde::Deserialize;

/// ```toml
/// nats_url = "nats://127.0.0.1:4222"
/// # nats_ca = "/etc/emory/ca.pem"                 # turns TLS on with this authority
/// # nats_nkey_seed_file = "/etc/emory/voice.nk"   # the seed of the voice's key (nkey)
/// engine_url = "http://127.0.0.1:8000"
/// voice = "estelle"
/// ```
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The address of the NATS server.
    pub nats_url: String,
    /// The certificate authority of the bus (TLS); without it, the connection is in the clear.
    pub nats_ca: Option<PathBuf>,
    /// The file that holds the seed of the voice's nkey.
    pub nats_nkey_seed_file: Option<PathBuf>,
    /// The address of the voice engine (`pocket-tts serve`).
    pub engine_url: String,
    /// The voice, by its name at the engine.
    #[serde(default = "default_voice")]
    pub voice: String,
    /// The target size of a chunk of audio sent to the mouth, in milliseconds.
    #[serde(default = "default_chunk_ms")]
    pub chunk_ms: u32,
    /// How long after the end of the audio the voice waits for one more sentence before saying
    /// that it has finished speaking (`Speaking` `end`), in milliseconds. Only a fallback: when
    /// the last sentence is marked `last`, `end` is sent as soon as it has been played.
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
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
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
            "chunk_ms must be between 10 and 500"
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
