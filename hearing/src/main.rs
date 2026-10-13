//! `hearing <configuration.toml>`: the auditory centre of Emory (button ear).

use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use hearing::{
    bus,
    config::Config,
    corrections::Corrections,
    engine::{DetectorPool, Transcriber},
    hotwords::Vocabulary,
    service::Service,
};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let Some(path) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: hearing <configuration.toml>");
        std::process::exit(2);
    };
    let config = Config::read(&path)?;

    // The table given in the configuration must be there; the one of the data folder may not be.
    let (path, given) = config.transcription.corrections_path();
    let corrections = if given || path.exists() {
        Corrections::read(&path)?
    } else {
        Corrections::default()
    };
    info!(rules = corrections.len(), path = %path.display(), "corrections table");

    let vocabulary = Arc::new(Vocabulary::load(&config.transcription.hotwords_path())?);
    info!(
        words = vocabulary.words().list.len(),
        path = %vocabulary.path().display(),
        "vocabulary"
    );

    let models = config.models.clone();
    let settings = config.transcription.clone();
    let words = vocabulary.clone();
    let transcriber =
        tokio::task::spawn_blocking(move || Transcriber::start(&models, &settings, words))
            .await
            .context("starting Parakeet")??;

    let detectors = DetectorPool::new(config.models.vad.as_ref(), &config.vad)?;

    let client = bus::connect(&config.nats, "hearing", true).await?;
    info!(url = %config.nats.url, "connected to the bus");

    Service {
        client,
        detectors,
        transcriber,
        corrections: Arc::new(corrections),
        max_turn: config.turns.max_turn(),
        vocabulary,
    }
    .run()
    .await
}
