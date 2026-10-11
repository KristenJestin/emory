//! The outputs: where the sound goes once it is ordered and converted.
//!
//! Both outputs work the same way: the bus side converts the sound and pushes it to a lock-free
//! [`ring`](crate::ring); a consumer pops it in real time, the audio callback of the speaker, or
//! a thread that plays the part of a speaker and writes a WAV file. Flushing makes the consumer
//! skip what is waiting, so the sound stops at the next callback.
//!
//! The speaker also drops and reopens its stream on a flush that cuts sound: the samples already
//! handed to the device (a few milliseconds with the small buffer asked for) are thrown away with
//! the stream instead of being played to the end. `pause` would be gentler but cpal's ALSA
//! backend ignores a failure to pause, and a paused stream would play its stale tail on resume.

use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow, bail};
use contract::audio::Format;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tracing::{error, info, warn};

use crate::{
    convert::Converter,
    jitter::Play,
    ring::{self, Played, PlayedReader, Reader, Writer},
};

/// How much sound the ring holds: a whole answer.
const RING_SECONDS: usize = 60;

/// The buffer asked of the speaker, in milliseconds of sound.
const BUFFER_MS: u32 = 10;

/// The speaker is declared dead when its callback has not run for this long.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(3);

pub trait Sink {
    /// Queues sound for playing.
    fn play(&mut self, play: Play);
    /// Stops at once and drops what was queued; returns how much sound that was.
    fn flush(&mut self) -> Duration;
    /// Moves to `out` the sentences whose first sample has gone to the output since last time.
    fn take_played(&mut self, out: &mut Vec<Played>);
    /// Why the output can no longer be used, once it is the case.
    fn failure(&self) -> Option<String>;
}

/// What went wrong with an output, written once from any thread.
#[derive(Default)]
struct Health(Mutex<Option<String>>);

impl Health {
    fn fail(&self, message: String) {
        error!("{message}");
        let mut failure = self.0.lock().unwrap_or_else(|e| e.into_inner());
        failure.get_or_insert(message);
    }

