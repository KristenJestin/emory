//! The engines, through sherpa-onnx: Silero VAD to cut the sentences, Parakeet TDT 0.6B v3 int8
//! to transcribe them.
//!
//! The detector is light (12 ms of compute per second of audio on the bench): it runs where the
//! audio arrives. The transcription is heavy: a thread of its own (see [`Transcriber`]), one
//! sentence at a time, away from the async executor.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use contract::TurnId;
use sherpa_onnx::{
    OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig,
    SileroVadModelConfig, VadModelConfig, VoiceActivityDetector,
};
use tokio::sync::{oneshot, watch};
use tracing::{debug, warn};

use crate::{
    config::{ModelsConfig, TranscriptionConfig, VadConfig},
    hotwords::{Decoding, HotwordsFile},
};

const SAMPLE_RATE: i32 = 16_000;

/// How many sentences, all turns together, may wait for Parakeet. A sentence takes 1.4 s to
/// transcribe on the E cores, so this is well over half a minute of backlog: reached only when the
/// transcription has fallen far behind.
const TRANSCRIPTION_BACKLOG: usize = 16;
/// How many idle detectors the pool keeps (a detector is a few MB).
const IDLE_DETECTORS: usize = 4;

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
        settings.provider.check()?;
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
            provider: Some(settings.provider.name().to_owned()),
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

    /// Forgets everything: the detector is ready for another turn.
    fn reset(&mut self) {
        self.vad.reset();
        self.vad.clear();
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

/// The detectors, created once and handed from turn to turn: loading the Silero model from the
/// disk (4 ms) is done at start, not on the loop that follows the bus.
#[derive(Clone)]
pub struct DetectorPool(Arc<PoolInner>);

struct PoolInner {
    model: PathBuf,
    settings: VadConfig,
    idle: Mutex<Vec<Detector>>,
}

impl DetectorPool {
    /// Loads a first detector, so that a model that does not load stops the program at start.
    pub fn new(model: &Path, settings: &VadConfig) -> Result<Self> {
        let first = Detector::new(model, settings)?;
        Ok(Self(Arc::new(PoolInner {
            model: model.to_owned(),
            settings: settings.clone(),
            idle: Mutex::new(vec![first]),
        })))
    }

    /// A detector ready for a turn: an idle one, or a new one if every one is in use.
    pub fn take(&self) -> Result<Detector> {
        let idle = lock(&self.0.idle).pop();
        match idle {
            Some(detector) => Ok(detector),
            None => Detector::new(&self.0.model, &self.0.settings),
        }
    }

    /// Takes a detector back, once its turn is over.
    pub fn give_back(&self, mut detector: Detector) {
        detector.reset();
        let mut idle = lock(&self.0.idle);
        if idle.len() < IDLE_DETECTORS {
            idle.push(detector);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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

/// What happens to a job pushed on a [`JobQueue`].
#[derive(Debug, PartialEq, Eq)]
enum Pushed<T> {
    Queued,
    /// The queue was full: the oldest job of the same turn was dropped to make room.
    DroppedOldest(T),
    /// The queue was full and holds nothing of this turn: the new job is refused.
    Refused(T),
}

/// The jobs waiting for the transcription thread: bounded, and never blocking the one who pushes.
/// When it is full, the oldest pending sentence of the same turn is dropped (the others belong to
/// other turns, which are not ours to cut); if the turn has none waiting, the new one is refused.
struct JobQueue<T> {
    state: Mutex<QueueState<T>>,
    ready: Condvar,
    capacity: usize,
}

struct QueueState<T> {
    jobs: VecDeque<(TurnId, T)>,
    closed: bool,
}

impl<T> JobQueue<T> {
    fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(QueueState {
                jobs: VecDeque::new(),
                closed: false,
            }),
            ready: Condvar::new(),
            capacity,
        }
    }

    fn push(&self, turn: TurnId, job: T) -> Pushed<T> {
        let mut state = lock(&self.state);
        let mut dropped = None;
        if state.jobs.len() >= self.capacity {
            let Some(oldest) = state.jobs.iter().position(|(t, _)| *t == turn) else {
                return Pushed::Refused(job);
            };
            dropped = state.jobs.remove(oldest).map(|(_, job)| job);
        }
        state.jobs.push_back((turn, job));
        drop(state);
        self.ready.notify_one();
        match dropped {
            Some(job) => Pushed::DroppedOldest(job),
            None => Pushed::Queued,
        }
    }

    /// The next job, waiting for one; `None` once the queue is closed and empty.
    fn pop(&self) -> Option<T> {
        let mut state = lock(&self.state);
        loop {
            if let Some((_, job)) = state.jobs.pop_front() {
                return Some(job);
            }
            if state.closed {
                return None;
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    fn close(&self) {
        lock(&self.state).closed = true;
        self.ready.notify_all();
    }
}

/// Closes the queue when the last [`Transcriber`] is gone: the thread then ends quietly.
struct Closer(Arc<JobQueue<Job>>);

impl Drop for Closer {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// Tells that the thread has ended, however it ended (a panic unwinds through it too).
struct Stopped(watch::Sender<bool>);

impl Drop for Stopped {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

/// Runs `work` on each job of the queue, on a thread of its own. The receiver turns `true` when
/// the thread has ended.
fn spawn_worker<T: Send + 'static>(
    name: &str,
    queue: Arc<JobQueue<T>>,
    mut work: impl FnMut(T) + Send + 'static,
) -> std::io::Result<watch::Receiver<bool>> {
    let (stopped, watch) = watch::channel(false);
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let _stopped = Stopped(stopped);
            while let Some(job) = queue.pop() {
                work(job);
            }
        })?;
    Ok(watch)
}

/// Parakeet on a thread of its own: the sentences pass one by one, in the order they arrive.
#[derive(Clone)]
pub struct Transcriber {
    queue: Arc<JobQueue<Job>>,
    stopped: watch::Receiver<bool>,
    _closer: Arc<Closer>,
}

impl Transcriber {
    /// Loads Parakeet, warms it up on a second of silence, then starts the thread.
    pub fn start(models: &ModelsConfig, settings: &TranscriptionConfig) -> Result<Self> {
        settings.provider.check()?;
        let folder = models.parakeet.as_ref();
        let decoding = Decoding::read(
            settings.decoding_method,
            settings.hotwords.as_ref().map(AsRef::as_ref),
        )?;
        let vocabulary = !decoding.hotwords.is_empty();
        // sherpa-onnx reads the words from a file, and needs the model's BPE vocabulary to cut
        // them into tokens.
        let hotwords_file = vocabulary
            .then(|| HotwordsFile::write(&decoding.hotwords))
            .transpose()?;
        let bpe_vocab = match (&settings.bpe_vocab, vocabulary) {
            (Some(path), true) => Some(path_string(path.as_ref())?),
            (None, true) => Some(path_string(&folder.join("bpe.vocab"))?),
            (_, false) => None,
        };
        let config = OfflineRecognizerConfig {
            model_config: OfflineModelConfig {
                transducer: OfflineTransducerModelConfig {
                    encoder: Some(path_string(&folder.join("encoder.int8.onnx"))?),
                    decoder: Some(path_string(&folder.join("decoder.int8.onnx"))?),
                    joiner: Some(path_string(&folder.join("joiner.int8.onnx"))?),
                },
                tokens: Some(path_string(&folder.join("tokens.txt"))?),
                num_threads: settings.threads.max(1),
                provider: Some(settings.provider.name().to_owned()),
                model_type: Some("nemo_transducer".to_owned()),
                modeling_unit: vocabulary.then(|| "bpe".to_owned()),
                bpe_vocab,
                ..OfflineModelConfig::default()
            },
            decoding_method: Some(decoding.method.name().to_owned()),
            hotwords_file: hotwords_file
                .as_ref()
                .map(|file| path_string(file.path()))
                .transpose()?,
            hotwords_score: settings.hotwords_score,
            ..OfflineRecognizerConfig::default()
        };
        let started = Instant::now();
        let recognizer = OfflineRecognizer::create(&config)
            .ok_or_else(|| anyhow!("Parakeet did not start ({})", folder.display()))?;
        let load = started.elapsed();
        // The recognizer has read the words: the copy is not needed any more.
        drop(hotwords_file);
        let warm_up = Instant::now();
        transcribe(&recognizer, &vec![0.0; SAMPLE_RATE as usize])?;
        tracing::info!(
            provider = settings.provider.name(),
            threads = settings.threads,
            decoding = decoding.method.name(),
            hotwords = decoding.hotwords.len(),
            load_ms = load.as_millis() as u64,
            warm_up_ms = warm_up.elapsed().as_millis() as u64,
            "Parakeet ready"
        );

        let queue = Arc::new(JobQueue::new(TRANSCRIPTION_BACKLOG));
        let stopped = spawn_worker("transcription", queue.clone(), move |job: Job| {
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
        })
        .context("transcription thread")?;
        Ok(Self {
            _closer: Arc::new(Closer(queue.clone())),
            queue,
            stopped,
        })
    }

    /// Queues a sentence of the turn; the reply arrives when its turn has come. Never blocks: when
    /// the backlog is full, the oldest pending sentence of the same turn gets an error instead.
    pub fn transcribe(
        &self,
        turn: TurnId,
        samples: Vec<f32>,
    ) -> oneshot::Receiver<Result<Transcription, String>> {
        let (reply, wait) = oneshot::channel();
        let job = Job {
            samples,
            queued: Instant::now(),
            reply,
        };
        match self.queue.push(turn, job) {
            Pushed::Queued => {}
            Pushed::DroppedOldest(dropped) => {
                warn!(%turn, "transcription backlog full: the oldest pending sentence of the turn is dropped");
                let _ = dropped.reply.send(Err(BACKLOG_FULL.to_owned()));
            }
            Pushed::Refused(refused) => {
                warn!(%turn, "transcription backlog full: the new sentence is dropped");
                let _ = refused.reply.send(Err(BACKLOG_FULL.to_owned()));
            }
        }
        wait
    }

    /// Resolves when the transcription thread has ended (it panicked): the service is deaf from
    /// then on.
    pub async fn stopped(&self) {
        let mut stopped = self.stopped.clone();
        // The sender lives as long as the thread; if it is gone, the thread is gone.
        let _ = stopped.wait_for(|stopped| *stopped).await;
    }
}

const BACKLOG_FULL: &str = "dropped, the transcription backlog is full";

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

#[cfg(test)]
mod tests {
    use super::*;

    fn turn() -> TurnId {
        TurnId::new()
    }

    #[test]
    fn jobs_come_out_in_the_order_they_went_in() {
        let (queue, t) = (JobQueue::new(4), turn());
        for job in 0..3 {
            assert_eq!(queue.push(t, job), Pushed::Queued);
        }
        assert_eq!(queue.pop(), Some(0));
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(2));
    }

    #[test]
    fn a_full_queue_drops_the_oldest_sentence_of_the_same_turn() {
        let (queue, mine, other) = (JobQueue::new(3), turn(), turn());
        queue.push(mine, "mine 0");
        queue.push(other, "other 0");
        queue.push(mine, "mine 1");
        assert_eq!(queue.push(mine, "mine 2"), Pushed::DroppedOldest("mine 0"));
        // The other turn kept its place, and the order of the rest is unchanged.
        assert_eq!(queue.pop(), Some("other 0"));
        assert_eq!(queue.pop(), Some("mine 1"));
        assert_eq!(queue.pop(), Some("mine 2"));
    }

    #[test]
    fn a_full_queue_refuses_a_turn_that_has_nothing_waiting() {
        let (queue, a, b) = (JobQueue::new(2), turn(), turn());
        queue.push(a, "a 0");
        queue.push(a, "a 1");
        assert_eq!(queue.push(b, "b 0"), Pushed::Refused("b 0"));
        assert_eq!(queue.pop(), Some("a 0"));
        assert_eq!(queue.pop(), Some("a 1"));
    }

    #[test]
    fn a_closed_queue_ends_once_empty() {
        let (queue, t) = (JobQueue::new(2), turn());
        queue.push(t, 1);
        queue.close();
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), None);
    }

    #[tokio::test]
    async fn a_thread_that_panics_is_reported_as_stopped() {
        let queue = Arc::new(JobQueue::new(2));
        let stopped = spawn_worker("test-panic", queue.clone(), |_: u8| panic!("boom")).unwrap();
        assert!(!*stopped.borrow());
        queue.push(turn(), 1);
        let mut stopped = stopped;
        tokio::time::timeout(Duration::from_secs(5), stopped.wait_for(|s| *s))
            .await
            .expect("the thread is reported as stopped")
            .unwrap();
    }

    #[tokio::test]
    async fn a_thread_that_ends_quietly_is_reported_as_stopped_too() {
        let queue = Arc::new(JobQueue::<u8>::new(2));
        let mut stopped = spawn_worker("test-quiet", queue.clone(), |_| {}).unwrap();
        queue.close();
        tokio::time::timeout(Duration::from_secs(5), stopped.wait_for(|s| *s))
            .await
            .expect("the thread is reported as stopped")
            .unwrap();
    }
}
