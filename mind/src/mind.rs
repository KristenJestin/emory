//! The relay: the conversations, the searches made while Kris speaks, the answer streamed
//! sentence by sentence to the device that heard him, and the barge-in.
//!
//! One [`Mind`] follows every device, fed in order by the bus loop ([`Input`]). Per device:
//!
//! - **the conversation**, opened at the first press: an LLM session, and what the memory flags
//!   for the moment, both fetched while Kris is still speaking. It closes after a time without an
//!   exchange; a session that failed to open is opened again at the next turn;
//! - **the turn being heard**: each transcribed sentence is searched in the memory at once, as it
//!   is, given up beyond the search timeout;
//! - **the answer**: a task that sends the request, cuts the streamed text into sentences and
//!   publishes each one (`voice.say`), the last one marked `last`.
//!
//! The presses and what the hearing says come on two subscriptions, in no order between them: a
//! sentence or a turn older than the newest turn seen on the device is dropped (the turns are
//! ULIDs, which sort by time), and a press never wipes the searches its own turn already started.
//!
//! A press while an answer is running or being said cancels the request (ACP `session/cancel`)
//! and publishes `voice.stop`. The request is sent and the sentences are queued under a lock that
//! the barge-in takes first: a request either never goes, or goes before its cancel; once
//! `voice.stop` is queued, no sentence of that turn follows it. The messages to the voice leave
//! from a queue of their own, in order: nothing waits on the bus under that lock.
//!
//! A session takes one request at a time: a request waits for the end (`Done`) of the previous
//! one, cancelling it. If the agent never ends it, the session is closed and another one opened.

use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use contract::{Button, Device, Say, Sentence, State, Stop, Turn, TurnId};
use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared, join_all},
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::{Instant, timeout},
};
use tracing::{debug, info, warn};

use crate::{
    brain::{Brain, Event, SessionId},
    config::has_words,
    memory::{Found, Memory},
    request,
    sentences::Splitter,
};

/// Said when the LLM cannot be reached, or gave nothing: Kris pressed and waits for something.
pub const FAILURE: &str = "Je n'arrive pas à réfléchir pour le moment.";

/// How long a request waits for the previous one of its session to end once cancelled, before
/// the session is given up as broken.
pub const CANCEL_WAIT: Duration = Duration::from_secs(10);
/// How long an answer may stay silent before Kris is told it failed.
pub const STALL: Duration = Duration::from_secs(60);
/// How long the heads-up may delay the opening of a conversation.
const HEADS_UP_WAIT: Duration = Duration::from_secs(2);

/// Where the sentences go: the bus, or a recorder in the tests.
pub trait Outbox: Send + Sync + 'static {
    fn say(&self, say: Say) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn stop(&self, stop: Stop) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// What the mind hears, in the order of the bus.
#[derive(Clone, Debug)]
pub enum Input {
    Button(Device, Button),
    Sentence(Sentence),
    Turn(Turn),
    /// Time passes: the idle conversations close.
    Tick,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub idle_timeout: Duration,
    pub search_timeout: Duration,
    pub search_limit: u32,
    /// Said when a turn has nothing in it, instead of asking the LLM.
    pub nothing_heard: String,
}

/// A conversation once open: its session, and what the memory flagged at its start.
#[derive(Clone, Debug)]
struct Opened {
    session: SessionId,
    heads_up: Option<String>,
}

type Opening = Shared<BoxFuture<'static, Result<Opened, String>>>;

/// The session of a conversation, shared with its answers: an answer that finds the session
/// broken opens another one.
struct Link {
    opening: Opening,
    /// The heads-up went with a request already: the session keeps it.
    told: bool,
}

impl Link {
    fn new(opening: Opening) -> Self {
        Self {
            opening,
            told: false,
        }
    }

    fn session(&self) -> Option<SessionId> {
        match self.opening.peek() {
            Some(Ok(opened)) => Some(opened.session.clone()),
            _ => None,
        }
    }

    fn failed(&self) -> bool {
        matches!(self.opening.peek(), Some(Err(_)))
    }
}

struct Conversation {
    link: Arc<Mutex<Link>>,
    last_exchange: Instant,
    /// The last answer: requests go one at a time.
    previous: Option<Previous>,
}

impl Conversation {
    fn session(&self) -> Option<SessionId> {
        self.link.lock().unwrap().session()
    }

    fn answering(&self) -> bool {
        self.previous
            .as_ref()
            .is_some_and(|p| !p.task.is_finished())
    }
}

/// Whether an answer task is still running: its sender is dropped when it ends, whatever way.
#[derive(Clone)]
struct Running(watch::Receiver<()>);

