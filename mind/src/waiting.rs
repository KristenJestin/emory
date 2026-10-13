//! The waiting cue: while the answer is prepared, the owner hears that Emory heard, as a tone or as
//! a short phrase, chosen in the configuration (`[waiting]`). See `specs/mind.md`.
//!
//! The mind owns the timing, because it alone knows when a turn closes, when the first sentence
//! of the answer leaves, and when the LLM calls a tool (the call reaches Emory's own MCP server
//! with its arguments, [`crate::tools::Observer`]). It only says *what* to play, in a
//! [`contract::Cue`] on `voice.cue`; the voice draws the sounds and speaks the phrases.
//!
//! - **`tone`**: a tone as soon as the turn is closed; if no answer has started after
//!   `pulse_delay_ms`, the voice repeats a soft pulse until the first word.
//! - **`speech`**: a phrase while the LLM works, after `speech_delay_ms` without an answer
//!   (pre-written) and/or at a tool call (about what is searched). At most `max_phrases` per
//!   turn, two seconds and a half apart at least, and none once the answer has started.
//!
//! The system prompt still forbids the LLM's own waiting phrases: here the code speaks, chosen
//! and bounded.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard, OnceLock},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use contract::{Cue, CueKind, Device, TurnId};
use rmcp::model::JsonObject;
use serde::Deserialize;
use tokio::{task::JoinHandle, time::Instant};
use tracing::{debug, info};

use crate::{config::ConfigPath, conversations::ConversationKey, tools::Observer};

/// The phrases shipped with Emory, used when the configuration gives no file.
pub const BUILTIN_PHRASES: &str = include_str!("../prompts/waiting.fr.toml");

/// The least time between two phrases of a turn: the voice says them in order, and two phrases
/// back to back sound like a stutter.
pub const PHRASE_SPACING: Duration = Duration::from_millis(2500);

/// How many of the last phrases of a pool are not repeated.
const NOT_REPEATED: usize = 8;

/// What the waiting cue is.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Nothing: the silence of before.
    #[default]
    Off,
    /// A tone when the turn closes, then a soft pulse while the answer is awaited.
    Tone,
    /// A short phrase while the LLM works.
    Speech,
}

/// Where the phrases of the `speech` mode come from.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Only the pre-written ones, after the delay.
    Prewritten,
    /// Only the ones about what is searched, at a tool call.
    Contextual,
    /// At a tool call the contextual one, after the delay otherwise a pre-written one.
    #[default]
    Both,
}

/// How the cue behaves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub mode: Mode,
    /// `tone`: how long without an answer before the pulse starts.
    pub pulse_delay: Duration,
    /// `speech`: how long without an answer before a pre-written phrase.
    pub speech_delay: Duration,
    pub source: Source,
    /// `speech`: the most phrases of a turn.
    pub max_phrases: u32,
}

impl Settings {
    pub fn off() -> Self {
        Self {
            mode: Mode::Off,
            pulse_delay: Duration::from_millis(1500),
            speech_delay: Duration::from_millis(800),
            source: Source::Both,
            max_phrases: 2,
        }
    }
}

/// What a tool says of itself while it works.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolPhrases {
    /// The argument whose value fills the `{}` of `phrases`.
    argument: Option<String>,
    phrases: Vec<String>,
    plain: Vec<String>,
    silent: bool,
}

