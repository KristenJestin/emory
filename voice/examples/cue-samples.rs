//! Writes the sounds of the waiting cue to WAV files, to listen to them without the chain: the
//! tones (alone, and followed by the pulse), and phrases spoken by the voice engine.
//!
//! ```text
//! cargo run -p voice --example cue-samples -- OUTPUT_DIR \
//!     [--volume 0.3] [--engine-url http://127.0.0.1:8000] [--voice estelle] \
//!     [--speech NAME=TEXT]...
//! ```
//!
//! Each `--speech` gives `NAME.wav`; a `+` in TEXT joins phrases that follow each other with a
//! short pause (a waiting phrase, then the answer). Without `--speech`, only the tones are written.
//! Nothing is played.

use std::{path::Path, time::Duration};

use anyhow::{Context, bail};
use voice::{
    cue::{self, Tone},
    engine::{Engine, Request},
};

/// The wait before the pulse starts and the time between two pulses, as in the default settings.
const PULSE_DELAY_MS: usize = 1500;
const PULSE_EVERY_MS: usize = 1600;
/// How long the pulse goes on in the sample.
const PULSE_SPAN_MS: usize = 3000;

fn samples_of(ms: usize) -> usize {
    cue::FORMAT.sample_rate as usize * ms / 1000
}

fn write_wav(path: &Path, rate: u32, pcm: &[u8]) -> anyhow::Result<()> {
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(pcm);
    std::fs::write(path, wav).with_context(|| format!("writing {}", path.display()))
}

/// Mixes `sound` into `into` at `at_ms`, growing it if needed.
fn lay(into: &mut Vec<u8>, at_ms: usize, sound: &[u8]) {
    let start = samples_of(at_ms) * 2;
    if into.len() < start + sound.len() {
        into.resize(start + sound.len(), 0);
    }
    into[start..start + sound.len()].copy_from_slice(sound);
}

/// The tone, then the pulse as the voice sends it when the answer is late.
fn tone_then_pulse(style: Tone, volume: f32) -> Vec<u8> {
    let mut timeline = Vec::new();
    lay(&mut timeline, 0, &cue::tone(style, volume));
    let pulse = cue::pulse(volume);
    let mut at = PULSE_DELAY_MS;
    while at < PULSE_DELAY_MS + PULSE_SPAN_MS {
        lay(&mut timeline, at, &pulse);
        at += PULSE_EVERY_MS;
    }
    let end = samples_of(PULSE_DELAY_MS + PULSE_SPAN_MS) * 2;
    timeline.resize(timeline.len().max(end), 0);
    timeline
}

async fn speak(engine: &Engine, text: &str) -> anyhow::Result<(u32, Vec<u8>)> {
    let mut speech = engine.speak(text).await?;
    let rate = speech.format.sample_rate;
    let mut pcm = speech.first.to_vec();
    while let Some(packet) = speech.next_packet().await? {
        pcm.extend_from_slice(&packet);
    }
    Ok((rate, pcm))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let out = args
        .next()
        .context("usage: cue-samples OUTPUT_DIR [options]")?;
    let (mut volume, mut url, mut voice) = (
        0.3_f32,
        "http://127.0.0.1:8000".to_owned(),
        "estelle".to_owned(),
    );
    let mut speeches = Vec::new();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().with_context(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--volume" => volume = value("--volume")?.parse()?,
            "--engine-url" => url = value("--engine-url")?,
            "--voice" => voice = value("--voice")?,
            "--speech" => {
                let spec = value("--speech")?;
                let (name, text) = spec.split_once('=').context("--speech wants NAME=TEXT")?;
                speeches.push((name.to_owned(), text.to_owned()));
            }
            other => bail!("unknown option {other}"),
        }
    }
    let out = Path::new(&out);
    std::fs::create_dir_all(out)?;

    for (name, style) in [
        ("rising", Tone::Rising),
        ("bright", Tone::Bright),
        ("warm", Tone::Warm),
        ("single", Tone::Single),
    ] {
        write_wav(
            &out.join(format!("ton-{name}-seul.wav")),
            cue::FORMAT.sample_rate,
            &cue::tone(style, volume),
        )?;
        write_wav(
            &out.join(format!("ton-{name}-puis-pouls.wav")),
            cue::FORMAT.sample_rate,
            &tone_then_pulse(style, volume),
        )?;
    }

    if !speeches.is_empty() {
        let engine = Engine::new(&url, Request::Pocket { voice }, Duration::from_secs(20), 1)?;
        engine.warm_up().await.context("the voice engine")?;
        for (name, text) in speeches {
            let mut all = Vec::new();
            let mut rate = 0;
            for (i, part) in text.split('+').enumerate() {
                let (r, pcm) = speak(&engine, part.trim()).await?;
                rate = r;
                if i > 0 {
                    all.extend(vec![0; (r as usize * 300 / 1000) * 2]);
                }
                all.extend(pcm);
            }
            write_wav(&out.join(format!("{name}.wav")), rate, &all)?;
        }
    }
    Ok(())
}
