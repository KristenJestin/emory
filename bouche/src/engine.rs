//! Le moteur de voix : `pocket-tts serve` (voir `moteurs/pocket/`). `POST /tts` rend un WAV
//! diffusé au fur et à mesure du calcul ; fermer la connexion arrête le calcul côté moteur.

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use bytes::Bytes;
use contract::son::Format;
use futures_util::{Stream, StreamExt};

use crate::audio::{WavHeader, parse_wav_header};

#[derive(Clone)]
pub struct Engine {
    http: reqwest::Client,
    url: String,
    voice: String,
}

/// Le son d'une phrase qui arrive : le format, puis le son brut par paquets.
pub struct Speech<S> {
    pub format: Format,
    /// Du début de la requête au premier octet de son.
    pub first_sound_after: Duration,
    /// Les premiers octets de son, déjà lus avec l'en-tête.
    pub first: Bytes,
    pub rest: S,
}

impl Engine {
    pub fn new(url: &str, voice: &str) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .build()?;
        Ok(Self {
            http,
            url: url.trim_end_matches('/').to_owned(),
            voice: voice.to_owned(),
        })
    }

    /// Lance la synthèse d'une phrase et rend le flux dès que l'en-tête et un peu de son sont là.
    /// Laisser tomber le flux ferme la connexion et arrête le moteur.
    pub async fn speak(
        &self,
        text: &str,
    ) -> anyhow::Result<Speech<impl Stream<Item = reqwest::Result<Bytes>> + Send + use<>>> {
        let started = Instant::now();
        let response = self
            .http
            .post(format!("{}/tts", self.url))
            .form(&[("text", text), ("voice_url", self.voice.as_str())])
            .send()
            .await
            .context("le moteur ne répond pas")?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            bail!("le moteur a répondu {status} : {detail}");
        }
        let mut stream = response.bytes_stream();
        let mut head = Vec::new();
        loop {
            let packet = stream
                .next()
                .await
                .context("le moteur a fermé avant d'envoyer le son")?
                .context("lecture du flux du moteur")?;
            head.extend_from_slice(&packet);
            match parse_wav_header(&head).map_err(anyhow::Error::msg)? {
                WavHeader::Incomplete => continue,
                WavHeader::Ready {
                    format,
                    data_offset,
                } if head.len() > data_offset => {
                    return Ok(Speech {
                        format,
                        first_sound_after: started.elapsed(),
                        first: Bytes::copy_from_slice(&head[data_offset..]),
                        rest: stream,
                    });
                }
                // L'en-tête est là mais pas encore de son : on continue de lire.
                WavHeader::Ready { .. } => continue,
            }
        }
    }

    /// Une phrase jetée, pour charger la voix et chauffer le moteur avant la première vraie.
    pub async fn warm_up(&self) -> anyhow::Result<()> {
        let speech = self.speak("Bonjour.").await?;
        let mut rest = speech.rest;
        while let Some(packet) = rest.next().await {
            packet?;
        }
        Ok(())
    }
}
