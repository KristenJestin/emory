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
//!   turn, `phrase_spacing_ms` apart at least, and none once the answer has started. The silence
//!   after a phrase can be filled: `pulse_after_speech` (the soft pulse of `tone`, after
//!   `pulse_delay_ms` of silence following the phrase) and `prewritten_repeat_ms` (another
//!   pre-written phrase after that much more silence).
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

/// The default least time between two phrases of a turn: the voice says them in order, and two
/// phrases back to back sound like a stutter.
pub const PHRASE_SPACING: Duration = Duration::from_millis(2500);

/// What the mind assumes of the voice to guess when a phrase is over (it knows the text, not the
/// audio): the synthesis starts to sound after `SPEECH_LEAD`, then a phrase lasts `SPEECH_BASE`
/// plus `SPEECH_PER_CHAR` a character. Measured on 2026-10-13 with a French voice: a nine
/// character acknowledgement was heard from +0.34 s and was 1.16 s long (this gives 1.48 s).
const SPEECH_LEAD: Duration = Duration::from_millis(350);
const SPEECH_BASE: Duration = Duration::from_millis(500);
const SPEECH_PER_CHAR: Duration = Duration::from_millis(70);

/// How long after it is queued a phrase is over, as far as the mind can tell.
pub fn estimated_speech(phrase: &str) -> Duration {
    SPEECH_LEAD + SPEECH_BASE + SPEECH_PER_CHAR * phrase.chars().count() as u32
}

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

/// From where the spacing between two phrases is measured.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpacingFrom {
    /// From the moment the previous phrase was sent (the first behaviour).
    #[default]
    Start,
    /// From the moment the previous phrase is over, as far as it can be estimated
    /// ([`estimated_speech`]): the spacing is then the silence between two phrases.
    End,
}

/// How the cue behaves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub mode: Mode,
    /// `tone`: how long without an answer before the pulse starts; `speech` with
    /// `pulse_after_speech`: how long without an answer after a phrase.
    pub pulse_delay: Duration,
    /// `speech`: how long without an answer before a pre-written phrase.
    pub speech_delay: Duration,
    pub source: Source,
    /// `speech`: the most phrases of a turn.
    pub max_phrases: u32,
    /// `speech`: the least time between two phrases of a turn.
    pub phrase_spacing: Duration,
    /// `speech`: where `phrase_spacing` is measured from.
    pub spacing_from: SpacingFrom,
    /// `speech`: the soft pulse fills the silence after a phrase.
    pub pulse_after_speech: bool,
    /// `speech`: another pre-written phrase after this much more silence since the last phrase
    /// (zero: none). Never closer than `phrase_spacing`.
    pub prewritten_repeat: Duration,
}

