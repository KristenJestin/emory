//! Un poste : sa file de phrases, la synthèse en cours, et ce qui part vers le bus.

use std::time::{Duration, Instant};

use async_nats::Client;
use bytes::Bytes;
use contract::{Appareil, Dire, IdTour, Parle, son::Format, sujets};
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

/// Ce qu'un poste reçoit : des ordres du bus, et des nouvelles de sa synthèse.
pub enum Input {
    Dire(Dire),
    Stop(IdTour),
    /// Du son de ce tour vient de partir.
    Sound {
        tour: IdTour,
        duration: Duration,
    },
    /// La phrase est finie d'être envoyée ; `seq` est le prochain numéro de morceau.
    Finished {
        tour: IdTour,
        numero: u32,
        seq: u32,
    },
    Shutdown,
}

pub type Inbox = UnboundedSender<Input>;

/// Le format dont on se sert pour annoncer la fin d'une phrase sans son (le moteur n'a rien dit).
const FALLBACK_FORMAT: Format = Format {
    frequence: 24_000,
    canaux: 1,
};

pub fn spawn(
    poste: Appareil,
    nats: Client,
    engine: Engine,
    config: Config,
) -> (Inbox, JoinHandle<()>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = tokio::spawn(run(poste, nats, engine, config, rx, tx.clone()));
    (tx, handle)
}

async fn run(
    poste: Appareil,
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
                poste: &poste,
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
                    Input::Dire(dire) => queue.dire(&dire, now),
                    Input::Stop(tour) => queue.stop(tour),
                    Input::Sound { tour, duration } => queue.sound_sent(tour, duration, now),
                    Input::Finished { tour, numero, seq } => queue.finished(tour, numero, seq),
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
    poste: &'a Appareil,
    nats: &'a Client,
    engine: &'a Engine,
    config: &'a Config,
    own: &'a Inbox,
}

async fn apply(effects: Vec<Effect>, cx: Context<'_>, job: &mut Option<JoinHandle<()>>) {
    let poste = cx.poste;
    for effect in effects {
        match effect {
            Effect::Cut => {
                if let Some(job) = job.take() {
                    job.abort();
                    debug!(%poste, "synthèse coupée");
                }
            }
            Effect::Parle(etat) => {
                debug!(%poste, ?etat, "parle");
                match serde_json::to_vec(&Parle { etat }) {
                    Ok(json) => {
                        if let Err(e) = cx.nats.publish(sujets::parle(poste), json.into()).await {
                            warn!(%poste, "publication de Parle impossible : {e}");
                        }
                    }
                    Err(e) => warn!("Parle illisible : {e}"),
                }
            }
            Effect::Synthesize {
                tour,
                numero,
                text,
                received,
                seq,
            } => {
                let sentence = Sentence {
                    poste: poste.clone(),
                    tour,
                    numero,
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
    poste: Appareil,
    tour: IdTour,
    numero: u32,
    text: String,
    received: Instant,
    seq: u32,
}

/// Envoie les morceaux d'une phrase, numérotés sur tout le tour.
struct Sender {
    nats: Client,
    subject: String,
    tour: IdTour,
    numero: u32,
    format: Format,
    seq: u32,
    audio: Duration,
    first_sound: Option<Instant>,
    inbox: Inbox,
}

impl Sender {
    async fn send(&mut self, payload: Vec<u8>, last: bool) -> anyhow::Result<()> {
        let duration = duration_of(payload.len(), self.format);
        let headers = chunk_headers(self.tour, self.seq, self.format, self.numero, last);
        self.nats
            .publish_with_headers(self.subject.clone(), headers, Bytes::from(payload))
            .await?;
        self.seq += 1;
        if !duration.is_zero() {
            self.first_sound.get_or_insert_with(Instant::now);
            self.audio += duration;
            let _ = self.inbox.send(Input::Sound {
                tour: self.tour,
                duration,
            });
        }
        Ok(())
    }

    /// Les derniers morceaux, le dernier portant `Emory-Fin` ; sans son, un morceau vide le porte.
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
        poste,
        tour,
        numero,
        text,
        received,
        seq,
    } = sentence;
    let started = Instant::now();
    let mut sender = Sender {
        subject: sujets::son_retour(&poste),
        nats,
        tour,
        numero,
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
        warn!(%poste, %tour, numero, "phrase interrompue : {e:#}");
    }
    let tail = chunker.map(Chunker::finish).unwrap_or_default();
    if let Err(e) = sender.finish(tail).await {
        warn!(%poste, %tour, numero, "fin de phrase non envoyée : {e:#}");
    }
    let synth = started.elapsed();
    let audio = sender.audio;
    info!(
        %poste, %tour, numero,
        first_sound_ms = sender.first_sound.map(|t| (t - received).as_millis() as u64),
        queue_wait_ms = (started - received).as_millis() as u64,
        engine_first_ms = engine_first.map(|d: Duration| d.as_millis() as u64),
        audio_s = format!("{:.2}", audio.as_secs_f64()),
        synth_s = format!("{:.2}", synth.as_secs_f64()),
        rtf = (!audio.is_zero()).then(|| format!("{:.2}", synth.as_secs_f64() / audio.as_secs_f64())),
        chunks = sender.seq - seq,
        "phrase dite"
    );
    let _ = inbox.send(Input::Finished {
        tour,
        numero,
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
    let frame = 2 * speech.format.canaux as usize;
    let chunker = chunker.insert(Chunker::new(
        speech.format.octets(Duration::from_millis(chunk_ms as u64)),
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
