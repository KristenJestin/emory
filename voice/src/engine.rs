//! The voice engine: `pocket-tts serve` (see `engines/pocket/`). `POST /tts` returns a WAV
//! streamed as the computation goes; closing the connection stops the computation on the engine
//! side.

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use bytes::Bytes;
use contract::audio::Format;
use futures_util::{Stream, StreamExt};

use crate::audio::{WavHeader, parse_wav_header};

#[derive(Clone)]
pub struct Engine {
    http: reqwest::Client,
    url: String,
    voice: String,
}

/// The audio of a sentence arriving: the format, then the raw sound in packets.
pub struct Speech<S> {
    pub format: Format,
    /// From the start of the request to the first byte of sound.
    pub first_sound_after: Duration,
    /// The first bytes of sound, already read with the header.
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

    /// Starts the synthesis of a sentence and returns the stream as soon as the header and a bit of
    /// sound are there. Dropping the stream closes the connection and stops the engine.
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
            .context("the engine does not answer")?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            bail!("the engine answered {status}: {detail}");
        }
        let mut stream = response.bytes_stream();
        let mut head = Vec::new();
        loop {
            let packet = stream
                .next()
                .await
                .context("the engine closed before sending the sound")?
                .context("reading the engine stream")?;
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
                // The header is there but no sound yet: keep reading.
                WavHeader::Ready { .. } => continue,
            }
        }
    }

    /// A throw-away sentence, to load the voice and warm the engine up before the first real one.
    pub async fn warm_up(&self) -> anyhow::Result<()> {
        let speech = self.speak("Bonjour.").await?;
        let mut rest = speech.rest;
        while let Some(packet) = rest.next().await {
            packet?;
        }
        Ok(())
    }
}
