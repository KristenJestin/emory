//! A device: its queue of sentences, the synthesis in progress, and what leaves for the bus.

use std::time::{Duration, Instant};

use async_nats::Client;
use bytes::Bytes;
use contract::{Device, Say, Speaking, TurnId, audio::Format, subjects};
use futures_util::StreamExt;
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
};
use tracing::{debug, info, warn};

use crate::{
    audio::{Chunker, chunk_headers, duration_of},
    config::Config,
    engine::Engine,
    queue::{Effect, Queue},
    text,
};

/// What a device receives: orders from the bus, and news of its synthesis.
pub enum Input {
    Say(Say),
    Stop(TurnId),
    /// Audio of this turn has just left.
    Sound {
        turn: TurnId,
        duration: Duration,
    },
    /// The sentence has finished being sent; `seq` is the next chunk number.
    Finished {
        turn: TurnId,
        index: u32,
        seq: u32,
    },
    Shutdown,
}

pub type Inbox = UnboundedSender<Input>;

/// The format used to announce the end of a sentence without sound (the engine said nothing).
const FALLBACK_FORMAT: Format = Format {
    sample_rate: 24_000,
    channels: 1,
};

pub fn spawn(
    device: Device,
    nats: Client,
    engine: Engine,
    config: Config,
) -> (Inbox, JoinHandle<()>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = tokio::spawn(run(device, nats, engine, config, rx, tx.clone()));
    (tx, handle)
}

async fn run(
    device: Device,
    nats: Client,
    engine: Engine,
    config: Config,
    mut inbox: UnboundedReceiver<Input>,
    own: Inbox,
) {
    let mut queue = Queue::new(config.end_grace());
    let mut job: Option<JoinHandle<()>> = None;
    macro_rules! cx {
        () => {
            Context {
                device: &device,
                nats: &nats,
                engine: &engine,
                config: &config,
                own: &own,
            }
        };
    }
    loop {
        let deadline = queue.deadline();
        let effects = tokio::select! {
            input = inbox.recv() => {
                let Some(input) = input else { break };
                let now = Instant::now();
                match input {
                    Input::Say(say) => queue.say(&say, now),
                    Input::Stop(turn) => queue.stop(turn),
                    Input::Sound { turn, duration } => queue.sound_sent(turn, duration, now),
                    Input::Finished { turn, index, seq } => queue.finished(turn, index, seq),
                    Input::Shutdown => {
                        let effects = queue.shutdown();
                        apply(effects, cx!(), &mut job).await;
                        break;
                    }
                }
            }
            () = sleep_until(deadline) => queue.expire(Instant::now()),
        };
        apply(effects, cx!(), &mut job).await;
    }
    if let Some(job) = job {
        job.abort();
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}

struct Context<'a> {
    device: &'a Device,
    nats: &'a Client,
    engine: &'a Engine,
    config: &'a Config,
    own: &'a Inbox,
}

async fn apply(effects: Vec<Effect>, cx: Context<'_>, job: &mut Option<JoinHandle<()>>) {
    let device = cx.device;
    for effect in effects {
        match effect {
            Effect::Cut => {
                if let Some(job) = job.take() {
                    job.abort();
                    debug!(%device, "synthesis cut");
                }
            }
            Effect::Speaking(state) => {
                debug!(%device, ?state, "speaking");
                match serde_json::to_vec(&Speaking { state }) {
                    Ok(json) => {
                        if let Err(e) = cx
                            .nats
                            .publish(subjects::speaking(device), json.into())
                            .await
                        {
                            warn!(%device, "cannot publish Speaking: {e}");
                        }
                    }
                    Err(e) => warn!("Speaking not serialisable: {e}"),
                }
            }
            Effect::Synthesize {
                turn,
                index,
                text,
                received,
                seq,
            } => {
                let sentence = Sentence {
                    device: device.clone(),
                    turn,
                    index,
                    text,
                    received,
                    seq,
                };
                *job = Some(tokio::spawn(say(
                    sentence,
                    cx.nats.clone(),
                    cx.engine.clone(),
                    cx.config.chunk_ms,
                    cx.own.clone(),
                )));
            }
        }
    }
}

