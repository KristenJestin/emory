//! The audio of the ear's messages: the headers and the samples.

use std::str::FromStr;

use anyhow::{Context, Result, bail};
use contract::{TurnId, audio};

/// What the headers of a chunk of ear audio say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioHeader {
    pub turn: TurnId,
    pub seq: u32,
}

/// Reads the headers of an audio message and checks that the format is the microphone's
/// ([`audio::MIC`]).
pub fn read_headers(headers: Option<&async_nats::HeaderMap>) -> Result<AudioHeader> {
    let headers = headers.context("audio message without headers")?;
    let value = |name: &str| {
        headers
            .get(name)
            .map(|v| v.as_str())
            .with_context(|| format!("header {name} missing"))
    };
    let format = audio::Format::from_str(value(audio::headers::FORMAT)?)?;
    if format != audio::MIC {
        bail!("format {format} instead of {}", audio::MIC);
    }
    Ok(AudioHeader {
        turn: value(audio::headers::TURN)?
            .parse()
            .context("header Emory-Turn")?,
        seq: value(audio::headers::SEQ)?
            .parse()
            .context("header Emory-Seq")?,
    })
}

/// The samples of a chunk of 16-bit little-endian PCM, between -1 and 1.
pub fn samples(bytes: &[u8]) -> Result<Vec<f32>> {
    if !bytes.len().is_multiple_of(2) {
        bail!("{} bytes: not a whole number of samples", bytes.len());
    }
    Ok(bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b) as f32 / 32768.0)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(format: &str, seq: &str) -> async_nats::HeaderMap {
        let mut h = async_nats::HeaderMap::new();
        h.insert(audio::headers::TURN, TurnId::new().to_string());
        h.insert(audio::headers::SEQ, seq);
        h.insert(audio::headers::FORMAT, format);
        h
    }

    #[test]
    fn the_microphone_headers_are_read() {
        let read = read_headers(Some(&headers(&audio::MIC.to_string(), "7"))).unwrap();
        assert_eq!(read.seq, 7);
    }

    #[test]
    fn another_format_and_a_missing_or_wrong_header_are_refused() {
        assert!(read_headers(Some(&headers("s16le;24000;1", "0"))).is_err());
        assert!(read_headers(Some(&headers("anything", "0"))).is_err());
        assert!(read_headers(Some(&headers("s16le;16000;1", "x"))).is_err());
        assert!(read_headers(Some(&async_nats::HeaderMap::new())).is_err());
        assert!(read_headers(None).is_err());
    }

    #[test]
    fn the_samples_are_decoded_as_little_endian() {
        let bytes = [0x00, 0x00, 0x00, 0x40, 0x00, 0x80, 0xff, 0x7f];
        let s = samples(&bytes).unwrap();
        assert_eq!(s[0], 0.0);
        assert_eq!(s[1], 0.5);
        assert_eq!(s[2], -1.0);
        assert!((s[3] - 0.99997).abs() < 1e-4);
        assert!(samples(&[0, 0, 0]).is_err());
        assert!(samples(&[]).unwrap().is_empty());
    }
}