    fn get(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// The conversion and the ring, common to both outputs.
struct Feed {
    writer: Writer,
    played: PlayedReader,
    converter: Converter,
}

impl Feed {
    /// Also returns the reader, for the consumer.
    fn new(output: Format) -> (Self, Reader) {
        let capacity = RING_SECONDS * output.sample_rate as usize * output.channels as usize;
        let (writer, reader, played) = ring::channel(capacity);
        let feed = Self {
            writer,
            played,
            converter: Converter::new(output),
        };
        (feed, reader)
    }

    fn play(&mut self, play: Play) {
        let samples = self.converter.convert(play.format, &play.pcm);
        let dropped = self.writer.push(&samples, play.mark);
        if dropped > 0 {
            warn!("the output is {RING_SECONDS} s behind: {dropped} samples dropped");
        }
    }

    fn flush(&mut self) -> Duration {
        let dropped = self.writer.flush();
        self.converter.reset();
        let output = self.converter.output();
        Duration::from_secs_f64(
            dropped as f64 / (output.sample_rate as f64 * output.channels as f64),
        )
    }
}

/// What the audio callback tells the rest of the program, without a lock.
struct Heartbeat {
    base: Instant,
    /// When the callback last ran, in nanoseconds after `base`.
    last: AtomicU64,
    /// How many frames it was asked for last time.
    frames: AtomicU32,
}

impl Heartbeat {
    fn new() -> Self {
        Self {
            base: Instant::now(),
            last: AtomicU64::new(0),
            frames: AtomicU32::new(0),
        }
    }

    fn beat(&self) {
        self.last
            .store(self.base.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }

    fn silence(&self) -> Duration {
        self.base
            .elapsed()
            .saturating_sub(Duration::from_nanos(self.last.load(Ordering::Relaxed)))
    }
}

enum Command {
    /// Drop the stream and open it again: what the device holds is thrown away.
    Reset,
}

/// A real speaker, through cpal.
pub struct SpeakerSink {
    feed: Feed,
    health: Arc<Health>,
    heartbeat: Arc<Heartbeat>,
    commands: mpsc::Sender<Command>,
    /// When the sound handed over so far will have been played.
    sound_until: Instant,
    /// What the speaker is, for the logs and the announcement.
    pub name: String,
    /// The buffer asked for and the one obtained, for the logs.
    pub buffer: String,
}

/// What the speaker thread tells `open` once the stream runs.
struct Ready {
    writer_feed: Feed,
    name: String,
    buffer: String,
    heartbeat: Arc<Heartbeat>,
    health: Arc<Health>,
}

impl SpeakerSink {
    /// Opens the output device: `"default"`, or a name from [`speaker_names`]. Nothing is played.
    pub fn open(speaker: &str) -> anyhow::Result<Self> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (commands, commands_rx) = mpsc::channel::<Command>();
        let speaker = speaker.to_owned();
        // The stream is not `Send` on every platform: it lives and dies in its own thread.
        std::thread::Builder::new()
            .name("speaker".into())
            .spawn(move || speaker_thread(&speaker, ready_tx, commands_rx))
            .context("starting the speaker thread")?;
        let ready = ready_rx
            .recv()
            .context("the speaker thread died")?
            .map_err(|e| anyhow!("{e:#}"))?;
        info!(speaker = %ready.name, "{}", ready.buffer);
        Ok(Self {
            feed: ready.writer_feed,
            health: ready.health,
            heartbeat: ready.heartbeat,
            commands,
            sound_until: Instant::now(),
            name: ready.name,
            buffer: ready.buffer,
        })
    }
}

impl Sink for SpeakerSink {
    fn play(&mut self, play: Play) {
        let now = Instant::now();
        self.sound_until = self.sound_until.max(now) + play.duration();
        self.feed.play(play);
    }

    fn flush(&mut self) -> Duration {
        let dropped = self.feed.flush();
        // Nothing playing (or about to): nothing in the device worth the cost of reopening it.
        if Instant::now() < self.sound_until + Duration::from_millis(100) {
            let _ = self.commands.send(Command::Reset);
        }
        self.sound_until = Instant::now();
        dropped
    }

    fn take_played(&mut self, out: &mut Vec<Played>) {
        self.feed.played.drain(out);
    }

    fn failure(&self) -> Option<String> {
        if let Some(failure) = self.health.get() {
            return Some(failure);
        }
        // A device that disappears does not always say so: its callback just stops.
        let silence = self.heartbeat.silence();
        if silence > CALLBACK_TIMEOUT {
            self.health.fail(format!(
                "the output has not asked for sound for {:.0} s: the speaker is gone",
                silence.as_secs_f64()
            ));
        }
        self.health.get()
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

/// What the stream needs, kept to open it again.
struct Setup {
    device: cpal::Device,
    supported: cpal::SupportedStreamConfig,
    reader: Arc<Mutex<Reader>>,
    health: Arc<Health>,
    heartbeat: Arc<Heartbeat>,
}

fn speaker_thread(
    speaker: &str,
    ready: mpsc::Sender<anyhow::Result<Ready>>,
    commands: mpsc::Receiver<Command>,
) {
    let opened = (|| {
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
        let (feed, reader) = Feed::new(output);
        let setup = Setup {
            device,
            supported,
            reader: Arc::new(Mutex::new(reader)),
            health: Arc::new(Health::default()),
            heartbeat: Arc::new(Heartbeat::new()),
        };
        let (stream, asked) = start_stream(&setup)?;
        // The size of the buffers is only known when the callback runs: wait for it.
        let deadline = Instant::now() + Duration::from_millis(500);
        while setup.heartbeat.frames.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let frames = setup.heartbeat.frames.load(Ordering::Relaxed);
        let buffer = format!(
            "{} Hz, {} channel(s); buffer {asked}; the callback is asked for {}",
            output.sample_rate,
            output.channels,
            if frames == 0 {
                "(no callback within 500 ms)".to_owned()
            } else {
                format!(
                    "{frames} frames ({:.1} ms)",
                    frames as f64 * 1e3 / output.sample_rate as f64
                )
            },
        );
        Ok((setup, stream, name, buffer, feed))
    })();
    let (setup, mut stream, name, buffer, feed) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(Ready {
        writer_feed: feed,
        name,
        buffer,
        heartbeat: Arc::clone(&setup.heartbeat),
        health: Arc::clone(&setup.health),
    }));
    // Until the sink is dropped.
    while commands.recv().is_ok() {
        while commands.try_recv().is_ok() {}
        drop(stream);
        match start_stream(&setup) {
            Ok((new, _)) => {
                stream = new;
                setup.heartbeat.beat();
            }
            Err(e) => {
                setup
                    .health
                    .fail(format!("reopening the output after a stop: {e:#}"));
                return;
            }
        }
    }
}

/// Opens the stream and starts it, with a small fixed buffer when the device accepts one (the
/// sound already handed to the device cannot be taken back: the smaller the buffer the sooner a
/// stop is heard). Returns the stream and a description of the buffer asked for.
fn start_stream(setup: &Setup) -> anyhow::Result<(cpal::Stream, String)> {
    let mut config = setup.supported.config();
    if let cpal::SupportedBufferSize::Range { min, max } = setup.supported.buffer_size() {
        let wanted = (config.sample_rate * BUFFER_MS / 1000).clamp(*min, *max);
        config.buffer_size = cpal::BufferSize::Fixed(wanted);
        match build_and_play(setup, &config) {
            Ok(stream) => return Ok((stream, format!("fixed, {wanted} frames asked"))),
            Err(e) => warn!(
                "the device refuses a fixed buffer of {wanted} frames ({e:#}): default buffer"
            ),
        }
    }
    config.buffer_size = cpal::BufferSize::Default;
    let stream = build_and_play(setup, &config)?;
    Ok((stream, "default (no fixed size asked)".to_owned()))
}

fn build_and_play(setup: &Setup, config: &cpal::StreamConfig) -> anyhow::Result<cpal::Stream> {
    let stream = match setup.supported.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(setup, config),
        cpal::SampleFormat::I16 => build::<i16>(setup, config),
        cpal::SampleFormat::U16 => build::<u16>(setup, config),
        other => bail!("the output asks for samples of type {other}, which are not handled"),
    }?;
    stream.play().context("starting the output stream")?;
    Ok(stream)
}

/// Samples taken from the ring in one go in the callback (on the stack, nothing to allocate).
const BLOCK: usize = 1024;

fn build<T>(setup: &Setup, config: &cpal::StreamConfig) -> anyhow::Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let reader = Arc::clone(&setup.reader);
    let heartbeat = Arc::clone(&setup.heartbeat);
    let health = Arc::clone(&setup.health);
    let channels = config.channels as usize;
    setup
        .device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                heartbeat.beat();
                heartbeat
                    .frames
                    .store((data.len() / channels) as u32, Ordering::Relaxed);
                // The only other holder is the thread that reopens the stream, once this
                // callback is gone: the lock is never contended.
                let Ok(mut reader) = reader.try_lock() else {
                    data.fill(T::EQUILIBRIUM);
                    return;
                };
                let mut block = [0.0f32; BLOCK];
                for out in data.chunks_mut(BLOCK) {
                    let block = &mut block[..out.len()];
                    reader.pop(block);
                    for (slot, sample) in out.iter_mut().zip(block.iter()) {
                        *slot = T::from_sample(*sample);
                    }
                }
            },
            move |e| match e {
                // A glitch, not the end of the speaker.
                cpal::StreamError::BufferUnderrun => warn!("output stream: {e}"),
                e => health.fail(format!("output stream: {e}")),
            },
            None,
        )
        .context("building the output stream")
}

