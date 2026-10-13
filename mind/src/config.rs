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
    /// Who Emory talks to. Absent: the instructions name nobody.
    #[serde(default)]
    pub owner: OwnerConfig,
    #[serde(default)]
    pub dictionary: DictionaryConfig,
    /// What the owner hears while the answer is prepared. Absent: nothing.
    #[serde(default)]
    pub waiting: WaitingConfig,
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

/// The LLM, reached through an ACP adapter that `mind` starts and drives over its standard input
/// and output.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
    /// The adapter's program (`node`).
    pub command: String,
    /// Its arguments (the path of `claude-agent-acp`'s `dist/index.js`).
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_effort")]
    pub effort: String,
    /// The French instructions of the LLM.
    pub system_prompt: ConfigPath,
    /// The working folder of the sessions. Claude Code has no tool of its own, so it never
    /// touches it; by default a folder of the system's temporary directory.
    pub workdir: Option<ConfigPath>,
}

fn default_model() -> String {
    "claude-haiku-5-5".to_owned()
}

fn default_effort() -> String {
    "low".to_owned()
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

/// The waiting cue (see [`crate::waiting`]): `off`, a `tone`, or a short phrase (`speech`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WaitingConfig {
    pub mode: crate::waiting::Mode,
    /// `tone`: milliseconds without an answer before the soft pulse starts.
    pub pulse_delay_ms: u64,
    /// `speech`: milliseconds without an answer before a pre-written phrase.
    pub speech_delay_ms: u64,
    /// `speech`: pre-written phrases only, ones about what is searched only, or both.
    pub source: crate::waiting::Source,
    /// `speech`: the most phrases said in a turn.
    pub max_phrases: u32,
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
        assert_eq!(c.llm.effort, "low");
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
}
