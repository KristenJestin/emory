//! The voice engine: `pocket-tts serve` (see `engines/pocket/`). `POST /tts` returns a WAV
//! streamed as the computation goes; closing the connection stops the computation on the engine
//! side.
//!
//! The engine is a single machine for every device: a semaphore (`engine_concurrency`, one by
//! default) lets one sentence at a time through, and the engine is never trusted to answer: it has
//! `engine_idle_timeout` to send each packet, the response included.

use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow, bail};
use bytes::Bytes;
use contract::audio::Format;
use futures_util::{Stream, StreamExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::audio::{WavHeader, parse_wav_header};

/// The warm-up at start-up gets this long, in all: the first sentence may load the voice at the
/// engine, which takes longer than `engine_idle_timeout`.
const WARM_UP_LIMIT: Duration = Duration::from_secs(30);
/// The header of the WAV is a few dozen bytes; a stream that goes on without a `data` block past
/// this is not a WAV we can play.
const MAX_HEADER_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct Engine {
    http: reqwest::Client,
    url: String,
    voice: String,
    idle: Duration,
    slots: Arc<Semaphore>,
    /// The format of the last sound the engine sent.
    format: Arc<Mutex<Option<Format>>>,
}

type Packets = Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>;

/// The audio of a sentence arriving: the format, then the raw sound in packets. It holds the
/// engine's place (see `engine_concurrency`) until it is dropped.
pub struct Speech {
    pub format: Format,
    /// From the start of the request to the first byte of sound.
    pub first_sound_after: Duration,
    /// The first bytes of sound, already read with the header.
    pub first: Bytes,
    rest: Packets,
    idle: Duration,
    _slot: OwnedSemaphorePermit,
}

impl Speech {
    /// The next packet of sound; `None` at the end of the sentence. An engine that stays silent
    /// for longer than the idle timeout is an error.
    pub async fn next_packet(&mut self) -> anyhow::Result<Option<Bytes>> {
        packet(&mut self.rest, self.idle).await
    }
}

async fn packet(stream: &mut Packets, idle: Duration) -> anyhow::Result<Option<Bytes>> {
    match tokio::time::timeout(idle, stream.next()).await {
        Ok(Some(packet)) => Ok(Some(packet.context("reading the engine stream")?)),
        Ok(None) => Ok(None),
        Err(_) => bail!("the engine has been silent for {idle:?}"),
    }
}

impl Engine {
    pub fn new(url: &str, voice: &str, idle: Duration, concurrency: usize) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .build()?;
        Ok(Self {
            http,
            url: url.trim_end_matches('/').to_owned(),
            voice: voice.to_owned(),
            idle,
            slots: Arc::new(Semaphore::new(concurrency.max(1))),
            format: Arc::default(),
        })
    }

    /// The format of the last sound the engine sent, once it has sent some.
    pub fn last_format(&self) -> Option<Format> {
        *self.format.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Starts the synthesis of a sentence and returns the stream as soon as the header and a bit of
    /// sound are there. Dropping the stream closes the connection and stops the engine.
    pub async fn speak(&self, text: &str) -> anyhow::Result<Speech> {
        self.speak_within(text, self.idle).await
    }

    async fn speak_within(&self, text: &str, idle: Duration) -> anyhow::Result<Speech> {
        let slot = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .context("the engine is closed")?;
        let started = Instant::now();
        let response = tokio::time::timeout(
            idle,
            self.http
                .post(format!("{}/tts", self.url))
                .form(&[("text", text), ("voice_url", self.voice.as_str())])
                .send(),
        )
        .await
        .map_err(|_| anyhow!("the engine has been silent for {idle:?}"))?
        .context("the engine does not answer")?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            bail!("the engine answered {status}: {detail}");
        }
        let mut stream: Packets = Box::pin(response.bytes_stream());
        let mut head = Vec::new();
        loop {
            let packet = packet(&mut stream, idle)
                .await?
                .context("the engine closed before sending the sound")?;
            head.extend_from_slice(&packet);
            match parse_wav_header(&head).map_err(anyhow::Error::msg)? {
                WavHeader::Ready {
                    format,
                    data_offset,
                } if head.len() > data_offset => {
                    *self.format.lock().unwrap_or_else(|e| e.into_inner()) = Some(format);
                    return Ok(Speech {
                        format,
                        first_sound_after: started.elapsed(),
                        first: Bytes::copy_from_slice(&head[data_offset..]),
                        rest: stream,
                        idle,
                        _slot: slot,
                    });
                }
                // Not enough bytes yet, or the header is there but no sound: keep reading.
                WavHeader::Incomplete | WavHeader::Ready { .. } => {
                    if head.len() > MAX_HEADER_BYTES {
                        bail!("the engine sent {} bytes without any sound", head.len());
                    }
                }
            }
        }
    }

    /// A throw-away sentence, to load the voice and warm the engine up before the first real one.
    /// It never takes longer than [`WARM_UP_LIMIT`].
    pub async fn warm_up(&self) -> anyhow::Result<()> {
        tokio::time::timeout(WARM_UP_LIMIT, async {
            let mut speech = self.speak_within("Bonjour.", WARM_UP_LIMIT).await?;
            while speech.next_packet().await?.is_some() {}
            anyhow::Ok(())
        })
        .await
        .map_err(|_| anyhow!("the engine did not warm up within {WARM_UP_LIMIT:?}"))?
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    /// A WAV header for 24 kHz mono 16-bit, the way the engine streams it.
    fn wav_header() -> Vec<u8> {
        let mut h = b"RIFF".to_vec();
        h.extend(0xFFFF_FFFFu32.to_le_bytes());
        h.extend(b"WAVEfmt ");
        h.extend(16u32.to_le_bytes());
        h.extend(1u16.to_le_bytes());
        h.extend(1u16.to_le_bytes());
        h.extend(24_000u32.to_le_bytes());
        h.extend(48_000u32.to_le_bytes());
        h.extend(2u16.to_le_bytes());
        h.extend(16u16.to_le_bytes());
        h.extend(b"data");
        h.extend(0xFFFF_FFFFu32.to_le_bytes());
        h
    }

    /// A fake engine that answers `chunks` (each preceded by a pause) in a chunked HTTP response,
    /// then holds the connection open without a word.
    fn fake_engine(chunks: Vec<(Duration, Vec<u8>)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let chunks = chunks.clone();
                std::thread::spawn(move || {
                    let mut request = [0u8; 4096];
                    let _ = stream.read(&mut request);
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: audio/wav\r\ntransfer-encoding: chunked\r\n\r\n",
                    );
                    for (pause, bytes) in chunks {
                        std::thread::sleep(pause);
                        let _ = write!(stream, "{:x}\r\n", bytes.len());
                        let _ = stream.write_all(&bytes);
                        let _ = stream.write_all(b"\r\n");
                        let _ = stream.flush();
                    }
                    // Stalled: no more packets, no end of the response.
                    std::thread::sleep(Duration::from_secs(30));
                });
            }
        });
        url
    }

    fn engine(url: &str, idle: Duration, concurrency: usize) -> Engine {
        Engine::new(url, "estelle", idle, concurrency).unwrap()
    }

    #[tokio::test]
    async fn a_stalled_sentence_ends_with_an_error_after_the_idle_timeout() {
        let mut first = wav_header();
        first.extend([1u8; 480]);
        let url = fake_engine(vec![(Duration::ZERO, first)]);
        let engine = engine(&url, Duration::from_millis(300), 1);
        let mut speech = engine.speak("Bonjour.").await.unwrap();
        assert_eq!(speech.first.len(), 480);
        assert_eq!(speech.format.sample_rate, 24_000);
        let started = Instant::now();
        let error = speech.next_packet().await.unwrap_err();
        assert!(format!("{error:#}").contains("silent"), "{error:#}");
        assert!(started.elapsed() < Duration::from_secs(3));
        // The format is remembered for the empty chunk of a later sentence that has no sound.
        assert_eq!(engine.last_format(), Some(speech.format));
    }

    #[tokio::test]
    async fn an_engine_that_sends_nothing_at_all_is_given_up_on() {
        let url = fake_engine(vec![(Duration::from_secs(30), vec![1])]);
        let engine = engine(&url, Duration::from_millis(300), 1);
        let started = Instant::now();
        // The headers come at once, but no sound: the first packet times out.
        assert!(engine.speak("Bonjour.").await.is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(engine.last_format(), None);
    }

    #[tokio::test]
    async fn the_warm_up_gives_up_instead_of_hanging() {
        // Nothing listens on this address once the listener is dropped: the connection is refused.
        let url = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", listener.local_addr().unwrap())
        };
        let engine = engine(&url, Duration::from_millis(300), 1);
        assert!(engine.warm_up().await.is_err());
    }

    #[tokio::test]
    async fn the_engine_takes_one_sentence_at_a_time() {
        let mut first = wav_header();
        first.extend([1u8; 480]);
        let url = fake_engine(vec![(Duration::ZERO, first)]);
        let engine = engine(&url, Duration::from_secs(5), 1);
        let speech = engine.speak("un").await.unwrap();
        // The second sentence waits for the place of the first.
        let second = engine.clone();
        let waiting = tokio::spawn(async move { second.speak("deux").await.map(|_| ()) });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!waiting.is_finished(), "the second sentence has to wait");
        drop(speech);
        let result = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("the second sentence goes through once the first is dropped")
            .unwrap();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn two_places_let_two_sentences_through() {
        let mut first = wav_header();
        first.extend([1u8; 480]);
        let url = fake_engine(vec![(Duration::ZERO, first)]);
        let engine = engine(&url, Duration::from_secs(5), 2);
        let _a = engine.speak("un").await.unwrap();
        let _b = tokio::time::timeout(Duration::from_secs(5), engine.speak("deux"))
            .await
            .expect("a second place is free")
            .unwrap();
    }
}