impl Settings {
    pub fn off() -> Self {
        Self {
            mode: Mode::Off,
            pulse_delay: Duration::from_millis(1500),
            speech_delay: Duration::from_millis(800),
            source: Source::Both,
            max_phrases: 2,
            phrase_spacing: PHRASE_SPACING,
            spacing_from: SpacingFrom::Start,
            pulse_after_speech: false,
            prewritten_repeat: Duration::ZERO,
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

/// What a timer of the cue is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Due {
    /// The delay before the first pre-written phrase.
    Delay,
    /// More silence after a phrase: another pre-written one.
    Repeat,
    /// The soft pulse.
    Pulse,
}

/// The last phrase sent in a turn.
#[derive(Clone, Copy)]
struct LastPhrase {
    sent: Instant,
    /// When it is over, as far as can be told.
    over: Instant,
}

/// The turn being awaited on a device.
struct Active {
    turn: TurnId,
    conversation: ConversationKey,
    phrases: u32,
    last_phrase: Option<LastPhrase>,
    /// The pulse has been asked of the voice, which repeats it by itself until the answer.
    pulse_sent: bool,
    /// What is still to happen if no answer starts, and when. One timer wakes for the soonest.
    due: Vec<(Instant, Due)>,
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
        let now = Instant::now();
        let mut active = Active {
            turn,
            conversation,
            phrases: 0,
            last_phrase: None,
            pulse_sent: false,
            due: Vec::new(),
            timer: None,
        };
        match settings.mode {
            Mode::Off => {}
            Mode::Tone => {
                self.0.emit(device, turn, CueKind::Tone, None);
                active.due.push((now + settings.pulse_delay, Due::Pulse));
            }
            Mode::Speech => {
                if settings.source != Source::Contextual {
                    active.due.push((now + settings.speech_delay, Due::Delay));
                }
            }
        }
        self.0.arm(device, &mut active);
        state.active.insert(device.clone(), active);
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

    /// Wakes, for the soonest of the things due in the turn, if no answer has started.
    fn arm(self: &Arc<Self>, device: &Device, active: &mut Active) {
        if let Some(timer) = active.timer.take() {
            timer.abort();
        }
        let Some(&(at, _)) = active.due.iter().min_by_key(|(at, _)| *at) else {
            return;
        };
        let inner = self.clone();
        let device = device.clone();
        let turn = active.turn;
        active.timer = Some(tokio::spawn(async move {
            tokio::time::sleep_until(at).await;
            inner.fire(&device, turn);
        }));
    }

    /// What was due in a turn without an answer has come.
    fn fire(self: &Arc<Self>, device: &Device, turn: TurnId) {
        let mut state = self.lock();
        let State { active, picker } = &mut *state;
        let Some(active) = active.get_mut(device).filter(|a| a.turn == turn) else {
            return;
        };
        // This very task: it ends by itself, and a new one is armed below.
        active.timer = None;
        let now = Instant::now();
        let mut ready: Vec<(Instant, Due)> = active
            .due
            .iter()
            .copied()
            .filter(|(at, _)| *at <= now)
            .collect();
        ready.sort_by_key(|(at, _)| *at);
        active.due.retain(|(at, _)| *at > now);
        for (_, due) in ready {
            match due {
                Due::Pulse => {
                    info!(%device, %turn, "no answer yet: the pulse");
                    active.pulse_sent = true;
                    self.emit(device, turn, CueKind::Pulse, None);
                }
                Due::Delay | Due::Repeat => {
                    if self.may_speak(active, now) {
                        let phrase = self.prewritten(picker);
                        self.say(active, device, phrase);
                    }
                }
            }
        }
        self.arm(device, active);
    }

    /// Whether the turn may have one more phrase at `now`.
    fn may_speak(&self, active: &Active, now: Instant) -> bool {
        active.phrases < self.settings.max_phrases
            && active
                .last_phrase
                .is_none_or(|last| now >= self.spacing_from(last) + self.settings.phrase_spacing)
    }

    /// The moment the spacing after a phrase is counted from.
    fn spacing_from(&self, last: LastPhrase) -> Instant {
        match self.settings.spacing_from {
            SpacingFrom::Start => last.sent,
            SpacingFrom::End => last.over,
        }
    }

    /// Sends a phrase, and plans what follows it: the pulse and the repeat start from its end
    /// (a pulse never plays over a phrase), whatever was planned before is replaced.
    fn say(self: &Arc<Self>, active: &mut Active, device: &Device, phrase: Option<String>) {
        let Some(phrase) = phrase else { return };
        info!(%device, turn = %active.turn, n = active.phrases + 1, "a waiting phrase");
        let now = Instant::now();
        let last = LastPhrase {
            sent: now,
            over: now + estimated_speech(&phrase),
        };
        self.emit(device, active.turn, CueKind::Speech, Some(phrase));
        active.phrases += 1;
        active.last_phrase = Some(last);

        let settings = &self.settings;
        let repeat = !settings.prewritten_repeat.is_zero() && settings.source != Source::Contextual;
        // With a repeat, the phrase just said replaces the first delay too; without one, the delay
        // stays as it was (it is dropped then if it comes too close to a phrase).
        active.due.retain(|(_, due)| *due == Due::Delay && !repeat);
        if settings.pulse_after_speech && !active.pulse_sent {
            active
                .due
                .push((last.over + settings.pulse_delay, Due::Pulse));
        }
        if repeat && active.phrases < settings.max_phrases {
            let wait = settings.prewritten_repeat.max(settings.phrase_spacing);
            active
                .due
                .push((self.spacing_from(last) + wait, Due::Repeat));
        }
        self.arm(device, active);
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
        if !inner.may_speak(active, Instant::now()) {
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
            ..Settings::off()
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
        b.tool("hippocampe_search", json!({"query": "Atlas"}));
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
        b.tool("hippocampe_search", json!({"query": "Atlas"}));
        assert!(b.kinds().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_tool_call_with_a_query_says_what_is_searched() {
        let b = Bench::new(settings(Mode::Speech, Source::Both), FILE);
        b.begin();
        pass(300).await;
        b.tool("hippocampe_search", json!({"query": "Atlas", "limit": 5}));
        assert_eq!(
            b.kinds(),
            [(
                CueKind::Speech,
                Some("Je regarde ce que j'ai sur Atlas.".to_owned())
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
        let unknown = said("web_search", json!({"query": "Atlas"}));
        assert!(["Je regarde.", "Je cherche.", "Voyons voir."].contains(&unknown[0].as_str()));
    }

    #[tokio::test(start_paused = true)]
    async fn phrases_are_few_and_spaced() {
        let b = Bench::new(settings(Mode::Speech, Source::Both), FILE);
        b.begin();
        pass(900).await;
        assert_eq!(b.kinds().len(), 1, "the phrase of the delay");
        b.tool("hippocampe_search", json!({"query": "Atlas"}));
        assert_eq!(b.kinds().len(), 1, "too soon after the first one");
        pass(2500).await;
        b.tool("hippocampe_search", json!({"query": "Atlas"}));
        assert_eq!(b.kinds().len(), 2, "the second one, 2.6 s after the first");
        pass(3000).await;
        b.tool("hippocampe_search", json!({"query": "Atlas"}));
        pass(3000).await;
        assert_eq!(b.kinds().len(), 2, "never more than max_phrases");
    }

    /// The settings of the owner's measured case: an acknowledgement at 100 ms, a tool call at 2.5 s.
    fn filling(spacing_from: SpacingFrom, spacing: u64) -> Settings {
        Settings {
            speech_delay: ms(100),
            phrase_spacing: ms(spacing),
            spacing_from,
            pulse_after_speech: true,
            pulse_delay: ms(600),
            ..settings(Mode::Speech, Source::Both)
        }
    }

    const ACK: &str = "prewritten = [\"D'accord.\"]\n[tools.hippocampe_search]\nargument = \"query\"\nphrases = [\"Je regarde ce que j'ai sur {}.\"]\n";

    #[tokio::test(start_paused = true)]
    async fn the_contextual_phrase_at_a_late_tool_call_is_spoken_when_spaced_from_the_end() {
        // Before: the spacing counted from the start of the first phrase (+0.1 s): a tool call at
        // +2.5 s was 2.4 s later, too soon, and nothing was said.
        let b = Bench::new(
            Settings {
                pulse_after_speech: false,
                ..filling(SpacingFrom::Start, 2500)
            },
            ACK,
        );
        b.begin();
        pass(2500).await;
        b.tool("hippocampe_search", json!({"query": "Ali"}));
        assert_eq!(b.kinds().len(), 1, "only the acknowledgement");

        // Now: the first phrase is over at about +1.6 s; 800 ms of silence after it is enough.
        let b = Bench::new(
            Settings {
                pulse_after_speech: false,
                ..filling(SpacingFrom::End, 800)
            },
            ACK,
        );
        b.begin();
        pass(2500).await;
        b.tool("hippocampe_search", json!({"query": "Ali"}));
        let said: Vec<_> = b.kinds().into_iter().filter_map(|k| k.1).collect();
        assert_eq!(said, ["D'accord.", "Je regarde ce que j'ai sur Ali."]);
        // And not before the first phrase is over: the same call at +1 s stays silent.
        let b = Bench::new(
            Settings {
                pulse_after_speech: false,
                ..filling(SpacingFrom::End, 800)
            },
            ACK,
        );
        b.begin();
        pass(1000).await;
        b.tool("hippocampe_search", json!({"query": "Ali"}));
        assert_eq!(b.kinds().len(), 1);
    }

    #[test]
    fn a_phrase_is_estimated_from_its_length() {
        // Measured: nine characters, 1.16 s heard from +0.34 s.
        let nine = estimated_speech("D'accord.");
        assert!(nine >= ms(1400) && nine <= ms(1600), "{nine:?}");
        assert!(estimated_speech("Je regarde ce que j'ai sur Ali.") > nine);
    }

    #[tokio::test(start_paused = true)]
    async fn the_pulse_fills_the_silence_after_a_phrase_and_stops_at_the_first_word() {
        let b = Bench::new(filling(SpacingFrom::End, 800), ACK);
        let turn = b.begin();
        pass(150).await;
        assert_eq!(b.kinds(), [(CueKind::Speech, Some("D'accord.".to_owned()))]);
        // The phrase is over at about +1.6 s, the pulse follows 600 ms later, not before.
        pass(1900).await; // +2.05 s
        assert_eq!(b.kinds().len(), 1, "not while the phrase is said");
        pass(300).await; // +2.35 s
        assert_eq!(b.kinds().len(), 2);
        assert_eq!(b.kinds()[1], (CueKind::Pulse, None));
        // The voice repeats it by itself: the mind sends it once.
        pass(10_000).await;
        assert_eq!(b.kinds().len(), 2);

        // An answer that starts before the pulse is due spares it.
        let b = Bench::new(filling(SpacingFrom::End, 800), ACK);
        let turn2 = b.begin();
        pass(2000).await;
        b.waiter.answer_started(&device(), turn2);
        pass(10_000).await;
        assert_eq!(b.kinds().len(), 1);
        assert_ne!(turn, turn2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_phrase_at_a_tool_call_moves_the_pulse_after_itself() {
        let b = Bench::new(
            Settings {
                pulse_delay: ms(900), // the pulse would be due at +2.5 s
                ..filling(SpacingFrom::End, 800)
            },
            ACK,
        );
        b.begin();
        pass(2400).await;
        b.tool("hippocampe_search", json!({"query": "Ali"}));
        let kinds: Vec<_> = b.kinds().into_iter().map(|k| k.0).collect();
        assert_eq!(kinds, [CueKind::Speech, CueKind::Speech]);
        // The second phrase (31 characters) is over at about +5.4 s: the pulse is due 900 ms after.
        pass(2500).await; // +4.9 s
        assert_eq!(b.kinds().len(), 2);
        pass(2000).await; // +6.9 s
        let kinds: Vec<_> = b.kinds().into_iter().map(|k| k.0).collect();
        assert_eq!(kinds, [CueKind::Speech, CueKind::Speech, CueKind::Pulse]);

        // A phrase after the pulse has started does not ask for it again: the voice goes on.
        let b = Bench::new(
            Settings {
                max_phrases: 3,
                ..filling(SpacingFrom::End, 800)
            },
            ACK,
        );
        b.begin();
        pass(2500).await; // pulse at +2.2 s
        b.tool("hippocampe_search", json!({"query": "Ali"}));
        pass(20_000).await;
        let kinds: Vec<_> = b.kinds().into_iter().map(|k| k.0).collect();
        assert_eq!(kinds, [CueKind::Speech, CueKind::Pulse, CueKind::Speech]);
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_is_sent_after_the_answer_has_started() {
        let b = Bench::new(
            Settings {
                prewritten_repeat: ms(3000),
                ..filling(SpacingFrom::End, 800)
            },
            ACK,
        );
        let turn = b.begin();
        pass(50).await;
        b.waiter.answer_started(&device(), turn);
        pass(20_000).await;
        b.tool("hippocampe_search", json!({"query": "Ali"}));
        assert!(b.kinds().is_empty(), "{:?}", b.kinds());
        // The same for a barge-in after the first phrase.
        let b = Bench::new(
            Settings {
                prewritten_repeat: ms(3000),
                ..filling(SpacingFrom::End, 800)
            },
            ACK,
        );
        b.begin();
        pass(500).await;
        b.waiter.cancel(&device());
        pass(20_000).await;
        assert_eq!(b.kinds().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_prewritten_phrase_can_repeat_after_more_silence_up_to_the_maximum() {
        let file = "prewritten = [\"A.\", \"B.\", \"C.\"]\n";
        let repeating = |max_phrases| Settings {
            speech_delay: ms(100),
            source: Source::Prewritten,
            prewritten_repeat: ms(3000),
            max_phrases,
            ..Settings::off()
        };
        let b = Bench::new(
            Settings {
                mode: Mode::Speech,
                ..repeating(3)
            },
            file,
        );
        b.begin();
        pass(150).await;
        assert_eq!(b.kinds().len(), 1);
        pass(2900).await; // +3.05 s: 3 s after the first (counted from its start by default)
        assert_eq!(b.kinds().len(), 1);
        pass(200).await;
        assert_eq!(b.kinds().len(), 2);
        pass(3000).await;
        assert_eq!(b.kinds().len(), 3);
        pass(30_000).await;
        assert_eq!(b.kinds().len(), 3, "never more than max_phrases");
        let said: Vec<_> = b.kinds().into_iter().filter_map(|k| k.1).collect();
        assert!(said[0] != said[1] && said[1] != said[2]);

        // With the default maximum of two, a single repeat.
        let b = Bench::new(
            Settings {
                mode: Mode::Speech,
                ..repeating(2)
            },
            file,
        );
        b.begin();
        pass(30_000).await;
        assert_eq!(b.kinds().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_repeat_never_goes_closer_than_the_spacing() {
        let b = Bench::new(
            Settings {
                mode: Mode::Speech,
                source: Source::Prewritten,
                speech_delay: ms(100),
                prewritten_repeat: ms(200),
                phrase_spacing: ms(2500),
                ..Settings::off()
            },
            "prewritten = [\"A.\", \"B.\"]\n",
        );
        b.begin();
        pass(2000).await;
        assert_eq!(b.kinds().len(), 1);
        pass(700).await; // +2.7 s: 2.5 s after the first one
        assert_eq!(b.kinds().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_defaults_keep_the_behaviour_of_before() {
        let d = Settings::off();
        assert_eq!(d.phrase_spacing, ms(2500));
        assert_eq!(d.spacing_from, SpacingFrom::Start);
        assert!(!d.pulse_after_speech);
        assert!(d.prewritten_repeat.is_zero());
        // One phrase of the delay, nothing after it, however long the wait.
        let b = Bench::new(settings(Mode::Speech, Source::Both), FILE);
        b.begin();
        pass(60_000).await;
        assert_eq!(b.kinds().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_source_chooses_between_the_delay_and_the_tool_call() {
        let b = Bench::new(settings(Mode::Speech, Source::Prewritten), FILE);
        b.begin();
        b.tool("hippocampe_search", json!({"query": "Atlas"}));
        assert!(b.kinds().is_empty(), "no phrase about the tool");
        pass(900).await;
        assert_eq!(b.kinds().len(), 1);

        let b = Bench::new(settings(Mode::Speech, Source::Contextual), FILE);
        b.begin();
        pass(5000).await;
        assert!(b.kinds().is_empty(), "no phrase on the delay");
        b.tool("hippocampe_search", json!({"query": "Atlas"}));
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
            json!({"query": "Atlas"}).as_object(),
        );
        b.waiter.called(
            None,
            "hippocampe_search",
            json!({"query": "Atlas"}).as_object(),
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
            ("Atlas", Some("Atlas")),
            ("  contrat internet ", Some("contrat internet")),
            ("« Atlas » ?", Some("Atlas")),
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
        let wikipedia = &phrases.tools["web_wikipedia"];
        assert_eq!(wikipedia.argument.as_deref(), Some("query"));
        assert!(!wikipedia.phrases.is_empty() && !wikipedia.plain.is_empty());
        let weather = &phrases.tools["web_weather"];
        assert_eq!(weather.argument.as_deref(), Some("place"));
        assert!(!weather.phrases.is_empty() && !weather.plain.is_empty());
    }
}
