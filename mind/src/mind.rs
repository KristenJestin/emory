//! The relay: the conversations, the searches made while Kris speaks, the answer streamed
//! sentence by sentence to the device that heard him, and the barge-in.
//!
//! One [`Mind`] follows every device, fed in order by the bus loop ([`Input`]). Per device:
//!
//! - **the conversation**, opened at the first press: an LLM session, and what the memory flags
//!   for the moment, both fetched while Kris is still speaking. It closes after a time without an
//!   exchange;
//! - **the turn being heard**: each transcribed sentence is searched in the memory at once, as it
//!   is, given up beyond the search timeout;
//! - **the answer**: a task that sends the request, cuts the streamed text into sentences and
//!   publishes each one (`voice.say`), the last one marked `last`.
//!
//! A press while an answer is running or being said cancels the request (ACP `session/cancel`)
//! and publishes `voice.stop`. The sentences are published under a lock that the barge-in takes
//! too: once `voice.stop` is out, no sentence of that turn follows it.

use std::{collections::HashMap, future::Future, sync::Arc, time::Duration};

use contract::{Button, Device, Say, Sentence, State, Stop, Turn, TurnId};
use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared, join_all},
};
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
    time::{Instant, timeout},
};
use tracing::{debug, info, warn};

use crate::{
    brain::{Brain, Event, SessionId},
    memory::{Found, Memory},
    request,
    sentences::Splitter,
};

/// Said when the LLM cannot be reached: Kris pressed and waits for something.
pub const FAILURE: &str = "Je n'arrive pas à réfléchir pour le moment.";

/// How long a new request waits for the previous one of the conversation to finish.
const PREVIOUS_WAIT: Duration = Duration::from_secs(10);
/// How long an answer may stay silent before it is given up.
const STALL: Duration = Duration::from_secs(60);
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
}

/// A conversation once open: its session, and what the memory flagged at its start.
#[derive(Clone, Debug)]
struct Opened {
    session: SessionId,
    heads_up: Option<String>,
}

type Opening = Shared<BoxFuture<'static, Result<Opened, String>>>;

struct Conversation {
    opening: Opening,
    last_exchange: Instant,
    /// The heads-up went with a request already: the session keeps it.
    told: bool,
    /// The last answer: requests go one at a time.
    previous: Option<Running>,
}

