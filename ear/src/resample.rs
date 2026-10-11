//! From the sound the microphone gives (any rate, any number of channels, float samples) to the
//! sound the bus carries (16 kHz mono 16-bit, in chunks of 20 ms).

use std::f64::consts::PI;

use contract::audio;

/// Half-width of the filter, in zero crossings of the low-pass at the output rate.
const CROSSINGS: f64 = 8.0;

/// A streaming band-limited resampler to the rate of the bus (windowed sinc), mono.
pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Cut-off as a fraction of the input Nyquist frequency (also the gain of the kernel).
    cutoff: f64,
    /// Half-width of the kernel, in input samples.
    half: f64,
    /// The input not yet used up.
    input: Vec<f32>,
    /// Position of the next output sample in `input`.
    position: f64,
}

impl Resampler {
    pub fn new(input_rate: u32) -> Self {
        let output_rate = audio::MIC.sample_rate as f64;
        let ratio = output_rate / input_rate as f64;
        // 5 % of margin below the output Nyquist frequency, never above the input one.
        let cutoff = (ratio * 0.95).min(1.0);
        Self {
            step: input_rate as f64 / output_rate,
            cutoff,
            half: (CROSSINGS / cutoff).ceil(),
            input: Vec::new(),
            position: 0.0,
        }
    }

    fn kernel(&self, t: f64) -> f64 {
        if t.abs() >= self.half {
            return 0.0;
        }
        let x = self.cutoff * t;
        let sinc = if x.abs() < 1e-9 {
            1.0
        } else {
            (PI * x).sin() / (PI * x)
        };
        let window = 0.5 * (1.0 + (PI * t / self.half).cos());
        self.cutoff * sinc * window
    }

    fn output(&self, centre: f64) -> f32 {
        let first = (centre - self.half).ceil() as i64;
        let last = (centre + self.half).floor() as i64;
        let mut sum = 0.0;
        for j in first.max(0)..=last {
            if let Some(&x) = self.input.get(j as usize) {
                sum += x as f64 * self.kernel(centre - j as f64);
            }
        }
        sum as f32
    }

    /// Resamples some input; the output goes to the end of `out`.
    pub fn process(&mut self, samples: &[f32], out: &mut Vec<f32>) {
        self.input.extend_from_slice(samples);
        // An output sample needs the input up to `half` after its centre.
        while self.position + self.half < self.input.len() as f64 {
            out.push(self.output(self.position));
            self.position += self.step;
        }
        let used = (self.position - self.half).floor();
        if used > 0.0 {
            self.input.drain(..used as usize);
            self.position -= used;
        }
    }

    /// The end of the stream: the last samples, as if silence followed.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        let silence = vec![0.0; self.half as usize + 1];
        self.process(&silence, out);
    }
}

/// Cuts a stream of samples into the chunks of the bus (20 ms of 16-bit little-endian).
#[derive(Default)]
pub struct Chunker {
    pending: Vec<u8>,
}

impl Chunker {
    pub fn push(&mut self, samples: &[f32], chunks: &mut Vec<Vec<u8>>) {
        for &s in samples {
            let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            self.pending.extend_from_slice(&v.to_le_bytes());
        }
        let whole = self.pending.len() / audio::MIC_CHUNK_BYTES * audio::MIC_CHUNK_BYTES;
        for chunk in self.pending[..whole].chunks(audio::MIC_CHUNK_BYTES) {
            chunks.push(chunk.to_vec());
        }
        self.pending.drain(..whole);
    }

    /// What is left, shorter than a chunk (`None` when nothing is).
    pub fn finish(&mut self) -> Option<Vec<u8>> {
        (!self.pending.is_empty()).then(|| std::mem::take(&mut self.pending))
    }
}

/// The whole path from the samples of the microphone to the chunks of the bus.
pub struct Pipeline {
    resampler: Option<Resampler>,
    chunker: Chunker,
    scratch: Vec<f32>,
}

