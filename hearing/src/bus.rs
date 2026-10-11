//! La connexion au bus NATS : en clair pour développer, en TLS avec une nkey sur le serveur.

use anyhow::{Context, Result};
use async_nats::ConnectOptions;
use serde::Serialize;

use crate::config::ConfigNats;

/// Se connecte au bus sous le nom `nom`. Avec `attendre`, un bus qui n'est pas encore là au
/// démarrage n'est pas une erreur : la connexion réessaie.
pub async fn connecter(
    config: &ConfigNats,
    nom: &str,
    attendre: bool,
) -> Result<async_nats::Client> {
    let mut options = match &config.nkey {
        Some(chemin) => {
            let graine = std::fs::read_to_string(chemin)
                .with_context(|| format!("lecture de la graine nkey {}", chemin.0.display()))?;
            ConnectOptions::with_nkey(graine.trim().to_owned())
        }
        None => ConnectOptions::new(),
    }
    .name(nom);
    if let Some(ca) = &config.ca {
        options = options
            .add_root_certificates(ca.0.clone())
            .require_tls(true);
    }
    if attendre {
        options = options.retry_on_initial_connect();
    }
    options
        .connect(&config.url)
        .await
        .with_context(|| format!("connexion à {}", config.url))
}

/// Publie un message JSON et attend qu'il soit parti vers le serveur.
pub async fn publier<T: Serialize>(
    client: &async_nats::Client,
    sujet: impl async_nats::subject::ToSubject,
    message: &T,
) -> Result<()> {
    let charge = serde_json::to_vec(message).context("sérialisation du message")?;
    client
        .publish(sujet, charge.into())
        .await
        .context("publication")?;
    client.flush().await.context("envoi vers le serveur")?;
    Ok(())
}