impl Conversation {
    fn session(&self) -> Option<SessionId> {
        match self.opening.peek() {
            Some(Ok(opened)) => Some(opened.session.clone()),
            _ => None,
        }
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

/// The turn being heard, and the searches of its sentences.
struct Hearing {
    turn: TurnId,
    searches: Vec<JoinHandle<Vec<Found>>>,
}

/// The last answer of a device, until the next press.
struct Answer {
    turn: TurnId,
    /// Set by the barge-in, under the lock the sentences are published with.
    stopped: Arc<Mutex<bool>>,
    task: Running,
}

#[derive(Default)]
struct DeviceState {
    conversation: Option<Conversation>,
    hearing: Option<Hearing>,
    answer: Option<Answer>,
}

pub struct Mind<B, M, O> {
    brain: Arc<B>,
    memory: Arc<M>,
    out: Arc<O>,
    settings: Settings,
    devices: HashMap<Device, DeviceState>,
}

impl<B: Brain, M: Memory, O: Outbox> Mind<B, M, O> {
    pub fn new(brain: Arc<B>, memory: Arc<M>, out: Arc<O>, settings: Settings) -> Self {
        Self {
            brain,
            memory,
            out,
            settings,
            devices: HashMap::new(),
        }
    }

    pub async fn handle(&mut self, input: Input) {
        match input {
            Input::Button(device, button) => {
                if button.state == State::Start {
                    self.barge_in(&device).await;
                    self.open(&device);
                    self.state(&device).hearing = Some(Hearing {
                        turn: button.turn,
                        searches: Vec::new(),
                    });
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

    /// Stops the answer of this device, if one is running or may still be said.
    async fn barge_in(&mut self, device: &Device) {
        let brain = self.brain.clone();
        let out = self.out.clone();
        let state = self.state(device);
        let Some(answer) = state.answer.take() else {
            return;
        };
        if !answer.task.is_finished()
            && let Some(session) = state.conversation.as_ref().and_then(Conversation::session)
        {
            brain.cancel(&session);
        }
        let mut stopped = answer.stopped.lock().await;
        if !*stopped {
            *stopped = true;
            let stop = Stop {
                turn: answer.turn,
                device: device.clone(),
            };
            match out.stop(stop).await {
                Ok(()) => info!(%device, turn = %answer.turn, "barge-in: answer stopped"),
                Err(e) => warn!("publishing voice.stop: {e:#}"),
            }
        }
    }

    /// Opens the conversation of this device if it has none (or if it has gone idle), and notes
    /// the exchange.
    fn open(&mut self, device: &Device) {
        let idle = self.settings.idle_timeout;
        let brain = self.brain.clone();
        let memory = self.memory.clone();
        let state = self.state(device);
        if let Some(c) = &state.conversation
            && c.last_exchange.elapsed() >= idle
            && c.previous.as_ref().is_none_or(Running::is_finished)
        {
            if let Some(session) = c.session() {
                brain.close(&session);
            }
            state.conversation = None;
            info!(%device, "conversation closed (idle)");
        }
        match &mut state.conversation {
            Some(c) => c.last_exchange = Instant::now(),
            None => {
                let started = Instant::now();
                let task = tokio::spawn({
                    let device = device.clone();
                    async move {
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
                    }
                });
                let opening = async move {
                    task.await
                        .map_err(|e| format!("opening the conversation: {e}"))?
                }
                .boxed()
                .shared();
                state.conversation = Some(Conversation {
                    opening,
                    last_exchange: Instant::now(),
                    told: false,
                    previous: None,
                });
            }
        }
    }

    /// A sentence heard while Kris speaks: searched in the memory at once.
    fn heard(&mut self, sentence: Sentence) {
        let device = sentence.device.clone();
        // The press may have been missed (a mind that restarts): the turn starts here.
        if self
            .devices
            .get(&device)
            .and_then(|s| s.hearing.as_ref())
            .is_none_or(|h| h.turn != sentence.turn)
        {
            self.open(&device);
            self.state(&device).hearing = Some(Hearing {
                turn: sentence.turn,
                searches: Vec::new(),
            });
        }
        if sentence.text.trim().is_empty() {
            return;
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
        let searches = match self.state(&device).hearing.take() {
            Some(h) if h.turn == turn.turn => h.searches,
            _ => Vec::new(),
        };
        if turn.text.trim().is_empty() {
            debug!(%device, turn = %turn.turn, "nothing heard: no answer");
            return;
        }
        self.open(&device);
        let brain = self.brain.clone();
        let out = self.out.clone();
        let state = self.state(&device);
        let Some(conversation) = state.conversation.as_mut() else {
            return;
        };
        let stopped = Arc::new(Mutex::new(false));
        let (done, running) = Running::new();
        let job = Job {
            brain,
            out,
            device: device.clone(),
            turn: turn.turn,
            text: turn.text,
            searches,
            opening: conversation.opening.clone(),
            tell: !conversation.told,
            previous: conversation.previous.take(),
            stopped: stopped.clone(),
            received,
            _done: done,
        };
        conversation.told = true;
        conversation.previous = Some(running.clone());
        tokio::spawn(job.run());
        state.answer = Some(Answer {
            turn: turn.turn,
            stopped,
            task: running,
        });
    }

    /// Closes the conversations without an exchange for the idle time, once their answer is over.
    fn close_idle(&mut self) {
        let idle = self.settings.idle_timeout;
        for (device, state) in &mut self.devices {
            let Some(c) = &state.conversation else {
                continue;
            };
            let answering = state.answer.as_ref().is_some_and(|a| !a.task.is_finished());
            if c.last_exchange.elapsed() >= idle && !answering && state.hearing.is_none() {
                if let Some(session) = c.session() {
                    self.brain.close(&session);
                }
                state.conversation = None;
                info!(%device, "conversation closed (idle)");
            }
        }
    }
}

/// One answer, from the request to the last sentence.
struct Job<B, O> {
    brain: Arc<B>,
    out: Arc<O>,
    device: Device,
    turn: TurnId,
    text: String,
    searches: Vec<JoinHandle<Vec<Found>>>,
    opening: Opening,
    /// The heads-up goes with this request.
    tell: bool,
    previous: Option<Running>,
    stopped: Arc<Mutex<bool>>,
    received: Instant,
    /// Dropped when the answer ends: the next request may go.
    _done: watch::Sender<()>,
}

impl<B: Brain, O: Outbox> Job<B, O> {
    async fn run(self) {
        let found = request::merge(
            join_all(self.searches)
                .await
                .into_iter()
                .map(Result::unwrap_or_default),
        );
        if let Some(previous) = self.previous
            && timeout(PREVIOUS_WAIT, previous.finished()).await.is_err()
        {
            warn!(device = %self.device, "the previous request is still running");
        }
        let mut say = Sayer {
            out: self.out,
            device: self.device.clone(),
            turn: self.turn,
            stopped: self.stopped,
            index: 0,
            received: self.received,
        };
        let opened = match self.opening.await {
            Ok(opened) => opened,
            Err(e) => {
                warn!(device = %self.device, "no conversation: {e}");
                say.publish(FAILURE.to_owned(), true).await;
                return;
            }
        };
        let heads_up = opened.heads_up.as_deref().filter(|_| self.tell);
        let text = request::build(heads_up, &found, &self.text);
        if *say.stopped.lock().await {
            return;
        }
        let ready = self.received.elapsed().as_millis() as u64;
        let mut events = match self.brain.prompt(&opened.session, text).await {
            Ok(events) => events,
            Err(e) => {
                warn!(device = %self.device, "request: {e:#}");
                say.publish(FAILURE.to_owned(), true).await;
                return;
            }
        };
        info!(
            device = %self.device,
            turn = %self.turn,
            found = found.len(),
            heads_up = heads_up.is_some(),
            ms = ready,
            "request sent"
        );
        let mut splitter = Splitter::new();
        let mut first_text = true;
        loop {
            match timeout(STALL, events.recv()).await {
                Ok(Some(Event::Text(piece))) => {
                    if first_text {
                        first_text = false;
                        info!(
                            turn = %self.turn,
                            ms = self.received.elapsed().as_millis() as u64,
                            "first word of the LLM"
                        );
                    }
                    for sentence in splitter.push(&piece) {
                        say.publish(sentence, false).await;
                    }
                }
                Ok(Some(Event::Tool(name))) => {
                    debug!(turn = %self.turn, tool = %name, "the LLM calls a tool");
                }
                Ok(Some(Event::Done(result))) => {
                    match splitter.finish() {
                        Some(last) => say.publish(last, true).await,
                        None if say.index == 0 && result.is_err() => {
                            say.publish(FAILURE.to_owned(), true).await
                        }
                        None => {}
                    }
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
                Ok(None) => return,
                Err(_) => {
                    warn!(turn = %self.turn, "the LLM is silent: request given up");
                    self.brain.cancel(&opened.session);
                    return;
                }
            }
        }
    }
}

/// Publishes the sentences of an answer, numbered, unless the answer was stopped.
struct Sayer<O> {
    out: Arc<O>,
    device: Device,
    turn: TurnId,
    stopped: Arc<Mutex<bool>>,
    index: u32,
    received: Instant,
}

impl<O: Outbox> Sayer<O> {
    async fn publish(&mut self, text: String, last: bool) {
        let stopped = self.stopped.lock().await;
        if *stopped {
            return;
        }
        let say = Say {
            turn: self.turn,
            device: self.device.clone(),
            index: self.index,
            text,
            last,
        };
        if let Err(e) = self.out.say(say).await {
            warn!("publishing voice.say: {e:#}");
            return;
        }
        if self.index == 0 {
            info!(
                turn = %self.turn,
                ms = self.received.elapsed().as_millis() as u64,
                "first sentence sent to the voice"
            );
        }
        self.index += 1;
    }
}
