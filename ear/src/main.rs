//! `emory-ear`: the ear of a device. It captures the microphone while a turn is open and sends
//! the sound to the hearing; what opens and closes a turn is a trigger (see `trigger`).
//!
//! ```text
//! emory-ear [--config FILE] [--wav FILE]
//! emory-ear [--config FILE] --list-mics | --check-mic | --check-trigger
//! ```

mod bus;
mod capture;
mod config;
mod resample;
mod trigger;
mod wav;

use std::{path::PathBuf, process::ExitCode, time::Duration};

use anyhow::{Context, Result, bail};
use bus::Bus;
use capture::Capture;
use config::{Config, EarConfig};
use contract::{State, TurnId, subjects};
use futures_util::StreamExt;
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::mpsc,
};
use trigger::Command;

const USAGE: &str = "\
usage: emory-ear [--config FILE] [--wav FILE]
       emory-ear [--config FILE] --list-mics | --check-mic | --check-trigger

  --config FILE      the device configuration (default: ~/.config/emory/config.toml)
  --wav FILE         play a 16 kHz mono WAV file as one turn instead of using the microphone
  --list-mics        list the input devices
  --check-mic        open the microphone for a moment and print its level; keeps nothing
  --check-trigger    run the trigger alone and print what it asks, without microphone or bus";

#[derive(Default)]
struct Arguments {
    config: Option<PathBuf>,
    wav: Option<String>,
    list_mics: bool,
    check_mic: bool,
    check_trigger: bool,
}

fn arguments() -> Result<Option<Arguments>> {
    let mut parsed = Arguments::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .with_context(|| format!("{name} expects a value"))
        };
        match arg.as_str() {
            "--config" => parsed.config = Some(value("--config")?.into()),
            "--wav" => parsed.wav = Some(value("--wav")?),
            "--list-mics" => parsed.list_mics = true,
            "--check-mic" => parsed.check_mic = true,
            "--check-trigger" => parsed.check_trigger = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "--version" => {
                println!("emory-ear {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            _ => bail!("unknown argument: {arg}\n{USAGE}"),
        }
    }
    Ok(Some(parsed))
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    match run().await {
        Ok(code) => code,
        Err(error) => {
            tracing::error!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<ExitCode> {
    let Some(args) = arguments()? else {
        return Ok(ExitCode::SUCCESS);
    };
    if args.list_mics {
        for name in capture::list_mics()? {
            println!("{name}");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let path = match &args.config {
        Some(path) => path.clone(),
        None => config::default_path()?,
    };
    let config = Config::load(&path)?;
    let Some(ear) = config.ear.clone() else {
        eprintln!(
            "emory-ear: no [ear] section in {}: this device has no ear, nothing to run",
            path.display()
        );
        return Ok(ExitCode::SUCCESS);
    };

    if args.check_mic {
        check_mic(&ear).await?;
        return Ok(ExitCode::SUCCESS);
    }
    if args.check_trigger {
        check_trigger(&ear).await?;
        return Ok(ExitCode::SUCCESS);
    }

    let device = config.device()?;
    tracing::info!(%device, bus = config.bus, "starting");
    let bus = Bus::connect(&config, device).await?;
    match args.wav {
        Some(file) => {
            bus.announce(&["wav".to_owned()]).await?;
            wav::play(&bus, &file).await?;
            // Everything has left for the server before the program ends.
            bus.client().flush().await?;
        }
        None => listen(&bus, &ear).await?,
    }
    Ok(ExitCode::SUCCESS)
}

/// A turn in progress.
struct Turn {
    id: TurnId,
    next_seq: u64,
    capture: Capture,
}

/// Waits for the next chunk of the turn in progress, forever when there is none.
async fn next_chunk(turn: &mut Option<Turn>) -> Option<Vec<u8>> {
    match turn {
        Some(turn) => turn.capture.next_chunk().await,
        None => std::future::pending().await,
    }
}

/// The ear at work: turns opened and closed by the trigger.
async fn listen(bus: &Bus, ear: &EarConfig) -> Result<()> {
    let trigger = trigger::build(ear)?;
    let modules = vec![ear.mic.clone(), trigger.name().to_owned()];
    let mut announce_requests = bus.client().subscribe(subjects::ANNOUNCE_REQUEST).await?;
    bus.announce(&modules).await?;

    let (commands_tx, mut commands) = mpsc::channel(8);
    let mut trigger_task = tokio::spawn(trigger.run(commands_tx));
    let mut terminate = signal(SignalKind::terminate())?;
    let mut turn: Option<Turn> = None;
    tracing::info!(mic = ear.mic, trigger = modules[1], "ready");

    let result = loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Open) => {
                    if turn.is_none() {
                        match open_turn(bus, ear).await {
                            Ok(opened) => turn = Some(opened),
                            Err(error) => tracing::error!("cannot open a turn: {error:#}"),
                        }
                    }
                }
                Some(Command::Close) => {
                    if let Some(open) = turn.take()
                        && let Err(error) = close_turn(bus, open).await
                    {
                        break Err(error);
                    }
                }
                None => break Ok(()),
            },
            chunk = next_chunk(&mut turn) => match chunk {
                Some(chunk) => {
                    let open = turn.as_mut().expect("a chunk comes from a turn");
                    let seq = open.next_seq;
                    open.next_seq += 1;
                    if let Err(error) = bus.audio(open.id, seq, chunk).await {
                        break Err(error);
                    }
                }
                // The microphone stopped by itself: the turn is over.
                None => {
                    if let Some(open) = turn.take() {
                        tracing::error!("the microphone stopped; ending the turn");
                        if let Err(error) = bus.button(open.id, State::End).await {
                            break Err(error);
                        }
                    }
                }
            },
            Some(_) = announce_requests.next() => {
                if let Err(error) = bus.announce(&modules).await {
                    tracing::warn!("announce: {error:#}");
                }
            }
            joined = &mut trigger_task => {
                break match joined {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(error)) => Err(error.context("the trigger failed")),
                    Err(error) => Err(anyhow::Error::new(error).context("the trigger panicked")),
                };
            }
            _ = tokio::signal::ctrl_c() => break Ok(()),
            _ = terminate.recv() => break Ok(()),
        }
    };

    // Whatever the reason, a turn in progress ends cleanly.
    if let Some(open) = turn.take() {
        let _ = close_turn(bus, open).await;
    }
    trigger_task.abort();
    result
}

