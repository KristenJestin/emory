//! The outputs: where the sound goes once it is ordered and converted.
//!
//! Both outputs work the same way: the bus side converts the sound and pushes it to a [`Queue`];
//! a consumer pops it in real time, the audio callback of the speaker, or a thread that plays the
//! part of a speaker and writes a WAV file. Flushing empties the queue, so the sound stops at the
//! next callback (a few milliseconds).

use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow, bail};
use contract::audio::Format;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{error, info};

use crate::{
    convert::Converter,
    jitter::{Mark, Play},
    queue::Queue,
};

/// A sentence whose first sample has just gone to the output, and when.
pub type Played = (Mark, Instant);
pub type PlayedTx = UnboundedSender<Played>;

pub trait Sink {
    /// Queues sound for playing.
    fn play(&mut self, play: Play);
    /// Stops at once and drops what was queued; returns how much sound that was.
    fn flush(&mut self) -> Duration;
}

/// The conversion and the queue, common to both outputs.
struct Feed {
    queue: Arc<Queue>,
    converter: Converter,
}

impl Feed {
    fn new(output: Format) -> Self {
        Self {
            queue: Arc::new(Queue::default()),
            converter: Converter::new(output),
        }
    }

    fn play(&mut self, play: Play) {
        let samples = self.converter.convert(play.format, &play.pcm);
        self.queue.push(samples, play.mark);
    }

    fn flush(&mut self) -> Duration {
        let dropped = self.queue.clear();
        self.converter.reset();
        let output = self.converter.output();
        Duration::from_secs_f64(
            dropped as f64 / (output.sample_rate as f64 * output.channels as f64),
        )
    }
}

/// A real speaker, through cpal.
pub struct SpeakerSink {
    feed: Feed,
    /// What the speaker is, for the logs and the announcement.
    pub name: String,
    /// Dropping it stops the thread that holds the stream.
    _stop: std::sync::mpsc::Sender<()>,
}

impl SpeakerSink {
    /// Opens the output device: `"default"`, or a name from [`speaker_names`]. Nothing is played.
    pub fn open(speaker: &str, played: PlayedTx) -> anyhow::Result<Self> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let speaker = speaker.to_owned();
        // The stream is not `Send` on every platform: it lives and dies in its own thread.
        std::thread::Builder::new()
            .name("speaker".into())
            .spawn(move || match open_stream(&speaker, played) {
                Ok((stream, queue, name, output)) => {
                    let _ = ready_tx.send(Ok((queue, name, output)));
                    let _ = stop_rx.recv(); // returns when the sink is dropped
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .context("starting the speaker thread")?;
        let (queue, name, output) = ready_rx
            .recv()
            .context("the speaker thread died")?
            .map_err(|e| anyhow!("{e:#}"))?;
        info!(speaker = %name, "{} Hz, {} channel(s)", output.sample_rate, output.channels);
        Ok(Self {
            feed: Feed {
                queue,
                converter: Converter::new(output),
            },
            name,
            _stop: stop_tx,
        })
    }
}

impl Sink for SpeakerSink {
    fn play(&mut self, play: Play) {
        self.feed.play(play);
    }

    fn flush(&mut self) -> Duration {
        self.feed.flush()
    }
}

/// The names the speaker option accepts.
pub fn speaker_names() -> Vec<String> {
    let host = cpal::default_host();
    let Ok(devices) = host.output_devices() else {
        return Vec::new();
    };
    devices
        .filter_map(|d| d.id().ok().map(|id| id.to_string()))
        .collect()
}

fn find_device(speaker: &str) -> anyhow::Result<cpal::Device> {
    let host = cpal::default_host();
    if speaker == "default" {
        return host
            .default_output_device()
            .context("there is no default output device");
    }
    let devices = host.output_devices().context("listing the outputs")?;
    for device in devices {
        let id = device.id().map(|id| id.to_string()).ok();
        let description = device.description().map(|d| d.name().to_owned()).ok();
        if id.as_deref() == Some(speaker) || description.as_deref() == Some(speaker) {
            return Ok(device);
        }
    }
    let known = speaker_names();
    bail!(
        "no output device named {speaker:?} (known: {})",
        if known.is_empty() {
            "none".to_owned()
        } else {
            known.join(", ")
        }
    )
}

type Opened = (cpal::Stream, Arc<Queue>, String, Format);

fn open_stream(speaker: &str, played: PlayedTx) -> anyhow::Result<Opened> {
    let device = find_device(speaker)?;
    let name = device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| speaker.to_owned());
    let supported = device
        .default_output_config()
        .context("the default configuration of the output")?;
    let output = Format {
        sample_rate: supported.sample_rate(),
        channels: supported.channels(),
    };
    let config = supported.config();
    let queue = Arc::new(Queue::default());
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(&device, &config, &queue, played),
        cpal::SampleFormat::I16 => build::<i16>(&device, &config, &queue, played),
        cpal::SampleFormat::U16 => build::<u16>(&device, &config, &queue, played),
        other => bail!("the output asks for samples of type {other}, which are not handled"),
    }?;
    stream.play().context("starting the output stream")?;
    Ok((stream, queue, name, output))
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    queue: &Arc<Queue>,
    played: PlayedTx,
) -> anyhow::Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let queue = Arc::clone(queue);
    let mut scratch: Vec<f32> = Vec::new();
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                scratch.resize(data.len(), 0.0);
                let (_, marks) = queue.pop(&mut scratch);
                for (out, sample) in data.iter_mut().zip(&scratch) {
                    *out = T::from_sample(*sample);
                }
                if !marks.is_empty() {
                    let now = Instant::now();
                    for mark in marks {
                        let _ = played.send((mark, now));
                    }
                }
            },
            |e| error!("output stream: {e}"),
            None,
        )
        .context("building the output stream")
}

