//! A device: its queue of sentences, the synthesis in progress, and what leaves for the bus.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use async_nats::Client;
use bytes::Bytes;
use contract::{Cue, Device, Say, Speaking, TurnId, audio::Format, subjects};
use tokio::{
    sync::{
        mpsc::{UnboundedReceiver, UnboundedSender},
        oneshot,
    },
    task::JoinHandle,
};
use tracing::{debug, info, warn};

use crate::{
    audio::{Chunker, chunk_headers, duration_of},
    config::{Config, LoudnessSettings},
    cue,
    engine::Engine,
    gain::Gain,
    loudness::Loudness,
    queue::{CueSound, Effect, Queue},
    text,
};

/// What a device receives: orders from the bus, and news of its synthesis.
pub enum Input {
    Say(Say),
    /// A waiting cue: a tone, a pulse, or a phrase.
    Cue(Cue),
    Stop(TurnId),
    /// Audio of this turn is leaving (or has just left). With `ack`, the chunk has not been
    /// published yet and waits for the answer: the `Speaking` `start` that this sound may cause
    /// must leave before it.
    Sound {
        turn: TurnId,
        duration: Duration,
        ack: Option<oneshot::Sender<()>>,
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

/// The format used to announce the end of a sentence without sound when the engine has never said
/// what its format is (otherwise: the last format it sent).
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
    let mut queue = Queue::new(config.end_grace(), config.missing_sentence_timeout())
        .with_pulse_interval(config.cue.pulse_interval());
    let mut job: Option<Job> = None;
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
        let mut ack = None;
        let effects = tokio::select! {
            input = inbox.recv() => {
                let Some(input) = input else { break };
                let now = Instant::now();
                match input {
                    Input::Say(say) => queue.say(&say, now),
                    Input::Cue(cue) => queue.cue(&cue, now),
                    Input::Stop(turn) => queue.stop(turn),
                    Input::Sound { turn, duration, ack: wanted } => {
                        ack = wanted;
                        queue.sound_sent(turn, duration, now)
                    }
                    Input::Finished { turn, index, seq } => queue.finished(turn, index, seq, now),
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
        // The effects, `Speaking` `start` included, have left: the sound may follow.
        if let Some(ack) = ack {
            let _ = ack.send(());
        }
    }
    if let Some(job) = job {
        job.cancel();
    }
}

/// The synthesis of a sentence in progress. Cutting it sets a flag that the task checks before
/// each chunk it publishes: aborting the task alone is not enough, it may still publish a few
/// chunks before it reaches a point where it stops.
struct Job {
    handle: JoinHandle<()>,
    cancelled: Arc<AtomicBool>,
}

impl Job {
    fn cancel(self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.handle.abort();
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

async fn apply(effects: Vec<Effect>, cx: Context<'_>, job: &mut Option<Job>) {
    let device = cx.device;
    for effect in effects {
        match effect {
            Effect::Cut => {
                if let Some(job) = job.take() {
                    job.cancel();
                    debug!(%device, "synthesis cut");
                }
            }
            Effect::Skipped { turn, from, to } => {
                warn!(
                    %device, %turn, from, to,
                    "sentence missing: skipped after the delay"
                );
            }
            Effect::PlayCue { turn, sound, seq } => {
                let pcm = match sound {
                    CueSound::Tone => cue::tone(cx.config.cue.tone, cx.config.cue.volume),
                    CueSound::Pulse => cue::pulse(cx.config.cue.volume),
                };
                let headers = chunk_headers(turn, seq, cue::FORMAT, cue::SENTENCE, true);
                match cx
                    .nats
                    .publish_with_headers(subjects::mouth_audio(device), headers, Bytes::from(pcm))
                    .await
                {
                    Ok(()) => debug!(%device, %turn, ?sound, seq, "cue sound sent"),
                    Err(e) => warn!(%device, %turn, "cue sound not sent: {e}"),
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
                let cancelled = Arc::new(AtomicBool::new(false));
                let handle = tokio::spawn(say(
                    sentence,
                    cx.nats.clone(),
                    cx.engine.clone(),
                    cx.config.chunk_ms,
                    Volume {
                        gain: Gain::new(cx.config.gain),
                        loudness: cx.config.loudness,
                    },
                    cx.own.clone(),
                    cancelled.clone(),
                ));
                *job = Some(Job { handle, cancelled });
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

/// How the audio of the engine is made louder before it leaves: the loudness stage when it is
/// enabled (then `gain` is ignored), otherwise the gain.
#[derive(Clone, Copy)]
struct Volume {
    gain: Gain,
    loudness: LoudnessSettings,
}

/// Turns the audio of the engine into what leaves, for one sentence.
struct Shaping {
    volume: Volume,
    /// The loudness stage of this sentence, made with the first sound (its format is then known).
    /// Its state belongs to the sentence: it is drained at the end and never carried over.
    stage: Option<Loudness>,
}

impl Shaping {
    fn new(volume: Volume) -> Self {
        Self {
            volume,
            stage: None,
        }
    }

    /// The audio as it leaves: through the loudness stage, or the gain. The stage delays the
    /// sound by its look-ahead, so the last chunk also carries what it still holds.
    fn shape(&mut self, mut payload: Vec<u8>, format: Format, last: bool) -> Vec<u8> {
        if !self.volume.loudness.enabled {
            self.volume.gain.apply(&mut payload);
            return payload;
        }
        let stage = self.stage.get_or_insert_with(|| {
            Loudness::new(&self.volume.loudness, format.sample_rate, format.channels)
        });
        let mut shaped = stage.process(&payload);
        if last {
            shaped.extend(stage.finish());
        }
        shaped
    }
}

/// Sends the chunks of a sentence, numbered over the whole turn.
struct Sender {
    nats: Client,
    subject: String,
    turn: TurnId,
    index: u32,
    format: Format,
    shaping: Shaping,
    seq: u32,
    audio: Duration,
    first_sound: Option<Instant>,
    inbox: Inbox,
    cancelled: Arc<AtomicBool>,
}

/// The sentence has been cut: nothing more of it is published.
#[derive(Debug)]
struct Cut;

impl std::fmt::Display for Cut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the sentence has been cut")
    }
}

impl std::error::Error for Cut {}

impl Sender {
    fn is_cut(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    async fn send(&mut self, payload: Vec<u8>, last: bool) -> anyhow::Result<()> {
        let payload = self.shaping.shape(payload, self.format, last);
        if payload.is_empty() && !last {
            // The stage is still filling its look-ahead: nothing to send yet.
            return Ok(());
        }
        let duration = duration_of(payload.len(), self.format);
        let first = self.first_sound.is_none() && !duration.is_zero();
        if first {
            // The device loop publishes `Speaking` `start` before this first chunk leaves.
            let (ack, acknowledged) = oneshot::channel();
            let _ = self.inbox.send(Input::Sound {
                turn: self.turn,
                duration,
                ack: Some(ack),
            });
            let _ = acknowledged.await;
        }
        if self.is_cut() {
            return Err(Cut.into());
        }
        let headers = chunk_headers(self.turn, self.seq, self.format, self.index, last);
        self.nats
            .publish_with_headers(self.subject.clone(), headers, Bytes::from(payload))
            .await?;
        self.seq = self.seq.saturating_add(1);
        if !duration.is_zero() {
            self.first_sound.get_or_insert_with(Instant::now);
            self.audio += duration;
            if !first {
                let _ = self.inbox.send(Input::Sound {
                    turn: self.turn,
                    duration,
                    ack: None,
                });
            }
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

async fn say(
    sentence: Sentence,
    nats: Client,
    engine: Engine,
    chunk_ms: u32,
    volume: Volume,
    inbox: Inbox,
    cancelled: Arc<AtomicBool>,
) {
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
        format: engine.last_format().unwrap_or(FALLBACK_FORMAT),
        shaping: Shaping::new(volume),
        seq,
        audio: Duration::ZERO,
        first_sound: None,
        inbox: inbox.clone(),
        cancelled,
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
    if sender.is_cut() {
        // Stopped: nothing more of this sentence leaves, not even its end.
        debug!(%device, %turn, index, "sentence cut");
        return;
    }
    if let Err(e) = &streamed {
        warn!(%device, %turn, index, "sentence interrupted: {e:#}");
    }
    let tail = chunker.map(Chunker::finish).unwrap_or_default();
    if let Err(e) = sender.finish(tail).await {
        warn!(%device, %turn, index, "end of sentence not sent: {e:#}");
    }
    let synth = started.elapsed();
    let audio = sender.audio;
    let loudness = sender.shaping.stage.as_ref();
    if let Some(clipped) = loudness.map(Loudness::clipped).filter(|&n| n > 0) {
        warn!(%device, %turn, index, clipped, "samples cut at the ceiling by the safety net of the limiter");
    }
    info!(
        %device, %turn, index,
        first_sound_ms = sender.first_sound.map(|t| (t - received).as_millis() as u64),
        queue_wait_ms = (started - received).as_millis() as u64,
        engine_first_ms = engine_first.map(|d: Duration| d.as_millis() as u64),
        audio_s = format!("{:.2}", audio.as_secs_f64()),
        synth_s = format!("{:.2}", synth.as_secs_f64()),
        rtf = (!audio.is_zero()).then(|| format!("{:.2}", synth.as_secs_f64() / audio.as_secs_f64())),
        chunks = sender.seq - seq,
        level_lufs = loudness.and_then(Loudness::level).map(|l| format!("{l:.1}")),
        limited_db = loudness.map(|s| format!("{:.1}", s.limited())),
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
    let mut speech = engine.speak(text).await?;
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
    while let Some(packet) = speech.next_packet().await? {
        for chunk in chunker.push(&packet) {
            sender.send(chunk, false).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORMAT: Format = Format {
        sample_rate: 24_000,
        channels: 1,
    };

    fn voice() -> Vec<u8> {
        // A few syllables of a made-up voice at a low level, as bytes of 16-bit PCM.
        (0..24_000)
            .flat_map(|i| {
                let t = i as f32 / 24_000.0;
                let syllable = (std::f32::consts::PI * 3.0 * t).sin().abs();
                let wave = (2.0 * std::f32::consts::PI * 150.0 * t).sin()
                    + 0.4 * (2.0 * std::f32::consts::PI * 450.0 * t).sin();
                ((syllable * wave * 2500.0) as i16).to_le_bytes()
            })
            .collect()
    }

    fn shaped(volume: Volume, audio: &[u8], piece: usize) -> Vec<u8> {
        let mut shaping = Shaping::new(volume);
        let pieces: Vec<&[u8]> = audio.chunks(piece).collect();
        let mut out = Vec::new();
        for (i, part) in pieces.iter().enumerate() {
            out.extend(shaping.shape(part.to_vec(), FORMAT, i + 1 == pieces.len()));
        }
        out
    }

    fn volume(gain: f32, enabled: bool) -> Volume {
        Volume {
            gain: Gain::new(gain),
            loudness: LoudnessSettings {
                enabled,
                ..LoudnessSettings::default()
            },
        }
    }

    #[test]
    fn by_default_the_audio_leaves_as_the_engine_said_it() {
        let config: Config = "nats_url = \"a\"\nengine_url = \"b\"\n".parse().unwrap();
        let volume = Volume {
            gain: Gain::new(config.gain),
            loudness: config.loudness,
        };
        assert!(!volume.loudness.enabled);
        let audio = voice();
        assert_eq!(shaped(volume, &audio, 2880), audio);
        // Even the full-scale extremes, and a stray byte.
        let mut odd = vec![0xff, 0x7f, 0x00, 0x80, 0x12];
        assert_eq!(shaped(volume, &odd.clone(), 5), odd);
        odd.clear();
        assert_eq!(shaped(volume, &odd, 5), odd);
    }

    #[test]
    fn without_the_stage_the_gain_still_works_as_before() {
        let audio = voice();
        let mut expected = audio.clone();
        Gain::new(2.0).apply(&mut expected);
        assert_eq!(shaped(volume(2.0, false), &audio, 2880), expected);
    }

    #[test]
    fn with_the_stage_the_sound_is_whole_and_the_gain_is_ignored() {
        let audio = voice();
        let one = shaped(volume(1.0, true), &audio, 2880);
        assert_eq!(
            one.len(),
            audio.len(),
            "the look-ahead is drained at the last chunk"
        );
        assert_ne!(one, audio);
        assert_eq!(shaped(volume(5.0, true), &audio, 2880), one);
        // Whatever the cutting by the chunker.
        assert_eq!(shaped(volume(1.0, true), &audio, 480), one);
        assert_eq!(shaped(volume(1.0, true), &audio, audio.len()), one);
    }

    #[test]
    fn the_first_chunk_is_shorter_by_the_look_ahead_and_the_last_has_it() {
        let audio = voice();
        let mut shaping = Shaping::new(volume(1.0, true));
        let first = shaping.shape(audio[..2880].to_vec(), FORMAT, false);
        assert_eq!(first.len(), 2880 - 2 * 120);
        let rest = shaping.shape(audio[2880..].to_vec(), FORMAT, true);
        assert_eq!(rest.len(), audio.len() - 2880 + 2 * 120);
        // A chunk smaller than the look-ahead gives nothing yet, and it is not lost.
        let mut shaping = Shaping::new(volume(1.0, true));
        assert!(
            shaping
                .shape(audio[..100].to_vec(), FORMAT, false)
                .is_empty()
        );
        let rest = shaping.shape(audio[100..].to_vec(), FORMAT, true);
        assert_eq!(rest.len(), audio.len());
    }

    #[test]
    fn a_sentence_without_sound_still_ends() {
        let mut shaping = Shaping::new(volume(1.0, true));
        assert!(shaping.shape(Vec::new(), FORMAT, true).is_empty());
    }
}