impl Running {
    fn new() -> (watch::Sender<()>, Self) {
        let (done, running) = watch::channel(());
        (done, Self(running))
    }

    fn is_finished(&self) -> bool {
        self.0.has_changed().is_err()
    }

    async fn finished(mut self) {
        while self.0.changed().await.is_ok() {}
    }
}

/// Decides, under one lock, between the request of an answer and its barge-in.
#[derive(Default)]
struct Gate(Mutex<GateState>);

#[derive(Default)]
struct GateState {
    /// Set by the barge-in: no request goes, no sentence is queued any more.
    stopped: bool,
    /// The session the request went to.
    prompted: Option<SessionId>,
}

impl Gate {
    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.0.lock().unwrap()
    }
}

/// The previous answer of a conversation, for the next request to wait for.
#[derive(Clone)]
struct Previous {
    gate: Arc<Gate>,
    task: Running,
}

/// The turn being heard, and the searches of its sentences.
struct Hearing {
    turn: TurnId,
    searches: Vec<JoinHandle<Vec<Found>>>,
}

/// The last answer of a device, until the next press.
struct Answer {
    turn: TurnId,
    gate: Arc<Gate>,
    task: Running,
}

#[derive(Default)]
struct DeviceState {
    conversation: Option<Conversation>,
    hearing: Option<Hearing>,
    answer: Option<Answer>,
    /// The newest turn seen on this device (press, sentence or turn): what is older is late.
    latest: Option<TurnId>,
}

impl DeviceState {
    /// Notes a turn seen on the device; `false` if it is older than the newest one.
    fn see(&mut self, turn: TurnId) -> bool {
        if self.latest.is_some_and(|latest| turn < latest) {
            return false;
        }
        self.latest = Some(turn);
        true
    }
}

/// What goes to the voice.
enum Message {
    Say(Say),
    Stop(Stop),
}

/// The messages to the voice, published in the order they were queued, by a task of their own.
#[derive(Clone)]
struct Mailbox(mpsc::UnboundedSender<Message>);

impl Mailbox {
    fn start<O: Outbox>(out: Arc<O>) -> Self {
        let (sender, mut messages) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(message) = messages.recv().await {
                match message {
                    Message::Say(say) => {
                        if let Err(e) = out.say(say).await {
                            warn!("publishing voice.say: {e:#}");
                        }
                    }
                    Message::Stop(stop) => {
                        if let Err(e) = out.stop(stop).await {
                            warn!("publishing voice.stop: {e:#}");
                        }
                    }
                }
            }
        });
        Self(sender)
    }

    fn send(&self, message: Message) {
        let _ = self.0.send(message);
    }
}

pub struct Mind<B, M> {
    brain: Arc<B>,
    memory: Arc<M>,
    mail: Mailbox,
    settings: Settings,
    devices: HashMap<Device, DeviceState>,
}

impl<B: Brain, M: Memory> Mind<B, M> {
    /// A mind publishing to `out`. Starts a task: call it inside the runtime.
    pub fn new<O: Outbox>(brain: Arc<B>, memory: Arc<M>, out: Arc<O>, settings: Settings) -> Self {
        Self {
            brain,
            memory,
            mail: Mailbox::start(out),
            settings,
            devices: HashMap::new(),
        }
    }

    pub async fn handle(&mut self, input: Input) {
        match input {
            Input::Button(device, button) => {
                if button.state == State::Start {
                    self.pressed(&device, button.turn);
                }
            }
            Input::Sentence(sentence) => self.heard(sentence),
            Input::Turn(turn) => self.answer(turn),
            Input::Tick => self.close_idle(),
        }
    }

    /// Whether a conversation is open on this device (for the tests and the logs).
    pub fn has_conversation(&self, device: &Device) -> bool {
        self.devices
            .get(device)
            .is_some_and(|s| s.conversation.is_some())
    }

    fn state(&mut self, device: &Device) -> &mut DeviceState {
        self.devices.entry(device.clone()).or_default()
    }

    /// Kris presses: what Emory says of an older turn stops, and this turn is heard.
    fn pressed(&mut self, device: &Device, turn: TurnId) {
        if !self.state(device).see(turn) {
            debug!(%device, %turn, "late press: ignored");
            return;
        }
        self.barge_in(device, turn);
        self.open(device);
        let state = self.state(device);
        // Its sentences may have come first: their searches are kept.
        if state.hearing.as_ref().is_none_or(|h| h.turn != turn) {
            state.hearing = Some(Hearing {
                turn,
                searches: Vec::new(),
            });
        }
    }

