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

/// See `examples/mind.toml`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub nats: NatsConfig,
    pub llm: LlmConfig,
    #[serde(default)]
    pub conversation: ConversationConfig,
    pub hippocampe: HippocampeConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    /// The internal entry: Emory speaking on her own when a program asks (`specs/events.md`).
    #[serde(default)]
    pub events: EventsConfig,
    /// Who Emory talks to. Absent: the instructions name nobody.
    #[serde(default)]
    pub owner: OwnerConfig,
    #[serde(default)]
    pub dictionary: DictionaryConfig,
    /// What the owner hears while the answer is prepared. Absent: nothing.
    #[serde(default)]
    pub waiting: WaitingConfig,
    /// The web tools (`web_weather`, `web_wikipedia`…). On by default.
    #[serde(default)]
    pub web: crate::web::WebConfig,
    /// Where `mind` keeps what must outlive it.
    #[serde(default)]
    pub data: crate::time::DataConfig,
    /// The timers and reminders (`time_timer`, `time_reminder`…). On by default.
    #[serde(default)]
    pub time: crate::time::TimeConfig,
}

/// The person Emory serves. Both fields fill the placeholders of the instructions
/// (`{owner}` and `{address}`, see [`crate::prompt`]); nothing about the owner is in the code.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerConfig {
    /// The name the owner goes by. Without it, the instructions say « la personne qui te parle ».
    pub name: Option<String>,
    /// How to speak to the owner, free text in the language of the instructions (« un homme ;
    /// tu le tutoies »). Without it, the instructions only say to use « tu ».
    pub address: Option<String>,
}

/// The connection to the bus. Development uses neither `ca` nor `nkey`; the locked-down server
/// uses both.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NatsConfig {
    #[serde(default = "default_nats_url")]
    pub url: String,
    /// The certificate of the authority that signed the NATS server's (TLS is required if given).
    pub ca: Option<ConfigPath>,
    /// The file with the seed of this program's nkey.
    pub nkey: Option<ConfigPath>,
}

fn default_nats_url() -> String {
    "nats://127.0.0.1:4222".to_owned()
}

impl Default for NatsConfig {
    fn default() -> Self {
        Self {
            url: default_nats_url(),
            ca: None,
            nkey: None,
        }
    }
}

/// How `mind` talks to the LLM: through an ACP agent it starts as a child process, or straight
/// to a provider's API.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LlmMode {
    #[default]
    Acp,
    Api,
}

/// Whether the LLM thinks before it answers: not at all, as much as it judges useful, or within
/// a budget of tokens. Written `"off"`, `"adaptive"` or a number in the file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Thinking {
    #[default]
    Off,
    Adaptive,
    Budget(u32),
}

impl<'de> Deserialize<'de> for Thinking {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Word(String),
            Budget(u32),
        }
        match Raw::deserialize(d)? {
            Raw::Word(w) if w == "off" => Ok(Self::Off),
            Raw::Word(w) if w == "adaptive" => Ok(Self::Adaptive),
            Raw::Word(w) => Err(serde::de::Error::custom(format!(
                "thinking is \"off\", \"adaptive\" or a budget of tokens, not \"{w}\""
            ))),
            Raw::Budget(n) => Ok(Self::Budget(n)),
        }
    }
}

/// The LLM. `mode = "acp"` (the default): an ACP adapter that `mind` starts and drives over its
/// standard input and output (`command`, `args`). `mode = "api"`: a provider's API, called by
/// `mind` itself (`provider`, `key_file`, `base_url`, `max_tokens`, `cache`). See
/// `examples/mind.toml`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
    #[serde(default)]
    pub mode: LlmMode,
    /// acp: the adapter's program (`node`, `npx`).
    pub command: Option<String>,
    /// acp: its arguments (the path of `claude-agent-acp`'s `dist/index.js`).
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_model")]
    pub model: String,
    /// The effort asked of the model (`low`, `medium`, `high`…). acp: `low` when absent; api: not
    /// sent when absent.
    pub effort: Option<String>,
    #[serde(default)]
    pub thinking: Thinking,
    /// Fast mode (Anthropic, Opus models only, billed at a premium: see `specs/mind.md`).
    #[serde(default)]
    pub fast: bool,
    /// api: the provider (see [`crate::api::PROVIDERS`]).
    pub provider: Option<String>,
    /// api: the file that holds the key, read at each request, never logged.
    pub key_file: Option<ConfigPath>,
    /// api: another address for the provider's API (an OpenAI-compatible server, a proxy).
    pub base_url: Option<String>,
    /// api: the longest answer, in tokens, thinking included (2048 when absent).
    pub max_tokens: Option<u32>,
    /// api: prompt caching, for the providers that have it (on when absent).
    pub cache: Option<bool>,
    /// The French instructions of the LLM.
    pub system_prompt: ConfigPath,
    /// acp: the working folder of the sessions. Claude Code has no tool of its own, so it never
    /// touches it; by default a folder of the system's temporary directory.
    pub workdir: Option<ConfigPath>,
}

fn default_model() -> String {
    "claude-haiku-5-5".to_owned()
}

