//! The configuration: a TOML file given as argument.

use std::{path::Path, path::PathBuf, time::Duration};

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// ```toml
/// nats_url = "nats://127.0.0.1:4222"
/// # nats_ca = "/etc/emory/ca.pem"                 # turns TLS on with this authority
/// # nats_nkey_seed_file = "/etc/emory/voice.nk"   # the seed of the voice's key (nkey)
/// # engine = "pocket"                # or "kyutai"
/// engine_url = "http://127.0.0.1:8000"
/// voice = "estelle"                  # by default, estelle for Pocket, another one for Kyutai
/// # engine_idle_timeout = 5          # seconds without a packet from the engine
/// # missing_sentence_timeout = 3     # seconds waiting for a missing sentence
/// # engine_concurrency = 1           # sentences the engine synthesises at once
///
/// # [cue]                            # the waiting cue (tone, pulse), these are the defaults
/// # volume = 0.3                     # peak of the tone, 0 to 1
/// # tone = "rising"                  # "rising", "bright", "warm" or "single"
/// # pulse_interval_ms = 1600         # between two pulses while the answer is awaited
///
/// # [kyutai]                         # the settings of a Kyutai sentence, these are the defaults
/// # padding_bonus = -3.0
/// # max_padding = 8
/// # temp = 0.6
/// # cfg_coef = 2.0
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
    /// Which voice engine answers at `engine_url`.
    #[serde(default)]
    pub engine: EngineKind,
    /// The address of the voice engine.
    pub engine_url: String,
    /// The voice, by its name at the engine; see [`Config::voice`] for the default.
    pub voice: Option<String>,
    /// The settings of a Kyutai sentence; ignored by Pocket.
    #[serde(default)]
    pub kyutai: KyutaiSettings,
    /// The sounds of the waiting cue (what the mind asks with `voice.cue`).
    #[serde(default)]
    pub cue: CueSettings,
    /// The target size of a chunk of audio sent to the mouth, in milliseconds.
    #[serde(default = "default_chunk_ms")]
    pub chunk_ms: u32,
    /// How long after the end of the audio the voice waits for one more sentence before saying
    /// that it has finished speaking (`Speaking` `end`), in milliseconds. Only a fallback: when
    /// the last sentence is marked `last`, `end` is sent as soon as it has been played.
    #[serde(default = "default_end_grace_ms")]
    pub end_grace_ms: u64,
    /// How long, in seconds, the engine may stay silent (no response, no packet of sound) before
    /// the sentence is given up: it ends like an engine error, with its sound so far and the empty
    /// `Emory-End` chunk.
    #[serde(default = "default_engine_idle_timeout")]
    pub engine_idle_timeout: f32,
    /// How long, in seconds, a turn waits for a missing sentence (an `index` that never arrives)
    /// before skipping it.
    #[serde(default = "default_missing_sentence_timeout")]
    pub missing_sentence_timeout: f32,
    /// How many sentences the engine synthesises at once, all devices together: one semaphore.
    #[serde(default = "default_engine_concurrency")]
    pub engine_concurrency: usize,
}

/// The voice engines `voice` knows how to call.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    /// Kyutai Pocket TTS, on the processor (`engines/pocket/`).
    #[default]
    Pocket,
    /// Kyutai TTS 1.6B, on the graphics card (`engines/kyutai/`).
    Kyutai,
}

/// The voice of Pocket chosen on 2026-10-11.
pub const POCKET_VOICE: &str = "estelle";
/// The voice of Kyutai TTS 1.6B chosen on 2026-10-12, in `kyutai/tts-voices`.
pub const KYUTAI_VOICE: &str = "cml-tts/fr/5476_3103_000072-0001.wav";

