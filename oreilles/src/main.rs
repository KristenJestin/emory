//! `oreilles <configuration.toml>` : le centre auditif d'Emory (oreille à bouton).

use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use oreilles::{
    bus, config::Config, corrections::Corrections, moteur::Transcripteur, service::Service,
};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let Some(chemin) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage : oreilles <configuration.toml>");
        std::process::exit(2);
    };
    let config = Config::lire(&chemin)?;

    let corrections = match &config.transcription.corrections {
        Some(chemin) => Corrections::lire(chemin.as_ref())?,
        None => Corrections::default(),
    };
    info!(regles = corrections.len(), "table de corrections");

    let modeles = config.modeles.clone();
    let reglages = config.transcription.clone();
    let transcripteur =
        tokio::task::spawn_blocking(move || Transcripteur::demarrer(&modeles, &reglages))
            .await
            .context("démarrage de Parakeet")??;

    let client = bus::connecter(&config.nats, "oreilles", true).await?;
    info!(url = %config.nats.url, "connecté au bus");

    Service {
        client,
        vad: config.modeles.vad,
        reglages_vad: config.vad,
        transcripteur,
        corrections: Arc::new(corrections),
    }
    .executer()
    .await
}