/// The models that have a fast mode, by Anthropic's documentation read on 2026-10-13.
pub const FAST_MODELS: [&str; 3] = ["claude-opus-5-5", "claude-opus-5", "claude-opus-4-8"];

fn has_fast_mode(model: &str) -> bool {
    FAST_MODELS
        .iter()
        .any(|m| model == *m || model.strip_prefix(m).is_some_and(|r| r.starts_with('-')))
}

/// The efforts a provider is asked for.
const EFFORTS: [&str; 6] = ["minimal", "low", "medium", "high", "xhigh", "max"];

impl LlmConfig {
    /// The effort sent through ACP.
    pub fn acp_effort(&self) -> String {
        self.effort.clone().unwrap_or_else(|| "low".to_owned())
    }

    /// Checks that the options fit the mode, before anything starts.
    fn check(&self) -> Result<()> {
        let unused = |options: &[(&str, bool)], mode: &str| -> Result<()> {
            for (name, given) in options {
                anyhow::ensure!(!given, "llm.{name} has no meaning with mode = \"{mode}\"");
            }
            Ok(())
        };
        anyhow::ensure!(!self.model.trim().is_empty(), "llm.model must not be blank");
        if let Thinking::Budget(n) = self.thinking {
            anyhow::ensure!(n > 0, "llm.thinking: a budget must be above 0 (or \"off\")");
        }
        if self.fast {
            anyhow::ensure!(
                has_fast_mode(&self.model),
                "llm.fast: {} has no fast mode (only {})",
                self.model,
                FAST_MODELS.join(", ")
            );
        }
        match self.mode {
            LlmMode::Acp => {
                anyhow::ensure!(
                    self.command
                        .as_deref()
                        .is_some_and(|c| !c.trim().is_empty()),
                    "llm.command is needed with mode = \"acp\" (the adapter's program)"
                );
                unused(
                    &[
                        ("provider", self.provider.is_some()),
                        ("key_file", self.key_file.is_some()),
                        ("base_url", self.base_url.is_some()),
                        ("max_tokens", self.max_tokens.is_some()),
                        ("cache", self.cache.is_some()),
                    ],
                    "acp",
                )?;
            }
            LlmMode::Api => {
                unused(
                    &[
                        ("command", self.command.is_some()),
                        ("args", !self.args.is_empty()),
                        ("workdir", self.workdir.is_some()),
                    ],
                    "api",
                )?;
                let name = self
                    .provider
                    .as_deref()
                    .context("llm.provider is needed with mode = \"api\"")?;
                let provider = crate::api::Provider::named(name).with_context(|| {
                    format!(
                        "llm.provider: unknown provider {name} (known: {})",
                        crate::api::PROVIDERS
                            .iter()
                            .map(|p| p.name)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?;
                anyhow::ensure!(
                    provider.endpoint.is_some() || self.base_url.is_some(),
                    "llm.base_url is needed with provider {name}"
                );
                if let Some(url) = &self.base_url {
                    anyhow::ensure!(
                        (url.starts_with("https://") || url.starts_with("http://"))
                            && url.ends_with('/'),
                        "llm.base_url must be an http(s) address ending with / (like https://host/v1/)"
                    );
                }
                anyhow::ensure!(
                    self.key_file.is_some() || !provider.needs_key,
                    "llm.key_file is needed with provider {name}"
                );
                anyhow::ensure!(
                    self.max_tokens.is_none_or(|n| (64..=64_000).contains(&n)),
                    "llm.max_tokens must be between 64 and 64000"
                );
                if let Some(effort) = &self.effort {
                    anyhow::ensure!(
                        EFFORTS.contains(&effort.as_str()),
                        "llm.effort is one of {}",
                        EFFORTS.join(", ")
                    );
                    anyhow::ensure!(
                        provider.anthropic || self.thinking == Thinking::Adaptive,
                        "llm.effort: with provider {name}, the effort is the depth of thinking: \
                         it needs thinking = \"adaptive\""
                    );
                }
                if !provider.anthropic {
                    anyhow::ensure!(
                        self.thinking != Thinking::Adaptive || self.effort.is_some(),
                        "llm.thinking = \"adaptive\" needs llm.effort with provider {name}"
                    );
                }
                if provider.anthropic
                    && let Thinking::Budget(n) = self.thinking
                {
                    anyhow::ensure!(
                        n >= 1024,
                        "llm.thinking: Anthropic wants a budget of at least 1024 tokens"
                    );
                }
                anyhow::ensure!(
                    !self.fast || provider.anthropic,
                    "llm.fast: only Anthropic has a fast mode"
                );
            }
        }
        Ok(())
    }

    /// Checks the files the LLM needs at start-up: the key file of the API. Its content is not
    /// read here.
    pub fn check_files(&self) -> Result<()> {
        if self.mode == LlmMode::Api
            && let Some(key) = &self.key_file
        {
            anyhow::ensure!(
                key.0.is_file(),
                "llm.key_file: no file at {}",
                key.0.display()
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationConfig {
    /// Minutes without an exchange before the conversation closes.
    #[serde(default = "default_idle_minutes")]
    pub idle_minutes: f32,
    /// What Emory says when a turn reaches her with nothing in it (near-silence heard): no
    /// request to the LLM.
    #[serde(default = "default_nothing_heard")]
    pub nothing_heard: String,
}

fn default_nothing_heard() -> String {
    "Je n'ai rien entendu.".to_owned()
}

fn default_idle_minutes() -> f32 {
    30.0
}

impl Default for ConversationConfig {
    fn default() -> Self {
        Self {
            idle_minutes: default_idle_minutes(),
            nothing_heard: default_nothing_heard(),
        }
    }
}

impl ConversationConfig {
    pub fn idle_timeout(&self) -> Duration {
        Duration::from_secs_f32(self.idle_minutes * 60.0)
    }
}

/// Hippocampe, the long-term memory, reached by MCP.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HippocampeConfig {
    /// The MCP endpoint (`https://hippocampe.example/mcp`).
    pub url: String,
    /// The file that holds the key. Read when the connection opens, never logged.
    pub token_file: ConfigPath,
    /// How many entries the search made on each sentence keeps.
    #[serde(default = "default_search_limit")]
    pub search_limit: u32,
    /// Milliseconds after which the search on a sentence is given up for this turn.
    #[serde(default = "default_search_timeout_ms")]
    pub search_timeout_ms: u64,
    /// Hippocampe's tools offered to the LLM, by their name at Hippocampe (those the key cannot
    /// use are not listed by Hippocampe, and are left out).
    #[serde(default = "default_hippocampe_tools")]
    pub tools: Vec<String>,
    /// Milliseconds a tool call of the LLM may wait for Hippocampe before it is an error.
    #[serde(default = "default_tool_timeout_ms")]
    pub tool_timeout_ms: u64,
}

fn default_search_limit() -> u32 {
    5
}

fn default_search_timeout_ms() -> u64 {
    300
}

fn default_tool_timeout_ms() -> u64 {
    10_000
}

fn default_hippocampe_tools() -> Vec<String> {
    ["search", "read", "write", "link", "types"]
        .map(str::to_owned)
        .to_vec()
}

impl HippocampeConfig {
    pub fn search_timeout(&self) -> Duration {
        Duration::from_millis(self.search_timeout_ms)
    }

    pub fn tool_timeout(&self) -> Duration {
        Duration::from_millis(self.tool_timeout_ms)
    }
}

/// Emory's MCP server, served by `mind` to the LLM.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    /// Where the server listens: always the loopback, port 0 for any free port.
    #[serde(default = "default_bind")]
    pub bind: std::net::SocketAddr,
}

fn default_bind() -> std::net::SocketAddr {
    ([127, 0, 0, 1], 0).into()
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
        }
    }
}

/// The dictionary: the LLM may add a word to the vocabulary of the hearing, once the user said
/// yes (`dictionary_propose`, then `dictionary_add`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DictionaryConfig {
    /// Whether the LLM gets the two tools.
    pub enabled: bool,
    /// Milliseconds to wait for the hearing's reply before telling the LLM it failed.
    pub timeout_ms: u64,
}

impl Default for DictionaryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_ms: 2000,
        }
    }
}

