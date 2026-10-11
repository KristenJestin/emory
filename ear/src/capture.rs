//! The microphone, through cpal (ALSA, which PipeWire serves on a PC and which a Raspberry has
//! too), as chunks of the bus: 16 kHz mono 16-bit, 20 ms each.
//!
//! The microphone is open only while a [`Capture`] lives. cpal's stream cannot leave the thread
//! that builds it, so a thread owns it and the sound comes out through a channel.

use std::{collections::VecDeque, sync::mpsc as std_mpsc, thread};

use anyhow::{Context, Result, anyhow, bail};
use contract::audio;
use cpal::{
    FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use tokio::sync::mpsc;

use crate::resample::Pipeline;

/// A running capture of the microphone.
pub struct Capture {
    raw: mpsc::UnboundedReceiver<Vec<f32>>,
    pipeline: Pipeline,
    ready: VecDeque<Vec<u8>>,
    /// Dropping it tells the thread to close the microphone.
    stop: Option<std_mpsc::Sender<()>>,
    finished: bool,
}

impl Capture {
    /// Opens the microphone (`"default"` or a device name) and starts capturing.
    pub async fn open(mic: &str) -> Result<Self> {
        let mic = mic.to_owned();
        tokio::task::spawn_blocking(move || Self::open_blocking(&mic))
            .await
            .context("the capture thread")?
    }

    fn open_blocking(mic: &str) -> Result<Self> {
        let (raw_tx, raw) = mpsc::unbounded_channel();
        let (stop_tx, stop_rx) = std_mpsc::channel::<()>();
        let (ready_tx, ready_rx) = std_mpsc::channel();
        let mic = mic.to_owned();
        thread::Builder::new()
            .name("microphone".into())
            .spawn(move || {
                let stream = match build_stream(&mic, raw_tx) {
                    Ok((stream, rate)) => {
                        let _ = ready_tx.send(Ok(rate));
                        stream
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                // Until the capture is stopped or dropped.
                let _ = stop_rx.recv();
                drop(stream);
            })
            .context("starting the capture thread")?;
        let rate = ready_rx
            .recv()
            .context("the capture thread died while opening the microphone")??;
        Ok(Self {
            raw,
            pipeline: Pipeline::new(rate),
            ready: VecDeque::new(),
            stop: Some(stop_tx),
            finished: false,
        })
    }

    /// Closes the microphone. [`Capture::next_chunk`] then gives what remains, then `None`.
    pub fn stop(&mut self) {
        self.stop = None;
    }

    /// The next chunk of 20 ms; after [`Capture::stop`], the last one may be shorter.
    pub async fn next_chunk(&mut self) -> Option<Vec<u8>> {
        loop {
            if let Some(chunk) = self.ready.pop_front() {
                return Some(chunk);
            }
            if self.finished {
                return None;
            }
            let mut chunks = Vec::new();
            match self.raw.recv().await {
                Some(samples) => self.pipeline.push(&samples, &mut chunks),
                None => {
                    self.finished = true;
                    self.pipeline.finish(&mut chunks);
                }
            }
            self.ready.extend(chunks);
        }
    }
}

fn find_device(host: &cpal::Host, mic: &str) -> Result<cpal::Device> {
    if mic == "default" {
        return host
            .default_input_device()
            .context("no default input device");
    }
    for device in host.input_devices().context("listing the input devices")? {
        let matches = device.description().is_ok_and(|d| d.name() == mic)
            || device.id().is_ok_and(|id| id.to_string() == mic);
        if matches {
            return Ok(device);
        }
    }
    bail!("no input device named {mic:?} (`emory-ear --list-mics` lists them)")
}

/// The names of the input devices.
pub fn list_mics() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let mut names = Vec::new();
    for device in host.input_devices().context("listing the input devices")? {
        let description = device.description().map(|d| d.name().to_owned());
        let id = device.id().map(|id| id.to_string());
        names.push(match (description, id) {
            (Ok(name), Ok(id)) if name != id => format!("{name}  [{id}]"),
            (Ok(name), _) => name,
            (_, Ok(id)) => id,
            _ => "(unnamed)".to_owned(),
        });
    }
    Ok(names)
}

/// The configuration to ask the device for: the bus format as it is when the device offers it
/// (no resampling), else its default one.
fn choose_config(device: &cpal::Device) -> Result<(StreamConfig, SampleFormat)> {
    let wanted = audio::MIC.sample_rate;
    let direct = device
        .supported_input_configs()
        .ok()
        .into_iter()
        .flatten()
        .filter(|c| {
            c.channels() == audio::MIC.channels
                && (c.min_sample_rate()..=c.max_sample_rate()).contains(&wanted)
        })
        .min_by_key(|c| match c.sample_format() {
            SampleFormat::I16 => 0,
            SampleFormat::F32 => 1,
            _ => 2,
        })
        .and_then(|c| c.try_with_sample_rate(wanted))
        .filter(|c| supports(c.sample_format()));
    let config = match direct {
        Some(config) => config,
        None => device
            .default_input_config()
            .context("the default configuration of the microphone")?,
    };
    if !supports(config.sample_format()) {
        bail!(
            "the microphone gives {} samples, not handled",
            config.sample_format()
        );
    }
    let format = config.sample_format();
    Ok((config.into(), format))
}

fn supports(format: SampleFormat) -> bool {
    matches!(
        format,
        SampleFormat::I16
            | SampleFormat::I32
            | SampleFormat::F32
            | SampleFormat::F64
            | SampleFormat::U16
    )
}

fn build_stream(mic: &str, sink: mpsc::UnboundedSender<Vec<f32>>) -> Result<(Stream, u32)> {
    let host = cpal::default_host();
    let device = find_device(&host, mic)?;
    let (config, format) = choose_config(&device)?;
    let channels = config.channels as usize;
    let rate = config.sample_rate;
    tracing::info!(
        mic,
        rate,
        channels,
        format = %format,
        "opening the microphone"
    );
    let stream = match format {
        SampleFormat::I16 => stream::<i16>(&device, &config, channels, sink),
        SampleFormat::I32 => stream::<i32>(&device, &config, channels, sink),
        SampleFormat::F32 => stream::<f32>(&device, &config, channels, sink),
        SampleFormat::F64 => stream::<f64>(&device, &config, channels, sink),
        SampleFormat::U16 => stream::<u16>(&device, &config, channels, sink),
        other => Err(anyhow!("{other} samples are not handled")),
    }?;
    stream.play().context("starting the microphone")?;
    Ok((stream, rate))
}

fn stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    channels: usize,
    sink: mpsc::UnboundedSender<Vec<f32>>,
) -> Result<Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    device
        .build_input_stream(
            *config,
            move |data: &[T], _: &_| {
                // Mono: the mean of the channels.
                let mono = data
                    .chunks_exact(channels)
                    .map(|frame| {
                        frame.iter().map(|s| f32::from_sample(*s)).sum::<f32>() / channels as f32
                    })
                    .collect();
                // The receiver is gone when the turn is over.
                let _ = sink.send(mono);
            },
            |error| tracing::error!(%error, "microphone stream error"),
            None,
        )
        .context("opening the microphone stream")
}