struct Sentence {
    device: Device,
    turn: TurnId,
    index: u32,
    text: String,
    received: Instant,
    seq: u32,
}

/// Sends the chunks of a sentence, numbered over the whole turn.
struct Sender {
    nats: Client,
    subject: String,
    turn: TurnId,
    index: u32,
    format: Format,
    seq: u32,
    audio: Duration,
    first_sound: Option<Instant>,
    inbox: Inbox,
}

impl Sender {
    async fn send(&mut self, payload: Vec<u8>, last: bool) -> anyhow::Result<()> {
        let duration = duration_of(payload.len(), self.format);
        let headers = chunk_headers(self.turn, self.seq, self.format, self.index, last);
        self.nats
            .publish_with_headers(self.subject.clone(), headers, Bytes::from(payload))
            .await?;
        self.seq += 1;
        if !duration.is_zero() {
            self.first_sound.get_or_insert_with(Instant::now);
            self.audio += duration;
            let _ = self.inbox.send(Input::Sound {
                turn: self.turn,
                duration,
            });
        }
        Ok(())
    }

    /// The last chunks, the last one carrying `Emory-End`; without sound, an empty chunk carries it.
    async fn finish(&mut self, mut tail: Vec<Vec<u8>>) -> anyhow::Result<()> {
        let last = tail.pop().unwrap_or_default();
        for chunk in tail {
            self.send(chunk, false).await?;
        }
        self.send(last, true).await
    }
}

async fn say(sentence: Sentence, nats: Client, engine: Engine, chunk_ms: u32, inbox: Inbox) {
    let Sentence {
        device,
        turn,
        index,
        text,
        received,
        seq,
    } = sentence;
    let started = Instant::now();
    let mut sender = Sender {
        subject: subjects::mouth_audio(&device),
        nats,
        turn,
        index,
        format: FALLBACK_FORMAT,
        seq,
        audio: Duration::ZERO,
        first_sound: None,
        inbox: inbox.clone(),
    };
    let mut chunker = None;
    let mut engine_first = None;
    let streamed = stream(
        &engine,
        &text::clean(&text),
        chunk_ms,
        &mut sender,
        &mut chunker,
        &mut engine_first,
    )
    .await;
    if let Err(e) = &streamed {
        warn!(%device, %turn, index, "sentence interrupted: {e:#}");
    }
    let tail = chunker.map(Chunker::finish).unwrap_or_default();
    if let Err(e) = sender.finish(tail).await {
        warn!(%device, %turn, index, "end of sentence not sent: {e:#}");
    }
    let synth = started.elapsed();
    let audio = sender.audio;
    info!(
        %device, %turn, index,
        first_sound_ms = sender.first_sound.map(|t| (t - received).as_millis() as u64),
        queue_wait_ms = (started - received).as_millis() as u64,
        engine_first_ms = engine_first.map(|d: Duration| d.as_millis() as u64),
        audio_s = format!("{:.2}", audio.as_secs_f64()),
        synth_s = format!("{:.2}", synth.as_secs_f64()),
        rtf = (!audio.is_zero()).then(|| format!("{:.2}", synth.as_secs_f64() / audio.as_secs_f64())),
        chunks = sender.seq - seq,
        "sentence said"
    );
    let _ = inbox.send(Input::Finished {
        turn,
        index,
        seq: sender.seq,
    });
}

async fn stream(
    engine: &Engine,
    text: &str,
    chunk_ms: u32,
    sender: &mut Sender,
    chunker: &mut Option<Chunker>,
    engine_first: &mut Option<Duration>,
) -> anyhow::Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    let speech = engine.speak(text).await?;
    *engine_first = Some(speech.first_sound_after);
    sender.format = speech.format;
    let frame = 2 * speech.format.channels as usize;
    let chunker = chunker.insert(Chunker::new(
        speech.format.bytes(Duration::from_millis(chunk_ms as u64)),
        frame,
    ));
    for chunk in chunker.push(&speech.first) {
        sender.send(chunk, false).await?;
    }
    let mut rest = std::pin::pin!(speech.rest);
    while let Some(packet) = rest.next().await {
        for chunk in chunker.push(&packet?) {
            sender.send(chunk, false).await?;
        }
    }
    Ok(())
}