impl DictionaryConfig {
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }
}

/// The internal entry (`specs/events.md`). Only what Emory does before speaking on her own for
/// now; the sources come with the entry itself.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EventsConfig {
    /// A short tone before speaking (`Cue` `tone`): a voice that comes out of nowhere startles.
    pub chime: bool,
}

impl Default for EventsConfig {
    fn default() -> Self {
        Self { chime: true }
    }
}

/// The waiting cue (see [`crate::waiting`]): `off`, a `tone`, or a short phrase (`speech`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WaitingConfig {
    pub mode: crate::waiting::Mode,
    /// `tone`: milliseconds without an answer before the soft pulse starts; `speech` with
    /// `pulse_after_speech`: milliseconds without an answer after a phrase is over.
    pub pulse_delay_ms: u64,
    /// `speech`: milliseconds without an answer before a pre-written phrase.
    pub speech_delay_ms: u64,
    /// `speech`: pre-written phrases only, ones about what is searched only, or both.
    pub source: crate::waiting::Source,
    /// `speech`: the most phrases said in a turn.
    pub max_phrases: u32,
    /// `speech`: the least milliseconds between two phrases of a turn.
    pub phrase_spacing_ms: u64,
    /// `speech`: whether the spacing is counted from the `start` of the previous phrase or from
    /// its `end` (estimated).
    pub phrase_spacing_from: crate::waiting::SpacingFrom,
    /// `speech`: the soft pulse fills the silence after a phrase until the answer.
    pub pulse_after_speech: bool,
    /// `speech`: another pre-written phrase after this many more milliseconds without an answer
    /// (0: none).
    pub prewritten_repeat_ms: u64,
    /// `speech`: the file of the phrases; the built-in French ones without it.
    pub phrases: Option<ConfigPath>,
}

