//! `hearing <configuration.toml>`: the auditory centre of Emory (button ear).

use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use hearing::{
    bus, config::Config, corrections::Corrections, engine::Transcriber, service::Service,
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

    let corrections = match &config.transcription.corrections {
        Some(path) => Corrections::read(path.as_ref())?,
        None => Corrections::default(),
    };
    info!(rules = corrections.len(), "corrections table");

    let models = config.models.clone();
    let settings = config.transcription.clone();
    let transcriber = tokio::task::spawn_blocking(move || Transcriber::start(&models, &settings))
        .await
        .context("starting Parakeet")??;

    let client = bus::connect(&config.nats, "hearing", true).await?;
    info!(url = %config.nats.url, "connected to the bus");

    Service {
        client,
        vad: config.models.vad,
        vad_settings: config.vad,
        transcriber,
        corrections: Arc::new(corrections),
    }
    .run()
    .await
}