async fn open_turn(bus: &Bus, ear: &EarConfig) -> Result<Turn> {
    let capture = Capture::open(&ear.mic).await?;
    let id = TurnId::new();
    bus.button(id, State::Start).await?;
    tracing::info!(turn = %id, "turn started");
    Ok(Turn {
        id,
        next_seq: 0,
        capture,
    })
}

/// Closes the microphone, sends what it still had, then the end.
async fn close_turn(bus: &Bus, mut turn: Turn) -> Result<()> {
    turn.capture.stop();
    while let Some(chunk) = turn.capture.next_chunk().await {
        bus.audio(turn.id, turn.next_seq, chunk).await?;
        turn.next_seq += 1;
    }
    bus.button(turn.id, State::End).await?;
    tracing::info!(turn = %turn.id, chunks = turn.next_seq, "turn ended");
    Ok(())
}

/// Opens the microphone for half a second and prints what came, to show it works. Nothing is
/// kept or sent.
async fn check_mic(ear: &EarConfig) -> Result<()> {
    let asked = tokio::time::Instant::now();
    let mut capture = Capture::open(&ear.mic).await?;
    let opened = asked.elapsed();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    let (mut chunks, mut peak, mut first) = (0u32, 0i16, None);
    while let Ok(Some(chunk)) = tokio::time::timeout_at(deadline, capture.next_chunk()).await {
        first.get_or_insert(asked.elapsed());
        chunks += 1;
        let loudest = chunk
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&b| i16::from_le_bytes(b).saturating_abs())
            .max()
            .unwrap_or(0);
        peak = peak.max(loudest);
    }
    capture.stop();
    println!(
        "microphone {:?}: opened in {} ms, first sound after {} ms, {chunks} chunks of 20 ms in 0.5 s, \
         peak {peak} of 32767",
        ear.mic,
        opened.as_millis(),
        first.map_or(0, |t| t.as_millis())
    );
    if chunks == 0 {
        bail!("the microphone gave no sound");
    }
    Ok(())
}

/// Runs the trigger alone and prints what it asks, until interrupted.
async fn check_trigger(ear: &EarConfig) -> Result<()> {
    let trigger = trigger::build(ear)?;
    println!("trigger {}: press the key (Ctrl-C to stop)", trigger.name());
    let (tx, mut rx) = mpsc::channel(8);
    let mut task = tokio::spawn(trigger.run(tx));
    let started = tokio::time::Instant::now();
    loop {
        tokio::select! {
            Some(command) = rx.recv() => {
                println!("{:>7.2} s  {command:?}", started.elapsed().as_secs_f64());
            }
            joined = &mut task => return joined.context("the trigger panicked")?,
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}
