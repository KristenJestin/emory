//! The waiting cue through the whole mind, with a fake LLM and the messages to the voice recorded:
//! the tone when the turn closes, the pulse when the answer is late, the phrase about a tool call,
//! and nothing once the answer has started or the owner has pressed again.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use contract::{Button, Cue, CueKind, Device, Say, Sentence, State, Stop, Turn, TurnId, Wake};
use mind::{
    brain::{Brain, Event, SessionId},
    conversations::{ConversationKey, Conversations},
    memory::{Found, Memory},
    mind::{Input, Mind, Outbox, Settings},
    tools::Observer,
    waiting::{Mode, Phrases, Settings as WaitingSettings, Source, Waiter},
};
use tokio::sync::mpsc;

const FILE: &str = r#"
prewritten = ["Je regarde."]

[tools.hippocampe_search]
argument = "query"
phrases = ["Je regarde ce que j'ai sur {}."]
plain = ["Je regarde dans mes notes."]
"#;

#[derive(Default)]
struct FakeBrain {
    keys: Mutex<Vec<ConversationKey>>,
    streams: Mutex<HashMap<SessionId, mpsc::UnboundedSender<Event>>>,
}

impl Brain for FakeBrain {
    async fn open(&self, conversation: &ConversationKey) -> anyhow::Result<SessionId> {
        let mut keys = self.keys.lock().unwrap();
        keys.push(conversation.clone());
        Ok(SessionId(format!("s{}", keys.len())))
    }

    fn prompt(&self, session: &SessionId, _text: String) -> mpsc::UnboundedReceiver<Event> {
        let (events, receiver) = mpsc::unbounded_channel();
        self.streams.lock().unwrap().insert(session.clone(), events);
        receiver
    }

    fn cancel(&self, session: &SessionId) {
        if let Some(events) = self.streams.lock().unwrap().get(session) {
            let _ = events.send(Event::Done(Ok("cancelled".to_owned())));
        }
    }

    fn close(&self, _session: &SessionId) {}
}

impl FakeBrain {
    /// The LLM says something (or ends) on its request.
    fn send(&self, event: Event) {
        let streams = self.streams.lock().unwrap();
        let _ = streams.values().next().unwrap().send(event);
    }
}

struct FakeMemory;

impl Memory for FakeMemory {
    async fn search(&self, _query: &str, _limit: u32) -> anyhow::Result<Vec<Found>> {
        Ok(Vec::new())
    }

    async fn heads_up(&self) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Out {
    Say(u32),
    Stop,
    Cue(CueKind, Option<String>),
}

#[derive(Default)]
struct Recorder(Mutex<Vec<Out>>);

impl Outbox for Recorder {
    async fn say(&self, say: Say) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(Out::Say(say.index));
        Ok(())
    }

    async fn stop(&self, _stop: Stop) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(Out::Stop);
        Ok(())
    }

    async fn cue(&self, cue: Cue) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(Out::Cue(cue.kind, cue.text));
        Ok(())
    }
}

impl Recorder {
    fn all(&self) -> Vec<Out> {
        self.0.lock().unwrap().clone()
    }
}

struct Bench {
    mind: Mind<FakeBrain, FakeMemory>,
    brain: Arc<FakeBrain>,
    out: Arc<Recorder>,
    waiter: Waiter,
}

fn bench(mode: Mode, source: Source) -> Bench {
    let brain = Arc::new(FakeBrain::default());
    let out = Arc::new(Recorder::default());
    let waiter = Waiter::with_seed(
        WaitingSettings {
            mode,
            pulse_delay: Duration::from_millis(1500),
            speech_delay: Duration::from_millis(800),
            source,
            max_phrases: 2,
        },
        Phrases::parse(FILE).unwrap(),
        3,
    );
    let mind = Mind::new(
        brain.clone(),
        Arc::new(FakeMemory),
        out.clone(),
        Settings {
            idle_timeout: Duration::from_secs(1800),
            search_timeout: Duration::from_millis(300),
            search_limit: 5,
            nothing_heard: "Je n'ai rien entendu.".to_owned(),
        },
    )
    .with_conversations(Conversations::default())
    .with_waiter(waiter.clone());
    Bench {
        mind,
        brain,
        out,
        waiter,
    }
}

fn device() -> Device {
    "office".parse().unwrap()
}

fn new_turn() -> TurnId {
    static LAST: Mutex<Option<TurnId>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    loop {
        let id = TurnId::new();
        if last.is_none_or(|l| id > l) {
            *last = Some(id);
            return id;
        }
    }
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(1)).await;
}

impl Bench {
    async fn press(&mut self, turn: TurnId) {
        self.mind
            .handle(Input::Button(
                device(),
                Button {
                    turn,
                    state: State::Start,
                    at: contract::now(),
                },
            ))
            .await;
    }

