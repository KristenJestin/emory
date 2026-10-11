//! The connection to the NATS bus: in the clear for development, in TLS with an nkey on the
//! server.

use anyhow::{Context, Result};
use async_nats::ConnectOptions;
use serde::Serialize;

use crate::config::NatsConfig;

/// Connects to the bus under the name `name`. With `wait`, a bus that is not there yet at start-up
/// is not an error: the connection retries.
pub async fn connect(config: &NatsConfig, name: &str, wait: bool) -> Result<async_nats::Client> {
    let mut options = match &config.nkey {
        Some(path) => {
            let seed = std::fs::read_to_string(path)
                .with_context(|| format!("reading the nkey seed {}", path.0.display()))?;
            ConnectOptions::with_nkey(seed.trim().to_owned())
        }
        None => ConnectOptions::new(),
    }
    .name(name);
    if let Some(ca) = &config.ca {
        options = options
            .add_root_certificates(ca.0.clone())
            .require_tls(true);
    }
    if wait {
        options = options.retry_on_initial_connect();
    }
    options
        .connect(&config.url)
        .await
        .with_context(|| format!("connecting to {}", config.url))
}

/// Publishes a JSON message and waits for it to leave for the server.
pub async fn publish<T: Serialize>(
    client: &async_nats::Client,
    subject: impl async_nats::subject::ToSubject,
    message: &T,
) -> Result<()> {
    let payload = serde_json::to_vec(message).context("serialising the message")?;
    client
        .publish(subject, payload.into())
        .await
        .context("publishing")?;
    client.flush().await.context("sending to the server")?;
    Ok(())
}