    /// Stops the answer of this device to a turn older than `turn`, if one is running or may
    /// still be said.
    fn barge_in(&mut self, device: &Device, turn: TurnId) {
        let brain = self.brain.clone();
        let mail = self.mail.clone();
        let state = self.state(device);
        if state.answer.as_ref().is_none_or(|a| a.turn >= turn) {
            return;
        }
        let Some(answer) = state.answer.take() else {
            return;
        };
        // Stopped first, under the lock the request is sent with: either the request went
        // before, and its cancel follows it to the agent, or it never goes.
        let prompted = {
            let mut gate = answer.gate.lock();
            if gate.stopped {
                return;
            }
            gate.stopped = true;
            gate.prompted.clone()
        };
        if !answer.task.is_finished()
            && let Some(session) = prompted
        {
            brain.cancel(&session);
        }
        mail.send(Message::Stop(Stop {
            turn: answer.turn,
            device: device.clone(),
        }));
        info!(%device, turn = %answer.turn, "barge-in: answer stopped");
    }

    /// Opens the conversation of this device if it has none (or if it has gone idle), opens its
    /// session again if it failed to open, and notes the exchange.
    fn open(&mut self, device: &Device) {
        let idle = self.settings.idle_timeout;
        let brain = self.brain.clone();
        let memory = self.memory.clone();
        let state = self.state(device);
        if let Some(c) = &state.conversation
            && c.last_exchange.elapsed() >= idle
            && !c.answering()
        {
            if let Some(session) = c.session() {
                brain.close(&session);
            }
            state.conversation = None;
            info!(%device, "conversation closed (idle)");
        }
        match &mut state.conversation {
            Some(c) => {
                let mut link = c.link.lock().unwrap();
                if link.failed() {
                    info!(%device, "the session failed to open: opening it again");
                    *link = Link::new(opening(brain, memory, device.clone()));
                }
                drop(link);
                c.last_exchange = Instant::now();
            }
            None => {
                state.conversation = Some(Conversation {
                    link: Arc::new(Mutex::new(Link::new(opening(
                        brain,
                        memory,
                        device.clone(),
                    )))),
                    last_exchange: Instant::now(),
                    previous: None,
                });
            }
        }
    }

    /// A sentence heard while Kris speaks: searched in the memory at once.
    fn heard(&mut self, sentence: Sentence) {
        let device = sentence.device.clone();
        if !self.state(&device).see(sentence.turn) {
            debug!(%device, turn = %sentence.turn, "sentence of an older turn: ignored");
            return;
        }
        // Nothing in it: nothing to search, and no conversation to open for it.
        if !has_words(&sentence.text) {
            return;
        }
        // The press may come after it, or have been missed (a mind that restarts): the turn
        // starts here.
        if self
            .state(&device)
            .hearing
            .as_ref()
            .is_none_or(|h| h.turn != sentence.turn)
        {
            self.open(&device);
            self.state(&device).hearing = Some(Hearing {
                turn: sentence.turn,
                searches: Vec::new(),
            });
        }
        let memory = self.memory.clone();
        let limit = self.settings.search_limit;
        let give_up = self.settings.search_timeout;
        let index = sentence.index;
        let search = tokio::spawn(async move {
            let started = Instant::now();
            match timeout(give_up, memory.search(&sentence.text, limit)).await {
                Ok(Ok(found)) => {
                    info!(
                        sentence = index,
                        found = found.len(),
                        ms = started.elapsed().as_millis() as u64,
                        "memory searched"
                    );
                    found
                }
                Ok(Err(e)) => {
                    info!(sentence = index, "memory search failed: {e:#}");
                    Vec::new()
                }
                Err(_) => {
                    info!(sentence = index, "memory search given up");
                    Vec::new()
                }
            }
        });
        if let Some(hearing) = self.state(&device).hearing.as_mut() {
            hearing.searches.push(search);
        }
    }

