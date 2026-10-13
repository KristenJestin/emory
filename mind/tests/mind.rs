//! The relay with a fake LLM, a fake memory and the sentences recorded instead of published: the
//! streaming cut, `last`, the routing, the pre-search and its timeout, the barge-in, the idle close,
//! Emory speaking on her own.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

use contract::{Button, Cue, CueKind, Device, Say, Sentence, State, Stop, Turn, TurnId, Wake};
use mind::{
    brain::{Brain, Event, SessionId},
    conversations::{ConversationKey, Conversations},
    memory::{Found, Memory},
    mind::{CHIME_LEAD, HELD_LIMIT, Input, Mind, Outbox, Settings},
    speech::{Origin, Speech},
};
use tokio::sync::{Notify, mpsc};

/// A request received by the fake LLM: the test streams its answer.
struct Prompt {
    session: SessionId,
    text: String,
    events: mpsc::UnboundedSender<Event>,
}

const NOTHING_HEARD: &str = "Je n'ai rien entendu.";

type Streams = Arc<Mutex<HashMap<SessionId, mpsc::UnboundedSender<Event>>>>;

/// Behaves like the ACP client over the agent: one request at a time per session (a request
/// while another runs is refused, and counted), a cancel ends the request `cancelled` (unless
/// `deaf`), a close ends it with an error.
#[derive(Default)]
struct FakeBrain {
    opened: Mutex<u32>,
    /// The conversation of each session opened, in order.
    keys: Mutex<Vec<ConversationKey>>,
    /// How many of the next openings fail.
    failing_opens: AtomicU32,
    /// The agent does not answer the cancels.
    deaf: AtomicBool,
    /// What the mind asked, in order: `prompt s1`, `cancel s1`, `close s1`.
    log: Mutex<Vec<String>>,
    /// Requests sent while another one was running on their session.
    refused: AtomicU32,
    cancels: Mutex<Vec<SessionId>>,
    closes: Mutex<Vec<SessionId>>,
    /// The requests running, by session; each ends at its `Done`.
    streams: Streams,
    prompts: Mutex<Option<mpsc::UnboundedSender<Prompt>>>,
}

impl FakeBrain {
    fn note(&self, what: &str, session: &SessionId) {
        self.log.lock().unwrap().push(format!("{what} {session}"));
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    /// The connection breaks: the streams close without their `Done`.
    fn drop_streams(&self) {
        self.streams.lock().unwrap().clear();
    }
}

impl Brain for FakeBrain {
    async fn open(&self, conversation: &ConversationKey) -> anyhow::Result<SessionId> {
        if self
            .failing_opens
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            anyhow::bail!("the adapter refused the session");
        }
        self.keys.lock().unwrap().push(conversation.clone());
        let mut opened = self.opened.lock().unwrap();
        *opened += 1;
        Ok(SessionId(format!("s{opened}")))
    }

    fn prompt(&self, session: &SessionId, text: String) -> mpsc::UnboundedReceiver<Event> {
        let (events, receiver) = mpsc::unbounded_channel();
        if self.streams.lock().unwrap().contains_key(session) {
            self.note("refused", session);
            self.refused.fetch_add(1, Ordering::SeqCst);
            let _ = events.send(Event::Done(Err("a request is still running".to_owned())));
            return receiver;
        }
        self.note("prompt", session);
        // What the test sends goes through here: the request ends at its `Done`.
        let (input, mut from_test) = mpsc::unbounded_channel::<Event>();
        self.streams
            .lock()
            .unwrap()
            .insert(session.clone(), input.clone());
        let streams = self.streams.clone();
        let id = session.clone();
        let mine = input.downgrade();
        tokio::spawn(async move {
            while let Some(event) = from_test.recv().await {
                let done = matches!(event, Event::Done(_));
                let _ = events.send(event);
                if done {
                    let mut streams = streams.lock().unwrap();
                    let current = streams.get(&id);
                    if mine
                        .upgrade()
                        .is_some_and(|m| current.is_some_and(|c| c.same_channel(&m)))
                    {
                        streams.remove(&id);
                    }
                    return;
                }
            }
        });
        let prompts = self.prompts.lock().unwrap();
        let _ = prompts.as_ref().unwrap().send(Prompt {
            session: session.clone(),
            text,
            events: input,
        });
        receiver
    }

    /// Like the agent: the request in progress ends, `cancelled`.
    fn cancel(&self, session: &SessionId) {
        self.note("cancel", session);
        self.cancels.lock().unwrap().push(session.clone());
        if self.deaf.load(Ordering::SeqCst) {
            return;
        }
        if let Some(events) = self.streams.lock().unwrap().get(session) {
            let _ = events.send(Event::Done(Ok("cancelled".to_owned())));
        }
    }

    fn close(&self, session: &SessionId) {
        self.note("close", session);
        self.closes.lock().unwrap().push(session.clone());
        if let Some(events) = self.streams.lock().unwrap().get(session) {
            let _ = events.send(Event::Done(Err("the session was closed".to_owned())));
        }
    }
}

/// Finds what a sentence names after a delay, by its text.
#[derive(Default)]
struct FakeMemory {
    entries: HashMap<String, (Duration, Vec<Found>)>,
}

impl Memory for FakeMemory {
    async fn search(&self, query: &str, _limit: u32) -> anyhow::Result<Vec<Found>> {
        let (delay, found) = self.entries.get(query).cloned().unwrap_or_default();
        tokio::time::sleep(delay).await;
        Ok(found)
    }

    async fn heads_up(&self) -> anyhow::Result<Option<String>> {
        Ok(Some("{\"upcoming\":[\"dentiste\"]}".to_owned()))
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Out {
    Say(Say),
    Stop(Stop),
    Cue(Cue),
}

#[derive(Default)]
struct Recorder {
    out: Mutex<Vec<Out>>,
    changed: Notify,
}

impl Outbox for Recorder {
    async fn say(&self, say: Say) -> anyhow::Result<()> {
        self.out.lock().unwrap().push(Out::Say(say));
        self.changed.notify_waiters();
        Ok(())
    }

    async fn stop(&self, stop: Stop) -> anyhow::Result<()> {
        self.out.lock().unwrap().push(Out::Stop(stop));
        self.changed.notify_waiters();
        Ok(())
    }

    async fn cue(&self, cue: Cue) -> anyhow::Result<()> {
        self.out.lock().unwrap().push(Out::Cue(cue));
        self.changed.notify_waiters();
        Ok(())
    }
}

impl Recorder {
    fn all(&self) -> Vec<Out> {
        self.out.lock().unwrap().clone()
    }

    fn says(&self) -> Vec<(String, u32, String, bool)> {
        self.all()
            .into_iter()
            .filter_map(|o| match o {
                Out::Say(s) => Some((s.device.to_string(), s.index, s.text, s.last)),
                _ => None,
            })
            .collect()
    }

    async fn wait_for(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let changed = self.changed.notified();
                if self.out.lock().unwrap().len() >= count {
                    return;
                }
                changed.await;
            }
        })
        .await
        .expect("the expected messages never came");
    }
}

struct Bench {
    mind: Mind<FakeBrain, FakeMemory>,
    brain: Arc<FakeBrain>,
    out: Arc<Recorder>,
    prompts: mpsc::UnboundedReceiver<Prompt>,
    conversations: Conversations,
}

fn bench(memory: FakeMemory) -> Bench {
    let brain = Arc::new(FakeBrain::default());
    let (tx, prompts) = mpsc::unbounded_channel();
    *brain.prompts.lock().unwrap() = Some(tx);
    let out = Arc::new(Recorder::default());
    let conversations = Conversations::default();
    let mind = Mind::new(
        brain.clone(),
        Arc::new(memory),
        out.clone(),
        Settings {
            idle_timeout: Duration::from_secs(30 * 60),
            search_timeout: Duration::from_millis(300),
            search_limit: 5,
            nothing_heard: NOTHING_HEARD.to_owned(),
        },
    )
    .with_conversations(conversations.clone());
    Bench {
        mind,
        brain,
        out,
        prompts,
        conversations,
    }
}