impl Default for WaitingConfig {
    fn default() -> Self {
        let defaults = crate::waiting::Settings::off();
        Self {
            mode: crate::waiting::Mode::Off,
            pulse_delay_ms: defaults.pulse_delay.as_millis() as u64,
            speech_delay_ms: defaults.speech_delay.as_millis() as u64,
            source: defaults.source,
            max_phrases: defaults.max_phrases,
            phrase_spacing_ms: defaults.phrase_spacing.as_millis() as u64,
            phrase_spacing_from: defaults.spacing_from,
            pulse_after_speech: defaults.pulse_after_speech,
            prewritten_repeat_ms: defaults.prewritten_repeat.as_millis() as u64,
            phrases: None,
        }
    }
}

impl WaitingConfig {
    pub fn settings(&self) -> crate::waiting::Settings {
        crate::waiting::Settings {
            mode: self.mode,
            pulse_delay: Duration::from_millis(self.pulse_delay_ms),
            speech_delay: Duration::from_millis(self.speech_delay_ms),
            source: self.source,
            max_phrases: self.max_phrases,
            phrase_spacing: Duration::from_millis(self.phrase_spacing_ms),
            spacing_from: self.phrase_spacing_from,
            pulse_after_speech: self.pulse_after_speech,
            prewritten_repeat: Duration::from_millis(self.prewritten_repeat_ms),
        }
    }

    /// Refuses absurd values.
    pub fn check(&self) -> Result<()> {
        anyhow::ensure!(
            (100..=60_000).contains(&self.pulse_delay_ms),
            "waiting.pulse_delay_ms must be between 100 and 60000"
        );
        anyhow::ensure!(
            (100..=60_000).contains(&self.speech_delay_ms),
            "waiting.speech_delay_ms must be between 100 and 60000"
        );
        anyhow::ensure!(
            (1..=5).contains(&self.max_phrases),
            "waiting.max_phrases must be between 1 and 5"
        );
        anyhow::ensure!(
            self.phrase_spacing_ms <= 60_000,
            "waiting.phrase_spacing_ms must be between 0 and 60000"
        );
        anyhow::ensure!(
            self.prewritten_repeat_ms == 0 || (100..=60_000).contains(&self.prewritten_repeat_ms),
            "waiting.prewritten_repeat_ms must be 0 (none) or between 100 and 60000"
        );
        // Keys of the `speech` mode that would do nothing elsewhere are a mistake, said loudly.
        if self.mode != crate::waiting::Mode::Speech {
            anyhow::ensure!(
                !self.pulse_after_speech,
                "waiting.pulse_after_speech only applies to mode = \"speech\" (the `tone` mode always pulses)"
            );
            anyhow::ensure!(
                self.prewritten_repeat_ms == 0,
                "waiting.prewritten_repeat_ms only applies to mode = \"speech\""
            );
        } else {
            anyhow::ensure!(
                self.prewritten_repeat_ms == 0 || self.source != crate::waiting::Source::Contextual,
                "waiting.prewritten_repeat_ms needs source = \"prewritten\" or \"both\""
            );
        }
        Ok(())
    }

    /// The waiter of this configuration, with its phrases read.
    pub fn waiter(&self) -> Result<crate::waiting::Waiter> {
        self.check()?;
        let settings = self.settings();
        let phrases = match settings.mode {
            crate::waiting::Mode::Speech => crate::waiting::Phrases::load(self.phrases.as_ref())?,
            _ => crate::waiting::Phrases::default(),
        };
        phrases.check_for(&settings)?;
        Ok(crate::waiting::Waiter::new(settings, phrases))
    }
}

