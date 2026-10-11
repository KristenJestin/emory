//! The service: the bus messages come in, the transcribed sentences and turns go out.
//!
//! A single task follows the bus and the state machine of the turns; the transcription is
//! elsewhere (see [`crate::engine`]). Each turn has its publishing task, which receives the
//! sentences in the order they were closed, waits for their transcription, publishes them, then
//! publishes the turn.
//!
//! The ears' messages come through one subscription (`ear.*.*`), so that the presses and the
//! audio of an ear arrive in the order the ear sent them: with two subscriptions, the `end` could
//! overtake the last chunks of audio.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use async_nats::Client;
use chrono::TimeDelta;
use contract::{
    Button, Device, Origin, Sentence, Speaker, State, Timestamp, Turn, TurnId, Wake, audio,
    subjects,
};
use futures_util::StreamExt;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinSet,
    time::MissedTickBehavior,
};
use tracing::{debug, info, warn};

use crate::{
    audio as ear_audio,
    bus::publish,
    corrections::Corrections,
    engine::{Detector, DetectorPool, Segment, Transcriber, Transcription},
    turns::{Chunk, Closed, Opening, Reason, Turns},
};

/// Everything the ears say, on one subscription: `ear.<name>.button`, `.audio` and `.announce`.
const EAR_SUBJECTS: &str = "ear.*.*";
/// How often we look whether a turn has gone quiet.
const WATCH: Duration = Duration::from_millis(250);
/// How long, on shutdown, we wait for the turns in progress to finish leaving.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
/// How many closed sentences of a turn may wait to be published. The transcription backlog is
/// bounded too (see [`crate::engine`]); a sentence that finds this queue full is dropped.
const TURN_BACKLOG: usize = 32;

/// What an ear message is, from the last token of its subject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EarMessage {
    Button,
    Audio,
}

/// Sorts the messages of `ear.*.*`: the buttons and the audio are ours, the announcements (and
/// anything else an ear may say) are for others.
fn ear_message(subject: &str) -> Option<EarMessage> {
    let mut tokens = subject.split('.');
    match (
        tokens.next()?,
        tokens.next()?,
        tokens.next()?,
        tokens.next(),
    ) {
        ("ear", _, "button", None) => Some(EarMessage::Button),
        ("ear", _, "audio", None) => Some(EarMessage::Audio),
        _ => None,
    }
}

/// A sentence handed to the transcriber, whose text is awaited.
struct Pending {
    start: Timestamp,
    end: Timestamp,
    duration: Duration,
    reply: oneshot::Receiver<Result<Transcription, String>>,
}

/// What the publishing task learns when the turn closes.
struct Closing {
    end: Timestamp,
    instant: Instant,
    reason: Reason,
}

/// A turn in progress: its detector, and the queue to the task that publishes.
struct Session {
    detector: Detector,
    pool: DetectorPool,
    transcriber: Transcriber,
    turn: TurnId,
    /// The closed sentences, in order, to the publishing task. Bounded: a sentence that does not
    /// fit is dropped (and logged) rather than blocking the bus loop.
    queue: mpsc::Sender<Pending>,
    closing: oneshot::Sender<Closing>,
    start: Timestamp,
    samples: u64,
}

impl Session {
    fn feed(&mut self, audio: &[f32]) {
        self.samples += audio.len() as u64;
        let segments = self.detector.feed(audio);
        self.send(segments);
    }

    /// Closes the turn: the sentence in progress is transcribed in its turn, then the turn goes.
    fn close(mut self, end: Timestamp, instant: Instant, reason: Reason) {
        let segments = self.detector.flush();
        self.send(segments);
        // The closing is sent before the queue is dropped (with `self`): the task reads the
        // sentences until the queue ends, then the closing.
        let _ = self.closing.send(Closing {
            end,
            instant,
            reason,
        });
        self.pool.give_back(self.detector);
    }

