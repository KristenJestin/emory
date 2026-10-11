//! `emory-mouth [--config FILE] [--wav FILE] [--check]`
//!
//! Plays the voice of Emory on the speaker of this device. `--wav FILE` writes what would be
//! played to a WAV file instead (no sound on the speaker); `--check` opens the speaker silently,
//! prints what it is and exits.

use std::{path::PathBuf, process::ExitCode};

use anyhow::{Context, bail};
use mouth::{
    bus,
    config::{self, Config},
    sink::{Sink, SpeakerSink, WavSink},
};

struct Args {
    config: Option<PathBuf>,
    wav: Option<PathBuf>,
    check: bool,
}

const USAGE: &str = "usage: emory-mouth [--config FILE] [--wav FILE] [--check]";

fn parse_args() -> anyhow::Result<Option<Args>> {
    let mut args = Args {
        config: None,
        wav: None,
        check: false,
    };
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .map(PathBuf::from)
                .with_context(|| format!("{name} expects a value"))
        };
        match arg.to_str() {
            Some("--config") => args.config = Some(value("--config")?),
            Some("--wav") => args.wav = Some(value("--wav")?),
            Some("--check") => args.check = true,
            Some("--help" | "-h") => {
                println!("{USAGE}");
                return Ok(None);
            }
            _ => bail!("unknown argument {arg:?}\n{USAGE}"),
        }
    }
    Ok(Some(args))
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mouth=info,emory_mouth=info".into()),
        )
        .init();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("emory-mouth: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let Some(args) = parse_args()? else {
        return Ok(());
    };
    let path = match args.config {
        Some(path) => path,
        None => config::default_path()?,
    };
    let config = Config::load(&path)?;
    let device = config.device()?;
    let speaker = config.mouth()?.speaker.clone();

    if args.check {
        match &args.wav {
            Some(_) => println!("device {device}: WAV output"),
            None => {
                let sink = SpeakerSink::open(&speaker)?;
                println!(
                    "device {device}: speaker {speaker:?} ({}) opens: {}",
                    sink.name, sink.buffer
                );
            }
        }
        return Ok(());
    }
    let mut sink: Box<dyn Sink> = match &args.wav {
        Some(file) => Box::new(WavSink::new(file.clone())),
        None => Box::new(SpeakerSink::open(&speaker)?),
    };

    let client = bus::connect(&config).await?;
    bus::run(
        &client,
        &device,
        &speaker,
        sink.as_mut(),
        bus::stop_signal(),
    )
    .await
}