/// The phrases: a file of data, so that the owner can write his own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Phrases {
    prewritten: Vec<String>,
    tools: HashMap<String, ToolPhrases>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPhrases {
    #[serde(default)]
    prewritten: Vec<String>,
    #[serde(default)]
    tools: HashMap<String, RawTool>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawTool {
    argument: Option<String>,
    phrases: Vec<String>,
    plain: Vec<String>,
    silent: bool,
}

impl Phrases {
    /// The phrases shipped with Emory.
    pub fn builtin() -> Self {
        Self::parse(BUILTIN_PHRASES).expect("the built-in phrases are valid")
    }

    /// The file at `path`, or the built-in phrases.
    pub fn load(path: Option<&ConfigPath>) -> Result<Self> {
        match path {
            None => Ok(Self::builtin()),
            Some(path) => {
                let text = std::fs::read_to_string(&path.0)
                    .with_context(|| format!("reading the phrases {}", path.0.display()))?;
                Self::parse(&text).with_context(|| format!("the phrases {}", path.0.display()))
            }
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        let raw: RawPhrases = toml::from_str(text)?;
        let clean = |list: Vec<String>, what: &str| -> Result<Vec<String>> {
            list.into_iter()
                .map(|p| {
                    let p = p.trim().to_owned();
                    if p.is_empty() {
                        bail!("an empty phrase in {what}");
                    }
                    Ok(p)
                })
                .collect()
        };
        let mut tools = HashMap::new();
        for (name, tool) in raw.tools {
            let what = format!("tools.{name}");
            let phrases = clean(tool.phrases, &what)?;
            let plain = clean(tool.plain, &what)?;
            if let Some(p) = phrases.iter().find(|p| p.matches("{}").count() != 1) {
                bail!("{what}.phrases: {p:?} needs exactly one {{}}");
            }
            if let Some(p) = plain.iter().find(|p| p.contains("{}")) {
                bail!("{what}.plain: {p:?} must not have a {{}}");
            }
            if !phrases.is_empty() && tool.argument.is_none() {
                bail!("{what}: phrases with {{}} need an `argument`");
            }
            tools.insert(
                name,
                ToolPhrases {
                    argument: tool.argument,
                    phrases,
                    plain,
                    silent: tool.silent,
                },
            );
        }
        Ok(Self {
            prewritten: clean(raw.prewritten, "prewritten")?,
            tools,
        })
    }

    /// The `speech` mode needs something to say after the delay.
    pub fn check_for(&self, settings: &Settings) -> Result<()> {
        if settings.mode == Mode::Speech
            && settings.source != Source::Contextual
            && self.prewritten.is_empty()
        {
            bail!("the phrases have no `prewritten` list, which this `[waiting] source` needs");
        }
        if settings.mode == Mode::Speech && self.prewritten.is_empty() {
            bail!("the phrases have no `prewritten` list, to fall back on");
        }
        Ok(())
    }
}

/// A value of a tool argument as it can be said aloud, or `None`: a few words, no identifier,
/// path, address or code.
pub fn sayable(value: &str) -> Option<String> {
    let value = value
        .trim()
        .trim_matches(|c: char| c.is_whitespace() || "\"'«»“”?!.,;:".contains(c));
    if value.is_empty() || value.chars().count() > 40 || !value.chars().any(char::is_alphabetic) {
        return None;
    }
    let words: Vec<&str> = value.split_whitespace().collect();
    if words.len() > 5 {
        return None;
    }
    let spoken = words.iter().all(|w| {
        // `contrat-internet`, `a1b2`, `x/y`, `a@b`, `snake_case`: written, not said.
        let has_digit = w.chars().any(|c| c.is_ascii_digit());
        let has_letter = w.chars().any(char::is_alphabetic);
        w.chars().count() <= 24
            && !(has_digit && has_letter)
            && !w.chars().any(|c| "_/\\@#=<>{}[]|*~^`$%&+".contains(c))
            && !(words.len() == 1 && w.contains('-'))
    });
    spoken.then(|| words.join(" "))
}

/// Picks at random without repeating the last picks of the same pool.
struct Picker {
    state: u64,
    recent: HashMap<String, VecDeque<usize>>,
}

impl Picker {
    fn new(seed: u64) -> Self {
        Self {
            state: seed | 1,
            recent: HashMap::new(),
        }
    }

    fn random(&mut self) -> u64 {
        // xorshift64*: enough to vary a phrase, nothing that needs to be unguessable.
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        self.state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// An index in `0..len` that is not one of the last picks of `pool`.
    fn pick(&mut self, pool: &str, len: usize) -> usize {
        debug_assert!(len > 0);
        let remembered = NOT_REPEATED.min(len - 1);
        let recent = self.recent.entry(pool.to_owned()).or_default();
        let free: Vec<usize> = (0..len).filter(|i| !recent.contains(i)).collect();
        let draw = self.random();
        let index = free[(draw % free.len() as u64) as usize];
        let recent = self.recent.entry(pool.to_owned()).or_default();
        recent.push_back(index);
        while recent.len() > remembered {
            recent.pop_front();
        }
        index
    }
}

/// Where the cues go: the mailbox of the mind, or a recorder in the tests.
pub type Sink = Arc<dyn Fn(Cue) + Send + Sync>;

/// The turn being awaited on a device.
struct Active {
    turn: TurnId,
    conversation: ConversationKey,
    phrases: u32,
    last_phrase: Option<Instant>,
    timer: Option<JoinHandle<()>>,
}

impl Active {
    fn end(self) {
        if let Some(timer) = self.timer {
            timer.abort();
        }
    }
}

struct State {
    active: HashMap<Device, Active>,
    picker: Picker,
}

struct Inner {
    settings: Settings,
    phrases: Phrases,
    sink: OnceLock<Sink>,
    state: Mutex<State>,
}

/// The waiting cue of the mind. Cheap to clone: the mind, its answers and the tools share it.
#[derive(Clone)]
pub struct Waiter(Arc<Inner>);

impl Waiter {
    /// A waiter that does nothing.
    pub fn off() -> Self {
        Self::new(Settings::off(), Phrases::default())
    }

    pub fn new(settings: Settings, phrases: Phrases) -> Self {
        let seed = uuid::Uuid::new_v4().as_u64_pair().0;
        Self::with_seed(settings, phrases, seed)
    }

    /// With the seed of the random picks, for the tests.
    pub fn with_seed(settings: Settings, phrases: Phrases, seed: u64) -> Self {
        Self(Arc::new(Inner {
            settings,
            phrases,
            sink: OnceLock::new(),
            state: Mutex::new(State {
                active: HashMap::new(),
                picker: Picker::new(seed),
            }),
        }))
    }

    /// Connects the cues to where they go. Once: the mind does it when it takes the waiter.
    pub fn connect(&self, sink: Sink) {
        let _ = self.0.sink.set(sink);
    }

    pub fn mode(&self) -> Mode {
        self.0.settings.mode
    }

    /// The turn of this device has closed and its answer is prepared: the cue starts. Replaces the
    /// cue of an older turn. Call it inside the runtime.
    pub fn begin(&self, device: &Device, turn: TurnId, conversation: ConversationKey) {
        let settings = &self.0.settings;
        if settings.mode == Mode::Off {
            return;
        }
        let mut state = self.0.lock();
        if let Some(old) = state.active.remove(device) {
            old.end();
        }
        let mut active = Active {
            turn,
            conversation,
            phrases: 0,
            last_phrase: None,
            timer: None,
        };
        match settings.mode {
            Mode::Off => {}
            Mode::Tone => {
                self.0.emit(device, turn, CueKind::Tone, None);
                active.timer = Some(self.spawn_timer(device, turn, settings.pulse_delay));
            }
            Mode::Speech => {
                if settings.source != Source::Contextual {
                    active.timer = Some(self.spawn_timer(device, turn, settings.speech_delay));
                }
            }
        }
        state.active.insert(device.clone(), active);
    }

    fn spawn_timer(&self, device: &Device, turn: TurnId, delay: Duration) -> JoinHandle<()> {
        let inner = self.0.clone();
        let device = device.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            inner.waited(&device, turn);
        })
    }

    /// The first sentence of the answer leaves: the cue is over, and nothing of it follows. Call
    /// it before the sentence is queued, so that no cue is queued after it.
    pub fn answer_started(&self, device: &Device, turn: TurnId) {
        let mut state = self.0.lock();
        if state.active.get(device).is_some_and(|a| a.turn == turn)
            && let Some(active) = state.active.remove(device)
        {
            active.end();
        }
    }

    /// The turn of this device is stopped (barge-in): the cue is over.
    pub fn cancel(&self, device: &Device) {
        if let Some(active) = self.0.lock().active.remove(device) {
            active.end();
        }
    }

    /// Whether the cue of this device is still awaiting its answer.
    pub fn is_waiting(&self, device: &Device) -> bool {
        self.0.lock().active.contains_key(device)
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn emit(&self, device: &Device, turn: TurnId, kind: CueKind, text: Option<String>) {
        if let Some(sink) = self.sink.get() {
            sink(Cue {
                turn,
                device: device.clone(),
                kind,
                text,
            });
        }
    }

    /// The delay of a turn without an answer has passed.
    fn waited(&self, device: &Device, turn: TurnId) {
        let mut state = self.lock();
        let State { active, picker } = &mut *state;
        let Some(active) = active.get_mut(device).filter(|a| a.turn == turn) else {
            return;
        };
        match self.settings.mode {
            Mode::Off => {}
            Mode::Tone => {
                info!(%device, %turn, "no answer yet: the pulse");
                self.emit(device, turn, CueKind::Pulse, None);
            }
            Mode::Speech => {
                if self.may_speak(active) {
                    let phrase = self.prewritten(picker);
                    self.say(active, device, phrase);
                }
            }
        }
    }

    /// Whether the turn may have one more phrase now.
    fn may_speak(&self, active: &Active) -> bool {
        active.phrases < self.settings.max_phrases
            && active
                .last_phrase
                .is_none_or(|at| at.elapsed() >= PHRASE_SPACING)
    }

    fn say(&self, active: &mut Active, device: &Device, phrase: Option<String>) {
        let Some(phrase) = phrase else { return };
        info!(%device, turn = %active.turn, n = active.phrases + 1, "a waiting phrase");
        self.emit(device, active.turn, CueKind::Speech, Some(phrase));
        active.phrases += 1;
        active.last_phrase = Some(Instant::now());
    }

    fn prewritten(&self, picker: &mut Picker) -> Option<String> {
        let list = &self.phrases.prewritten;
        (!list.is_empty()).then(|| list[picker.pick("prewritten", list.len())].clone())
    }

    /// The phrase about a tool call; the pre-written ones when the tool or its arguments give
    /// nothing to say. `None` for a tool that is silent.
    fn contextual(
        &self,
        picker: &mut Picker,
        name: &str,
        arguments: Option<&JsonObject>,
    ) -> Option<String> {
        let Some(tool) = self.phrases.tools.get(name) else {
            return self.prewritten(picker);
        };
        if tool.silent {
            return None;
        }
        let spoken = tool
            .argument
            .as_deref()
            .zip(arguments)
            .and_then(|(argument, arguments)| arguments.get(argument))
            .and_then(|value| value.as_str())
            .and_then(sayable);
        if let Some(spoken) = spoken
            && !tool.phrases.is_empty()
        {
            let pick = picker.pick(&format!("{name}/phrases"), tool.phrases.len());
            return Some(tool.phrases[pick].replace("{}", &spoken));
        }
        if !tool.plain.is_empty() {
            let pick = picker.pick(&format!("{name}/plain"), tool.plain.len());
            return Some(tool.plain[pick].clone());
        }
        self.prewritten(picker)
    }
}

impl Observer for Waiter {
    /// The LLM calls a tool: a phrase about it, if the turn is being awaited.
    fn called(
        &self,
        conversation: Option<&ConversationKey>,
        tool: &str,
        arguments: Option<&JsonObject>,
    ) {
        let inner = &self.0;
        if inner.settings.mode != Mode::Speech || inner.settings.source == Source::Prewritten {
            return;
        }
        let Some(conversation) = conversation else {
            return;
        };
        let mut state = inner.lock();
        let State { active, picker } = &mut *state;
        let Some((device, active)) = active
            .iter_mut()
            .find(|(_, a)| &a.conversation == conversation)
        else {
            return;
        };
        if !inner.may_speak(active) {
            debug!(%device, tool, "tool called: no phrase (enough said, or too soon)");
            return;
        }
        let phrase = inner.contextual(picker, tool, arguments);
        let device = device.clone();
        inner.say(active, &device, phrase);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn device() -> Device {
        Device::new("office").unwrap()
    }

    fn settings(mode: Mode, source: Source) -> Settings {
        Settings {
            mode,
            pulse_delay: ms(1500),
            speech_delay: ms(800),
            source,
            max_phrases: 2,
        }
    }

    const FILE: &str = r#"
prewritten = ["Je regarde.", "Je cherche.", "Voyons voir."]

[tools.hippocampe_search]
argument = "query"
phrases = ["Je regarde ce que j'ai sur {}."]
plain = ["Je regarde dans mes notes."]

[tools.hippocampe_read]
plain = ["Je regarde cette fiche."]

[tools.hippocampe_types]
silent = true
"#;

    struct Bench {
        waiter: Waiter,
        cues: Arc<Mutex<Vec<Cue>>>,
        key: ConversationKey,
    }

    impl Bench {
        fn new(settings: Settings, file: &str) -> Self {
            let waiter = Waiter::with_seed(settings, Phrases::parse(file).unwrap(), 7);
            let cues = Arc::new(Mutex::new(Vec::new()));
            let seen = cues.clone();
            waiter.connect(Arc::new(move |cue| seen.lock().unwrap().push(cue)));
            Self {
                waiter,
                cues,
                key: crate::conversations::Conversations::default().open(),
            }
        }

        fn begin(&self) -> TurnId {
            let turn = TurnId::new();
            self.waiter.begin(&device(), turn, self.key.clone());
            turn
        }

        fn kinds(&self) -> Vec<(CueKind, Option<String>)> {
            self.cues
                .lock()
                .unwrap()
                .iter()
                .map(|c| (c.kind, c.text.clone()))
                .collect()
        }

        fn tool(&self, name: &str, arguments: serde_json::Value) {
            self.waiter
                .called(Some(&self.key), name, arguments.as_object());
        }
    }

    async fn pass(ms_: u64) {
        tokio::time::sleep(ms(ms_)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn the_tone_comes_at_once_and_the_pulse_after_the_delay() {
        let b = Bench::new(settings(Mode::Tone, Source::Both), FILE);
        let turn = b.begin();
        assert_eq!(b.kinds(), [(CueKind::Tone, None)]);
        pass(1400).await;
        assert_eq!(b.kinds().len(), 1);
        pass(200).await;
        assert_eq!(b.kinds(), [(CueKind::Tone, None), (CueKind::Pulse, None)]);
        assert!(b.cues.lock().unwrap().iter().all(|c| c.turn == turn));
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_that_starts_in_time_spares_the_pulse() {
        let b = Bench::new(settings(Mode::Tone, Source::Both), FILE);
        let turn = b.begin();
        pass(900).await;
        b.waiter.answer_started(&device(), turn);
        pass(5000).await;
        assert_eq!(b.kinds(), [(CueKind::Tone, None)]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_answer_of_another_turn_does_not_end_the_cue() {
        let b = Bench::new(settings(Mode::Tone, Source::Both), FILE);
        b.begin();
        b.waiter.answer_started(&device(), TurnId::new());
        pass(2000).await;
        assert_eq!(b.kinds().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_barge_in_ends_the_cue() {
        let b = Bench::new(settings(Mode::Tone, Source::Both), FILE);
        b.begin();
        b.waiter.cancel(&device());
        pass(5000).await;
        assert_eq!(b.kinds().len(), 1);
        assert!(!b.waiter.is_waiting(&device()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_turn_replaces_the_cue_of_the_old_one() {
        let b = Bench::new(settings(Mode::Tone, Source::Both), FILE);
        let old = b.begin();
        pass(1000).await;
        let new = b.begin();
        pass(1000).await;
        // The old pulse (due at 1500 ms) never came; the new one is not due yet.
        let turns: Vec<_> = b
            .cues
            .lock()
            .unwrap()
            .iter()
            .map(|c| (c.turn, c.kind))
            .collect();
        assert_eq!(turns, [(old, CueKind::Tone), (new, CueKind::Tone)]);
        pass(600).await;
        assert_eq!(b.kinds().len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn off_does_nothing_at_all() {
        let b = Bench::new(settings(Mode::Off, Source::Both), FILE);
        b.begin();
        b.tool("hippocampe_search", json!({"query": "Ali"}));
        pass(5000).await;
        assert!(b.kinds().is_empty() && !b.waiter.is_waiting(&device()));
        let off = Waiter::off();
        off.begin(&device(), TurnId::new(), b.key.clone());
        assert!(!off.is_waiting(&device()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_prewritten_phrase_comes_after_the_delay_and_not_if_the_answer_came() {
        let b = Bench::new(settings(Mode::Speech, Source::Both), FILE);
        b.begin();
        assert!(b.kinds().is_empty(), "a phrase is not said at once");
        pass(700).await;
        assert!(b.kinds().is_empty());
        pass(200).await;
        let kinds = b.kinds();
        assert_eq!(kinds.len(), 1);
        assert_eq!(kinds[0].0, CueKind::Speech);
        assert!(
            ["Je regarde.", "Je cherche.", "Voyons voir."]
                .contains(&kinds[0].1.as_deref().unwrap())
        );

        let b = Bench::new(settings(Mode::Speech, Source::Both), FILE);
        let turn = b.begin();
        pass(500).await;
        b.waiter.answer_started(&device(), turn);
        pass(5000).await;
        assert!(b.kinds().is_empty());
        // And a tool called after the answer started says nothing either.
        b.tool("hippocampe_search", json!({"query": "Ali"}));
        assert!(b.kinds().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_tool_call_with_a_query_says_what_is_searched() {
        let b = Bench::new(settings(Mode::Speech, Source::Both), FILE);
        b.begin();
        pass(300).await;
        b.tool("hippocampe_search", json!({"query": "Ali", "limit": 5}));
        assert_eq!(
            b.kinds(),
            [(
                CueKind::Speech,
                Some("Je regarde ce que j'ai sur Ali.".to_owned())
            )]
        );
        // The phrase of the delay does not follow: one phrase for this wait.
        pass(2000).await;
        assert_eq!(b.kinds().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_tool_call_that_gives_nothing_sayable_falls_back() {
        let said = |name: &str, arguments: serde_json::Value| {
            let b = Bench::new(settings(Mode::Speech, Source::Contextual), FILE);
            b.begin();
            b.tool(name, arguments);
            b.kinds()
                .into_iter()
                .filter_map(|k| k.1)
                .collect::<Vec<_>>()
        };
        // An identifier, a path, a long sentence, nothing at all: the plain phrase of the tool.
        for query in [
            json!({"query": "mon-contrat-internet"}),
            json!({"query": "01K9ZQ3V7X"}),
            json!({"query": "notes/contrat.md"}),
            json!({"query": "quand est-ce que le contrat internet arrive à échéance cette année"}),
            json!({"query": 12}),
            json!({"query": ""}),
            json!({}),
        ] {
            assert_eq!(
                said("hippocampe_search", query.clone()),
                ["Je regarde dans mes notes."],
                "{query}"
            );
        }
        // A tool with no argument to say: its plain phrase; the identifier is never said.
        assert_eq!(
            said("hippocampe_read", json!({"entry": "mon-contrat-internet"})),
            ["Je regarde cette fiche."]
        );
        // A tool of the file that is silent, and a tool the file does not know.
        assert!(said("hippocampe_types", json!({})).is_empty());
        let unknown = said("web_search", json!({"query": "Ali"}));
        assert!(["Je regarde.", "Je cherche.", "Voyons voir."].contains(&unknown[0].as_str()));
    }

    #[tokio::test(start_paused = true)]
    async fn phrases_are_few_and_spaced() {
        let b = Bench::new(settings(Mode::Speech, Source::Both), FILE);
        b.begin();
        pass(900).await;
        assert_eq!(b.kinds().len(), 1, "the phrase of the delay");
        b.tool("hippocampe_search", json!({"query": "Ali"}));
        assert_eq!(b.kinds().len(), 1, "too soon after the first one");
        pass(2500).await;
        b.tool("hippocampe_search", json!({"query": "Ali"}));
        assert_eq!(b.kinds().len(), 2, "the second one, 2.6 s after the first");
        pass(3000).await;
        b.tool("hippocampe_search", json!({"query": "Ali"}));
        pass(3000).await;
        assert_eq!(b.kinds().len(), 2, "never more than max_phrases");
    }

    #[tokio::test(start_paused = true)]
    async fn the_source_chooses_between_the_delay_and_the_tool_call() {
        let b = Bench::new(settings(Mode::Speech, Source::Prewritten), FILE);
        b.begin();
        b.tool("hippocampe_search", json!({"query": "Ali"}));
        assert!(b.kinds().is_empty(), "no phrase about the tool");
        pass(900).await;
        assert_eq!(b.kinds().len(), 1);

        let b = Bench::new(settings(Mode::Speech, Source::Contextual), FILE);
        b.begin();
        pass(5000).await;
        assert!(b.kinds().is_empty(), "no phrase on the delay");
        b.tool("hippocampe_search", json!({"query": "Ali"}));
        assert_eq!(b.kinds().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_call_of_another_conversation_or_of_none_says_nothing() {
        let b = Bench::new(settings(Mode::Speech, Source::Both), FILE);
        b.begin();
        let other = crate::conversations::Conversations::default().open();
        b.waiter.called(
            Some(&other),
            "hippocampe_search",
            json!({"query": "Ali"}).as_object(),
        );
        b.waiter.called(
            None,
            "hippocampe_search",
            json!({"query": "Ali"}).as_object(),
        );
        assert!(b.kinds().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn the_last_phrases_are_not_repeated() {
        let file = "prewritten = [\"A.\", \"B.\", \"C.\", \"D.\", \"E.\"]\n";
        let b = Bench::new(settings(Mode::Speech, Source::Prewritten), file);
        for _ in 0..40 {
            b.begin();
            pass(900).await;
        }
        let said: Vec<String> = b.kinds().into_iter().filter_map(|k| k.1).collect();
        assert_eq!(said.len(), 40);
        // 5 phrases: the 4 last are remembered, so any 5 in a row are all different.
        for window in said.windows(5) {
            let mut sorted = window.to_vec();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), 5, "{window:?}");
        }
        // And they do vary: not one phrase said over and over.
        let mut distinct = said.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), 5);
    }

    #[test]
    fn a_single_phrase_can_repeat_since_there_is_no_other() {
        let mut picker = Picker::new(1);
        assert!((0..5).all(|_| picker.pick("one", 1) == 0));
    }

    #[test]
    fn what_is_sayable_is_a_few_words_and_never_an_identifier() {
        for (value, said) in [
            ("Ali", Some("Ali")),
            ("  contrat internet ", Some("contrat internet")),
            ("« Ali » ?", Some("Ali")),
            (
                "la perceuse prêtée à Marc",
                Some("la perceuse prêtée à Marc"),
            ),
            ("Hemera", Some("Hemera")),
            ("", None),
            ("   ", None),
            ("?!", None),
            ("2026", None),
            ("mon-contrat-internet", None),
            ("snake_case", None),
            ("a1b2c3", None),
            ("01K9ZQ3V7X", None),
            ("notes/contrat", None),
            ("C:\\notes", None),
            ("ali@example.org", None),
            ("https://example.org/x", None),
            ("un deux trois quatre cinq six", None),
            ("x".repeat(41).as_str(), None),
            ("Jean-Pierre et Marie", Some("Jean-Pierre et Marie")),
        ] {
            assert_eq!(sayable(value).as_deref(), said, "{value:?}");
        }
    }

    #[test]
    fn the_phrases_file_is_checked() {
        assert!(Phrases::parse("prewritten = [\"Je regarde.\"]").is_ok());
        for bad in [
            "prewritten = [\"  \"]",
            "nope = 1",
            "[tools.a]\nphrases = [\"sans trou\"]\nargument = \"q\"",
            "[tools.a]\nphrases = [\"deux {} et {}\"]\nargument = \"q\"",
            "[tools.a]\nphrases = [\"un {}\"]",
            "[tools.a]\nplain = [\"un {}\"]",
            "[tools.a]\nwat = 1",
        ] {
            assert!(Phrases::parse(bad).is_err(), "{bad}");
        }
        let empty = Phrases::parse("").unwrap();
        assert!(empty.check_for(&settings(Mode::Tone, Source::Both)).is_ok());
        assert!(
            empty
                .check_for(&settings(Mode::Speech, Source::Both))
                .is_err()
        );
        assert!(
            empty
                .check_for(&settings(Mode::Speech, Source::Contextual))
                .is_err()
        );
    }

    #[test]
    fn the_built_in_phrases_are_short_plain_and_generic() {
        let phrases = Phrases::builtin();
        assert!(phrases.prewritten.len() >= 30, "dozens of them");
        let mut seen = std::collections::HashSet::new();
        for p in &phrases.prewritten {
            assert!(seen.insert(p), "twice: {p}");
            assert!(p.chars().count() <= 40, "short: {p}");
            assert!(p.ends_with('.'), "{p}");
            let lower = p.to_lowercase();
            for promise in ["seconde", "instant", "minute", "tout de suite", "?"] {
                assert!(!lower.contains(promise), "{p}");
            }
            // Nothing that talks to the person: it would have to know « tu » from « vous ».
            for word in [" te ", " toi", "tu ", "vous", "laisse"] {
                assert!(!format!(" {lower} ").contains(word), "{p}");
            }
        }
        let search = &phrases.tools["hippocampe_search"];
        assert_eq!(search.argument.as_deref(), Some("query"));
        assert!(!search.phrases.is_empty() && !search.plain.is_empty());
        assert!(phrases.tools["hippocampe_read"].argument.is_none());
        assert!(phrases.tools["dictionary_propose"].silent);
    }
}
