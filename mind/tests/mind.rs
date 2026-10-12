//! The relay with a fake LLM, a fake memory and the sentences recorded instead of published: the
//! streaming cut, `last`, the routing, the pre-search and its timeout, the barge-in, the idle close.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use contract::{Button, Device, Say, Sentence, State, Stop, Turn, TurnId, Wake};
use mind::{
    brain::{Brain, Event, SessionId},
    memory::{Found, Memory},
    mind::{Input, Mind, Outbox, Settings},
};
use tokio::sync::{Notify, mpsc};

/// A request received by the fake LLM: the test streams its answer.
struct Prompt {
    session: SessionId,
    text: String,
    events: mpsc::UnboundedSender<Event>,
}

#[derive(Default)]
struct FakeBrain {
    opened: Mutex<u32>,
    cancels: Mutex<Vec<SessionId>>,
    closes: Mutex<Vec<SessionId>>,
    streams: Mutex<HashMap<SessionId, mpsc::UnboundedSender<Event>>>,
    prompts: Mutex<Option<mpsc::UnboundedSender<Prompt>>>,
}

impl Brain for FakeBrain {
    async fn open(&self) -> anyhow::Result<SessionId> {
        let mut opened = self.opened.lock().unwrap();
        *opened += 1;
        Ok(SessionId(format!("s{opened}")))
    }

    async fn prompt(
        &self,
        session: &SessionId,
        text: String,
    ) -> anyhow::Result<mpsc::UnboundedReceiver<Event>> {
        let (events, receiver) = mpsc::unbounded_channel();
        self.streams
            .lock()
            .unwrap()
            .insert(session.clone(), events.clone());
        let prompts = self.prompts.lock().unwrap();
        prompts.as_ref().unwrap().send(Prompt {
            session: session.clone(),
            text,
            events,
        })?;
        Ok(receiver)
    }

    /// Like the agent: the request in progress ends, `cancelled`.
    fn cancel(&self, session: &SessionId) {
        self.cancels.lock().unwrap().push(session.clone());
        if let Some(events) = self.streams.lock().unwrap().get(session) {
            let _ = events.send(Event::Done(Ok("cancelled".to_owned())));
        }
    }

    fn close(&self, session: &SessionId) {
        self.closes.lock().unwrap().push(session.clone());
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
                Out::Stop(_) => None,
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
    mind: Mind<FakeBrain, FakeMemory, Recorder>,
    brain: Arc<FakeBrain>,
    out: Arc<Recorder>,
    prompts: mpsc::UnboundedReceiver<Prompt>,
}

fn bench(memory: FakeMemory) -> Bench {
    let brain = Arc::new(FakeBrain::default());
    let (tx, prompts) = mpsc::unbounded_channel();
    *brain.prompts.lock().unwrap() = Some(tx);
    let out = Arc::new(Recorder::default());
    let mind = Mind::new(
        brain.clone(),
        Arc::new(memory),
        out.clone(),
        Settings {
            idle_timeout: Duration::from_secs(30 * 60),
            search_timeout: Duration::from_millis(300),
            search_limit: 5,
        },
    );
    Bench {
        mind,
        brain,
        out,
        prompts,
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
        let id = TurnId::new();
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
    prompt.events.send(text("Bonjour Kris. Le con")).unwrap();
    prompt.events.send(text("trat finit ")).unwrap();
    b.out.wait_for(1).await;
    assert_eq!(
        b.out.says(),
        [("office".into(), 0, "Bonjour Kris.".into(), false)]
    );
    prompt.events.send(text("en mars.")).unwrap();
    prompt.events.send(done()).unwrap();
    b.out.wait_for(2).await;
    assert_eq!(
        b.out.says(),
        [
            ("office".into(), 0, "Bonjour Kris.".into(), false),
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
            .ends_with("[Kris dit]\nla musique du salon rien du tout")
    );
    first.events.send(done()).unwrap();

    let (_, second) = b.ask("office", &["et après"]).await;
    assert_eq!(second.session, first.session);
    assert!(second.text.starts_with("[Maintenant] "));
    assert!(second.text.ends_with("\n\n[Kris dit]\net après"));
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

    // Kris presses again: the request is cancelled, the voice stopped.
    let next = TurnId::new();
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
    b.mind.handle(press("office", TurnId::new())).await;
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
async fn nothing_heard_gets_no_answer() {
    let mut b = bench(FakeMemory::default());
    let id = TurnId::new();
    b.mind.handle(press("office", id)).await;
    b.mind.handle(turn("office", id, "  ")).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(b.prompts.try_recv().is_err());
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