    /// The press, then the turn closed with `text`.
    async fn ask(&mut self, text: &str) -> TurnId {
        let id = new_turn();
        self.press(id).await;
        self.mind
            .handle(Input::Sentence(Sentence {
                turn: id,
                device: device(),
                index: 0,
                text: text.to_owned(),
                start: contract::now(),
                end: contract::now(),
            }))
            .await;
        self.mind
            .handle(Input::Turn(Turn {
                turn: id,
                device: device(),
                start: contract::now(),
                end: contract::now(),
                text: text.to_owned(),
                wake: Wake::Button,
                speaker: None,
                transcription_confidence: None,
                context: Vec::new(),
            }))
            .await;
        settle().await;
        id
    }

    fn tool(&self, name: &str, arguments: serde_json::Value) {
        let key = self.brain.keys.lock().unwrap().last().cloned();
        self.waiter
            .called(key.as_ref(), name, arguments.as_object());
    }
}

#[tokio::test(start_paused = true)]
async fn the_tone_leaves_when_the_turn_closes_before_any_sentence() {
    let mut b = bench(Mode::Tone, Source::Both);
    b.ask("quelle heure").await;
    assert_eq!(b.out.all(), [Out::Cue(CueKind::Tone, None)]);
    b.brain.send(Event::Text("Il est midi.".to_owned()));
    b.brain.send(Event::Done(Ok("end_turn".to_owned())));
    tokio::time::sleep(Duration::from_secs(10)).await;
    // The tone, then the answer: no pulse, the answer came in time.
    assert_eq!(b.out.all(), [Out::Cue(CueKind::Tone, None), Out::Say(0)]);
}

#[tokio::test(start_paused = true)]
async fn a_late_answer_gets_the_pulse_and_then_nothing_once_it_starts() {
    let mut b = bench(Mode::Tone, Source::Both);
    b.ask("quelle heure").await;
    tokio::time::sleep(Duration::from_millis(1600)).await;
    assert_eq!(
        b.out.all(),
        [
            Out::Cue(CueKind::Tone, None),
            Out::Cue(CueKind::Pulse, None)
        ]
    );
    b.brain.send(Event::Text("Il est midi. ".to_owned()));
    b.brain.send(Event::Text("Et demain.".to_owned()));
    b.brain.send(Event::Done(Ok("end_turn".to_owned())));
    tokio::time::sleep(Duration::from_secs(10)).await;
    let all = b.out.all();
    assert_eq!(all[2..], [Out::Say(0), Out::Say(1)]);
    assert!(!b.waiter.is_waiting(&device()));
}

#[tokio::test(start_paused = true)]
async fn off_sends_no_cue() {
    let mut b = bench(Mode::Off, Source::Both);
    b.ask("quelle heure").await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    b.brain.send(Event::Text("Midi.".to_owned()));
    b.brain.send(Event::Done(Ok("end_turn".to_owned())));
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(b.out.all().iter().all(|o| matches!(o, Out::Say(_))));
}

#[tokio::test(start_paused = true)]
async fn a_tool_call_makes_the_phrase_about_what_is_searched() {
    let mut b = bench(Mode::Speech, Source::Both);
    b.ask("on avait décidé quoi pour Atlas").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    b.tool("hippocampe_search", serde_json::json!({"query": "Atlas"}));
    settle().await;
    assert_eq!(
        b.out.all(),
        [Out::Cue(
            CueKind::Speech,
            Some("Je regarde ce que j'ai sur Atlas.".to_owned())
        )]
    );
    // The answer comes: nothing more of the cue.
    b.brain.send(Event::Text("On avait dit oui.".to_owned()));
    b.brain.send(Event::Done(Ok("end_turn".to_owned())));
    tokio::time::sleep(Duration::from_secs(10)).await;
    b.tool("hippocampe_search", serde_json::json!({"query": "Marc"}));
    settle().await;
    // The phrase, the answer, and nothing after it.
    assert_eq!(b.out.all().len(), 2);
    assert!(matches!(b.out.all()[1], Out::Say(0)));
}

#[tokio::test(start_paused = true)]
async fn the_prewritten_phrase_comes_when_the_wait_goes_on_and_not_when_the_answer_is_fast() {
    let mut b = bench(Mode::Speech, Source::Both);
    b.ask("bonjour").await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(
        b.out.all(),
        [Out::Cue(CueKind::Speech, Some("Je regarde.".to_owned()))]
    );

    let mut b = bench(Mode::Speech, Source::Both);
    b.ask("bonjour").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    b.brain.send(Event::Text("Bonjour !".to_owned()));
    b.brain.send(Event::Done(Ok("end_turn".to_owned())));
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(b.out.all().iter().all(|o| matches!(o, Out::Say(_))));
}

#[tokio::test(start_paused = true)]
async fn a_new_press_ends_the_cue_of_the_turn_before() {
    let mut b = bench(Mode::Tone, Source::Both);
    b.ask("quelle heure").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    // The owner presses again before the pulse: the voice is told to stop, and no pulse follows.
    b.press(new_turn()).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(b.out.all(), [Out::Cue(CueKind::Tone, None), Out::Stop]);
    assert!(!b.waiter.is_waiting(&device()));
}

#[tokio::test(start_paused = true)]
async fn a_turn_with_nothing_heard_gets_no_cue() {
    let mut b = bench(Mode::Tone, Source::Both);
    b.ask("  ").await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(b.out.all(), [Out::Say(0)]);
}