    /// The turn is over: the answer starts.
    fn answer(&mut self, turn: Turn) {
        let received = Instant::now();
        let device = turn.device.clone();
        if !self.state(&device).see(turn.turn) {
            info!(%device, turn = %turn.turn, "turn older than the last press: not answered");
            return;
        }
        let state = self.state(&device);
        let searches = match state.hearing.take_if(|h| h.turn == turn.turn) {
            Some(h) => h.searches,
            None => Vec::new(),
        };
        // An answer to an older turn whose press never came stops as at a press.
        self.barge_in(&device, turn.turn);
        if !has_words(&turn.text) {
            self.nothing_heard(&device, turn.turn);
            return;
        }
        self.open(&device);
        let brain = self.brain.clone();
        let memory = self.memory.clone();
        let mail = self.mail.clone();
        let state = self.state(&device);
        let Some(conversation) = state.conversation.as_mut() else {
            return;
        };
        let gate = Arc::new(Gate::default());
        let (done, running) = Running::new();
        let job = Job {
            brain,
            memory,
            mail,
            device: device.clone(),
            turn: turn.turn,
            text: turn.text,
            searches,
            link: conversation.link.clone(),
            previous: conversation.previous.take(),
            gate: gate.clone(),
            received,
            _done: done,
        };
        conversation.previous = Some(Previous {
            gate: gate.clone(),
            task: running.clone(),
        });
        tokio::spawn(job.run());
        state.answer = Some(Answer {
            turn: turn.turn,
            gate,
            task: running,
        });
    }

    /// The turn has nothing in it (near-silence): Emory says so, with no request to the LLM and
    /// no conversation opened for it. Like any answer, a press stops it.
    fn nothing_heard(&mut self, device: &Device, turn: TurnId) {
        info!(%device, %turn, "nothing heard: said so, no request");
        let gate = Arc::new(Gate::default());
        let mut sayer = Sayer {
            mail: self.mail.clone(),
            device: device.clone(),
            turn,
            gate: gate.clone(),
            index: 0,
            last_sent: false,
            received: Instant::now(),
        };
        sayer.publish(self.settings.nothing_heard.clone(), true);
        // No task: the answer is already over, but the voice may still be saying it.
        let (_, task) = Running::new();
        self.state(device).answer = Some(Answer { turn, gate, task });
    }

    /// Closes the conversations without an exchange for the idle time, once their answer is over.
    fn close_idle(&mut self) {
        let idle = self.settings.idle_timeout;
        for (device, state) in &mut self.devices {
            let Some(c) = &state.conversation else {
                continue;
            };
            let answering = state.answer.as_ref().is_some_and(|a| !a.task.is_finished());
            if c.last_exchange.elapsed() >= idle && !answering && !c.answering() {
                if let Some(session) = c.session() {
                    self.brain.close(&session);
                }
                state.conversation = None;
                info!(%device, "conversation closed (idle)");
            }
        }
    }
}

/// Opens a session and reads the heads-up, in a task of their own: the opening goes on while
/// Kris speaks.
fn opening<B: Brain, M: Memory>(brain: Arc<B>, memory: Arc<M>, device: Device) -> Opening {
    let started = Instant::now();
    let task = tokio::spawn(async move {
        let heads_up = async {
            match timeout(HEADS_UP_WAIT, memory.heads_up()).await {
                Ok(Ok(h)) => h,
                Ok(Err(e)) => {
                    warn!("heads-up: {e:#}");
                    None
                }
                Err(_) => {
                    warn!("heads-up: no answer in time");
                    None
                }
            }
        };
        let (session, heads_up) = tokio::join!(brain.open(), heads_up);
        let session = session.map_err(|e| format!("{e:#}"))?;
        info!(
            %device,
            %session,
            heads_up = heads_up.is_some(),
            ms = started.elapsed().as_millis() as u64,
            "conversation opened"
        );
        Ok(Opened { session, heads_up })
    });
    async move {
        task.await
            .map_err(|e| format!("opening the conversation: {e}"))?
    }
    .boxed()
    .shared()
}

/// One answer, from the request to the last sentence.
struct Job<B, M> {
    brain: Arc<B>,
    memory: Arc<M>,
    mail: Mailbox,
    device: Device,
    turn: TurnId,
    text: String,
    searches: Vec<JoinHandle<Vec<Found>>>,
    link: Arc<Mutex<Link>>,
    previous: Option<Previous>,
    gate: Arc<Gate>,
    received: Instant,
    /// Dropped when the answer ends: the next request may go.
    _done: watch::Sender<()>,
}

