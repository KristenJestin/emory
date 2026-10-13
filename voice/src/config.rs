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
/// # gain = 1.0                       # loudness of the voice, 0.1 to 8: 1 = as the engine says it
///
/// # [loudness]                       # compressor, look-ahead limiter and target level of the voice
/// # enabled = false                  # off by default: the voice is sent as the engine says it
/// # target = -16.0                   # LUFS the speech is brought to (-40 to -6)
/// # ceiling = -1.0                   # dBFS nothing goes past (-12 to -0.1)
/// # lookahead_ms = 5.0               # delay the limiter looks ahead (1 to 20)
/// # ratio = 3.0                      # compression ratio (1 = none, up to 20)
/// # threshold = 3.0                  # dB above the speech level where the compression starts
/// # attack_ms = 5.0
/// # release_ms = 80.0
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
    /// The loudness of the voice (not of the cue, which has its own `[cue] volume`): a factor on
    /// the amplitude of the engine's audio, whatever the engine. 1 sends it as it comes, 2 is
    /// twice the amplitude (about +6 dB). What would go past full scale is eased, not clipped.
    #[serde(default = "default_gain")]
    pub gain: f32,
    /// The loudness stage of the voice (compressor, look-ahead limiter, target level). When it is
    /// enabled it sets the level and `gain` is ignored.
    #[serde(default)]
    pub loudness: LoudnessSettings,
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

/// The loudness stage of the voice (`voice/src/loudness.rs`). Disabled by default: with
/// `enabled = false` the audio is sent as the engine says it (or with `gain`).
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LoudnessSettings {
    /// Turns the stage on. When it is on, `target` sets the level and `gain` is ignored.
    pub enabled: bool,
    /// The loudness the speech is brought to, in LUFS (K-weighted, gated, over the sentence so
    /// far). Streaming services normalise to about -14 to -16.
    pub target: f32,
    /// What no sample goes past, in dBFS.
    pub ceiling: f32,
    /// How far ahead of a peak the limiter starts lowering the gain, in milliseconds. It is also
    /// the delay the stage adds to the first sound.
    pub lookahead_ms: f32,
    /// Compression ratio above the threshold: 3 lets 3 dB over come out 1 dB over. 1 is none.
    pub ratio: f32,
    /// Where the compression starts, in dB above the speech level (not an absolute level, so it
    /// follows the engine and the `target`).
    pub threshold: f32,
    /// How fast the compressor lowers the gain, in milliseconds.
    pub attack_ms: f32,
    /// How fast the compressor lets the gain come back, in milliseconds.
    pub release_ms: f32,
}

impl Default for LoudnessSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            target: -16.0,
            ceiling: -1.0,
            lookahead_ms: 5.0,
            ratio: 3.0,
            threshold: 3.0,
            attack_ms: 5.0,
            release_ms: 80.0,
        }
    }
}

impl LoudnessSettings {
    fn check(&self) -> anyhow::Result<()> {
        let range = |name: &str, value: f32, low: f32, high: f32| {
            anyhow::ensure!(
                (low..=high).contains(&value),
                "loudness.{name} must be between {low} and {high}"
            );
            Ok(())
        };
        range("target", self.target, -40.0, -6.0)?;
        range("ceiling", self.ceiling, -12.0, -0.1)?;
        range("lookahead_ms", self.lookahead_ms, 1.0, 20.0)?;
        range("ratio", self.ratio, 1.0, 20.0)?;
        range("threshold", self.threshold, -20.0, 20.0)?;
        range("attack_ms", self.attack_ms, 0.1, 100.0)?;
        range("release_ms", self.release_ms, 1.0, 2000.0)?;
        anyhow::ensure!(
            self.target + 3.0 <= self.ceiling,
            "loudness.target ({}) must be at least 3 dB below loudness.ceiling ({}): speech cannot be that loud without being flattened",
            self.target,
            self.ceiling
        );
        Ok(())
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

fn default_gain() -> f32 {
    1.0
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
            (0.1..=8.0).contains(&config.gain),
            "gain must be between 0.1 and 8"
        );
        config.loudness.check()?;
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
        assert_eq!(c.gain, 1.0);
    }

