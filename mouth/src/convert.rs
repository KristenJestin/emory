//! From the PCM of the bus (16-bit signed, little-endian) to what the output accepts: `f32`
//! samples at its sample rate and number of channels.
//!
//! The resampling is a linear interpolation, kept across chunks so there is no click at their
//! seams. It is good enough for speech from 24 kHz to the 44.1 or 48 kHz an output asks for;
//! it is not a studio resampler.

use contract::audio::Format;

/// Converts the chunks of a stream to one output format.
pub struct Converter {
    channels: u16,
    sample_rate: u32,
    source: Option<Format>,
    resampler: Resampler,
}

impl Converter {
    pub fn new(output: Format) -> Self {
        Self {
            channels: output.channels,
            sample_rate: output.sample_rate,
            source: None,
            resampler: Resampler::new(1, output.sample_rate),
        }
    }

    pub fn output(&self) -> Format {
        Format {
            sample_rate: self.sample_rate,
            channels: self.channels,
        }
    }

    /// Forgets the state between chunks (after a flush the next sound starts afresh).
    pub fn reset(&mut self) {
        self.resampler.reset();
    }

    /// Converts a chunk to interleaved `f32` samples in the output format.
    pub fn convert(&mut self, format: Format, pcm: &[u8]) -> Vec<f32> {
        if self.source != Some(format) {
            self.source = Some(format);
            self.resampler = Resampler::new(format.sample_rate, self.sample_rate);
        }
        let channels = format.channels as usize;
        let frame = 2 * channels;
        // The mono mix: the average of the channels.
        let mono: Vec<f32> = pcm
            .chunks_exact(frame)
            .map(|frame| {
                let sum: f32 = frame
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|s| i16::from_le_bytes(*s) as f32 / 32768.0)
                    .sum();
                sum / channels as f32
            })
            .collect();
        let mut resampled = Vec::with_capacity(mono.len() * self.sample_rate as usize / 8000 + 4);
        self.resampler.process(&mono, &mut resampled);
        if self.channels == 1 {
            return resampled;
        }
        resampled
            .into_iter()
            .flat_map(|s| std::iter::repeat_n(s, self.channels as usize))
            .collect()
    }
}

/// A streaming linear resampler for one channel.
struct Resampler {
    /// Input samples per output sample.
    ratio: f64,
    /// Position of the next output sample, in input samples, counted from `previous`.
    position: f64,
    /// The last sample of the previous chunk (the interpolation needs it at the seam).
    previous: f32,
}

impl Resampler {
    fn new(from: u32, to: u32) -> Self {
        Self {
            ratio: from as f64 / to as f64,
            position: 0.0,
            previous: 0.0,
        }
    }

    fn reset(&mut self) {
        self.position = 0.0;
        self.previous = 0.0;
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let Some(&last) = input.last() else { return };
        if self.ratio == 1.0 {
            out.extend_from_slice(input);
            return;
        }
        let len = input.len() as f64;
        while self.position < len {
            let i = self.position.floor() as usize;
            let fraction = (self.position - i as f64) as f32;
            // The signal is `previous` followed by `input`: `a` is at index `i`, `b` just after.
            let a = if i == 0 { self.previous } else { input[i - 1] };
            let b = input[i];
            out.push(a + (b - a) * fraction);
            self.position += self.ratio;
        }
        self.position -= len;
        self.previous = last;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    fn format(sample_rate: u32, channels: u16) -> Format {
        Format {
            sample_rate,
            channels,
        }
    }

    #[test]
    fn same_rate_same_channels_only_changes_the_sample_type() {
        let mut c = Converter::new(format(24_000, 1));
        let out = c.convert(format(24_000, 1), &pcm(&[0, 16384, -16384, 32767]));
        assert_eq!(out.len(), 4);
        assert_eq!(out[0], 0.0);
        assert!((out[1] - 0.5).abs() < 1e-6 && (out[2] + 0.5).abs() < 1e-6);
    }

    #[test]
    fn upsampling_doubles_the_number_of_samples() {
        let mut c = Converter::new(format(48_000, 1));
        let out = c.convert(format(24_000, 1), &pcm(&[1000; 240]));
        assert_eq!(out.len(), 480);
    }

    #[test]
    fn the_length_is_right_over_many_chunks() {
        // 24 kHz to 44.1 kHz in 60 ms chunks: no drift.
        let mut c = Converter::new(format(44_100, 1));
        let chunk = pcm(&[500; 1440]);
        let total: usize = (0..100)
            .map(|_| c.convert(format(24_000, 1), &chunk).len())
            .sum();
        let expected = 100 * 1440 * 44_100 / 24_000;
        assert!(total.abs_diff(expected) <= 2, "{total} vs {expected}");
    }

    #[test]
    fn chunks_are_joined_without_a_jump() {
        // A ramp cut in two chunks must come out as the same smooth ramp.
        let ramp: Vec<i16> = (0..480).map(|i| i * 50).collect();
        let mut whole = Converter::new(format(48_000, 1));
        let expected = whole.convert(format(24_000, 1), &pcm(&ramp));
        let mut cut = Converter::new(format(48_000, 1));
        let mut got = cut.convert(format(24_000, 1), &pcm(&ramp[..200]));
        got.extend(cut.convert(format(24_000, 1), &pcm(&ramp[200..])));
        assert_eq!(got.len(), expected.len());
        for (a, b) in got.iter().zip(&expected) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn mono_is_copied_to_every_output_channel() {
        let mut c = Converter::new(format(24_000, 2));
        let out = c.convert(format(24_000, 1), &pcm(&[0, 16384]));
        assert_eq!(out.len(), 4);
        assert_eq!(out[2], out[3]);
    }

    #[test]
    fn stereo_is_mixed_down_to_mono() {
        let mut c = Converter::new(format(24_000, 1));
        let out = c.convert(format(24_000, 2), &pcm(&[0, 0, 16384, 0]));
        assert_eq!(out.len(), 2);
        // The left channel at half scale, the right one silent: a quarter of the scale.
        assert!((out[1] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn a_change_of_source_format_is_followed() {
        let mut c = Converter::new(format(48_000, 1));
        assert_eq!(c.convert(format(24_000, 1), &pcm(&[1; 240])).len(), 480);
        assert_eq!(c.convert(format(48_000, 1), &pcm(&[1; 240])).len(), 240);
    }

    #[test]
    fn a_trailing_odd_byte_is_ignored() {
        let mut c = Converter::new(format(24_000, 1));
        let mut bytes = pcm(&[1, 2, 3]);
        bytes.push(7);
        assert_eq!(c.convert(format(24_000, 1), &bytes).len(), 3);
    }
}
