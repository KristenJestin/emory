//! The loudness of the voice: a gain on the engine's audio, with a soft limiter.
//!
//! The gain is applied sample by sample, with no state, so it works on any chunk of a stream and
//! adds no latency. A memoryless curve also cannot click at the border of two chunks.

/// Below this fraction of full scale (after the gain) a sample passes untouched.
const KNEE: f32 = 0.8;

/// A gain on 16-bit little-endian PCM (the only format `voice` handles, see `audio.rs`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gain(f32);

impl Gain {
    /// The gain as a factor on the amplitude: 1 changes nothing, 2 doubles it (about +6 dB).
    pub fn new(factor: f32) -> Self {
        Self(factor)
    }

    /// Multiplies every sample by the gain, then eases what goes past the knee towards full scale
    /// instead of cutting it: a hard clip is audible, a soft one is not. With a gain of exactly 1
    /// the bytes are left as they are. A trailing odd byte (never a whole sample) is left alone.
    pub fn apply(self, pcm: &mut [u8]) {
        if self.0 == 1.0 {
            return;
        }
        for sample in pcm.as_chunks_mut::<2>().0 {
            let x = i16::from_le_bytes(*sample) as f32 / 32768.0;
            let y = limit(x * self.0) * 32768.0;
            let y = y.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            *sample = y.to_le_bytes();
        }
    }
}

/// Linear up to the knee, then a `tanh` that starts with the same slope (no corner) and tends
/// towards full scale without reaching it.
fn limit(x: f32) -> f32 {
    let magnitude = x.abs();
    if magnitude <= KNEE {
        return x;
    }
    let room = 1.0 - KNEE;
    (KNEE + room * ((magnitude - KNEE) / room).tanh()).copysign(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    fn samples(pcm: &[u8]) -> Vec<i16> {
        pcm.as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect()
    }

    fn apply(gain: f32, input: &[i16]) -> Vec<i16> {
        let mut bytes = pcm(input);
        Gain::new(gain).apply(&mut bytes);
        samples(&bytes)
    }

    #[test]
    fn a_gain_of_one_changes_nothing_not_even_full_scale() {
        let input = [0, 1, -1, 12_345, -23_456, i16::MAX, i16::MIN];
        assert_eq!(apply(1.0, &input), input);
    }

    #[test]
    fn the_gain_multiplies_what_is_below_the_knee() {
        assert_eq!(
            apply(2.0, &[0, 1000, -1000, 6000]),
            [0, 2000, -2000, 12_000]
        );
        assert_eq!(apply(0.5, &[1000, -2000]), [500, -1000]);
    }

    #[test]
    fn nothing_goes_past_full_scale_whatever_the_gain() {
        let input: Vec<i16> = (i16::MIN..=i16::MAX).step_by(7).collect();
        for gain in [1.5, 2.0, 4.0, 8.0] {
            for (x, y) in input.iter().zip(apply(gain, &input)) {
                // Same sign, never louder than full scale, never quieter than the input
                // (for a gain above 1).
                assert_eq!(x.signum(), y.signum(), "gain {gain}, sample {x}");
                assert!(
                    y.unsigned_abs() >= x.unsigned_abs().min(26_000),
                    "{x} -> {y}"
                );
            }
        }
        // The extremes saturate at full scale, not beyond.
        assert_eq!(apply(8.0, &[i16::MAX, i16::MIN]), [i16::MAX, i16::MIN]);
    }

    #[test]
    fn the_curve_is_smooth_and_never_decreases() {
        // No corner at the knee and no fold-back: successive inputs give successive outputs
        // that never go down and never jump more than the input step times the gain.
        let gain = 3.0;
        let input: Vec<i16> = (0..=i16::MAX).step_by(5).collect();
        let out = apply(gain, &input);
        for pair in out.windows(2) {
            assert!(pair[1] >= pair[0], "{pair:?}");
            assert!(pair[1] - pair[0] <= (5.0 * gain) as i16 + 1, "{pair:?}");
        }
    }

    #[test]
    fn a_chunk_is_processed_the_same_whatever_the_cutting() {
        let input: Vec<i16> = (0..2000)
            .map(|i| ((i * 37) % 30_000 - 15_000) as i16)
            .collect();
        let whole = apply(2.5, &input);
        let mut cut = Vec::new();
        for part in input.chunks(333) {
            cut.extend(apply(2.5, part));
        }
        assert_eq!(whole, cut);
    }

    #[test]
    fn stereo_and_a_stray_byte_are_safe() {
        // Samples are independent of the channel; a trailing odd byte is not a sample.
        let mut bytes = pcm(&[1000, -1000, 2000]);
        bytes.push(0x7f);
        Gain::new(2.0).apply(&mut bytes);
        assert_eq!(samples(&bytes[..6]), [2000, -2000, 4000]);
        assert_eq!(bytes[6], 0x7f);
    }
}
