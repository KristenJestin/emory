//! A WAV file in place of the microphone, for one turn: the test input of the whole chain.

use anyhow::{Context, Result, bail};
use contract::{State, TurnId, audio};
use tokio::time::{Instant, sleep_until};

use crate::bus::Bus;

/// The samples of a 16 kHz mono 16-bit WAV file.
fn read(path: &str) -> Result<Vec<i16>> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("opening {path}"))?;
    let spec = reader.spec();
    if spec.sample_rate != audio::MIC.sample_rate
        || spec.channels != audio::MIC.channels
        || spec.sample_format != hound::SampleFormat::Int
        || spec.bits_per_sample != 16
    {
        bail!(
            "{path}: {} Hz, {} channel, {} bits; 16 kHz mono 16-bit is needed \
             (ffmpeg -i input.wav -ar 16000 -ac 1 -c:a pcm_s16le output.wav)",
            spec.sample_rate,
            spec.channels,
            spec.bits_per_sample
        );
    }
    Ok(reader.samples::<i16>().collect::<Result<_, _>>()?)
}

/// Plays the file as one turn: start, the chunks at real-time pace, end.
pub async fn play(bus: &Bus, path: &str) -> Result<()> {
    let samples = read(path)?;
    let per_chunk = audio::MIC_CHUNK_BYTES / 2;
    let chunks = samples.len().div_ceil(per_chunk);
    let turn = TurnId::new();
    tracing::info!(%turn, chunks, "playing a file as one turn");

    bus.button(turn, State::Start).await?;
    let begin = Instant::now();
    for (seq, piece) in samples.chunks(per_chunk).enumerate() {
        sleep_until(begin + audio::MIC_CHUNK * seq as u32).await;
        let bytes = piece.iter().flat_map(|s| s.to_le_bytes()).collect();
        bus.audio(turn, seq as u64, bytes).await?;
    }
    sleep_until(begin + audio::MIC_CHUNK * chunks as u32).await;
    bus.button(turn, State::End).await?;
    tracing::info!(%turn, "turn ended");
    Ok(())
}