/// A WAV file instead of a speaker: it receives what a speaker would play, and at the pace a
/// speaker would play it, so a stop in the middle leaves a file that stops in the middle. The file
/// is created at the first sound, in the format of that sound (later sounds are converted to it);
/// the silences are not written. It stops writing, with a warning, at the 4 GB a WAV can hold.
pub struct WavSink {
    path: PathBuf,
    health: Arc<Health>,
    started: Option<Started>,
}

struct Started {
    feed: Feed,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WavSink {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            health: Arc::new(Health::default()),
            started: None,
        }
    }

    fn start(&mut self, format: Format) -> anyhow::Result<&mut Started> {
        let (feed, reader) = Feed::new(format);
        let file = File::create(&self.path)
            .with_context(|| format!("creating {}", self.path.display()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (stop, health) = (Arc::clone(&stop), Arc::clone(&self.health));
            std::thread::Builder::new()
                .name("wav".into())
                .spawn(move || {
                    if let Err(e) = write_wav(file, format, reader, &stop) {
                        health.fail(format!("writing the WAV: {e:#}"));
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
                    self.health.fail(format!("{e:#}"));
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

    fn take_played(&mut self, out: &mut Vec<Played>) {
        if let Some(started) = &mut self.started {
            started.feed.played.drain(out);
        }
    }

    fn failure(&self) -> Option<String> {
        self.health.get()
    }
}

impl Drop for WavSink {
    fn drop(&mut self) {
        if let Some(started) = &mut self.started {
            // Let what is queued be played (and written), as a speaker would.
            let thread = started.thread.take();
            while started.feed.writer.pending() > 0
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

/// The most a WAV can hold: sizes are 32 bits, and the header takes 36 of them.
const WAV_MAX_DATA: u64 = u32::MAX as u64 - 36;

fn wav_has_room(data_len: u64, more: u64) -> bool {
    data_len + more <= WAV_MAX_DATA
}

fn write_wav(
    mut file: File,
    format: Format,
    mut reader: Reader,
    stop: &AtomicBool,
) -> anyhow::Result<()> {
    let frames_per_tick = format.sample_rate as usize * TICK.as_millis() as usize / 1000;
    let mut samples = vec![0.0f32; frames_per_tick * format.channels as usize];
    let mut bytes: Vec<u8> = Vec::new();
    let mut data_len: u64 = 0;
    let mut full = false;
    file.write_all(&wav_header(format, 0))?;
    let mut next = Instant::now();
    loop {
        let real = reader.pop(&mut samples);
        if real > 0 && !full {
            if !wav_has_room(data_len, 2 * real as u64) {
                warn!("the WAV has reached the 4 GB limit: nothing more is written");
                full = true;
            } else {
                bytes.clear();
                for sample in &samples[..real] {
                    let s = (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                    bytes.extend(s.to_le_bytes());
                }
                file.write_all(&bytes)?;
                data_len += bytes.len() as u64;
                // Keep the sizes right as we go: a program that is killed still leaves a valid file.
                let end = file.stream_position()?;
                file.seek(SeekFrom::Start(0))?;
                file.write_all(&wav_header(format, data_len as u32))?;
                file.seek(SeekFrom::Start(end))?;
            }
        } else if real == 0 && stop.load(Ordering::Relaxed) {
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

    use crate::jitter::Mark;

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
        let mark = Mark {
            turn: contract::TurnId::new(),
            sentence: 3,
            received: Instant::now(),
        };
        let mut sink = WavSink::new(path.clone());
        sink.play(play(100, Some(mark)));
        std::thread::sleep(Duration::from_millis(50));
        let mut played = Vec::new();
        sink.take_played(&mut played);
        assert_eq!(
            played.iter().map(|(m, _)| m.sentence).collect::<Vec<_>>(),
            [3]
        );
        drop(sink); // waits for the 100 ms to be played
        // 100 ms at 24 kHz, 16 bits.
        assert_eq!(data_len(&path), 4800);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_flush_cuts_the_wav_where_the_speaker_would_have_stopped() {
        let path = temp_wav("flush");
        let mut sink = WavSink::new(path.clone());
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
    fn sound_after_a_flush_is_played() {
        let path = temp_wav("after-flush");
        let mut sink = WavSink::new(path.clone());
        sink.play(play(2000, None));
        std::thread::sleep(Duration::from_millis(100));
        sink.flush();
        sink.play(play(100, None));
        drop(sink);
        let written = data_len(&path) as f64 / 48_000.0;
        assert!((0.2..0.3).contains(&written), "{written} s written");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn no_sound_no_file() {
        let path = temp_wav("none");
        drop(WavSink::new(path.clone()));
        assert!(!path.exists());
    }

    #[test]
    fn a_wav_stops_before_its_sizes_overflow() {
        assert!(wav_has_room(0, 1_000_000));
        assert!(wav_has_room(WAV_MAX_DATA - 10, 10));
        assert!(!wav_has_room(WAV_MAX_DATA - 10, 11));
        assert!(!wav_has_room(u32::MAX as u64, 2));
        // And the header it would write stays right at the limit.
        let h = wav_header(
            Format {
                sample_rate: 24_000,
                channels: 1,
            },
            WAV_MAX_DATA as u32,
        );
        assert_eq!(u32::from_le_bytes(h[4..8].try_into().unwrap()), u32::MAX);
    }

    /// Needs a sound card; plays only silence. `cargo test -p mouth -- --ignored`.
    #[test]
    #[ignore]
    fn the_real_speaker_survives_a_flush_in_silence() {
        let mut sink = SpeakerSink::open("default").expect("the default speaker opens");
        let silence = |ms: usize| Play {
            format: Format {
                sample_rate: 24_000,
                channels: 1,
            },
            pcm: Bytes::from(vec![0u8; ms * 24 * 2]),
            mark: None,
        };
        for _ in 0..3 {
            sink.play(silence(500));
            std::thread::sleep(Duration::from_millis(100));
            let dropped = sink.flush();
            assert!(dropped > Duration::from_millis(300), "{dropped:?}");
            // The longest stretch without a callback while the stream is reopened.
            let mut longest = Duration::ZERO;
            for _ in 0..300 {
                std::thread::sleep(Duration::from_millis(1));
                longest = longest.max(sink.heartbeat.silence());
            }
            eprintln!("no callback for {longest:?} while the stream was reopened");
            assert_eq!(sink.failure(), None);
            assert!(
                sink.heartbeat.silence() < Duration::from_millis(100),
                "the callback runs again after the reopening"
            );
        }
        // A flush with nothing playing does not reopen anything.
        let before = sink.heartbeat.silence();
        sink.flush();
        assert!(before < Duration::from_millis(100));
    }
}
