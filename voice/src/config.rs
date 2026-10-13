//! The configuration: a TOML file given as argument.

use std::{path::Path, path::PathBuf, sync::Arc, time::Duration};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::text::Lexicon;

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
/// # lexicon_enabled = true           # the pronunciation lexicon and the spelled acronyms
/// # lexicon = "lexicon.fr.toml"      # its file; the built-in one without it (relative: from this file)
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
    /// The pronunciation stage (the lexicon and the spelled acronyms of `text.rs`) is on. `false`
    /// leaves the words as the mind wrote them.
    #[serde(default = "default_lexicon_enabled")]
    pub lexicon_enabled: bool,
    /// The lexicon file (`data/lexicon.fr.toml` is the format); the built-in lexicon without it. A
    /// file given replaces the built-in one. `~/` stands for the home folder and a relative path is
    /// read from the folder of the configuration file, as `hearing` does.
    pub lexicon: Option<PathBuf>,
    /// The lexicon, read when the configuration is: not a key of the file.
    #[serde(skip)]
    pub text: Arc<Lexicon>,
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

fn default_gain() -> f32 {
    1.0
}

fn default_lexicon_enabled() -> bool {
    true
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new(""));
        Self::parse_in(&text, base).with_context(|| format!("configuration {}", path.display()))
    }

    /// Parses a configuration whose relative paths start at `base`.
    pub fn parse_in(s: &str, base: &Path) -> anyhow::Result<Self> {
        let mut config = Self::parse(s)?;
        config.text = Arc::new(config.read_lexicon(base)?);
        Ok(config)
    }

    /// The lexicon of this configuration: off, the file given or the built-in one.
    fn read_lexicon(&self, base: &Path) -> anyhow::Result<Lexicon> {
        if !self.lexicon_enabled {
            return Ok(Lexicon::disabled());
        }
        match &self.lexicon {
            None => Lexicon::builtin(),
            Some(path) => {
                let home = std::env::var_os("HOME");
                let path = match (path.strip_prefix("~"), home) {
                    (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
                    _ => base.join(path),
                };
                Lexicon::read(&path).context("lexicon")
            }
        }
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
    /// A configuration whose relative paths start at the working directory; [`Config::load`] reads
    /// them from the folder of the file.
    fn from_str(s: &str) -> anyhow::Result<Self> {
        Self::parse_in(s, Path::new(""))
    }
}

impl Config {
    /// The keys, checked; the lexicon is not read yet (its `text` is the stage off).
    fn parse(s: &str) -> anyhow::Result<Self> {
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
        // Read as the program reads them: the lexicon file is found from their folder.
        for name in ["voice.toml", "voice-kyutai.toml"] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../dev")
                .join(name);
            let c = Config::load(&path).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert!(c.text.is_enabled(), "{name}: the stage is on");
        }
    }

    #[test]
    fn the_lexicon_stage_is_on_with_the_built_in_lexicon_by_default() {
        let base = "nats_url = \"a\"\nengine_url = \"b\"\n";
        let c: Config = base.parse().unwrap();
        assert!(c.lexicon_enabled && c.lexicon.is_none());
        assert!(c.text.is_enabled());
        assert_eq!(c.text.apply("SSH"), "èsse èsse ache");
        assert_eq!(c.text.len(), crate::text::Lexicon::builtin().unwrap().len());
    }

    #[test]
    fn the_lexicon_stage_can_be_switched_off() {
        let base = "nats_url = \"a\"\nengine_url = \"b\"\n";
        // Off: the file is not even read.
        let c: Config = format!("{base}lexicon_enabled = false\nlexicon = \"/nonexistent.toml\"\n")
            .parse()
            .unwrap();
        assert!(!c.text.is_enabled());
        assert_eq!(c.text.apply("SSH GitHub"), "SSH GitHub");
        assert!(
            format!("{base}lexicon_enabled = \"no\"\n")
                .parse::<Config>()
                .is_err()
        );
    }

    /// A folder of the system's temporary one, removed with the test.
    struct Folder(std::path::PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("voice-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_lexicon_file_replaces_the_built_in_one_and_is_found_from_the_configuration() {
        let folder = Folder::new("lexicon-file");
        std::fs::write(
            folder.0.join("words.toml"),
            "[replace]\nGitHub = \"guite\"\n",
        )
        .unwrap();
        let base = "nats_url = \"a\"\nengine_url = \"b\"\n";
        // Relative: from the folder given (the one of the configuration file).
        let c = Config::parse_in(&format!("{base}lexicon = \"words.toml\"\n"), &folder.0).unwrap();
        assert_eq!(c.text.len(), 1);
        assert_eq!(c.text.apply("GitHub Docker"), "guite Docker");
        // Absolute.
        let absolute = format!("{base}lexicon = {:?}\n", folder.0.join("words.toml"));
        let c = Config::parse_in(&absolute, std::path::Path::new("/elsewhere")).unwrap();
        assert_eq!(c.text.apply("GitHub"), "guite");
        // `Config::load` does the same from the path of the configuration.
        std::fs::write(
            folder.0.join("voice.toml"),
            format!("{base}lexicon = \"words.toml\"\n"),
        )
        .unwrap();
        let c = Config::load(&folder.0.join("voice.toml")).unwrap();
        assert_eq!(c.text.apply("GitHub"), "guite");
    }

    #[test]
    fn a_bad_lexicon_stops_the_start_with_a_message_that_says_why() {
        let folder = Folder::new("lexicon-bad");
        std::fs::write(folder.0.join("bad.toml"), "[replace]\nGitHub = 3\n").unwrap();
        std::fs::write(
            folder.0.join("empty-word.toml"),
            "[replace]\nGitHub = \" \"\n",
        )
        .unwrap();
        let base = "nats_url = \"a\"\nengine_url = \"b\"\n";
        for (file, hint) in [
            ("missing.toml", "missing.toml"),
            ("bad.toml", "expected a string"),
            ("empty-word.toml", "written form is empty"),
        ] {
            let error =
                Config::parse_in(&format!("{base}lexicon = \"{file}\"\n"), &folder.0).unwrap_err();
            let error = format!("{error:#}");
            assert!(
                error.contains(hint) && error.contains(file),
                "{file}: {error}"
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
