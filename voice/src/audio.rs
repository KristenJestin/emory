//! The audio between the engine and the bus: the WAV header of the engine stream, the cutting
//! into chunks and the NATS headers of a chunk.

use std::time::Duration;

use async_nats::HeaderMap;
use contract::{TurnId, audio::Format, audio::headers as audio_headers};

/// What the WAV header of the engine stream says.
#[derive(Debug, PartialEq, Eq)]
pub enum WavHeader {
    /// Not enough bytes to decide.
    Incomplete,
    /// The format, and the number of bytes of the header (the sound starts right after).
    Ready { format: Format, data_offset: usize },
}

/// Reads the header of a 16-bit PCM WAV, without relying on its announced length (the engine
/// streams: the size is wrong or huge).
pub fn parse_wav_header(bytes: &[u8]) -> Result<WavHeader, String> {
    if bytes.len() < 12 {
        return Ok(WavHeader::Incomplete);
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("the engine did not answer a WAV".to_owned());
    }
    let mut format = None;
    let mut at = 12usize;
    loop {
        if bytes.len() < at.saturating_add(8) {
            return Ok(WavHeader::Incomplete);
        }
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        if id == b"data" {
            let format = format.ok_or("WAV without a fmt block before the sound")?;
            return Ok(WavHeader::Ready {
                format,
                data_offset: at + 8,
            });
        }
        let body = at + 8;
        // A block is padded to an even size.
        let end = body.saturating_add(size.saturating_add(size % 2));
        if id == b"fmt " {
            if size < 16 {
                return Err(format!("WAV fmt block of {size} bytes, 16 are needed"));
            }
            if bytes.len() < body + 16 {
                return Ok(WavHeader::Incomplete);
            }
            let field =
                |o: usize| u16::from_le_bytes(bytes[body + o..body + o + 2].try_into().unwrap());
            let (tag, channels, bits) = (field(0), field(2), field(14));
            let sample_rate = u32::from_le_bytes(bytes[body + 4..body + 8].try_into().unwrap());
            if tag != 1 || bits != 16 || channels == 0 || sample_rate == 0 {
                return Err(format!(
                    "unsupported WAV (format {tag}, {bits} bits, {channels} channels, {sample_rate} Hz)"
                ));
            }
            format = Some(Format {
                sample_rate,
                channels,
            });
        }
        at = end;
    }
}

/// Cuts the audio arriving in irregular packets into chunks of a fixed size, keeping the last
/// one back: the chunk that ends a sentence must carry `Emory-End`, and we only know it is the
/// last one at the end of the stream.
///
/// A full chunk is returned at once if there is sound left behind it (so it is not the last one);
/// otherwise it is held until the next packet or until the end. The first sound thus leaves
/// without waiting, except when the stream falls exactly on the size of a chunk.
pub struct Chunker {
    size: usize,
    buffer: Vec<u8>,
    held: Option<Vec<u8>>,
}

impl Chunker {
    /// `size` in bytes, rounded to a whole number of samples.
    pub fn new(size: usize, bytes_per_frame: usize) -> Self {
        let size = (size / bytes_per_frame).max(1) * bytes_per_frame;
        Self {
            size,
            buffer: Vec::new(),
            held: None,
        }
    }

    /// The chunks to send now (none of them is the last of the sentence).
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();
        while self.buffer.len() >= self.size {
            let chunk: Vec<u8> = self.buffer.drain(..self.size).collect();
            if let Some(previous) = self.held.replace(chunk) {
                out.push(previous);
            }
            if !self.buffer.is_empty() {
                out.extend(self.held.take());
            }
        }
        out
    }

    /// The end of the stream: the remaining chunks, the last one is the one that carries
    /// `Emory-End`. Empty if the engine returned nothing.
    pub fn finish(mut self) -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = self.held.take().into_iter().collect();
        if !self.buffer.is_empty() {
            out.push(self.buffer);
        }
        out
    }
}

/// The duration of the sound of this number of bytes.
pub fn duration_of(bytes: usize, format: Format) -> Duration {
    let frames = bytes as u64 / (2 * format.channels as u64);
    Duration::from_micros(frames * 1_000_000 / format.sample_rate as u64)
}

