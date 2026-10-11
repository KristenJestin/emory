//! The engines, through sherpa-onnx: Silero VAD to cut the sentences, Parakeet TDT 0.6B v3 int8
//! to transcribe them.
//!
//! The detector is light (12 ms of compute per second of audio on the bench): it runs where the
//! audio arrives. The transcription is heavy: a thread of its own (see [`Transcriber`]), one
//! sentence at a time, away from the async executor.

use std::{
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use sherpa_onnx::{
    OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig,
    SileroVadModelConfig, VadModelConfig, VoiceActivityDetector,
};
use tokio::sync::oneshot;
use tracing::{debug, error};

use crate::config::{ModelsConfig, TranscriptionConfig, VadConfig};

const SAMPLE_RATE: i32 = 16_000;

/// A sentence that the VAD has closed.
pub struct Segment {
    /// The sample, counted from the start of the turn, where the sentence begins.
    pub start: u64,
    pub samples: Vec<f32>,
}

impl Segment {
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.samples.len() as f64 / SAMPLE_RATE as f64)
    }
}

/// Silero VAD for one turn: it receives the audio as it flows and returns the closed sentences.
pub struct Detector {
    vad: VoiceActivityDetector,
}

impl Detector {
    pub fn new(model: &Path, settings: &VadConfig) -> Result<Self> {
        let config = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(path_string(model)?),
                threshold: settings.threshold,
                min_silence_duration: settings.min_silence,
                min_speech_duration: settings.min_speech,
                window_size: 512,
                max_speech_duration: settings.max_speech,
            },
            sample_rate: SAMPLE_RATE,
            num_threads: 1,
            provider: Some("cpu".to_owned()),
            ..VadModelConfig::default()
        };
        let vad = VoiceActivityDetector::create(&config, 30.0)
            .ok_or_else(|| anyhow!("Silero VAD did not start ({})", model.display()))?;
        Ok(Self { vad })
    }

    /// Feeds audio to the detector; returns the sentences it has just closed.
    pub fn feed(&mut self, samples: &[f32]) -> Vec<Segment> {
        self.vad.accept_waveform(samples);
        self.collect()
    }

    /// The end of the turn: the sentence in progress, if it lasts, is closed by force.
    pub fn flush(&mut self) -> Vec<Segment> {
        self.vad.flush();
        self.collect()
    }

    fn collect(&mut self) -> Vec<Segment> {
        let mut segments = Vec::new();
        while let Some(segment) = self.vad.front() {
            segments.push(Segment {
                start: segment.start().max(0) as u64,
                samples: segment.samples().to_vec(),
            });
            self.vad.pop();
        }
        segments
    }
}

/// What a transcription returns.
#[derive(Debug)]
pub struct Transcription {
    pub text: String,
    /// The compute time.
    pub duration: Duration,
    /// The time spent waiting for its turn before the compute.
    pub wait: Duration,
}

struct Job {
    samples: Vec<f32>,
    queued: Instant,
    reply: oneshot::Sender<Result<Transcription, String>>,
}

/// Parakeet on a thread of its own: the sentences pass one by one, in the order they arrive.
#[derive(Clone)]
pub struct Transcriber {
    send: mpsc::Sender<Job>,
}

impl Transcriber {
    /// Loads Parakeet, warms it up on a second of silence, then starts the thread.
    pub fn start(models: &ModelsConfig, settings: &TranscriptionConfig) -> Result<Self> {
        let folder = models.parakeet.as_ref();
        let config = OfflineRecognizerConfig {
            model_config: OfflineModelConfig {
                transducer: OfflineTransducerModelConfig {
                    encoder: Some(path_string(&folder.join("encoder.int8.onnx"))?),
                    decoder: Some(path_string(&folder.join("decoder.int8.onnx"))?),
                    joiner: Some(path_string(&folder.join("joiner.int8.onnx"))?),
                },
                tokens: Some(path_string(&folder.join("tokens.txt"))?),
                num_threads: settings.threads.max(1),
                provider: Some("cpu".to_owned()),
                model_type: Some("nemo_transducer".to_owned()),
                ..OfflineModelConfig::default()
            },
            decoding_method: Some("greedy_search".to_owned()),
            ..OfflineRecognizerConfig::default()
        };
        let started = Instant::now();
        let recognizer = OfflineRecognizer::create(&config)
            .ok_or_else(|| anyhow!("Parakeet did not start ({})", folder.display()))?;
        let load = started.elapsed();
        let warm_up = Instant::now();
        transcribe(&recognizer, &vec![0.0; SAMPLE_RATE as usize])?;
        tracing::info!(
            threads = settings.threads,
            load_ms = load.as_millis() as u64,
            warm_up_ms = warm_up.elapsed().as_millis() as u64,
            "Parakeet ready"
        );

        let (send, receive) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("transcription".to_owned())
            .spawn(move || {
                for job in receive {
                    let wait = job.queued.elapsed();
                    let started = Instant::now();
                    let result = transcribe(&recognizer, &job.samples)
                        .map(|text| Transcription {
                            text,
                            duration: started.elapsed(),
                            wait,
                        })
                        .map_err(|e| format!("{e:#}"));
                    if job.reply.send(result).is_err() {
                        debug!("transcription done, but nobody is waiting for it any more");
                    }
                }
            })
            .context("transcription thread")?;
        Ok(Self { send })
    }

    /// Queues a sentence; the reply arrives when its turn has come.
    pub fn transcribe(
        &self,
        samples: Vec<f32>,
    ) -> oneshot::Receiver<Result<Transcription, String>> {
        let (reply, wait) = oneshot::channel();
        let job = Job {
            samples,
            queued: Instant::now(),
            reply,
        };
        if self.send.send(job).is_err() {
            error!("the transcription thread has stopped");
        }
        wait
    }
}

fn transcribe(recognizer: &OfflineRecognizer, samples: &[f32]) -> Result<String> {
    let stream = recognizer.create_stream();
    stream.accept_waveform(SAMPLE_RATE, samples);
    recognizer.decode(&stream);
    let result = stream
        .get_result()
        .ok_or_else(|| anyhow!("Parakeet returned nothing"))?;
    Ok(result.text.trim().to_owned())
}

fn path_string(path: &Path) -> Result<String> {
    if !path.exists() {
        anyhow::bail!(
            "{} does not exist (see scripts/download-models.sh)",
            path.display()
        );
    }
    path.to_str()
        .map(str::to_owned)
        .with_context(|| format!("path is not UTF-8: {}", path.display()))
}