    #[test]
    fn the_gain_is_set_and_checked() {
        let base = "nats_url = \"a\"\nengine_url = \"b\"\n";
        let c: Config = format!("{base}gain = 2.5\n").parse().unwrap();
        assert_eq!(c.gain, 2.5);
        for ok in ["gain = 0.1", "gain = 8", "gain = 1"] {
            assert!(format!("{base}{ok}\n").parse::<Config>().is_ok(), "{ok}");
        }
        for bad in [
            "gain = 0",
            "gain = -1",
            "gain = 0.09",
            "gain = 8.5",
            "gain = nan",
            "gain = \"2\"",
        ] {
            assert!(format!("{base}{bad}\n").parse::<Config>().is_err(), "{bad}");
        }
    }

    #[test]
    fn the_loudness_stage_is_off_by_default_and_its_keys_are_set_and_checked() {
        let base = "nats_url = \"a\"\nengine_url = \"b\"\n";
        let c: Config = base.parse().unwrap();
        assert_eq!(c.loudness, super::LoudnessSettings::default());
        assert!(!c.loudness.enabled);
        assert_eq!((c.loudness.target, c.loudness.ceiling), (-16.0, -1.0));
        let c: Config = format!(
            "{base}gain = 2\n[loudness]\nenabled = true\ntarget = -14\nceiling = -2\nlookahead_ms = 8\nratio = 2.5\nthreshold = 0\nattack_ms = 3\nrelease_ms = 120\n"
        )
        .parse()
        .unwrap();
        assert_eq!(
            c.loudness,
            super::LoudnessSettings {
                enabled: true,
                target: -14.0,
                ceiling: -2.0,
                lookahead_ms: 8.0,
                ratio: 2.5,
                threshold: 0.0,
                attack_ms: 3.0,
                release_ms: 120.0,
            }
        );
        assert_eq!(c.gain, 2.0, "the gain is kept");
        // One key alone leaves the others at their defaults.
        let c: Config = format!("{base}[loudness]\nenabled = true\n")
            .parse()
            .unwrap();
        assert_eq!(
            c.loudness,
            super::LoudnessSettings {
                enabled: true,
                ..Default::default()
            }
        );
        for ok in [
            "ratio = 1",
            "ratio = 20",
            "target = -40",
            "ceiling = -0.1",
            "lookahead_ms = 20",
        ] {
            assert!(
                format!("{base}[loudness]\n{ok}\n")
                    .parse::<Config>()
                    .is_ok(),
                "{ok}"
            );
        }
        for bad in [
            "target = -5",
            "target = -41",
            "target = 0",
            "ceiling = 0",
            "ceiling = -13",
            "ceiling = -20\ntarget = -18",
            "target = -10\nceiling = -11",
            "lookahead_ms = 0.5",
            "lookahead_ms = 25",
            "ratio = 0.5",
            "ratio = 21",
            "threshold = 25",
            "attack_ms = 0",
            "release_ms = 3000",
            "release_ms = -1",
            "target = nan",
            "enabled = \"yes\"",
            "nope = 1",
        ] {
            assert!(
                format!("{base}[loudness]\n{bad}\n")
                    .parse::<Config>()
                    .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_loudness_error_names_the_key_and_the_range() {
        let base = "nats_url = \"a\"\nengine_url = \"b\"\n";
        let message = |text: &str| {
            format!(
                "{:#}",
                format!("{base}{text}").parse::<Config>().unwrap_err()
            )
        };
        assert!(
            message("[loudness]\nratio = 50\n").contains("loudness.ratio must be between 1 and 20")
        );
        let m = message("[loudness]\ntarget = -10\nceiling = -11\n");
        assert!(
            m.contains("loudness.target") && m.contains("loudness.ceiling") && m.contains("3 dB"),
            "{m}"
        );
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
    fn the_development_configurations_are_valid() {
        for text in [
            include_str!("../../dev/voice.toml"),
            include_str!("../../dev/voice-kyutai.toml"),
        ] {
            text.parse::<Config>().unwrap();
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