/// Whether a text has anything to say or to be said: a letter or a digit, not only blanks and
/// punctuation.
pub fn has_words(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

impl Config {
    pub fn read(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        text.parse()
            .with_context(|| format!("configuration {}", path.display()))
    }
}

impl std::str::FromStr for Config {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        let config: Config = toml::from_str(s)?;
        config.llm.check()?;
        anyhow::ensure!(
            config.conversation.idle_minutes > 0.0
                && config.conversation.idle_minutes <= 24.0 * 60.0,
            "conversation.idle_minutes must be above 0 and at most a day"
        );
        anyhow::ensure!(
            has_words(&config.conversation.nothing_heard),
            "conversation.nothing_heard must be a sentence to say"
        );
        anyhow::ensure!(
            (1..=20).contains(&config.hippocampe.search_limit),
            "hippocampe.search_limit must be between 1 and 20"
        );
        anyhow::ensure!(
            (1..=5000).contains(&config.hippocampe.search_timeout_ms),
            "hippocampe.search_timeout_ms must be between 1 and 5000"
        );
        anyhow::ensure!(
            (100..=120_000).contains(&config.hippocampe.tool_timeout_ms),
            "hippocampe.tool_timeout_ms must be between 100 and 120000"
        );
        for (key, value) in [
            ("owner.name", &config.owner.name),
            ("owner.address", &config.owner.address),
        ] {
            anyhow::ensure!(
                value.as_deref().is_none_or(|v| !v.trim().is_empty()),
                "{key} must not be blank (leave it out instead)"
            );
        }
        anyhow::ensure!(
            (100..=30_000).contains(&config.dictionary.timeout_ms),
            "dictionary.timeout_ms must be between 100 and 30000"
        );
        config.waiting.check()?;
        config.web.check()?;
        config.data.check()?;
        config.time.check()?;
        anyhow::ensure!(
            config.tools.bind.ip().is_loopback(),
            "tools.bind must be a loopback address: the server has no other protection than its key"
        );
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
[llm]
command = "node"
system_prompt = "prompts/system.fr.md"

[hippocampe]
url = "https://h/mcp"
token_file = "~/t.token"
"#;

    #[test]
    fn minimal_file_gets_the_defaults() {
        let c: Config = MINIMAL.parse().unwrap();
        assert_eq!(c.nats.url, "nats://127.0.0.1:4222");
        assert_eq!(c.llm.model, "claude-haiku-5-5");
        assert_eq!(c.llm.mode, LlmMode::Acp);
        assert_eq!(c.llm.acp_effort(), "low");
        assert_eq!(c.llm.thinking, Thinking::Off);
        assert!(!c.llm.fast);
        assert!(c.llm.args.is_empty() && c.llm.workdir.is_none());
        assert_eq!(c.conversation.idle_timeout(), Duration::from_secs(1800));
        assert_eq!(c.conversation.nothing_heard, "Je n'ai rien entendu.");
        assert_eq!(c.hippocampe.search_limit, 5);
        assert_eq!(c.hippocampe.search_timeout(), Duration::from_millis(300));
        assert_eq!(c.hippocampe.tool_timeout(), Duration::from_secs(10));
        assert!(c.hippocampe.tools.contains(&"search".to_owned()));
        assert!(c.tools.bind.ip().is_loopback());
        assert!(c.owner.name.is_none() && c.owner.address.is_none());
        assert!(c.dictionary.enabled);
        assert_eq!(c.dictionary.timeout(), Duration::from_secs(2));
        assert!(c.events.chime);
    }

    #[test]
    fn the_tone_before_speaking_on_her_own_can_be_turned_off() {
        let c: Config = format!("{MINIMAL}\n[events]\nchime = false\n")
            .parse()
            .unwrap();
        assert!(!c.events.chime);
        assert!(
            format!("{MINIMAL}\n[events]\nchimes = true\n")
                .parse::<Config>()
                .is_err()
        );
    }

    #[test]
    fn the_owner_section_gives_a_name_and_how_to_address_them() {
        let c: Config = format!(
            "{MINIMAL}\n[owner]\nname = \"Alex\"\naddress = \"une femme ; tu la tutoies\"\n"
        )
        .parse()
        .unwrap();
        assert_eq!(c.owner.name.as_deref(), Some("Alex"));
        assert_eq!(
            c.owner.address.as_deref(),
            Some("une femme ; tu la tutoies")
        );
        let c: Config = format!("{MINIMAL}\n[owner]\nname = \"Alex\"\n")
            .parse()
            .unwrap();
        assert!(c.owner.address.is_none());
        for bad in ["name = \" \"", "address = \"\"", "nick = \"x\""] {
            assert!(
                format!("{MINIMAL}\n[owner]\n{bad}\n")
                    .parse::<Config>()
                    .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_waiting_cue_is_off_by_default_and_set_by_its_section() {
        use crate::waiting::{Mode, Source};
        let c: Config = MINIMAL.parse().unwrap();
        assert_eq!(c.waiting.mode, Mode::Off);
        assert!(c.waiting.waiter().is_ok());
        let c: Config = format!("{MINIMAL}\n[waiting]\nmode = \"tone\"\n")
            .parse()
            .unwrap();
        assert_eq!(c.waiting.mode, Mode::Tone);
        assert_eq!(
            c.waiting.settings().pulse_delay,
            Duration::from_millis(1500)
        );
        let c: Config = format!(
            "{MINIMAL}\n[waiting]\nmode = \"speech\"\nsource = \"contextual\"\nspeech_delay_ms = 1200\nmax_phrases = 1\n"
        )
        .parse()
        .unwrap();
        let settings = c.waiting.settings();
        assert_eq!(
            (settings.mode, settings.source, settings.max_phrases),
            (Mode::Speech, Source::Contextual, 1)
        );
        assert_eq!(settings.speech_delay, Duration::from_millis(1200));
        // Speech without a file takes the built-in phrases.
        assert!(c.waiting.waiter().is_ok());
        for bad in [
            "mode = \"beep\"",
            "pulse_delay_ms = 5",
            "speech_delay_ms = 999999",
            "max_phrases = 0",
            "source = \"random\"",
            "volume = 1",
        ] {
            assert!(
                format!("{MINIMAL}\n[waiting]\n{bad}\n")
                    .parse::<Config>()
                    .is_err(),
                "{bad}"
            );
        }
        // A phrases file that does not exist is an error at start-up, in speech mode only.
        let missing: Config = format!(
            "{MINIMAL}\n[waiting]\nmode = \"speech\"\nphrases = \"/nonexistent/phrases.toml\"\n"
        )
        .parse()
        .unwrap();
        assert!(missing.waiting.waiter().is_err());
        let unused: Config = format!(
            "{MINIMAL}\n[waiting]\nmode = \"tone\"\nphrases = \"/nonexistent/phrases.toml\"\n"
        )
        .parse()
        .unwrap();
        assert!(unused.waiting.waiter().is_ok());
    }

    #[test]
    fn the_keys_that_fill_the_silence_are_set_and_checked() {
        use crate::waiting::{Mode, SpacingFrom};
        let c: Config = format!(
            "{MINIMAL}\n[waiting]\nmode = \"speech\"\nphrase_spacing_ms = 800\nphrase_spacing_from = \"end\"\npulse_after_speech = true\npulse_delay_ms = 600\nprewritten_repeat_ms = 4000\n"
        )
        .parse()
        .unwrap();
        let settings = c.waiting.settings();
        assert_eq!(settings.mode, Mode::Speech);
        assert_eq!(settings.phrase_spacing, Duration::from_millis(800));
        assert_eq!(settings.spacing_from, SpacingFrom::End);
        assert!(settings.pulse_after_speech);
        assert_eq!(settings.prewritten_repeat, Duration::from_millis(4000));
        assert!(c.waiting.waiter().is_ok());
        // The defaults are the behaviour of before.
        let d = MINIMAL.parse::<Config>().unwrap().waiting.settings();
        assert_eq!(d.phrase_spacing, crate::waiting::PHRASE_SPACING);
        assert_eq!(d.spacing_from, SpacingFrom::Start);
        assert!(!d.pulse_after_speech && d.prewritten_repeat.is_zero());
        for (bad, why) in [
            (
                "mode = \"speech\"\nphrase_spacing_ms = 999999",
                "phrase_spacing_ms",
            ),
            (
                "mode = \"speech\"\nphrase_spacing_from = \"middle\"",
                "phrase_spacing_from",
            ),
            (
                "mode = \"speech\"\nprewritten_repeat_ms = 5",
                "prewritten_repeat_ms",
            ),
            (
                "mode = \"speech\"\nprewritten_repeat_ms = 999999",
                "prewritten_repeat_ms",
            ),
            (
                "mode = \"speech\"\nsource = \"contextual\"\nprewritten_repeat_ms = 3000",
                "prewritten_repeat_ms",
            ),
            (
                "mode = \"tone\"\npulse_after_speech = true",
                "pulse_after_speech",
            ),
            (
                "mode = \"off\"\nprewritten_repeat_ms = 3000",
                "prewritten_repeat_ms",
            ),
            (
                "mode = \"speech\"\npulse_after_speech = \"yes\"",
                "pulse_after_speech",
            ),
        ] {
            let error = format!("{MINIMAL}\n[waiting]\n{bad}\n")
                .parse::<Config>()
                .unwrap_err();
            assert!(format!("{error:#}").contains(why), "{bad}: {error:#}");
        }
    }

    #[test]
    fn the_shipped_configurations_are_valid() {
        use crate::waiting::Mode;
        let example: Config = include_str!("../examples/mind.toml").parse().unwrap();
        assert_eq!(example.waiting.mode, Mode::Off);
        // The example in each mode, with its commented keys switched on.
        let speech = include_str!("../examples/mind.toml")
            .replace("mode = \"off\"", "mode = \"speech\"")
            .replace("# speech_delay_ms", "speech_delay_ms")
            .replace("# source =", "source =")
            .replace("# max_phrases", "max_phrases")
            .replace("# phrase_spacing_ms", "phrase_spacing_ms")
            .replace("# phrase_spacing_from", "phrase_spacing_from")
            .replace("# pulse_after_speech", "pulse_after_speech")
            .replace("# prewritten_repeat_ms", "prewritten_repeat_ms")
            .replace("# phrases =", "phrases =");
        let speech: Config = speech.parse().unwrap();
        assert_eq!(speech.waiting.mode, Mode::Speech);
        assert_eq!(speech.waiting.max_phrases, 2);
        // The example's values are the defaults: switching its keys on changes nothing.
        let defaults = WaitingConfig::default();
        assert_eq!(speech.waiting.phrase_spacing_ms, defaults.phrase_spacing_ms);
        assert_eq!(
            speech.waiting.phrase_spacing_from,
            defaults.phrase_spacing_from
        );
        assert_eq!(
            speech.waiting.pulse_after_speech,
            defaults.pulse_after_speech
        );
        assert_eq!(
            speech.waiting.prewritten_repeat_ms,
            defaults.prewritten_repeat_ms
        );
        // The developer switches modes while trying them: only its validity is checked.
        let dev: Config = include_str!("../../dev/mind.toml").parse().unwrap();
        assert!(dev.web.enabled && dev.web.wikipedia.enabled && dev.web.weather.enabled);
        assert!(!dev.web.weather.place.is_empty());
        assert!(dev.time.enabled && !dev.time.timezone.is_empty());
        assert!(dev.data.dir().unwrap().ends_with("emory/mind-dev"));
        // (its phrases file is relative to the root of the repository: only the values are checked)
        assert!(dev.waiting.check().is_ok());
    }

    #[test]
    fn the_web_module_is_on_by_default_and_its_section_is_checked() {
        let c: Config = MINIMAL.parse().unwrap();
        assert!(c.web.enabled && c.web.wikipedia.enabled);
        assert_eq!(c.web.wikipedia.languages, ["fr", "en"]);
        assert!(c.web.weather.enabled && c.web.weather.place.is_empty());
        let c: Config =
            format!("{MINIMAL}\n[web]\nenabled = false\n[web.wikipedia]\nlanguages = [\"en\"]\n")
                .parse()
                .unwrap();
        assert!(!c.web.enabled && c.web.wikipedia.languages == ["en"]);
        for (bad, key) in [
            ("[web]\ntimeout_ms = 0", "web.timeout_ms"),
            ("[web.wikipedia]\nlanguages = []", "web.wikipedia.languages"),
            ("[web.wikipedia]\nmax_chars = 1", "web.wikipedia.max_chars"),
            ("[web]\nvolume = 3", "volume"),
            ("[web.weathr]\nplace = \"x\"", "weathr"),
        ] {
            let error = format!(
                "{:#}",
                format!("{MINIMAL}\n{bad}\n").parse::<Config>().unwrap_err()
            );
            assert!(error.contains(key), "{bad}: {error}");
        }
    }

    #[test]
    fn the_time_module_is_on_by_default_and_its_sections_are_checked() {
        let c: Config = MINIMAL.parse().unwrap();
        assert!(c.time.enabled && c.time.timezone.is_empty() && c.data.dir.is_none());
        assert_eq!(c.time.max_active, 50);
        let c: Config = format!(
            "{MINIMAL}\n[data]\ndir = \"/var/lib/emory\"\n[time]\nenabled = false\ntimezone = \"Europe/Paris\"\n"
        )
        .parse()
        .unwrap();
        assert!(!c.time.enabled);
        assert_eq!(c.data.dir().unwrap(), PathBuf::from("/var/lib/emory"));
        for (bad, key) in [
            ("[time]\ntimezone = \"Mars/Olympus\"", "time.timezone"),
            ("[time]\nmax_active = 0", "time.max_active"),
            ("[time]\nrepeat = 9", "time.repeat"),
            ("[time]\nreminder = \"Rappel.\"", "time.reminder"),
            ("[data]\ndir = \"\"", "data.dir"),
            ("[data]\nfolder = \"x\"", "folder"),
            ("[time]\nvolume = 3", "volume"),
            ("[timer]\nenabled = true", "timer"),
        ] {
            let error = format!(
                "{:#}",
                format!("{MINIMAL}\n{bad}\n").parse::<Config>().unwrap_err()
            );
            assert!(error.contains(key), "{bad}: {error}");
        }
    }

    #[test]
    fn only_letters_and_digits_count_as_words() {
        assert!(has_words("Je n'ai rien entendu."));
        assert!(has_words("2"));
        assert!(!has_words(""));
        assert!(!has_words(" \n\t"));
        assert!(!has_words(" ... ,; ? "));
    }

    #[test]
    fn the_home_folder_is_expanded() {
        assert_eq!(
            expand("~/a/b", Some("/home/k".into())),
            PathBuf::from("/home/k/a/b")
        );
        assert_eq!(expand("/a", Some("/home/k".into())), PathBuf::from("/a"));
    }

    #[test]
    fn absurd_values_and_unknown_keys_are_refused() {
        for bad in [
            "[conversation]\nidle_minutes = 0\n",
            "[conversation]\nnothing_heard = \" ... \"\n",
            "[tools]\nbind = \"0.0.0.0:0\"\n",
            "[dictionary]\ntimeout_ms = 0\n",
            "nope = 1\n",
        ] {
            assert!(
                format!("{bad}{MINIMAL}").parse::<Config>().is_err(),
                "{bad}"
            );
        }
        let limit = MINIMAL.replace("token_file", "search_limit = 0\ntoken_file");
        assert!(limit.parse::<Config>().is_err());
    }

    fn with_llm(llm: &str) -> Result<Config> {
        format!(
            "[llm]\nsystem_prompt = \"p.md\"\n{llm}\n[hippocampe]\nurl = \"https://h/mcp\"\ntoken_file = \"t\"\n"
        )
        .parse()
    }

    #[test]
    fn the_api_mode_takes_a_provider_a_key_and_its_options() {
        let c = with_llm(
            "mode = \"api\"\nprovider = \"anthropic\"\nkey_file = \"/k\"\nthinking = \"off\"\nmax_tokens = 1024\ncache = false",
        )
        .unwrap();
        assert_eq!(c.llm.mode, LlmMode::Api);
        assert_eq!(c.llm.provider.as_deref(), Some("anthropic"));
        assert_eq!(c.llm.key_file, Some(ConfigPath("/k".into())));
        assert_eq!(c.llm.max_tokens, Some(1024));
        assert_eq!(c.llm.cache, Some(false));
        let c = with_llm(
            "mode = \"api\"\nprovider = \"openai_compatible\"\nbase_url = \"https://h/v1/\"\nkey_file = \"/k\"\nmodel = \"m\"\nthinking = 2048\n",
        )
        .unwrap();
        assert_eq!(c.llm.thinking, Thinking::Budget(2048));
        let c = with_llm(
            "mode = \"api\"\nprovider = \"anthropic\"\nkey_file = \"/k\"\nmodel = \"claude-opus-5-5\"\nfast = true\nthinking = \"adaptive\"\neffort = \"low\"",
        )
        .unwrap();
        assert!(c.llm.fast && c.llm.thinking == Thinking::Adaptive);
        // The local server needs no key.
        assert!(with_llm("mode = \"api\"\nprovider = \"ollama\"\nmodel = \"m\"").is_ok());
        // Today's files, without a mode, are ACP.
        let c =
            with_llm("command = \"npx\"\neffort = \"medium\"\nthinking = \"adaptive\"").unwrap();
        assert_eq!(c.llm.mode, LlmMode::Acp);
        assert_eq!(c.llm.acp_effort(), "medium");
    }

    #[test]
    fn an_option_the_mode_cannot_honour_is_refused_with_its_name() {
        let api = "mode = \"api\"\nprovider = \"anthropic\"\nkey_file = \"/k\"\n";
        for (llm, says) in [
            ("mode = \"api\"\nkey_file = \"/k\"", "llm.provider"),
            (
                "mode = \"api\"\nprovider = \"nope\"\nkey_file = \"/k\"",
                "unknown provider nope",
            ),
            (
                "mode = \"api\"\nprovider = \"groq\"\nmodel = \"m\"",
                "llm.key_file",
            ),
            (
                "mode = \"api\"\nprovider = \"openai_compatible\"\nkey_file = \"/k\"",
                "llm.base_url",
            ),
            (&format!("{api}command = \"npx\""), "llm.command"),
            (&format!("{api}workdir = \"/w\""), "llm.workdir"),
            (&format!("{api}fast = true"), "no fast mode"),
            (&format!("{api}thinking = 512"), "1024"),
            (&format!("{api}thinking = \"maybe\""), "thinking"),
            (&format!("{api}effort = \"huge\""), "llm.effort"),
            (&format!("{api}base_url = \"https://h/v1\""), "llm.base_url"),
            (&format!("{api}max_tokens = 1"), "llm.max_tokens"),
            (
                "mode = \"api\"\nprovider = \"groq\"\nkey_file = \"/k\"\nmodel = \"m\"\neffort = \"low\"",
                "thinking = \"adaptive\"",
            ),
            (
                "mode = \"api\"\nprovider = \"groq\"\nkey_file = \"/k\"\nmodel = \"m\"\nthinking = \"adaptive\"",
                "needs llm.effort",
            ),
            (
                "mode = \"api\"\nprovider = \"groq\"\nkey_file = \"/k\"\nmodel = \"claude-opus-5-5\"\nfast = true",
                "only Anthropic",
            ),
            ("mode = \"acp\"", "llm.command"),
            (
                "command = \"npx\"\nprovider = \"anthropic\"",
                "llm.provider",
            ),
            ("command = \"npx\"\nkey_file = \"/k\"", "llm.key_file"),
            ("command = \"npx\"\nfast = true", "no fast mode"),
            ("command = \"npx\"\nmode = \"rest\"", "mode"),
        ] {
            let error = format!("{:#}", with_llm(llm).unwrap_err());
            assert!(error.contains(says), "{llm}: {error}");
        }
    }

    #[test]
    fn a_missing_key_file_is_found_at_start_up() {
        let c = with_llm(
            "mode = \"api\"\nprovider = \"anthropic\"\nkey_file = \"/nonexistent/emory/key\"",
        )
        .unwrap();
        let error = c.llm.check_files().unwrap_err().to_string();
        assert!(error.contains("/nonexistent/emory/key"), "{error}");
        let acp = with_llm("command = \"npx\"").unwrap();
        assert!(acp.llm.check_files().is_ok());
    }

    #[test]
    fn the_example_and_development_files_are_valid() {
        let example = include_str!("../examples/mind.toml");
        let c: Config = example.parse().unwrap();
        assert_eq!(c.llm.mode, LlmMode::Acp);
        // The api section of the example, uncommented in place of the acp one.
        let start = example.find("# [llm]\n# mode = \"api\"").unwrap();
        let end = start + example[start..].find("\n\n").unwrap();
        let api: String = example[start..end]
            .lines()
            .map(|l| l.strip_prefix("# ").unwrap_or(l))
            .filter(|l| !l.starts_with("base_url") && !l.starts_with(' '))
            .map(|l| format!("{l}\n"))
            .collect();
        let acp_start = example.find("[llm]\nmode = \"acp\"").unwrap();
        let swapped = format!(
            "{}{api}\n{}",
            &example[..acp_start],
            &example[example.find("[conversation]").unwrap()..]
        );
        let c: Config = swapped.parse().unwrap();
        assert_eq!(c.llm.mode, LlmMode::Api);
        assert_eq!(c.llm.provider.as_deref(), Some("anthropic"));
        for dev in [
            include_str!("../../dev/mind.toml"),
            include_str!("../../dev/mind-api.toml"),
        ] {
            dev.parse::<Config>().unwrap();
        }
    }
}