/// The headers of a chunk of the voice going back to the mouth.
pub fn chunk_headers(
    turn: TurnId,
    seq: u32,
    format: Format,
    sentence: u32,
    last: bool,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(audio_headers::TURN, turn.to_string());
    headers.insert(audio_headers::SEQ, seq.to_string());
    headers.insert(audio_headers::FORMAT, format.to_string());
    headers.insert(audio_headers::SENTENCE, sentence.to_string());
    if last {
        headers.insert(audio_headers::END, "1");
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_header(sample_rate: u32, bits: u16) -> Vec<u8> {
        let mut h = b"RIFF".to_vec();
        h.extend(0xFFFF_FFFFu32.to_le_bytes());
        h.extend(b"WAVEfmt ");
        h.extend(16u32.to_le_bytes());
        h.extend(1u16.to_le_bytes());
        h.extend(1u16.to_le_bytes());
        h.extend(sample_rate.to_le_bytes());
        h.extend((sample_rate * 2).to_le_bytes());
        h.extend(2u16.to_le_bytes());
        h.extend(bits.to_le_bytes());
        h.extend(b"data");
        h.extend(1_000_000_000u32.to_le_bytes());
        h
    }

    #[test]
    fn reads_the_streaming_header_of_the_engine() {
        let h = wav_header(24_000, 16);
        assert_eq!(h.len(), 44);
        assert_eq!(
            parse_wav_header(&h),
            Ok(WavHeader::Ready {
                format: Format {
                    sample_rate: 24_000,
                    channels: 1
                },
                data_offset: 44
            })
        );
    }

    #[test]
    fn a_short_header_is_incomplete_not_an_error() {
        let h = wav_header(24_000, 16);
        for n in [0, 11, 20, 35, 43] {
            assert_eq!(parse_wav_header(&h[..n]), Ok(WavHeader::Incomplete), "{n}");
        }
    }

    #[test]
    fn skips_unknown_blocks_and_refuses_what_it_cannot_play() {
        let mut h = wav_header(24_000, 16);
        let data = h.split_off(36);
        h.extend(b"LIST");
        h.extend(3u32.to_le_bytes());
        h.extend([1, 2, 3, 0]); // odd size: padded to even
        h.extend(data);
        assert!(matches!(
            parse_wav_header(&h),
            Ok(WavHeader::Ready {
                data_offset: 56,
                ..
            })
        ));
        assert!(parse_wav_header(&wav_header(24_000, 8)).is_err());
        assert!(parse_wav_header(b"<html>not a wav at all</html>").is_err());
    }

    #[test]
    fn a_fmt_block_shorter_than_sixteen_bytes_is_an_error() {
        // A fmt block of 8 bytes, followed by what would be read as its missing fields.
        let mut h = b"RIFF".to_vec();
        h.extend(0xFFFF_FFFFu32.to_le_bytes());
        h.extend(b"WAVEfmt ");
        h.extend(8u32.to_le_bytes());
        h.extend([0u8; 8]);
        h.extend(b"data");
        h.extend(1_000u32.to_le_bytes());
        h.extend([0u8; 64]);
        assert!(parse_wav_header(&h).is_err());
        // The same with nothing after the short block yet: still an error, not "incomplete".
        assert!(parse_wav_header(&h[..28]).is_err());
    }

    #[test]
    fn a_huge_block_size_does_not_overflow() {
        let mut h = wav_header(24_000, 16);
        let data = h.split_off(36);
        h.extend(b"LIST");
        h.extend(u32::MAX.to_le_bytes());
        h.extend(data);
        // The block is declared 4 GiB long: we simply have not seen the end of it.
        assert_eq!(parse_wav_header(&h), Ok(WavHeader::Incomplete));
    }

    fn feed(chunker: &mut Chunker, pieces: &[usize]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut n = 0u8;
        for len in pieces {
            let piece: Vec<u8> = (0..*len)
                .map(|_| {
                    n = n.wrapping_add(1);
                    n
                })
                .collect();
            out.extend(chunker.push(&piece));
        }
        out
    }

    #[test]
    fn chunks_have_a_fixed_size_and_nothing_is_lost() {
        let mut c = Chunker::new(10, 2);
        let mut all = feed(&mut c, &[7, 3, 25, 1, 9]); // 45 bytes
        all.extend(c.finish());
        assert!(all[..all.len() - 1].iter().all(|x| x.len() == 10));
        assert_eq!(all.last().unwrap().len(), 5);
        let flat: Vec<u8> = all.concat();
        assert_eq!(flat, (1..=45).collect::<Vec<u8>>());
    }

    #[test]
    fn the_last_chunk_is_held_back_when_the_stream_ends_on_a_boundary() {
        let mut c = Chunker::new(10, 2);
        let sent = feed(&mut c, &[10, 10]);
        assert_eq!(sent.len(), 1, "the second full chunk could be the last one");
        let rest = c.finish();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].len(), 10);
    }

    #[test]
    fn a_chunk_followed_by_more_sound_goes_out_at_once() {
        let mut c = Chunker::new(10, 2);
        assert_eq!(feed(&mut c, &[12]).len(), 1);
    }

    #[test]
    fn no_sound_gives_no_chunk() {
        assert!(Chunker::new(10, 2).finish().is_empty());
    }

    #[test]
    fn chunk_size_is_a_whole_number_of_samples() {
        let mut c = Chunker::new(2879, 2);
        let out = feed(&mut c, &[6000]);
        assert_eq!(out[0].len(), 2878);
    }

    #[test]
    fn durations() {
        let f = Format {
            sample_rate: 24_000,
            channels: 1,
        };
        assert_eq!(duration_of(2880, f), Duration::from_millis(60));
    }

    #[test]
    fn headers_follow_the_contract() {
        let turn = TurnId::new();
        let f = Format {
            sample_rate: 24_000,
            channels: 1,
        };
        let h = chunk_headers(turn, 12, f, 3, false);
        assert_eq!(
            h.get(audio_headers::TURN).unwrap().as_str(),
            turn.to_string()
        );
        assert_eq!(h.get(audio_headers::SEQ).unwrap().as_str(), "12");
        assert_eq!(
            h.get(audio_headers::FORMAT).unwrap().as_str(),
            "s16le;24000;1"
        );
        assert_eq!(h.get(audio_headers::SENTENCE).unwrap().as_str(), "3");
        assert!(h.get(audio_headers::END).is_none());
        let h = chunk_headers(turn, 13, f, 3, true);
        assert_eq!(h.get(audio_headers::END).unwrap().as_str(), "1");
    }
}