/// How Kyutai TTS 1.6B says a sentence. The defaults are the ones chosen on 2026-10-12 (see
/// `specs/voice.md`).
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct KyutaiSettings {
    /// Added to the odds of a pause between two words: negative speaks faster. -3 is about a
    /// quarter faster than 0, which was found slow.
    pub padding_bonus: f32,
    /// At most this many pauses in a row.
    pub max_padding: u32,
    /// The temperature of the sampling.
    pub temp: f32,
    /// How closely the voice is followed (classifier-free guidance); the model only knows a few
    /// values, the engine says which.
    pub cfg_coef: f32,
}

impl Default for KyutaiSettings {
    fn default() -> Self {
        Self {
            padding_bonus: -3.0,
            max_padding: 8,
            temp: 0.6,
            cfg_coef: 2.0,
        }
    }
}

/// How the waiting cue sounds. The mind decides when (`[waiting]` in its configuration); the voice
/// draws the sounds.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct CueSettings {
    /// The peak of the tone, as a fraction of the maximum (0 to 1). The pulse is quieter still.
    pub volume: f32,
    /// The shape of the tone.
    pub tone: crate::cue::Tone,
    /// Milliseconds between two pulses, while the answer is awaited.
    pub pulse_interval_ms: u64,
}

impl Default for CueSettings {
    fn default() -> Self {
        Self {
            volume: 0.3,
            tone: crate::cue::Tone::default(),
            pulse_interval_ms: 1600,
        }
    }
}

impl CueSettings {
    pub fn pulse_interval(&self) -> Duration {
        Duration::from_millis(self.pulse_interval_ms)
    }
}

fn default_chunk_ms() -> u32 {
    60
}

fn default_end_grace_ms() -> u64 {
    700
}

fn default_engine_idle_timeout() -> f32 {
    5.0
}

fn default_missing_sentence_timeout() -> f32 {
    3.0
}

