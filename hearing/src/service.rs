//! The service: the bus messages come in, the transcribed sentences and turns go out.
//!
//! A single task follows the bus and the state machine of the turns; the transcription is
//! elsewhere (see [`crate::engine`]). Each turn has its publishing task, which receives the
//! sentences in the order they were closed, waits for their transcription, publishes them, then
//! publishes the turn.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
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
    config::{ConfigPath, VadConfig},
    corrections::Corrections,
    engine::{Detector, Segment, Transcriber, Transcription},
    turns::{Chunk, Closed, Opening, Reason, Turns},
};

/// How often we look whether a turn has gone quiet.
const WATCH: Duration = Duration::from_millis(250);
/// How long, on shutdown, we wait for the turns in progress to finish leaving.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// A sentence handed to the transcriber, whose text is awaited.
struct Pending {
    start: Timestamp,
    end: Timestamp,
    duration: Duration,
    reply: oneshot::Receiver<Result<Transcription, String>>,
}

enum Message {
    Sentence(Pending),
    End {
        end: Timestamp,
        instant: Instant,
        reason: Reason,
    },
}

/// A turn in progress: its detector, and the queue to the task that publishes.
struct Session {
    detector: Detector,
    transcriber: Transcriber,
    queue: mpsc::UnboundedSender<Message>,
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
        let _ = self.queue.send(Message::End {
            end,
            instant,
            reason,
        });
    }

    fn send(&self, segments: Vec<Segment>) {
        for segment in segments {
            let moment = |samples: u64| {
                self.start + TimeDelta::microseconds((samples * 1_000_000 / 16_000) as i64)
            };
            let duration = segment.duration();
            let pending = Pending {
                start: moment(segment.start),
                end: moment(segment.start + segment.samples.len() as u64),
                duration,
                reply: self.transcriber.transcribe(segment.samples),
            };
            let _ = self.queue.send(Message::Sentence(pending));
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
    pub vad: ConfigPath,
    pub vad_settings: VadConfig,
    pub transcriber: Transcriber,
    pub corrections: Arc<Corrections>,
}

impl Service {
    /// Listens to the bus until the program stops (Ctrl-C or SIGTERM).
    pub async fn run(self) -> Result<()> {
        let mut buttons = self.client.subscribe(subjects::ALL_BUTTONS).await?;
        let mut audios = self.client.subscribe(subjects::ALL_EAR_AUDIO).await?;
        info!(
            buttons = subjects::ALL_BUTTONS,
            audio = subjects::ALL_EAR_AUDIO,
            "hearing is listening"
        );

        let mut turns = Turns::<Session>::new(audio::SILENCE_CLOSES_TURN);
        let mut publications = JoinSet::new();
        let mut warned = Warnings::default();
        let mut watch = tokio::time::interval(WATCH);
        watch.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let stop = shutdown_signal();
        tokio::pin!(stop);

        loop {
            tokio::select! {
                Some(message) = buttons.next() => {
                    self.button(&mut turns, &mut publications, message);
                }
                Some(message) = audios.next() => {
                    handle_audio(&mut turns, &mut warned, message);
                }
                _ = watch.tick() => {
                    for closed in turns.expire(Instant::now()) {
                        warn!(device = %closed.device, turn = %closed.turn, "no more audio: the turn is closed");
                        close(closed, contract::now(), Instant::now());
                    }
                    while publications.try_join_next().is_some() {}
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
                let (queue, receive) = mpsc::unbounded_channel();
                let session =
                    Detector::new(self.vad.as_ref(), &self.vad_settings).map(|detector| Session {
                        detector,
                        transcriber: self.transcriber.clone(),
                        queue,
                        start: button.at,
                        samples: 0,
                    });
                let session = match session {
                    Ok(session) => session,
                    Err(error) => {
                        warn!(%device, turn = %button.turn, "{error:#}");
                        return;
                    }
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
                        info!(%device, turn = %button.turn, "turn started");
                        publications.spawn(publish_turn(
                            self.client.clone(),
                            device,
                            button.turn,
                            button.at,
                            receive,
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
    mut queue: mpsc::UnboundedReceiver<Message>,
    corrections: Arc<Corrections>,
) {
    let mut texts: Vec<String> = Vec::new();
    let mut closing = None;
    while let Some(message) = queue.recv().await {
        match message {
            Message::Sentence(pending) => {
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
            Message::End {
                end,
                instant,
                reason,
            } => {
                closing = Some((end, instant, reason));
                break;
            }
        }
    }
    let (end, instant, reason) =
        closing.unwrap_or_else(|| (contract::now(), Instant::now(), Reason::Shutdown));
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