/// A turn newer than all the others, as the ear makes them (ULIDs made in the same millisecond do
/// not sort by time).
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

fn device(name: &str) -> Device {
    name.parse().unwrap()
}

fn press(name: &str, turn: TurnId) -> Input {
    Input::Button(
        device(name),
        Button {
            turn,
            state: State::Start,
            at: contract::now(),
        },
    )
}

fn sentence(name: &str, turn: TurnId, index: u32, text: &str) -> Input {
    Input::Sentence(Sentence {
        turn,
        device: device(name),
        index,
        text: text.to_owned(),
        start: contract::now(),
        end: contract::now(),
    })
}

fn turn(name: &str, turn: TurnId, text: &str) -> Input {
    Input::Turn(Turn {
        turn,
        device: device(name),
        start: contract::now(),
        end: contract::now(),
        text: text.to_owned(),
        wake: Wake::Button,
        speaker: None,
        transcription_confidence: None,
        context: Vec::new(),
    })
}

fn found(slug: &str) -> Found {
    Found {
        slug: slug.to_owned(),
        kind: "decision".to_owned(),
        title: format!("Fiche {slug}"),
        summary: format!("Résumé de {slug}."),
        excerpt: String::new(),
        date: None,
    }
}

impl Bench {
    /// One whole turn heard on a device: the press, the sentences, the turn.
    async fn ask(&mut self, name: &str, sentences: &[&str]) -> (TurnId, Prompt) {
        let id = new_turn();
        self.mind.handle(press(name, id)).await;
        for (i, s) in sentences.iter().enumerate() {
            self.mind.handle(sentence(name, id, i as u32, s)).await;
        }
        self.mind.handle(turn(name, id, &sentences.join(" "))).await;
        let prompt = tokio::time::timeout(Duration::from_secs(5), self.prompts.recv())
            .await
            .expect("no request reached the LLM")
            .unwrap();
        (id, prompt)
    }
}

fn text(piece: &str) -> Event {
    Event::Text(piece.to_owned())
}

fn done() -> Event {
    Event::Done(Ok("end_turn".to_owned()))
}

#[tokio::test]
async fn the_answer_streams_sentence_by_sentence_with_the_last_marked() {
    let mut b = bench(FakeMemory::default());
    let (id, prompt) = b.ask("office", &["bonjour"]).await;
    prompt.events.send(text("Bonjour Léa. Le con")).unwrap();
    prompt.events.send(text("trat finit ")).unwrap();
    b.out.wait_for(1).await;
    assert_eq!(
        b.out.says(),
        [("office".into(), 0, "Bonjour Léa.".into(), false)]
    );
    prompt.events.send(text("en mars.")).unwrap();
    prompt.events.send(done()).unwrap();
    b.out.wait_for(2).await;
    assert_eq!(
        b.out.says(),
        [
            ("office".into(), 0, "Bonjour Léa.".into(), false),
            ("office".into(), 1, "Le contrat finit en mars.".into(), true)
        ]
    );
    assert!(
        b.out
            .all()
            .iter()
            .all(|o| matches!(o, Out::Say(s) if s.turn == id))
    );
}

#[tokio::test]
async fn the_answer_goes_to_the_device_that_heard_in_its_own_conversation() {
    let mut b = bench(FakeMemory::default());
    let (_, office) = b.ask("office", &["un"]).await;
    let (_, salon) = b.ask("salon", &["deux"]).await;
    assert_ne!(office.session, salon.session);
    salon.events.send(text("Pour le salon.")).unwrap();
    salon.events.send(done()).unwrap();
    b.out.wait_for(1).await;
    office.events.send(text("Pour le bureau.")).unwrap();
    office.events.send(done()).unwrap();
    b.out.wait_for(2).await;
    assert_eq!(
        b.out.says(),
        [
            ("salon".into(), 0, "Pour le salon.".into(), true),
            ("office".into(), 0, "Pour le bureau.".into(), true)
        ]
    );
    assert_eq!(*b.brain.opened.lock().unwrap(), 2);
}

#[tokio::test]
async fn the_request_carries_the_heads_up_once_and_the_entries_found() {
    let mut memory = FakeMemory::default();
    memory.entries.insert(
        "la musique".into(),
        (Duration::ZERO, vec![found("musique"), found("salon")]),
    );
    memory.entries.insert(
        "du salon".into(),
        (Duration::ZERO, vec![found("salon"), found("enceinte")]),
    );
    let mut b = bench(memory);
    let (_, first) = b
        .ask("office", &["la musique", "du salon", "rien du tout"])
        .await;
    assert!(
        first
            .text
            .contains("[Ce que la mémoire signale pour aujourd'hui]")
    );
    assert!(first.text.contains("dentiste"));
    // Each entry once, in the order found.
    let titles: Vec<&str> = first
        .text
        .lines()
        .filter_map(|l| l.strip_prefix("- "))
        .map(|l| l.split(" (").next().unwrap())
        .collect();
    assert_eq!(titles, ["Fiche musique", "Fiche salon", "Fiche enceinte"]);
    assert!(
        first
            .text
            .ends_with("[On te dit]\nla musique du salon rien du tout")
    );
    first.events.send(done()).unwrap();

    let (_, second) = b.ask("office", &["et après"]).await;
    assert_eq!(second.session, first.session);
    assert!(second.text.starts_with("[Maintenant] "));
    assert!(second.text.ends_with("\n\n[On te dit]\net après"));
    assert!(!second.text.contains("[Ce que la mémoire") && !second.text.contains("[Fiches"));
}

#[tokio::test(start_paused = true)]
async fn a_slow_search_is_given_up_and_the_answer_goes_on() {
    let mut memory = FakeMemory::default();
    memory.entries.insert(
        "vite".into(),
        (Duration::from_millis(50), vec![found("vite")]),
    );
    memory
        .entries
        .insert("lent".into(), (Duration::from_secs(2), vec![found("lent")]));
    let mut b = bench(memory);
    let started = tokio::time::Instant::now();
    let (_, prompt) = b.ask("office", &["vite", "lent"]).await;
    assert!(started.elapsed() < Duration::from_millis(400));
    assert!(prompt.text.contains("Fiche vite"));
    assert!(!prompt.text.contains("Fiche lent"));
}

#[tokio::test]
async fn a_press_while_answering_cancels_the_llm_and_stops_the_voice() {
    let mut b = bench(FakeMemory::default());
    let (first, prompt) = b.ask("office", &["raconte"]).await;
    prompt
        .events
        .send(text("Première phrase. Deuxième"))
        .unwrap();
    b.out.wait_for(1).await;

    // The owner presses again: the request is cancelled, the voice stopped.
    let next = new_turn();
    b.mind.handle(press("office", next)).await;
    assert_eq!(
        *b.brain.cancels.lock().unwrap(),
        std::slice::from_ref(&prompt.session)
    );
    // What the LLM still sends is not said.
    let _ = prompt.events.send(text(" phrase. Troisième."));
    let _ = prompt.events.send(done());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        b.out.all(),
        [
            Out::Say(Say {
                turn: first,
                device: device("office"),
                index: 0,
                text: "Première phrase.".into(),
                last: false
            }),
            Out::Stop(Stop {
                turn: first,
                device: device("office")
            })
        ]
    );

    // The next turn is answered in the same conversation.
    b.mind.handle(sentence("office", next, 0, "et alors")).await;
    b.mind.handle(turn("office", next, "et alors")).await;
    let again = b.prompts.recv().await.unwrap();
    assert_eq!(again.session, prompt.session);
    again.events.send(text("Alors rien.")).unwrap();
    again.events.send(done()).unwrap();
    b.out.wait_for(3).await;
    assert_eq!(
        b.out.says()[1],
        ("office".into(), 0, "Alors rien.".into(), true)
    );
}