    fn send(&self, segments: Vec<Segment>) {
        for segment in segments {
            // The place in the turn's queue is taken before the sentence goes to the
            // transcription, so that a dropped sentence is not transcribed for nothing.
            let Ok(permit) = self.queue.try_reserve() else {
                warn!(turn = %self.turn, "too many sentences waiting in the turn: a sentence is dropped");
                continue;
            };
            let moment = |samples: u64| {
                self.start + TimeDelta::microseconds((samples * 1_000_000 / 16_000) as i64)
            };
            let duration = segment.duration();
            permit.send(Pending {
                start: moment(segment.start),
                end: moment(segment.start + segment.samples.len() as u64),
                duration,
                reply: self.transcriber.transcribe(self.turn, segment.samples),
            });
        }
    }
}

/// One warning per device and turn, not one per audio chunk (50 per second).
#[derive(Default)]
struct Warnings(HashMap<(Device, &'static str), TurnId>);

impl Warnings {
    fn first_time(&mut self, device: &Device, kind: &'static str, turn: TurnId) -> bool {
        self.0.insert((device.clone(), kind), turn) != Some(turn)
    }
}

pub struct Service {
    pub client: Client,
    pub detectors: DetectorPool,
    pub transcriber: Transcriber,
    pub corrections: Arc<Corrections>,
    /// A turn is closed after this long, whatever the ear does.
    pub max_turn: Duration,
}

impl Service {
    /// Listens to the bus until the program stops (Ctrl-C or SIGTERM). Fails if the
    /// transcription thread dies: the service would be deaf, so the process exits and its
    /// supervisor restarts it.
    pub async fn run(self) -> Result<()> {
        let mut ear = self.client.subscribe(EAR_SUBJECTS).await?;
        info!(subjects = EAR_SUBJECTS, "hearing is listening");

        let mut turns = Turns::<Session>::new(audio::SILENCE_CLOSES_TURN, self.max_turn);
        let mut publications = JoinSet::new();
        let mut warned = Warnings::default();
        let mut watch = tokio::time::interval(WATCH);
        watch.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let stop = shutdown_signal();
        tokio::pin!(stop);
        let transcription_stopped = self.transcriber.stopped();
        tokio::pin!(transcription_stopped);

        loop {
            tokio::select! {
                Some(message) = ear.next() => {
                    match ear_message(&message.subject) {
                        Some(EarMessage::Button) => {
                            self.button(&mut turns, &mut publications, message)
                        }
                        Some(EarMessage::Audio) => handle_audio(&mut turns, &mut warned, message),
                        None => {}
                    }
                }
                _ = watch.tick() => {
                    for closed in turns.expire(Instant::now()) {
                        match closed.reason {
                            Reason::MaxLength => warn!(
                                device = %closed.device, turn = %closed.turn,
                                max_turn_s = self.max_turn.as_secs(),
                                "the turn has reached its maximum length: it is closed"
                            ),
                            _ => warn!(
                                device = %closed.device, turn = %closed.turn,
                                "no more audio: the turn is closed"
                            ),
                        }
                        close(closed, contract::now(), Instant::now());
                    }
                    while publications.try_join_next().is_some() {}
                }
                _ = &mut transcription_stopped => {
                    bail!("the transcription thread has stopped: the service is deaf, exiting");
                }
                _ = &mut stop => break,
                else => break,
            }
        }

        info!("stopping: closing the turns in progress");
        for closed in turns.close_all() {
            close(closed, contract::now(), Instant::now());
        }
        let finished = tokio::time::timeout(SHUTDOWN_TIMEOUT, async {
            while publications.join_next().await.is_some() {}
        })
        .await;
        if finished.is_err() {
            warn!("some turns did not finish leaving");
        }
        Ok(())
    }

    fn button(
        &self,
        turns: &mut Turns<Session>,
        publications: &mut JoinSet<()>,
        message: async_nats::Message,
    ) {
        let Some(device) = subjects::device_of_subject(&message.subject) else {
            warn!(subject = %message.subject, "button on a subject without a device");
            return;
        };
        let button: Button = match serde_json::from_slice(&message.payload) {
            Ok(button) => button,
            Err(error) => {
                warn!(%device, %error, "unreadable button");
                return;
            }
        };
        let now = Instant::now();
        match button.state {
            State::Start => {
                let created = Instant::now();
                let detector = match self.detectors.take() {
                    Ok(detector) => detector,
                    Err(error) => {
                        warn!(%device, turn = %button.turn, "{error:#}");
                        // The ear has started another turn: the old one is over all the same.
                        if let Some(old) = turns.close(&device, Reason::Replaced) {
                            warn!(%device, turn = %old.turn, "new start: the old turn is closed");
                            close(old, contract::now(), now);
                        }
                        return;
                    }
                };
                let (queue, receive) = mpsc::channel(TURN_BACKLOG);
                let (closing, closing_receive) = oneshot::channel();
                let session = Session {
                    detector,
                    pool: self.detectors.clone(),
                    transcriber: self.transcriber.clone(),
                    turn: button.turn,
                    queue,
                    closing,
                    start: button.at,
                    samples: 0,
                };
                match turns.start(&device, button.turn, button.at, now, || session) {
                    Opening::Duplicate => {
                        debug!(%device, turn = %button.turn, "start already received")
                    }
                    Opening::Opened { replaced } => {
                        if let Some(old) = replaced {
                            warn!(%device, turn = %old.turn, "new start: the old turn is closed");
                            close(old, contract::now(), now);
                        }
                        info!(
                            %device, turn = %button.turn,
                            detector_ms = created.elapsed().as_millis() as u64,
                            "turn started"
                        );
                        publications.spawn(publish_turn(
                            self.client.clone(),
                            device,
                            button.turn,
                            button.at,
                            receive,
                            closing_receive,
                            self.corrections.clone(),
                        ));
                    }
                }
            }
            State::End => match turns.end(&device, button.turn) {
                Some(closed) => close(closed, button.at, now),
                None => debug!(%device, turn = %button.turn, "end of a turn that is not open"),
            },
        }
    }
}

fn close(closed: Closed<Session>, end: Timestamp, instant: Instant) {
    closed.session.close(end, instant, closed.reason);
}

fn handle_audio(turns: &mut Turns<Session>, warned: &mut Warnings, message: async_nats::Message) {
    let Some(device) = subjects::device_of_subject(&message.subject) else {
        return;
    };
    let header = match ear_audio::read_headers(message.headers.as_ref()) {
        Ok(header) => header,
        Err(error) => {
            warn!(%device, "audio thrown away: {error:#}");
            return;
        }
    };
    let samples = match ear_audio::samples(&message.payload) {
        Ok(s) => s,
        Err(error) => {
            warn!(%device, turn = %header.turn, "audio thrown away: {error:#}");
            return;
        }
    };
    match turns.audio(&device, header.turn, header.seq, Instant::now()) {
        Chunk::Accepted { session, gap } => {
            if let Some(gap) = gap {
                warn!(
                    %device, turn = %header.turn,
                    expected = gap.expected, received = gap.received,
                    missing = gap.received - gap.expected,
                    "gap in the audio"
                );
            }
            session.feed(&samples);
        }
        Chunk::Stale => {
            if warned.first_time(&device, "stale", header.turn) {
                warn!(%device, turn = %header.turn, seq = header.seq, "duplicate or late audio chunk: thrown away");
            }
        }
        Chunk::Unknown => {
            if warned.first_time(&device, "unknown", header.turn) {
                warn!(%device, turn = %header.turn, "audio of a turn that is not open: thrown away");
            }
        }
    }
}

/// The task of a turn: publishes its sentences in order, then the turn.
async fn publish_turn(
    client: Client,
    device: Device,
    turn: TurnId,
    start: Timestamp,
    mut queue: mpsc::Receiver<Pending>,
    closing: oneshot::Receiver<Closing>,
    corrections: Arc<Corrections>,
) {
    let mut texts: Vec<String> = Vec::new();
    while let Some(pending) = queue.recv().await {
        let Pending {
            start,
            end,
            duration,
            reply,
        } = pending;
        let index = texts.len() as u32;
        let transcription = match reply.await {
            Ok(Ok(transcription)) => transcription,
            Ok(Err(error)) => {
                warn!(%device, %turn, "sentence not transcribed: {error}");
                continue;
            }
            Err(_) => {
                warn!(%device, %turn, "sentence not transcribed: the transcription thread has stopped");
                continue;
            }
        };
        let text = corrections.apply(&transcription.text);
        info!(
            %device, %turn, index,
            audio_ms = duration.as_millis() as u64,
            transcription_ms = transcription.duration.as_millis() as u64,
            wait_ms = transcription.wait.as_millis() as u64,
            characters = text.chars().count(),
            "sentence transcribed"
        );
        if text.trim().is_empty() {
            debug!(%device, %turn, "sentence without text: thrown away");
            continue;
        }
        let sentence = Sentence {
            turn,
            device: device.clone(),
            index,
            text: text.clone(),
            start,
            end,
        };
        if let Err(error) = publish(&client, subjects::SENTENCE, &sentence).await {
            warn!(%device, %turn, "sentence not published: {error:#}");
        }
        texts.push(text);
    }
    // The queue has ended: the session is closed, and its closing was sent before.
    let Closing {
        end,
        instant,
        reason,
    } = closing.await.unwrap_or_else(|_| Closing {
        end: contract::now(),
        instant: Instant::now(),
        reason: Reason::Shutdown,
    });
    let sentences = texts.len();
    let message = Turn {
        turn,
        device: device.clone(),
        start,
        end,
        text: texts.join(" "),
        wake: Wake::Button,
        speaker: Some(Speaker {
            voice: "kris".to_owned(),
            person: Some("kris".to_owned()),
            origin: Origin::Known,
            confidence: 1.0,
        }),
        transcription_confidence: None,
        context: Vec::new(),
    };
    let result = publish(&client, subjects::TURN, &message).await;
    let end_to_turn = instant.elapsed();
    match result {
        Ok(()) => info!(
            %device, %turn, ?reason, sentences,
            characters = message.text.chars().count(),
            end_to_turn_ms = end_to_turn.as_millis() as u64,
            "turn published"
        ),
        Err(error) => warn!(%device, %turn, "turn not published: {error:#}"),
    }
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM signal");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ear_subscription_is_dispatched_on_the_last_token() {
        assert_eq!(ear_message("ear.office.button"), Some(EarMessage::Button));
        assert_eq!(ear_message("ear.office.audio"), Some(EarMessage::Audio));
        // The announcements match `ear.*.*` too: they are not for the hearing.
        assert_eq!(ear_message("ear.office.announce"), None);
        assert_eq!(ear_message("ear.office.other"), None);
        assert_eq!(ear_message("ear.office"), None);
        assert_eq!(ear_message("ear.office.audio.extra"), None);
        assert_eq!(ear_message("mouth.office.audio"), None);
    }

    #[test]
    fn the_subjects_of_the_contract_are_dispatched_and_carried_by_the_subscription() {
        let office: Device = "office".parse().unwrap();
        assert_eq!(
            ear_message(&subjects::button(&office)),
            Some(EarMessage::Button)
        );
        assert_eq!(
            ear_message(&subjects::ear_audio(&office)),
            Some(EarMessage::Audio)
        );
        assert_eq!(ear_message(&subjects::ear_announce(&office)), None);
        // `ear.*.*` is three tokens, like every subject of an ear.
        assert_eq!(EAR_SUBJECTS.split('.').count(), 3);
        for subject in [subjects::ALL_BUTTONS, subjects::ALL_EAR_AUDIO] {
            assert!(subject.starts_with("ear.*."));
        }
    }
}