impl Pipeline {
    /// For mono samples at `input_rate`.
    pub fn new(input_rate: u32) -> Self {
        Self {
            resampler: (input_rate != audio::MIC.sample_rate).then(|| Resampler::new(input_rate)),
            chunker: Chunker::default(),
            scratch: Vec::new(),
        }
    }

    pub fn push(&mut self, samples: &[f32], chunks: &mut Vec<Vec<u8>>) {
        match &mut self.resampler {
            Some(resampler) => {
                self.scratch.clear();
                resampler.process(samples, &mut self.scratch);
                self.chunker.push(&self.scratch, chunks);
            }
            None => self.chunker.push(samples, chunks),
        }
    }

    /// The end of the stream: the chunks that remain, the last one possibly shorter.
    pub fn finish(&mut self, chunks: &mut Vec<Vec<u8>>) {
        if let Some(resampler) = &mut self.resampler {
            self.scratch.clear();
            resampler.finish(&mut self.scratch);
            self.chunker.push(&self.scratch, chunks);
        }
        chunks.extend(self.chunker.finish());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f64, seconds: f64) -> Vec<f32> {
        (0..(rate as f64 * seconds) as usize)
            .map(|i| (2.0 * PI * hz * i as f64 / rate as f64).sin() as f32 * 0.5)
            .collect()
    }

    fn resample(rate: u32, input: &[f32]) -> Vec<f32> {
        let mut r = Resampler::new(rate);
        let mut out = Vec::new();
        // In odd-sized pieces, as a sound card gives them.
        for piece in input.chunks(441) {
            r.process(piece, &mut out);
        }
        r.finish(&mut out);
        out
    }

    /// Root mean square of the middle of the signal (away from the edges).
    fn rms(x: &[f32]) -> f64 {
        let middle = &x[x.len() / 4..x.len() * 3 / 4];
        (middle.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / middle.len() as f64).sqrt()
    }

    #[test]
    fn a_tone_of_the_speech_band_keeps_its_level_and_the_length_follows_the_rate() {
        for rate in [48_000, 44_100, 32_000, 8_000] {
            let input = sine(rate, 1000.0, 1.0);
            let out = resample(rate, &input);
            assert!(
                (out.len() as i64 - 16_000).abs() <= 2,
                "{rate}: {}",
                out.len()
            );
            let expected = 0.5 / 2f64.sqrt();
            assert!((rms(&out) - expected).abs() < 0.01, "{rate}: {}", rms(&out));
        }
    }

    #[test]
    fn what_the_output_cannot_carry_is_cut_not_folded() {
        // 10 kHz at 48 kHz would fold back to 6 kHz at 16 kHz without the filter.
        let out = resample(48_000, &sine(48_000, 10_000.0, 1.0));
        assert!(rms(&out) < 0.01, "{}", rms(&out));
    }

    #[test]
    fn the_frequency_is_right() {
        let out = resample(44_100, &sine(44_100, 2000.0, 1.0));
        let crossings = out.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((crossings as i64 - 2000).abs() <= 2, "{crossings}");
    }

    #[test]
    fn chunks_are_twenty_milliseconds_and_the_tail_is_kept() {
        let mut p = Pipeline::new(16_000);
        let mut chunks = Vec::new();
        // 50 ms: two whole chunks and a tail of 10 ms.
        p.push(&vec![0.25; 800], &mut chunks);
        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().all(|c| c.len() == audio::MIC_CHUNK_BYTES));
        p.finish(&mut chunks);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[2].len(), 320);
        // 0.25 of full scale, little-endian.
        assert_eq!(i16::from_le_bytes([chunks[0][0], chunks[0][1]]), 8192);
    }

    #[test]
    fn nothing_after_a_whole_number_of_chunks() {
        let mut p = Pipeline::new(16_000);
        let mut chunks = Vec::new();
        p.push(&vec![0.0; 320], &mut chunks);
        p.finish(&mut chunks);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn out_of_range_samples_are_clipped() {
        let mut chunker = Chunker::default();
        let mut chunks = Vec::new();
        chunker.push(&vec![3.0; 320], &mut chunks);
        assert_eq!(i16::from_le_bytes([chunks[0][0], chunks[0][1]]), i16::MAX);
    }
}
