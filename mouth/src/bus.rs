//! The NATS side: connects, announces, listens to the voice and to the button, and drives the
//! jitter buffer and the output.

use std::time::{Duration, Instant};

use anyhow::Context;
use contract::{
    Announce, Button, Device, Program, State, TurnId,
    audio::{Format, headers},
    subjects,
};
use futures_util::StreamExt;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{debug, info, warn};

use crate::{
    config::Config,
    jitter::{Cause, Chunk, Jitter, Output, Settings},
    sink::{Played, Sink},
};

/// How often the jitter buffer is told that time passes.
const TICK: Duration = Duration::from_millis(10);

pub async fn connect(config: &Config) -> anyhow::Result<async_nats::Client> {
    let mut options = async_nats::ConnectOptions::new().name("emory-mouth");
    if let Some(ca) = &config.bus_ca {
        options = options
            .require_tls(true)
            .add_root_certificates(ca.to_owned());
    }
    if let Some(file) = &config.bus_nkey_seed_file {
        let seed = std::fs::read_to_string(file)
            .with_context(|| format!("reading the seed {}", file.display()))?;
        options = options.nkey(seed.trim().to_owned());
    }
    options
        .connect(&config.bus)
        .await
        .with_context(|| format!("connecting to NATS {}", config.bus))
}

/// Reads an audio message of the voice.
pub fn parse_chunk(message: &async_nats::Message) -> Result<Chunk, String> {
    let message_headers = message.headers.as_ref().ok_or("no headers")?;
    let header = |name: &str| message_headers.get(name).map(|v| v.as_str());
    let required = |name: &str| header(name).ok_or_else(|| format!("{name} missing"));
    let turn: TurnId = required(headers::TURN)?
        .parse()
        .map_err(|e| format!("{}: {e}", headers::TURN))?;
    let seq: u32 = required(headers::SEQ)?
        .parse()
        .map_err(|e| format!("{}: {e}", headers::SEQ))?;
    let format: Format = required(headers::FORMAT)?
        .parse()
        .map_err(|e| format!("{}: {e}", headers::FORMAT))?;
    // Not on every message of a hand-made stream: sentence 0 then.
    let sentence = header(headers::SENTENCE)
        .map(|s| s.parse().map_err(|e| format!("{}: {e}", headers::SENTENCE)))
        .transpose()?
        .unwrap_or(0);
    Ok(Chunk {
        turn,
        seq,
        format,
        sentence,
        end: header(headers::END) == Some("1"),
        pcm: message.payload.clone(),
    })
}

async fn announce(
    client: &async_nats::Client,
    device: &Device,
    speaker: &str,
) -> anyhow::Result<()> {
    let announce = Announce {
        device: device.clone(),
        program: Program::Mouth,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        modules: vec![speaker.to_owned()],
        actions: Vec::new(),
    };
    client
        .publish(
            subjects::mouth_announce(device),
            serde_json::to_vec(&announce)?.into(),
        )
        .await
        .context("announcing")?;
    client.flush().await.context("announcing")
}

/// Plays what comes back from the voice until `shutdown` is ready.
pub async fn run(
    client: &async_nats::Client,
    device: &Device,
    speaker: &str,
    sink: &mut dyn Sink,
    mut played: UnboundedReceiver<Played>,
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    let mut audio = client.subscribe(subjects::mouth_audio(device)).await?;
    let mut buttons = client.subscribe(subjects::button(device)).await?;
    let mut requests = client.subscribe(subjects::ANNOUNCE_REQUEST).await?;
    announce(client, device, speaker).await?;
    info!(%device, speaker, "listening on {}", subjects::mouth_audio(device));

    let mut jitter = Jitter::new(Settings::default());
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tokio::pin!(shutdown);
    loop {
        let outputs = tokio::select! {
            () = &mut shutdown => break,
            message = audio.next() => {
                let message = message.context("the bus has closed")?;
                match parse_chunk(&message) {
                    Ok(chunk) => jitter.chunk(chunk, Instant::now()),
                    Err(e) => {
                        warn!("audio message dropped: {e}");
                        continue;
                    }
                }
            }
            message = buttons.next() => {
                let message = message.context("the bus has closed")?;
                let received = Instant::now();
                match serde_json::from_slice::<Button>(&message.payload) {
                    Ok(button) if button.state == State::Start => {
                        let outputs = jitter.press(button.turn, received);
                        apply(sink, outputs, Some(received));
                    }
                    Ok(_) => debug!("button end: nothing to do"),
                    Err(e) => warn!("button message dropped: {e}"),
                }
                continue;
            }
            message = requests.next() => {
                message.context("the bus has closed")?;
                announce(client, device, speaker).await?;
                continue;
            }
            Some((mark, at)) = played.recv() => {
                info!(
                    turn = %mark.turn,
                    sentence = mark.sentence,
                    "first sample played {:.0} ms after the first chunk was received",
                    at.saturating_duration_since(mark.received).as_secs_f64() * 1e3,
                );
                continue;
            }
            _ = tick.tick() => jitter.poll(Instant::now()),
        };
        apply(sink, outputs, None);
    }
    Ok(())
}

/// Gives the outputs of the jitter buffer to the sink. `pressed` is when the button message was
/// received, to log how long the silence took.
fn apply(sink: &mut dyn Sink, outputs: Vec<Output>, pressed: Option<Instant>) {
    for output in outputs {
        match output {
            Output::Play(play) => sink.play(play),
            Output::Flush(cause) => {
                let dropped = sink.flush();
                let why = match cause {
                    Cause::Button => "button pressed",
                    Cause::NewerTurn => "a newer turn started without a press",
                };
                info!(
                    "{why}: playback stopped and {:.0} ms of buffered sound dropped ({:.1} ms after the message was received)",
                    dropped.as_secs_f64() * 1e3,
                    pressed.map_or(0.0, |at| at.elapsed().as_secs_f64() * 1e3),
                );
            }
        }
    }
}

/// Resolves when the process is asked to stop (Ctrl-C or SIGTERM).
pub async fn stop_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let Ok(mut term) = signal(SignalKind::terminate()) else {
        let _ = tokio::signal::ctrl_c().await;
        return;
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}