fn default_engine_concurrency() -> usize {
    1
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        text.parse()
            .with_context(|| format!("configuration {}", path.display()))
    }

    /// The voice given, or the one chosen for the engine.
    pub fn voice(&self) -> &str {
        self.voice.as_deref().unwrap_or(match self.engine {
            EngineKind::Pocket => POCKET_VOICE,
            EngineKind::Kyutai => KYUTAI_VOICE,
        })
    }

    pub fn end_grace(&self) -> Duration {
        Duration::from_millis(self.end_grace_ms)
    }

    pub fn engine_idle_timeout(&self) -> Duration {
        Duration::from_secs_f32(self.engine_idle_timeout)
    }

    pub fn missing_sentence_timeout(&self) -> Duration {
        Duration::from_secs_f32(self.missing_sentence_timeout)
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
        anyhow::ensure!(
            (0.1..=600.0).contains(&config.engine_idle_timeout),
            "engine_idle_timeout must be between 0.1 and 600 seconds"
        );
        anyhow::ensure!(
            (0.1..=600.0).contains(&config.missing_sentence_timeout),
            "missing_sentence_timeout must be between 0.1 and 600 seconds"
        );
        anyhow::ensure!(
            (1..=64).contains(&config.engine_concurrency),
            "engine_concurrency must be between 1 and 64"
        );
        anyhow::ensure!(
            (0.0..=1.0).contains(&config.cue.volume),
            "cue.volume must be between 0 and 1"
        );
        anyhow::ensure!(
            (300..=60_000).contains(&config.cue.pulse_interval_ms),
            "cue.pulse_interval_ms must be between 300 and 60000"
        );
        let k = &config.kyutai;
        anyhow::ensure!(
            (-10.0..=10.0).contains(&k.padding_bonus),
            "kyutai.padding_bonus must be between -10 and 10"
        );
        anyhow::ensure!(
            (1..=64).contains(&k.max_padding),
            "kyutai.max_padding must be between 1 and 64"
        );
        anyhow::ensure!(
            k.temp > 0.0 && k.temp <= 2.0,
            "kyutai.temp must be in ]0, 2]"
        );
        anyhow::ensure!(
            (1.0..=4.0).contains(&k.cfg_coef),
            "kyutai.cfg_coef must be between 1 and 4"
        );
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::Config;

    #[test]
    fn minimal_file_gets_the_defaults() {
        let c: Config = "nats_url = \"nats://x:4222\"\nengine_url = \"http://e:8000\"\n"
            .parse()
            .unwrap();
        assert_eq!(c.engine, super::EngineKind::Pocket);
        assert_eq!(c.voice(), "estelle");
        assert_eq!(c.chunk_ms, 60);
        assert!(c.nats_ca.is_none() && c.nats_nkey_seed_file.is_none());
        assert_eq!(c.engine_idle_timeout(), Duration::from_secs(5));
        assert_eq!(c.missing_sentence_timeout(), Duration::from_secs(3));
        assert_eq!(c.engine_concurrency, 1);
    }

    #[test]
    fn the_timeouts_and_the_concurrency_are_set_and_checked() {
        let base = "nats_url = \"a\"\nengine_url = \"b\"\n";
        let c: Config = format!(
            "{base}engine_idle_timeout = 2.5\nmissing_sentence_timeout = 1\nengine_concurrency = 2\n"
        )
        .parse()
        .unwrap();
        assert_eq!(c.engine_idle_timeout(), Duration::from_millis(2500));
        assert_eq!(c.missing_sentence_timeout(), Duration::from_secs(1));
        assert_eq!(c.engine_concurrency, 2);
        for bad in [
            "engine_idle_timeout = 0",
            "engine_idle_timeout = -1",
            "missing_sentence_timeout = 0",
            "engine_concurrency = 0",
        ] {
            assert!(format!("{base}{bad}\n").parse::<Config>().is_err(), "{bad}");
        }
    }

    #[test]
    fn kyutai_gets_its_default_voice_and_settings() {
        let base = "nats_url = \"a\"\nengine_url = \"b\"\nengine = \"kyutai\"\n";
        let c: Config = base.parse().unwrap();
        assert_eq!(c.engine, super::EngineKind::Kyutai);
        assert_eq!(c.voice(), "cml-tts/fr/5476_3103_000072-0001.wav");
        assert_eq!(c.kyutai, super::KyutaiSettings::default());
        assert_eq!(c.kyutai.padding_bonus, -3.0);
        let c: Config =
            format!("{base}voice = \"v.wav\"\n[kyutai]\npadding_bonus = 0\ntemp = 0.8\n")
                .parse()
                .unwrap();
        assert_eq!(c.voice(), "v.wav");
        assert_eq!((c.kyutai.padding_bonus, c.kyutai.temp), (0.0, 0.8));
        assert_eq!(c.kyutai.max_padding, 8);
        for bad in [
            "temp = 0",
            "max_padding = 0",
            "padding_bonus = -11",
            "cfg_coef = 5",
            "nope = 1",
        ] {
            assert!(
                format!("{base}[kyutai]\n{bad}\n")
                    .parse::<Config>()
                    .is_err(),
                "{bad}"
            );
        }
        assert!(
            "nats_url = \"a\"\nengine_url = \"b\"\nengine = \"piper\"\n"
                .parse::<Config>()
                .is_err()
        );
    }

    #[test]
    fn the_cue_has_defaults_and_is_checked() {
        let base = "nats_url = \"a\"\nengine_url = \"b\"\n";
        let c: Config = base.parse().unwrap();
        assert_eq!(c.cue, super::CueSettings::default());
        assert_eq!(c.cue.pulse_interval(), Duration::from_millis(1600));
        let c: Config = format!("{base}[cue]\nvolume = 0.5\ntone = \"warm\"\n")
            .parse()
            .unwrap();
        assert_eq!((c.cue.volume, c.cue.tone), (0.5, crate::cue::Tone::Warm));
        for bad in [
            "volume = 1.5",
            "volume = -0.1",
            "tone = \"loud\"",
            "pulse_interval_ms = 100",
            "nope = 1",
        ] {
            assert!(
                format!("{base}[cue]\n{bad}\n").parse::<Config>().is_err(),
                "{bad}"
            );
        }
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
