//! The configuration: a TOML file given as argument to the program.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer};

/// A path in the configuration file; `~/` stands for the home folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigPath(pub PathBuf);

impl<'de> Deserialize<'de> for ConfigPath {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Ok(Self(expand(&raw, std::env::var_os("HOME"))))
    }
}

impl ConfigPath {
    /// A relative path is read from `base`, the folder of the configuration file, not from where
    /// the program was started.
    fn resolve(&mut self, base: &Path) {
        if self.0.is_relative() {
            self.0 = base.join(&self.0);
        }
    }
}

impl AsRef<Path> for ConfigPath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

fn expand(raw: &str, home: Option<std::ffi::OsString>) -> PathBuf {
    match (raw.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(raw),
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub nats: NatsConfig,
    pub models: ModelsConfig,
    #[serde(default)]
    pub transcription: TranscriptionConfig,
    #[serde(default)]
    pub vad: VadConfig,
    #[serde(default)]
    pub turns: TurnsConfig,
}

/// The connection to the bus. Development uses neither `ca` nor `nkey`; the locked-down server
/// uses both.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NatsConfig {
    #[serde(default = "default_url")]
    pub url: String,
    /// The certificate of the authority that signed the NATS server's (TLS is required if given).
    pub ca: Option<ConfigPath>,
    /// The file with the seed of this program's nkey.
    pub nkey: Option<ConfigPath>,
}

fn default_url() -> String {
    "nats://127.0.0.1:4222".to_owned()
}

impl Default for NatsConfig {
    fn default() -> Self {
        Self {
            url: default_url(),
            ca: None,
            nkey: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelsConfig {
    /// The `silero_vad.onnx` file.
    pub vad: ConfigPath,
    /// The Parakeet folder (`encoder.int8.onnx`, `decoder.int8.onnx`, `joiner.int8.onnx`,
    /// `tokens.txt`).
    pub parakeet: ConfigPath,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptionConfig {
    /// The number of Parakeet compute threads.
    #[serde(default = "default_threads")]
    pub threads: i32,
    /// The corrections table (TSV), applied after the transcription.
    pub corrections: Option<ConfigPath>,
}

fn default_threads() -> i32 {
    4
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            threads: default_threads(),
            corrections: None,
        }
    }
}

/// The Silero VAD settings; the default values are those of the test bench.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct VadConfig {
    pub threshold: f32,
    /// The silence that closes a sentence, in seconds.
    pub min_silence: f32,
    /// Below this, a noise is not a sentence, in seconds.
    pub min_speech: f32,
    /// Beyond this, a sentence is cut by force, in seconds.
    pub max_speech: f32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            min_silence: 0.5,
            min_speech: 0.25,
            max_speech: 20.0,
        }
    }
}

/// The limits of a turn.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TurnsConfig {
    /// The turn closes by itself, as with `end`, after this long, in seconds: an ear that keeps
    /// streaming without ever sending the end would hold it open forever.
    pub max_turn: f32,
}

impl Default for TurnsConfig {
    fn default() -> Self {
        Self { max_turn: 120.0 }
    }
}

impl TurnsConfig {
    pub fn max_turn(&self) -> Duration {
        Duration::from_secs_f32(self.max_turn.max(1.0))
    }
}

impl Config {
    /// Reads the file; the relative paths it holds are resolved from its folder.
    pub fn read(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut config: Self =
            toml::from_str(&text).with_context(|| format!("configuration {}", path.display()))?;
        config.resolve_paths(path.parent().unwrap_or(Path::new("")));
        Ok(config)
    }

    fn resolve_paths(&mut self, base: &Path) {
        let paths = [
            self.nats.ca.as_mut(),
            self.nats.nkey.as_mut(),
            Some(&mut self.models.vad),
            Some(&mut self.models.parakeet),
            self.transcription.corrections.as_mut(),
        ];
        for path in paths.into_iter().flatten() {
            path.resolve(base);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_minimal_configuration_takes_the_default_values() {
        let config: Config = toml::from_str(
            r#"
            [models]
            vad = "/m/silero_vad.onnx"
            parakeet = "/m/parakeet"
            "#,
        )
        .unwrap();
        assert_eq!(config.nats.url, "nats://127.0.0.1:4222");
        assert!(config.nats.ca.is_none() && config.nats.nkey.is_none());
        assert_eq!(config.transcription.threads, 4);
        assert!(config.transcription.corrections.is_none());
        assert_eq!(config.vad.min_silence, 0.5);
        assert_eq!(config.turns.max_turn(), Duration::from_secs(120));
    }

    #[test]
    fn the_maximum_length_of_a_turn_is_set_in_seconds() {
        let config: Config = toml::from_str(
            r#"
            [models]
            vad = "/m/v.onnx"
            parakeet = "/m/p"
            [turns]
            max_turn = 45.5
            "#,
        )
        .unwrap();
        assert_eq!(config.turns.max_turn(), Duration::from_secs_f32(45.5));
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let result = toml::from_str::<Config>(
            r#"
            [models]
            vad = "/m/v.onnx"
            parakeet = "/m/p"
            [transcription]
            threadz = 2
            "#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn relative_paths_are_read_from_the_folder_of_the_configuration_file() {
        let folder = std::env::temp_dir().join(format!("hearing-config-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("hearing.toml");
        std::fs::write(
            &file,
            r#"
            [nats]
            ca = "certs/ca.pem"
            [models]
            vad = "/m/silero_vad.onnx"
            parakeet = "models/parakeet"
            [transcription]
            corrections = "corrections.tsv"
            "#,
        )
        .unwrap();
        let config = Config::read(&file).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();
        assert_eq!(config.nats.ca.unwrap().0, folder.join("certs/ca.pem"));
        assert_eq!(config.models.vad.0, PathBuf::from("/m/silero_vad.onnx"));
        assert_eq!(config.models.parakeet.0, folder.join("models/parakeet"));
        assert_eq!(
            config.transcription.corrections.unwrap().0,
            folder.join("corrections.tsv")
        );
    }

    #[test]
    fn the_tilde_stands_for_the_home_folder() {
        let home = Some("/home/kris".into());
        assert_eq!(
            expand("~/.cache/emory", home.clone()),
            PathBuf::from("/home/kris/.cache/emory")
        );
        assert_eq!(expand("/abs/x", home), PathBuf::from("/abs/x"));
        assert_eq!(expand("~/x", None), PathBuf::from("~/x"));
    }
}