impl<B: Brain, M: Memory> Job<B, M> {
    async fn run(self) {
        let found = request::merge(
            join_all(self.searches)
                .await
                .into_iter()
                .map(Result::unwrap_or_default),
        );
        // The session takes one request at a time: the previous one is cancelled, and waited for
        // until it ends. If it never does, the session is broken: another one is opened.
        let mut broken = false;
        if let Some(previous) = self.previous
            && !previous.task.is_finished()
        {
            // Stopped, it was cancelled by its barge-in already.
            let unstopped = {
                let gate = previous.gate.lock();
                gate.prompted.clone().filter(|_| !gate.stopped)
            };
            if let Some(session) = unstopped {
                self.brain.cancel(&session);
            }
            if timeout(CANCEL_WAIT, previous.task.finished())
                .await
                .is_err()
            {
                warn!(device = %self.device, "the previous request never ended: new session");
                broken = true;
            }
        }
        let (opening, tell) = {
            let mut link = self.link.lock().unwrap();
            if broken {
                if let Some(session) = link.session() {
                    self.brain.close(&session);
                }
                *link = Link::new(opening(
                    self.brain.clone(),
                    self.memory.clone(),
                    self.device.clone(),
                ));
            }
            let tell = !link.told;
            link.told = true;
            (link.opening.clone(), tell)
        };
        let mut say = Sayer {
            mail: self.mail,
            device: self.device.clone(),
            turn: self.turn,
            gate: self.gate.clone(),
            index: 0,
            last_sent: false,
            received: self.received,
        };
        let opened = match opening.await {
            Ok(opened) => opened,
            Err(e) => {
                warn!(device = %self.device, "no conversation: {e}");
                say.finish();
                return;
            }
        };
        let heads_up = opened.heads_up.as_deref().filter(|_| tell);
        let text = request::build(contract::now(), heads_up, &found, &self.text);
        let mut events = {
            let mut gate = self.gate.lock();
            if gate.stopped {
                return;
            }
            gate.prompted = Some(opened.session.clone());
            self.brain.prompt(&opened.session, text)
        };
        info!(
            device = %self.device,
            turn = %self.turn,
            found = found.len(),
            heads_up = heads_up.is_some(),
            ms = self.received.elapsed().as_millis() as u64,
            "request sent"
        );
        let mut splitter = Splitter::new();
        let mut first_text = true;
        // Once the LLM stalled, Kris was told; what still comes is dropped until `Done`.
        let mut given_up = false;
        loop {
            let event = if given_up {
                Ok(events.recv().await)
            } else {
                timeout(STALL, events.recv()).await
            };
            match event {
                Ok(Some(Event::Text(piece))) if !given_up => {
                    if first_text {
                        first_text = false;
                        info!(
                            turn = %self.turn,
                            ms = self.received.elapsed().as_millis() as u64,
                            "first word of the LLM"
                        );
                    }
                    for sentence in splitter.push(&piece) {
                        say.publish(sentence, false);
                    }
                }
                Ok(Some(Event::Text(_))) => {}
                Ok(Some(Event::Tool(name))) => {
                    debug!(turn = %self.turn, tool = %name, "the LLM calls a tool");
                }
                Ok(Some(Event::Done(result))) => {
                    if !given_up && let Some(last) = splitter.finish() {
                        say.publish(last, true);
                    }
                    say.finish();
                    match result {
                        Ok(reason) => info!(
                            turn = %self.turn,
                            sentences = say.index,
                            reason = %reason,
                            ms = self.received.elapsed().as_millis() as u64,
                            "answer over"
                        ),
                        Err(e) => warn!(turn = %self.turn, "the request failed: {e}"),
                    }
                    return;
                }
                Ok(None) => {
                    warn!(turn = %self.turn, "the answer ended without its end");
                    say.finish();
                    return;
                }
                Err(_) => {
                    warn!(turn = %self.turn, "the LLM is silent: request given up");
                    self.brain.cancel(&opened.session);
                    say.finish();
                    given_up = true;
                }
            }
        }
    }
}

/// Queues the sentences of an answer, numbered, unless the answer was stopped.
struct Sayer {
    mail: Mailbox,
    device: Device,
    turn: TurnId,
    gate: Arc<Gate>,
    index: u32,
    last_sent: bool,
    received: Instant,
}

impl Sayer {
    fn publish(&mut self, text: String, last: bool) {
        if self.last_sent {
            return;
        }
        {
            let gate = self.gate.lock();
            if gate.stopped {
                return;
            }
            self.mail.send(Message::Say(Say {
                turn: self.turn,
                device: self.device.clone(),
                index: self.index,
                text,
                last,
            }));
        }
        if self.index == 0 {
            info!(
                turn = %self.turn,
                ms = self.received.elapsed().as_millis() as u64,
                "first sentence sent to the voice"
            );
        }
        self.index += 1;
        self.last_sent = last;
    }

    /// The answer is over, whatever way: the voice gets its `last`. When nothing was said, Kris
    /// is told it failed; otherwise an empty last sentence closes the turn (the voice ends it
    /// with an empty chunk carrying the end).
    fn finish(&mut self) {
        if self.index == 0 {
            self.publish(FAILURE.to_owned(), true);
        } else {
            self.publish(String::new(), true);
        }
    }
}