#[tokio::test]
async fn a_press_after_the_answer_still_stops_the_voice_without_cancelling() {
    let mut b = bench(FakeMemory::default());
    let (first, prompt) = b.ask("office", &["bonjour"]).await;
    prompt.events.send(text("Salut.")).unwrap();
    prompt.events.send(done()).unwrap();
    b.out.wait_for(1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    b.mind.handle(press("office", new_turn())).await;
    b.out.wait_for(2).await;
    assert!(b.brain.cancels.lock().unwrap().is_empty());
    assert_eq!(
        b.out.all().last(),
        Some(&Out::Stop(Stop {
            turn: first,
            device: device("office")
        }))
    );
}

#[tokio::test]
async fn nothing_heard_is_said_without_asking_the_llm() {
    for empty in ["", "  ", " ... ", "?! , ;"] {
        let mut b = bench(FakeMemory::default());
        let id = new_turn();
        b.mind.handle(press("office", id)).await;
        b.mind.handle(turn("office", id, empty)).await;
        b.out.wait_for(1).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(b.prompts.try_recv().is_err(), "{empty:?}");
        assert_eq!(
            b.out.all(),
            [Out::Say(Say {
                turn: id,
                device: device("office"),
                index: 0,
                text: NOTHING_HEARD.into(),
                last: true
            })],
            "{empty:?}"
        );
        assert!(!b.brain.log().iter().any(|l| l.starts_with("prompt")));
    }
}

#[tokio::test]
async fn nothing_heard_opens_no_session_of_its_own() {
    // The press opens the conversation (while the owner speaks): the empty turn opens nothing more,
    // and a turn that came without its press opens nothing at all.
    let mut b = bench(FakeMemory::default());
    let id = new_turn();
    b.mind.handle(turn("office", id, " ")).await;
    b.out.wait_for(1).await;
    assert!(!b.mind.has_conversation(&device("office")));
    assert_eq!(*b.brain.opened.lock().unwrap(), 0);
    assert_eq!(b.out.says().len(), 1);
}

#[tokio::test]
async fn a_press_while_nothing_heard_is_said_stops_it() {
    let mut b = bench(FakeMemory::default());
    let id = new_turn();
    b.mind.handle(press("office", id)).await;
    b.mind.handle(turn("office", id, "")).await;
    b.out.wait_for(1).await;
    b.mind.handle(press("office", new_turn())).await;
    b.out.wait_for(2).await;
    assert!(b.brain.cancels.lock().unwrap().is_empty());
    assert_eq!(
        b.out.all().last(),
        Some(&Out::Stop(Stop {
            turn: id,
            device: device("office")
        }))
    );
}

#[tokio::test]
async fn a_turn_heard_after_nothing_heard_is_answered_in_the_same_conversation() {
    let mut b = bench(FakeMemory::default());
    let empty = new_turn();
    b.mind.handle(press("office", empty)).await;
    b.mind.handle(turn("office", empty, "")).await;
    let (id, prompt) = b.ask("office", &["bonjour"]).await;
    prompt.events.send(text("Salut.")).unwrap();
    prompt.events.send(done()).unwrap();
    b.out.wait_for(3).await;
    assert_eq!(*b.brain.opened.lock().unwrap(), 1);
    let all = b.out.all();
    assert!(matches!(&all[0], Out::Say(s) if s.turn == empty && s.last));
    assert_eq!(
        all[1],
        Out::Stop(Stop {
            turn: empty,
            device: device("office")
        })
    );
    assert!(matches!(&all[2], Out::Say(s) if s.turn == id && s.text == "Salut."));
}

#[tokio::test]
async fn nothing_heard_without_its_press_stops_the_older_answer() {
    let mut b = bench(FakeMemory::default());
    let (first, prompt) = b.ask("office", &["raconte"]).await;
    prompt
        .events
        .send(text("Première phrase. Deuxième"))
        .unwrap();
    b.out.wait_for(1).await;
    let next = new_turn();
    b.mind.handle(turn("office", next, "...")).await;
    b.out.wait_for(3).await;
    assert_eq!(
        *b.brain.cancels.lock().unwrap(),
        std::slice::from_ref(&prompt.session)
    );
    let all = b.out.all();
    assert_eq!(
        all[1],
        Out::Stop(Stop {
            turn: first,
            device: device("office")
        })
    );
    assert!(matches!(&all[2], Out::Say(s) if s.turn == next && s.text == NOTHING_HEARD && s.last));
}

#[tokio::test]
async fn an_empty_sentence_opens_no_conversation() {
    let mut b = bench(FakeMemory::default());
    b.mind
        .handle(sentence("office", new_turn(), 0, " ... "))
        .await;
    assert!(!b.mind.has_conversation(&device("office")));
    assert_eq!(*b.brain.opened.lock().unwrap(), 0);
}

#[tokio::test]
async fn a_late_empty_turn_is_not_said() {
    let mut b = bench(FakeMemory::default());
    let old = new_turn();
    let newer = new_turn();
    b.mind.handle(press("office", newer)).await;
    b.mind.handle(turn("office", old, "")).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(b.out.all().is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_idle_conversation_closes_and_the_next_press_opens_another() {
    let mut b = bench(FakeMemory::default());
    let (_, prompt) = b.ask("office", &["bonjour"]).await;
    prompt.events.send(text("Salut.")).unwrap();
    prompt.events.send(done()).unwrap();
    b.out.wait_for(1).await;

    tokio::time::sleep(Duration::from_secs(29 * 60)).await;
    b.mind.handle(Input::Tick).await;
    assert!(b.mind.has_conversation(&device("office")));
    tokio::time::sleep(Duration::from_secs(2 * 60)).await;
    b.mind.handle(Input::Tick).await;
    assert!(!b.mind.has_conversation(&device("office")));
    assert_eq!(
        *b.brain.closes.lock().unwrap(),
        std::slice::from_ref(&prompt.session)
    );

    let (_, again) = b.ask("office", &["re"]).await;
    assert_ne!(again.session, prompt.session);
    assert!(again.text.contains("[Ce que la mémoire signale"));
}

/// What the tools see of a conversation: its key, given to its session, and the user's last turn,
/// until it closes.
#[tokio::test(start_paused = true)]
async fn the_tools_follow_the_turns_of_the_conversation() {
    let mut b = bench(FakeMemory::default());
    let (first, prompt) = b.ask("office", &["bonjour"]).await;
    let key = b.brain.keys.lock().unwrap()[0].clone();
    assert_eq!(b.conversations.turn(&key), Some(Some(first)));
    prompt.events.send(done()).unwrap();
    b.out.wait_for(1).await;
    let (second, prompt) = b.ask("office", &["oui"]).await;
    assert_eq!(b.conversations.turn(&key), Some(Some(second)));
    // Another device has its own conversation.
    prompt.events.send(done()).unwrap();
    let (other, _) = b.ask("kitchen", &["salut"]).await;
    let other_key = b.brain.keys.lock().unwrap()[1].clone();
    assert_ne!(other_key, key);
    assert_eq!(b.conversations.turn(&other_key), Some(Some(other)));
    assert_eq!(b.conversations.turn(&key), Some(Some(second)));

    tokio::time::sleep(Duration::from_secs(31 * 60)).await;
    b.mind.handle(Input::Tick).await;
    assert_eq!(b.conversations.turn(&key), None);
}

#[tokio::test]
async fn a_failed_request_says_so() {
    let mut b = bench(FakeMemory::default());
    let (_, prompt) = b.ask("office", &["bonjour"]).await;
    prompt
        .events
        .send(Event::Done(Err("rate limited".to_owned())))
        .unwrap();
    b.out.wait_for(1).await;
    assert_eq!(
        b.out.says(),
        [("office".into(), 0, mind::mind::FAILURE.into(), true)]
    );
}

/// Waits until the fake LLM gets a request.
async fn next_prompt(b: &mut Bench) -> Prompt {
    tokio::time::timeout(Duration::from_secs(30), b.prompts.recv())
        .await
        .expect("no request reached the LLM")
        .unwrap()
}

/// Lets the spawned tasks run (the clock is paused in these tests, and moves only when they
/// all wait).
async fn settle() {
    tokio::time::sleep(Duration::from_millis(1)).await;
}

// Finding 1: the barge-in takes the lock before cancelling.

#[tokio::test(start_paused = true)]
async fn a_press_before_the_request_leaves_means_no_request() {
    let mut memory = FakeMemory::default();
    memory.entries.insert(
        "lent".into(),
        (Duration::from_millis(200), vec![found("lent")]),
    );
    let mut b = bench(memory);
    let first = new_turn();
    b.mind.handle(press("office", first)).await;
    b.mind.handle(sentence("office", first, 0, "lent")).await;
    b.mind.handle(turn("office", first, "lent")).await;
    // The answer waits for its search: the owner presses again.
    settle().await;
    b.mind.handle(press("office", new_turn())).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        b.prompts.try_recv().is_err(),
        "a stopped answer sent its request"
    );
    assert!(!b.brain.log().iter().any(|l| l.starts_with("prompt")));
    b.out.wait_for(1).await;
    assert_eq!(
        b.out.all(),
        [Out::Stop(Stop {
            turn: first,
            device: device("office")
        })]
    );
}

#[tokio::test]
async fn the_cancel_of_a_barge_in_follows_its_request() {
    let mut b = bench(FakeMemory::default());
    let (_, prompt) = b.ask("office", &["raconte"]).await;
    b.mind.handle(press("office", new_turn())).await;
    let s = &prompt.session;
    assert_eq!(
        b.brain.log(),
        [format!("prompt {s}"), format!("cancel {s}")]
    );
}

// Finding 2: a session takes one request at a time.

#[tokio::test(start_paused = true)]
async fn a_request_waits_for_the_end_of_the_previous_one() {
    let mut b = bench(FakeMemory::default());
    b.brain.deaf.store(true, Ordering::SeqCst);
    let (_, first) = b.ask("office", &["raconte"]).await;
    let next = new_turn();
    b.mind.handle(press("office", next)).await;
    b.mind.handle(turn("office", next, "et alors")).await;
    // The agent has not ended the first request yet: the second one waits.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(b.prompts.try_recv().is_err());
    // Its late text is not taken for the next answer.
    first.events.send(text("Une vieille phrase. Et")).unwrap();
    first
        .events
        .send(Event::Done(Ok("cancelled".into())))
        .unwrap();
    let second = next_prompt(&mut b).await;
    assert_eq!(second.session, first.session);
    second.events.send(text("Rien.")).unwrap();
    second.events.send(done()).unwrap();
    b.out.wait_for(2).await;
    assert_eq!(b.out.says(), [("office".into(), 0, "Rien.".into(), true)]);
    assert_eq!(b.brain.refused.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn a_turn_without_its_press_cancels_the_previous_request_and_waits_for_it() {
    let mut b = bench(FakeMemory::default());
    let (_, first) = b.ask("office", &["raconte"]).await;
    first.events.send(text("Il était une fois. Un")).unwrap();
    b.out.wait_for(1).await;
    // The press was lost: the turn comes alone.
    b.mind.handle(turn("office", new_turn(), "stop")).await;
    let second = next_prompt(&mut b).await;
    let s = &first.session;
    assert_eq!(
        b.brain.log(),
        [
            format!("prompt {s}"),
            format!("cancel {s}"),
            format!("prompt {s}")
        ]
    );
    assert_eq!(second.session, first.session);
    assert_eq!(b.brain.refused.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn a_session_whose_request_never_ends_is_replaced() {
    let mut b = bench(FakeMemory::default());
    b.brain.deaf.store(true, Ordering::SeqCst);
    let (first_turn, first) = b.ask("office", &["raconte"]).await;
    let next = new_turn();
    b.mind.handle(press("office", next)).await;
    b.mind.handle(turn("office", next, "et alors")).await;
    let second = next_prompt(&mut b).await;
    // Given up after the wait, closed, and a new session opened, told the heads-up again.
    assert_ne!(second.session, first.session);
    assert!(second.text.contains("[Ce que la mémoire signale"));
    let log = b.brain.log();
    let (s1, s2) = (&first.session, &second.session);
    assert_eq!(
        log,
        [
            format!("prompt {s1}"),
            format!("cancel {s1}"),
            format!("close {s1}"),
            format!("prompt {s2}")
        ]
    );
    // The old request, ended by the close, says nothing more.
    let _ = first.events.send(text("Trop tard."));
    second.events.send(text("Voilà.")).unwrap();
    second.events.send(done()).unwrap();
    b.out.wait_for(2).await;
    assert_eq!(
        b.out.all(),
        [
            Out::Stop(Stop {
                turn: first_turn,
                device: device("office")
            }),
            Out::Say(Say {
                turn: next,
                device: device("office"),
                index: 0,
                text: "Voilà.".into(),
                last: true
            })
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_silent_llm_is_given_up_but_its_request_is_waited_for() {
    let mut b = bench(FakeMemory::default());
    b.brain.deaf.store(true, Ordering::SeqCst);
    let (_, first) = b.ask("office", &["raconte"]).await;
    // Nothing comes for a minute: the owner is told, the request cancelled.
    tokio::time::sleep(mind::mind::STALL + Duration::from_secs(1)).await;
    assert_eq!(
        b.out.says(),
        [("office".into(), 0, mind::mind::FAILURE.into(), true)]
    );
    assert_eq!(
        *b.brain.cancels.lock().unwrap(),
        std::slice::from_ref(&first.session)
    );
    // What comes late is not said, and the session stays busy until the end comes.
    first.events.send(text("Enfin une réponse. Et")).unwrap();
    let next = new_turn();
    b.mind.handle(press("office", next)).await;
    b.mind.handle(turn("office", next, "allô")).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(b.prompts.try_recv().is_err());
    first.events.send(done()).unwrap();
    let second = next_prompt(&mut b).await;
    assert_eq!(second.session, first.session);
    assert_eq!(b.out.says().len(), 1);
    assert_eq!(b.brain.refused.load(Ordering::SeqCst), 0);
}

// Findings 3 and 5: the presses and the hearing come in no order between them.

#[tokio::test]
async fn a_late_turn_after_a_newer_press_is_not_answered() {
    let mut memory = FakeMemory::default();
    memory
        .entries
        .insert("vieux".into(), (Duration::ZERO, vec![found("vieux")]));
    let mut b = bench(memory);
    let old = new_turn();
    let new = new_turn();
    b.mind.handle(press("office", old)).await;
    b.mind.handle(press("office", new)).await;
    // What the hearing says of the old turn comes after the new press.
    b.mind.handle(sentence("office", old, 0, "vieux")).await;
    b.mind.handle(turn("office", old, "vieux")).await;
    b.mind.handle(sentence("office", new, 0, "neuf")).await;
    b.mind.handle(turn("office", new, "neuf")).await;
    let prompt = next_prompt(&mut b).await;
    assert!(prompt.text.ends_with("[On te dit]\nneuf"));
    assert!(!prompt.text.contains("Fiche vieux"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(b.prompts.try_recv().is_err(), "the old turn was answered");
}

#[tokio::test]
async fn a_late_press_does_not_stop_the_answer_of_its_turn() {
    let mut b = bench(FakeMemory::default());
    let id = new_turn();
    // The turn comes before its press.
    b.mind.handle(sentence("office", id, 0, "bonjour")).await;
    b.mind.handle(turn("office", id, "bonjour")).await;
    let prompt = next_prompt(&mut b).await;
    b.mind.handle(press("office", id)).await;
    prompt.events.send(text("Salut.")).unwrap();
    prompt.events.send(done()).unwrap();
    b.out.wait_for(1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(b.brain.cancels.lock().unwrap().is_empty());
    assert_eq!(b.out.says(), [("office".into(), 0, "Salut.".into(), true)]);
    assert!(b.out.all().iter().all(|o| matches!(o, Out::Say(_))));
}

#[tokio::test]
async fn a_press_keeps_the_searches_its_turn_already_started() {
    let mut memory = FakeMemory::default();
    memory.entries.insert(
        "la musique".into(),
        (Duration::ZERO, vec![found("musique")]),
    );
    let mut b = bench(memory);
    let id = new_turn();
    // The first sentence comes before the press.
    b.mind.handle(sentence("office", id, 0, "la musique")).await;
    b.mind.handle(press("office", id)).await;
    b.mind.handle(sentence("office", id, 1, "du salon")).await;
    b.mind
        .handle(turn("office", id, "la musique du salon"))
        .await;
    let prompt = next_prompt(&mut b).await;
    assert!(prompt.text.contains("Fiche musique"));
}

// Finding 4: a session that failed to open is opened again.

#[tokio::test]
async fn a_failed_opening_is_tried_again_at_the_next_turn() {
    let mut b = bench(FakeMemory::default());
    b.brain.failing_opens.store(1, Ordering::SeqCst);
    let id = new_turn();
    b.mind.handle(press("office", id)).await;
    b.mind.handle(turn("office", id, "bonjour")).await;
    b.out.wait_for(1).await;
    assert_eq!(
        b.out.says(),
        [("office".into(), 0, mind::mind::FAILURE.into(), true)]
    );
    let (_, prompt) = b.ask("office", &["bonjour"]).await;
    assert_eq!(prompt.session.0, "s1");
    assert!(prompt.text.contains("[Ce que la mémoire signale"));
}

// Finding 6: the voice always gets its `last`.

#[tokio::test]
async fn a_failure_after_some_sentences_still_ends_the_turn() {
    let mut b = bench(FakeMemory::default());
    let (_, prompt) = b.ask("office", &["raconte"]).await;
    prompt.events.send(text("Un. Deux. Trois")).unwrap();
    prompt
        .events
        .send(Event::Done(Err("overloaded".into())))
        .unwrap();
    b.out.wait_for(3).await;
    assert_eq!(
        b.out.says(),
        [
            ("office".into(), 0, "Un.".into(), false),
            ("office".into(), 1, "Deux.".into(), false),
            ("office".into(), 2, "Trois".into(), true)
        ]
    );
}

#[tokio::test]
async fn a_remainder_with_nothing_to_say_still_ends_the_turn() {
    let mut b = bench(FakeMemory::default());
    let (_, prompt) = b.ask("office", &["raconte"]).await;
    prompt.events.send(text("Un. Deux. **")).unwrap();
    prompt.events.send(done()).unwrap();
    b.out.wait_for(3).await;
    assert_eq!(
        b.out.says(),
        [
            ("office".into(), 0, "Un.".into(), false),
            ("office".into(), 1, "Deux.".into(), false),
            ("office".into(), 2, "".into(), true)
        ]
    );
}

#[tokio::test]
async fn a_stream_that_closes_without_its_end_still_ends_the_turn() {
    let mut b = bench(FakeMemory::default());
    let (_, prompt) = b.ask("office", &["raconte"]).await;
    prompt.events.send(text("Un. Deux")).unwrap();
    b.out.wait_for(1).await;
    b.brain.drop_streams();
    drop(prompt);
    b.out.wait_for(2).await;
    assert_eq!(
        b.out.says(),
        [
            ("office".into(), 0, "Un.".into(), false),
            ("office".into(), 1, "".into(), true)
        ]
    );
}

#[tokio::test]
async fn an_empty_answer_says_it_failed() {
    let mut b = bench(FakeMemory::default());
    let (_, prompt) = b.ask("office", &["raconte"]).await;
    prompt.events.send(done()).unwrap();
    b.out.wait_for(1).await;
    assert_eq!(
        b.out.says(),
        [("office".into(), 0, mind::mind::FAILURE.into(), true)]
    );
}

// Emory speaking on her own (`specs/events.md`, E1). The clock is paused: the voice is guessed to
// speak for the estimated length of what was queued.

const OWN: &str = "Le minuteur est fini.";

fn speech(name: &str, chime: bool) -> Input {
    Input::Speak(Speech::new(device(name), OWN, Origin::Timer).chime(chime))
}

fn release(name: &str, turn: TurnId) -> Input {
    Input::Button(
        device(name),
        Button {
            turn,
            state: State::End,
            at: contract::now(),
        },
    )
}

impl Bench {
    /// Lets the time pass, then lets the mind look at its waiting speeches.
    async fn after(&mut self, wait: Duration) {
        tokio::time::sleep(wait).await;
        self.mind.handle(Input::Tick).await;
        settle().await;
    }

    /// The sentences said on Emory's own (`OWN`), with their turn.
    fn own(&self) -> Vec<Say> {
        self.out
            .all()
            .into_iter()
            .filter_map(|o| match o {
                Out::Say(s) if s.text == OWN => Some(s),
                _ => None,
            })
            .collect()
    }
}

#[tokio::test(start_paused = true)]
async fn a_free_device_hears_the_tone_then_the_sentence_in_a_turn_of_its_own() {
    let mut b = bench(FakeMemory::default());
    b.mind.handle(speech("office", true)).await;
    settle().await;
    let all = b.out.all();
    let [Out::Cue(cue)] = all.as_slice() else {
        panic!("the tone first, alone: {all:?}");
    };
    assert_eq!(cue.kind, CueKind::Tone);
    b.after(CHIME_LEAD).await;
    let all = b.out.all();
    assert_eq!(
        all[1],
        Out::Say(Say {
            turn: cue.turn,
            device: device("office"),
            index: 0,
            text: OWN.into(),
            last: true
        })
    );
    assert_eq!(all.len(), 2);
    // No LLM, no conversation.
    assert_eq!(*b.brain.opened.lock().unwrap(), 0);
    assert!(!b.mind.has_conversation(&device("office")));
    assert!(!b.mind.has_pending_speech());
}

#[tokio::test(start_paused = true)]
async fn without_the_chime_only_the_sentence_goes() {
    let mut b = bench(FakeMemory::default());
    b.mind.handle(speech("office", false)).await;
    settle().await;
    assert_eq!(b.out.says(), [("office".into(), 0, OWN.into(), true)]);
    assert!(!b.out.all().iter().any(|o| matches!(o, Out::Cue(_))));
}

#[tokio::test(start_paused = true)]
async fn a_long_text_is_cut_into_sentences_the_last_marked() {
    let mut b = bench(FakeMemory::default());
    let text = "La sauvegarde a échoué. Le disque est plein.";
    b.mind
        .handle(Input::Speak(
            Speech::new(device("office"), text, Origin::Event).chime(false),
        ))
        .await;
    settle().await;
    assert_eq!(
        b.out.says(),
        [
            ("office".into(), 0, "La sauvegarde a échoué.".into(), false),
            ("office".into(), 1, "Le disque est plein.".into(), true)
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn it_waits_for_the_end_of_an_answer_and_for_the_voice_to_say_it() {
    let mut b = bench(FakeMemory::default());
    let (_, prompt) = b.ask("office", &["raconte"]).await;
    prompt.events.send(text("Il était une fois.")).unwrap();
    settle().await;
    b.mind.handle(speech("office", false)).await;
    b.after(Duration::from_secs(30)).await;
    assert!(b.own().is_empty(), "spoken during the answer");

    prompt.events.send(text(" Fin.")).unwrap();
    prompt.events.send(done()).unwrap();
    settle().await;
    b.mind.handle(Input::Tick).await;
    settle().await;
    assert!(b.own().is_empty(), "spoken while the voice says the answer");
    // The answer is over and said (as far as the mind can tell): Emory speaks at once.
    b.after(Duration::from_secs(5)).await;
    let says = b.out.says();
    assert_eq!(says.len(), 3);
    assert_eq!(says[2], ("office".into(), 0, OWN.into(), true));
}

#[tokio::test(start_paused = true)]
async fn it_waits_for_the_button_and_the_turn_heard() {
    let mut b = bench(FakeMemory::default());
    let id = new_turn();
    b.mind.handle(press("office", id)).await;
    b.mind.handle(speech("other", false)).await;
    b.mind.handle(speech("office", false)).await;
    settle().await;
    // Another device is free.
    assert_eq!(b.out.says(), [("other".into(), 0, OWN.into(), true)]);
    b.after(Duration::from_secs(10)).await;
    assert_eq!(b.own().len(), 1, "spoken while the button is held");
    b.mind.handle(release("office", id)).await;
    b.after(Duration::from_secs(1)).await;
    assert_eq!(b.own().len(), 1, "spoken before the turn is heard");
    // Nothing in the turn: Emory says so, then her own sentence, once the voice is done.
    b.mind.handle(turn("office", id, "")).await;
    settle().await;
    assert_eq!(b.own().len(), 1, "spoken over the answer");
    b.after(Duration::from_secs(5)).await;
    let says = b.out.says();
    assert_eq!(says[1], ("office".into(), 0, NOTHING_HEARD.into(), true));
    assert_eq!(says[2], ("office".into(), 0, OWN.into(), true));
}

#[tokio::test(start_paused = true)]
async fn a_lost_release_does_not_silence_her_for_good() {
    let mut b = bench(FakeMemory::default());
    b.mind.handle(press("office", new_turn())).await;
    b.mind.handle(speech("office", false)).await;
    b.after(HELD_LIMIT - Duration::from_secs(1)).await;
    assert!(b.own().is_empty());
    b.after(Duration::from_secs(2)).await;
    assert_eq!(b.own().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_speech_that_waits_too_long_is_dropped() {
    let mut b = bench(FakeMemory::default());
    b.mind.handle(press("office", new_turn())).await;
    b.mind
        .handle(Input::Speak(
            Speech::new(device("office"), OWN, Origin::Event)
                .chime(false)
                .expires_in(Duration::from_secs(30)),
        ))
        .await;
    b.after(Duration::from_secs(31)).await;
    assert!(!b.mind.has_pending_speech());
    b.after(HELD_LIMIT).await;
    assert!(b.own().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_press_stops_it_whatever_the_order_of_the_turns() {
    let mut b = bench(FakeMemory::default());
    // Drawn before the mind draws its own: an ear whose clock is behind.
    let early = new_turn();
    b.mind.handle(speech("office", false)).await;
    settle().await;
    let said = b.own();
    assert_eq!(said.len(), 1);
    b.mind.handle(press("office", early)).await;
    settle().await;
    assert_eq!(
        b.out.all().last(),
        Some(&Out::Stop(Stop {
            turn: said[0].turn,
            device: device("office")
        }))
    );
}

#[tokio::test(start_paused = true)]
async fn a_press_during_the_tone_means_no_sentence() {
    let mut b = bench(FakeMemory::default());
    b.mind.handle(speech("office", true)).await;
    settle().await;
    b.mind.handle(press("office", new_turn())).await;
    b.after(CHIME_LEAD * 2).await;
    let all = b.out.all();
    assert!(matches!(&all[..], [Out::Cue(_), Out::Stop(_)]), "{all:?}");
}

#[tokio::test(start_paused = true)]
async fn the_next_request_carries_what_she_said_once() {
    let mut b = bench(FakeMemory::default());
    b.mind.handle(speech("office", false)).await;
    settle().await;
    b.after(Duration::from_secs(5)).await;
    let (_, prompt) = b.ask("office", &["relance-le"]).await;
    assert!(
        prompt
            .text
            .contains(&format!("[Tu as dit de toi-même]\n- {OWN}\n")),
        "{}",
        prompt.text
    );
    prompt.events.send(text("D'accord.")).unwrap();
    prompt.events.send(done()).unwrap();
    b.after(Duration::from_secs(5)).await;
    let (_, again) = b.ask("office", &["merci"]).await;
    assert!(!again.text.contains("[Tu as dit de toi-même]"));
}

#[tokio::test(start_paused = true)]
async fn what_she_said_is_forgotten_after_half_an_hour() {
    let mut b = bench(FakeMemory::default());
    b.mind.handle(speech("office", false)).await;
    settle().await;
    b.after(Duration::from_secs(31 * 60)).await;
    let (_, prompt) = b.ask("office", &["bonjour"]).await;
    assert!(!prompt.text.contains("[Tu as dit de toi-même]"));
}

#[tokio::test(start_paused = true)]
async fn a_followed_speech_is_told_when_said_then_of_the_next_press() {
    use mind::speech::Heard;
    let mut b = bench(FakeMemory::default());
    let id = new_turn();
    b.mind.handle(press("office", id)).await;
    let (said, mut follow) = Speech::new(device("office"), OWN, Origin::Timer).followed();
    b.mind.handle(Input::Speak(said)).await;
    // Pressed before it is said: not a press after it.
    b.mind.handle(release("office", id)).await;
    b.mind.handle(turn("office", id, "")).await;
    b.after(Duration::from_secs(1)).await;
    assert!(follow.try_recv().is_err(), "said over the answer");
    b.after(Duration::from_secs(5)).await;
    assert_eq!(follow.try_recv(), Ok(Heard::Said));
    assert!(follow.try_recv().is_err());
    b.mind.handle(press("office", new_turn())).await;
    assert_eq!(follow.try_recv(), Ok(Heard::Pressed));
    // Nothing more comes.
    assert!(follow.recv().await.is_none());

    // One never said is told so.
    b.mind.handle(press("salon", new_turn())).await;
    let (dropped, mut follow) = Speech::new(device("salon"), OWN, Origin::Timer)
        .expires_in(Duration::from_secs(10))
        .followed();
    b.mind.handle(Input::Speak(dropped)).await;
    b.after(Duration::from_secs(11)).await;
    assert_eq!(follow.try_recv(), Ok(Heard::Dropped));
}

// The timers and reminders ringing (`specs/time.md`, T2): the module, its ringer and the mind,
// wired as in `mind::run`, the clock paused. The time of the module follows tokio's.

mod ring {
    use super::*;
    use contract::{TimerFired, TimerKind, Timers};
    use mind::{
        speech::{self, Speeches},
        time::{Time, TimeConfig, TokioClock, ring::Board, store::Kind},
        tools::Module,
    };
    use serde_json::json;

    // A Tuesday evening: 19:00 in Paris.
    const EVENING: &str = "2026-10-13T17:00:00Z";
    const PASTA: &str = "Le minuteur pâtes est fini.";

    #[derive(Debug, Clone, PartialEq)]
    enum Posted {
        Changed(Timers),
        Fired(TimerFired),
    }

    #[derive(Default)]
    struct FakeBoard(Mutex<Vec<Posted>>);

    impl Board for FakeBoard {
        async fn changed(&self, timers: Timers) {
            self.0.lock().unwrap().push(Posted::Changed(timers));
        }

        async fn fired(&self, fired: TimerFired) {
            self.0.lock().unwrap().push(Posted::Fired(fired));
        }
    }

    impl FakeBoard {
        fn posted(&self) -> Vec<Posted> {
            self.0.lock().unwrap().clone()
        }

        fn fired(&self) -> Vec<TimerFired> {
            self.posted()
                .into_iter()
                .filter_map(|p| match p {
                    Posted::Fired(f) => Some(f),
                    _ => None,
                })
                .collect()
        }

        /// How many items each `mind.timer.changed` listed, in order.
        fn changes(&self) -> Vec<usize> {
            self.posted()
                .into_iter()
                .filter_map(|p| match p {
                    Posted::Changed(t) => Some(t.items.len()),
                    _ => None,
                })
                .collect()
        }
    }

    struct Ring {
        b: Bench,
        time: Arc<Time>,
        speeches: Speeches,
        board: Arc<FakeBoard>,
        dir: tempfile::TempDir,
    }

    fn config() -> TimeConfig {
        TimeConfig {
            timezone: "Europe/Paris".to_owned(),
            ..TimeConfig::default()
        }
    }

    /// The module and its ringer, started at `origin` (the wall clock) on a file of `dir`.
    fn start(b: Bench, dir: tempfile::TempDir, origin: &str, config: TimeConfig) -> Ring {
        let time = Arc::new(
            Time::with_clock(
                &config,
                &dir.path().join("timers.json"),
                Arc::new(TokioClock::new(origin.parse().unwrap())),
                b.conversations.clone(),
            )
            .unwrap(),
        );
        let (speaker, speeches) = speech::channel();
        let board = Arc::new(FakeBoard::default());
        tokio::spawn(time.ringer(&config, speaker, board.clone()).run());
        Ring {
            b,
            time,
            speeches,
            board,
            dir,
        }
    }

    fn ring(config: TimeConfig) -> Ring {
        start(
            bench(FakeMemory::default()),
            tempfile::tempdir().unwrap(),
            EVENING,
            config,
        )
    }

    impl Ring {
        /// Lets the time pass as the run loop of `mind` would: the speeches asked go to the mind,
        /// which looks at its waiting ones every 250 ms.
        async fn run_for(&mut self, wait: Duration) {
            let end = tokio::time::Instant::now() + wait;
            loop {
                settle().await;
                while let Ok(speech) = self.speeches.try_recv() {
                    self.b.mind.handle(Input::Speak(speech)).await;
                }
                self.b.mind.handle(Input::Tick).await;
                settle().await;
                let now = tokio::time::Instant::now();
                if now >= end {
                    return;
                }
                tokio::time::sleep((end - now).min(Duration::from_millis(250))).await;
            }
        }

        /// Sets a timer through the tool, in a conversation (or none).
        async fn timer(&self, seconds: u64, label: Option<&str>, key: Option<ConversationKey>) {
            let mut arguments = json!({ "seconds": seconds });
            if let Some(label) = label {
                arguments["label"] = json!(label);
            }
            let result = self
                .time
                .call_in("timer", arguments.as_object().cloned(), key)
                .await;
            assert_ne!(result.is_error, Some(true), "{result:?}");
        }

        /// A whole exchange on a device, answered: its conversation is the mind's last one.
        async fn exchange(&mut self, name: &str) -> ConversationKey {
            let (_, prompt) = self.b.ask(name, &["mets un minuteur"]).await;
            prompt.events.send(text("C'est parti.")).unwrap();
            prompt.events.send(done()).unwrap();
            settle().await;
            self.b.brain.keys.lock().unwrap().last().unwrap().clone()
        }

        /// The phrases said, with their device.
        fn said(&self, phrase: &str) -> Vec<String> {
            self.b
                .out
                .says()
                .into_iter()
                .filter(|(_, _, text, _)| text == phrase)
                .map(|(device, ..)| device)
                .collect()
        }

        fn file(&self) -> String {
            std::fs::read_to_string(self.dir.path().join("timers.json")).unwrap_or_default()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_phrase_leaves_at_the_due_time_on_the_device_it_was_set_on() {
        let mut r = ring(config());
        // Set in a conversation of the office: the mind opens it on its device.
        let key = r.exchange("office").await;
        r.run_for(Duration::from_secs(5)).await;
        r.timer(60, Some("pâtes"), Some(key)).await;
        assert!(r.file().contains("\"device\": \"office\""), "{}", r.file());
        r.run_for(Duration::from_secs(59)).await;
        assert!(r.said(PASTA).is_empty(), "said before its time");
        assert!(r.board.fired().is_empty());
        r.run_for(Duration::from_secs(2)).await;
        assert_eq!(r.said(PASTA), ["office"]);
        // The tone first, in the turn of the phrase.
        let all = r.b.out.all();
        let at = all
            .iter()
            .position(|o| matches!(o, Out::Say(s) if s.text == PASTA))
            .unwrap();
        let Out::Say(say) = &all[at] else {
            unreachable!()
        };
        assert!(
            matches!(&all[at - 1], Out::Cue(c) if c.kind == CueKind::Tone && c.turn == say.turn),
            "{all:?}"
        );
        // On the bus: fired, the list before, with it, and empty once it is said.
        let fired = r.board.fired();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].item.kind, TimerKind::Timer);
        assert_eq!(fired[0].item.label.as_deref(), Some("pâtes"));
        assert_eq!(fired[0].item.device, Some(device("office")));
        // Set five seconds (and the few milliseconds the test lets pass) after 19:00.
        let due = fired[0].item.due.to_rfc3339();
        assert!(due.starts_with("2026-10-13T19:01:05"), "{due}");
        assert_eq!(fired[0].late_s, 0);
        assert_eq!(r.board.changes(), [0, 1, 0]);
        // Out of the file.
        assert!(!r.file().contains("\"timer\""), "{}", r.file());
    }

    #[tokio::test(start_paused = true)]
    async fn it_waits_for_the_end_of_an_answer() {
        let mut r = ring(config());
        r.timer(
            30,
            Some("pâtes"),
            Some(r.b.conversations.open_on(device("office"))),
        )
        .await;
        r.run_for(Duration::from_secs(20)).await;
        let (_, prompt) = r.b.ask("office", &["raconte"]).await;
        prompt.events.send(text("Il était une fois.")).unwrap();
        r.run_for(Duration::from_secs(30)).await;
        assert!(r.said(PASTA).is_empty(), "said over the answer");
        // It rang at its time all the same.
        assert_eq!(r.board.fired().len(), 1);
        prompt.events.send(done()).unwrap();
        r.run_for(Duration::from_secs(5)).await;
        assert_eq!(r.said(PASTA), ["office"]);
        let says = r.b.out.says();
        let answer = says.iter().position(|s| s.2 == "Il était une fois.");
        let phrase = says.iter().position(|s| s.2 == PASTA);
        assert!(answer < phrase, "{says:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn it_is_said_again_once_when_nobody_presses_then_stops() {
        let mut r = ring(config());
        r.timer(
            10,
            Some("pâtes"),
            Some(r.b.conversations.open_on(device("office"))),
        )
        .await;
        r.run_for(Duration::from_secs(15)).await;
        assert_eq!(r.said(PASTA).len(), 1);
        r.run_for(Duration::from_secs(55)).await;
        assert_eq!(r.said(PASTA).len(), 1, "said again too soon");
        r.run_for(Duration::from_secs(10)).await;
        assert_eq!(r.said(PASTA).len(), 2);
        // `repeat = 1`: no third time; it rang once on the bus.
        r.run_for(Duration::from_secs(600)).await;
        assert_eq!(r.said(PASTA).len(), 2);
        assert_eq!(r.board.fired().len(), 1);
        // Each time in a turn of its own, after its tone.
        let tones =
            r.b.out
                .all()
                .iter()
                .filter(|o| matches!(o, Out::Cue(c) if c.kind == CueKind::Tone))
                .count();
        assert_eq!(tones, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_repetitions_follow_the_configuration() {
        let mut r = ring(TimeConfig {
            repeat: 2,
            repeat_after_s: 20,
            chime: false,
            ..config()
        });
        r.timer(
            10,
            Some("pâtes"),
            Some(r.b.conversations.open_on(device("office"))),
        )
        .await;
        r.run_for(Duration::from_secs(200)).await;
        assert_eq!(r.said(PASTA).len(), 3);
        assert!(!r.b.out.all().iter().any(|o| matches!(o, Out::Cue(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn a_press_on_the_device_stops_the_repetition() {
        let mut r = ring(config());
        r.timer(
            10,
            Some("pâtes"),
            Some(r.b.conversations.open_on(device("office"))),
        )
        .await;
        r.run_for(Duration::from_secs(15)).await;
        assert_eq!(r.said(PASTA).len(), 1);
        // The owner presses to say something else; a press elsewhere would not count.
        r.b.mind.handle(press("salon", new_turn())).await;
        r.run_for(Duration::from_secs(10)).await;
        let id = new_turn();
        r.b.mind.handle(press("office", id)).await;
        r.b.mind.handle(release("office", id)).await;
        r.b.mind.handle(turn("office", id, "")).await;
        r.run_for(Duration::from_secs(300)).await;
        assert_eq!(r.said(PASTA).len(), 1, "said again after a press");
    }

    #[tokio::test(start_paused = true)]
    async fn a_press_elsewhere_does_not_stop_it() {
        let mut r = ring(config());
        r.timer(
            10,
            Some("pâtes"),
            Some(r.b.conversations.open_on(device("office"))),
        )
        .await;
        r.run_for(Duration::from_secs(15)).await;
        let id = new_turn();
        r.b.mind.handle(press("salon", id)).await;
        r.b.mind.handle(release("salon", id)).await;
        r.run_for(Duration::from_secs(120)).await;
        assert_eq!(r.said(PASTA), ["office", "office"]);
    }

    #[tokio::test(start_paused = true)]
    async fn what_came_due_during_a_stop_is_said_late_at_start_up() {
        let dir = tempfile::tempdir().unwrap();
        let first = start(bench(FakeMemory::default()), dir, EVENING, config());
        first
            .timer(
                60,
                Some("pâtes"),
                Some(first.b.conversations.open_on(device("office"))),
            )
            .await;
        // `mind` stops before it rings, and starts again three hours later.
        let Ring { dir, .. } = first;
        let mut r = start(
            bench(FakeMemory::default()),
            dir,
            "2026-10-13T20:00:00Z",
            config(),
        );
        r.run_for(Duration::from_secs(1)).await;
        assert_eq!(
            r.said("Avec du retard : le minuteur pâtes est fini."),
            ["office"]
        );
        let fired = r.board.fired();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].late_s, 3 * 3600 - 60);
        assert_eq!(r.board.changes(), [1, 0]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_minute_late_is_not_late() {
        let dir = tempfile::tempdir().unwrap();
        let first = start(bench(FakeMemory::default()), dir, EVENING, config());
        first
            .timer(
                60,
                Some("pâtes"),
                Some(first.b.conversations.open_on(device("office"))),
            )
            .await;
        let Ring { dir, .. } = first;
        let mut r = start(
            bench(FakeMemory::default()),
            dir,
            "2026-10-13T17:01:30Z",
            config(),
        );
        r.run_for(Duration::from_secs(1)).await;
        assert_eq!(r.said(PASTA), ["office"]);
        assert_eq!(r.board.fired()[0].late_s, 30);
    }

    #[tokio::test(start_paused = true)]
    async fn what_is_later_than_the_limit_is_dropped_not_said() {
        let dir = tempfile::tempdir().unwrap();
        let first = start(bench(FakeMemory::default()), dir, EVENING, config());
        first
            .timer(
                60,
                Some("pâtes"),
                Some(first.b.conversations.open_on(device("office"))),
            )
            .await;
        let Ring { dir, .. } = first;
        // Thirteen hours: past the twelve of `late_limit_hours`.
        let mut r = start(
            bench(FakeMemory::default()),
            dir,
            "2026-10-14T06:00:00Z",
            config(),
        );
        r.run_for(Duration::from_secs(120)).await;
        assert!(r.b.out.all().is_empty(), "{:?}", r.b.out.all());
        assert!(r.board.fired().is_empty());
        assert_eq!(r.board.changes(), [0]);
        assert!(!r.file().contains("\"timer\""), "{}", r.file());
    }

    #[tokio::test(start_paused = true)]
    async fn without_its_device_it_rings_on_the_default_then_where_the_owner_last_spoke() {
        let mut r = ring(TimeConfig {
            default_device: Some("kitchen".to_owned()),
            ..config()
        });
        r.timer(10, Some("pâtes"), None).await;
        r.run_for(Duration::from_secs(15)).await;
        assert_eq!(r.said(PASTA), ["kitchen"]);

        let mut r = ring(config());
        r.exchange("salon").await;
        r.timer(10, Some("pâtes"), None).await;
        r.run_for(Duration::from_secs(15)).await;
        assert_eq!(r.said(PASTA), ["salon"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_reminder_says_its_text_and_a_timer_without_label_its_length() {
        let mut r = ring(config());
        let office = r.b.conversations.open_on(device("office"));
        r.timer(90, None, Some(office.clone())).await;
        let result = r
            .time
            .call_in(
                "reminder",
                json!({"text": "sortir le linge", "in_seconds": 120})
                    .as_object()
                    .cloned(),
                Some(office),
            )
            .await;
        assert_ne!(result.is_error, Some(true));
        r.run_for(Duration::from_secs(125)).await;
        assert_eq!(
            r.said("Le minuteur de une minute et trente secondes est fini."),
            ["office"]
        );
        assert_eq!(r.said("Rappel : sortir le linge."), ["office"]);
        let fired = r.board.fired();
        assert_eq!(fired[1].item.kind, TimerKind::Reminder);
        assert_eq!(fired[1].item.text.as_deref(), Some("sortir le linge"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancelled_timer_never_rings() {
        let mut r = ring(config());
        // The ringer starts and publishes the empty list.
        settle().await;
        r.timer(
            30,
            Some("pâtes"),
            Some(r.b.conversations.open_on(device("office"))),
        )
        .await;
        r.run_for(Duration::from_secs(10)).await;
        let result = r
            .time
            .call_in("cancel", json!({"id": 1}).as_object().cloned(), None)
            .await;
        assert_ne!(result.is_error, Some(true));
        r.run_for(Duration::from_secs(60)).await;
        assert!(r.said(PASTA).is_empty());
        assert!(r.board.fired().is_empty());
        assert_eq!(r.board.changes(), [0, 1, 0]);
    }

    #[test]
    fn the_store_kinds_are_the_bus_kinds() {
        // The file says `timer` and `reminder`, like the contract.
        assert_eq!(
            serde_json::to_value(Kind::Reminder).unwrap(),
            serde_json::to_value(TimerKind::Reminder).unwrap()
        );
    }
}