/// A WAV file instead of a speaker: it receives what a speaker would play, and at the pace a
/// speaker would play it, so a stop in the middle leaves a file that stops in the middle. The file
/// is created at the first sound, in the format of that sound (later sounds are converted to it);
/// the silences are not written.
pub struct WavSink {
    path: PathBuf,
    played: PlayedTx,
    started: Option<Started>,
}

struct Started {
    feed: Feed,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WavSink {
    pub fn new(path: PathBuf, played: PlayedTx) -> Self {
        Self {
            path,
            played,
            started: None,
        }
    }

    fn start(&mut self, format: Format) -> anyhow::Result<&mut Started> {
        let feed = Feed::new(format);
        let queue = Arc::clone(&feed.queue);
        let file = File::create(&self.path)
            .with_context(|| format!("creating {}", self.path.display()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (stop, played) = (Arc::clone(&stop), self.played.clone());
            std::thread::Builder::new()
                .name("wav".into())
                .spawn(move || {
                    if let Err(e) = write_wav(file, format, &queue, &played, &stop) {
                        error!("writing the WAV: {e:#}");
                    }
                })
                .context("starting the WAV thread")?
        };
        info!(file = %self.path.display(), "{} Hz, {} channel(s)", format.sample_rate, format.channels);
        Ok(self.started.insert(Started {
            feed,
            stop,
            thread: Some(thread),
        }))
    }
}

impl Sink for WavSink {
    fn play(&mut self, play: Play) {
        let started = match self.started.as_mut() {
            Some(started) => started,
            None => match self.start(play.format) {
                Ok(started) => started,
                Err(e) => {
                    error!("{e:#}");
                    return;
                }
            },
        };
        started.feed.play(play);
    }

    fn flush(&mut self) -> Duration {
        self.started
            .as_mut()
            .map_or(Duration::ZERO, |started| started.feed.flush())
    }
}

impl Drop for WavSink {
    fn drop(&mut self) {
        if let Some(started) = &mut self.started {
            // Let what is queued be played (and written), as a speaker would.
            let thread = started.thread.take();
            while !started.feed.queue.is_empty()
                && thread.as_ref().is_some_and(|t| !t.is_finished())
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            started.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = thread {
                let _ = thread.join();
            }
        }
    }
}

const TICK: Duration = Duration::from_millis(10);

fn write_wav(
    mut file: File,
    format: Format,
    queue: &Queue,
    played: &PlayedTx,
    stop: &AtomicBool,
) -> anyhow::Result<()> {
    let frames_per_tick = format.sample_rate as usize * TICK.as_millis() as usize / 1000;
    let mut samples = vec![0.0f32; frames_per_tick * format.channels as usize];
    let mut bytes: Vec<u8> = Vec::new();
    let mut data_len: u32 = 0;
    file.write_all(&wav_header(format, 0))?;
    let mut next = Instant::now();
    loop {
        let (real, marks) = queue.pop(&mut samples);
        if !marks.is_empty() {
            let now = Instant::now();
            for mark in marks {
                let _ = played.send((mark, now));
            }
        }
        if real > 0 {
            bytes.clear();
            for sample in &samples[..real] {
                let s = (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                bytes.extend(s.to_le_bytes());
            }
            file.write_all(&bytes)?;
            data_len += bytes.len() as u32;
            // Keep the sizes right as we go: a program that is killed still leaves a valid file.
            let end = file.stream_position()?;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&wav_header(format, data_len))?;
            file.seek(SeekFrom::Start(end))?;
        } else if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        next += TICK;
        match next.checked_duration_since(Instant::now()) {
            Some(wait) => std::thread::sleep(wait),
            None => next = Instant::now(), // late: do not try to catch up
        }
    }
}

/// The 44 bytes that open a 16-bit PCM WAV with `data_len` bytes of sound.
pub fn wav_header(format: Format, data_len: u32) -> [u8; 44] {
    let block = format.channels * 2;
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data_len).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&format.channels.to_le_bytes());
    h[24..28].copy_from_slice(&format.sample_rate.to_le_bytes());
    h[28..32].copy_from_slice(&(format.sample_rate * block as u32).to_le_bytes());
    h[32..34].copy_from_slice(&block.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;

    const MONO_24K: Format = Format {
        sample_rate: 24_000,
        channels: 1,
    };

    fn play(ms: usize, mark: Option<Mark>) -> Play {
        Play {
            format: MONO_24K,
            pcm: Bytes::from(vec![0x10; ms * 24 * 2]),
            mark,
        }
    }

    fn temp_wav(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "emory-mouth-test-{}-{name}.wav",
            std::process::id()
        ))
    }

    fn data_len(path: &PathBuf) -> usize {
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        let declared = u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize;
        assert_eq!(declared, bytes.len() - 44, "the header says the truth");
        declared
    }

    #[test]
    fn the_wav_gets_what_is_played_and_reports_the_first_sample() {
        let path = temp_wav("played");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mark = Mark {
            turn: contract::TurnId::new(),
            sentence: 3,
            received: Instant::now(),
        };
        let mut sink = WavSink::new(path.clone(), tx);
        sink.play(play(100, Some(mark)));
        drop(sink); // waits for the 100 ms to be played
        // 100 ms at 24 kHz, 16 bits.
        assert_eq!(data_len(&path), 4800);
        assert_eq!(rx.try_recv().unwrap().0.sentence, 3);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_flush_cuts_the_wav_where_the_speaker_would_have_stopped() {
        let path = temp_wav("flush");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut sink = WavSink::new(path.clone(), tx);
        sink.play(play(2000, None));
        std::thread::sleep(Duration::from_millis(300));
        let dropped = sink.flush();
        drop(sink);
        let written = data_len(&path) as f64 / 48_000.0; // seconds
        assert!((0.2..0.45).contains(&written), "{written} s written");
        assert!(dropped > Duration::from_millis(1500), "{dropped:?} dropped");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn no_sound_no_file() {
        let path = temp_wav("none");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        drop(WavSink::new(path.clone(), tx));
        assert!(!path.exists());
    }
}
