//! Audio on the bus: the payload of a message is the raw sound (16-bit signed PCM, little-endian),
//! its information is in the NATS headers.

use std::{fmt, str::FromStr, time::Duration};

/// The headers of an audio message.
pub mod headers {
    /// The identifier of the turn.
    pub const TURN: &str = "Emory-Turn";
    /// The number of the chunk in the stream, starting at 0: a gap shows.
    pub const SEQ: &str = "Emory-Seq";
    /// The format of the audio ([`super::Format`]).
    pub const FORMAT: &str = "Emory-Format";
    /// On the way back only: the number of the sentence said.
    pub const SENTENCE: &str = "Emory-Sentence";
    /// On the way back only: `1` on the last chunk of a sentence.
    pub const END: &str = "Emory-End";
}

/// The format of an audio: 16-bit signed PCM, little-endian, at a sample rate and a number of
/// channels. Written `s16le;16000;1` in the [`headers::FORMAT`] header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub sample_rate: u32,
    pub channels: u16,
}

/// The audio from the ear to the hearing: 16 kHz mono, what the VAD and Parakeet ask for.
pub const MIC: Format = Format {
    sample_rate: 16_000,
    channels: 1,
};

/// The duration of a chunk of microphone audio.
pub const MIC_CHUNK: Duration = Duration::from_millis(20);

/// The size of a chunk of microphone audio, in bytes (20 ms at 16 kHz mono, 2 bytes per sample).
pub const MIC_CHUNK_BYTES: usize = 640;

/// The hearing closes by itself a turn that has received no audio for this long: an ear that dies
/// between the two presses will never send the end.
pub const SILENCE_CLOSES_TURN: Duration = Duration::from_secs(3);

const ENCODING: &str = "s16le";

impl Format {
    /// The number of bytes of a duration of audio in this format.
    pub fn bytes(&self, duration: Duration) -> usize {
        let samples = (duration.as_micros() * self.sample_rate as u128 / 1_000_000) as usize;
        samples * self.channels as usize * 2
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{ENCODING};{};{}", self.sample_rate, self.channels)
    }
}

/// A format the contract does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownFormat(pub String);

impl fmt::Display for UnknownFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown audio format: {:?}", self.0)
    }
}

impl std::error::Error for UnknownFormat {}

impl FromStr for Format {
    type Err = UnknownFormat;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let unknown = || UnknownFormat(s.to_owned());
        let mut parts = s.split(';');
        if parts.next() != Some(ENCODING) {
            return Err(unknown());
        }
        let sample_rate = parts
            .next()
            .and_then(|p| p.parse().ok())
            .ok_or_else(unknown)?;
        let channels = parts
            .next()
            .and_then(|p| p.parse().ok())
            .ok_or_else(unknown)?;
        if parts.next().is_some() || sample_rate == 0 || channels == 0 {
            return Err(unknown());
        }
        Ok(Self {
            sample_rate,
            channels,
        })
    }
}
