//! The connection to the NATS bus, and what the ear says on it.

use anyhow::{Context, Result};
use async_nats::{ConnectOptions, HeaderMap};
use contract::{Announce, Button, Device, Program, State, TurnId, audio, subjects};
use serde::Serialize;

use crate::config::Config;

pub struct Bus {
    client: async_nats::Client,
    device: Device,
    button: String,
    audio: String,
    announce: String,
}

impl Bus {
    /// Connects to the bus; a bus that is not there yet at start-up is not an error: the
    /// connection retries.
    pub async fn connect(config: &Config, device: Device) -> Result<Self> {
        let mut options = match &config.bus_nkey_seed_file {
            Some(path) => {
                let seed = std::fs::read_to_string(path)
                    .with_context(|| format!("reading the nkey seed {}", path.0.display()))?;
                ConnectOptions::with_nkey(seed.trim().to_owned())
            }
            None => ConnectOptions::new(),
        }
        .name(format!("emory-ear-{device}"))
        .retry_on_initial_connect();
        if let Some(ca) = &config.bus_ca {
            options = options
                .add_root_certificates(ca.0.clone())
                .require_tls(true);
        }
        let client = options
            .connect(&config.bus)
            .await
            .with_context(|| format!("connecting to {}", config.bus))?;
        Ok(Self {
            button: subjects::button(&device),
            audio: subjects::ear_audio(&device),
            announce: subjects::ear_announce(&device),
            client,
            device,
        })
    }

    pub fn client(&self) -> &async_nats::Client {
        &self.client
    }

    async fn publish_json<T: Serialize>(&self, subject: &str, message: &T) -> Result<()> {
        let payload = serde_json::to_vec(message).context("serialising the message")?;
        self.client
            .publish(subject.to_owned(), payload.into())
            .await
            .context("publishing")?;
        self.client.flush().await.context("sending to the server")?;
        Ok(())
    }

    /// The start or the end of a turn.
    pub async fn button(&self, turn: TurnId, state: State) -> Result<()> {
        let message = Button {
            turn,
            state,
            at: contract::now(),
        };
        self.publish_json(&self.button, &message).await
    }

    /// A chunk of the audio of a turn (`seq` counts from 0 in the turn).
    pub async fn audio(&self, turn: TurnId, seq: u64, bytes: Vec<u8>) -> Result<()> {
        let mut headers = HeaderMap::new();
        headers.insert(audio::headers::TURN, turn.to_string());
        headers.insert(audio::headers::SEQ, seq.to_string());
        headers.insert(audio::headers::FORMAT, audio::MIC.to_string());
        self.client
            .publish_with_headers(self.audio.clone(), headers, bytes.into())
            .await
            .context("publishing audio")
    }

    /// What this ear is made of: the microphone and the trigger.
    pub async fn announce(&self, modules: &[String]) -> Result<()> {
        let message = Announce {
            device: self.device.clone(),
            program: Program::Ear,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            modules: modules.to_vec(),
            actions: Vec::new(),
        };
        self.publish_json(&self.announce, &message).await
    }
}
