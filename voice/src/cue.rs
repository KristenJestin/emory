//! The sounds of the waiting cue, drawn by code: sines under an envelope, no audio file. A tone
//! of two rising notes tells the owner that the turn was heard; a soft pulse fills the wait that
//! follows. Both are 16-bit mono PCM at [`FORMAT`], like the voice.

use contract::audio::Format;
use serde::Deserialize;

/// The format of the cue sounds: that of the voice engines, which the mouth plays already.
pub const FORMAT: Format = Format {
    sample_rate: 24_000,
    channels: 1,
};

/// The `Emory-Sentence` header of a cue chunk: no sentence of the answer has this number.
pub const SENTENCE: u32 = u32::MAX;

/// The shapes of the tone, to try one after the other (`[cue] tone`).
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    /// Two rising notes a fourth apart, the default.
    #[default]
    Rising,
    /// The same, an octave-ish higher and shorter: brighter.
    Bright,
    /// Two lower, rounder notes: warmer.
    Warm,
    /// One note alone.
    Single,
}

/// A note: its frequency, its length, and the time its envelope takes to fall to a third of its
/// peak (the shorter, the drier).
struct Note {
    hz: f32,
    ms: u32,
    fall_ms: f32,
}

impl Tone {
    fn notes(self) -> &'static [Note] {
        match self {
            Self::Rising => &[
                Note {
                    hz: 660.0,
                    ms: 130,
                    fall_ms: 45.0,
                },
                Note {
                    hz: 880.0,
                    ms: 190,
                    fall_ms: 70.0,
                },
            ],
            Self::Bright => &[
                Note {
                    hz: 988.0,
                    ms: 100,
                    fall_ms: 35.0,
                },
                Note {
                    hz: 1319.0,
                    ms: 150,
                    fall_ms: 55.0,
                },
            ],
            Self::Warm => &[
                Note {
                    hz: 440.0,
                    ms: 150,
                    fall_ms: 60.0,
                },
                Note {
                    hz: 587.0,
                    ms: 220,
                    fall_ms: 90.0,
                },
            ],
            Self::Single => &[Note {
                hz: 784.0,
                ms: 220,
                fall_ms: 80.0,
            }],
        }
    }
}

/// The silence between the notes of a tone.
const NOTE_GAP_MS: u32 = 25;
/// How fast a note comes in and goes out, so that it does not click.
const EDGE_MS: f32 = 6.0;
/// The pulse: one soft low note.
const PULSE_HZ: f32 = 523.0;
const PULSE_MS: u32 = 160;
/// A pulse is quieter than the tone: it is heard for a while.
const PULSE_LEVEL: f32 = 0.45;

/// The tone, as PCM at `volume` (0 to 1: the peak of the sound, as a fraction of the maximum).
pub fn tone(style: Tone, volume: f32) -> Vec<u8> {
    let mut samples: Vec<f32> = Vec::new();
    for (i, note) in style.notes().iter().enumerate() {
        if i > 0 {
            samples.extend(silence(NOTE_GAP_MS));
        }
        samples.extend(note_samples(note.hz, note.ms, note.fall_ms));
    }
    to_pcm(&samples, volume)
}

/// One pulse, as PCM at `volume`.
pub fn pulse(volume: f32) -> Vec<u8> {
    let n = count(PULSE_MS);
    let samples: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / n as f32;
            // A raised cosine: it swells and fades without an edge.
            let envelope = 0.5 - 0.5 * (std::f32::consts::TAU * t).cos();
            envelope * sine(PULSE_HZ, i)
        })
        .collect();
    to_pcm(&samples, volume * PULSE_LEVEL)
}

/// The length of a sound in samples.
fn count(ms: u32) -> usize {
    (FORMAT.sample_rate as u64 * ms as u64 / 1000) as usize
}

fn silence(ms: u32) -> impl Iterator<Item = f32> {
    std::iter::repeat_n(0.0, count(ms))
}

fn sine(hz: f32, i: usize) -> f32 {
    (std::f32::consts::TAU * hz * i as f32 / FORMAT.sample_rate as f32).sin()
}

/// A note: a sine and a faint second harmonic (a little bell), a fast attack, an exponential fall.
fn note_samples(hz: f32, ms: u32, fall_ms: f32) -> Vec<f32> {
    let rate = FORMAT.sample_rate as f32;
    let n = count(ms);
    let edge = (EDGE_MS * rate / 1000.0) as usize;
    (0..n)
        .map(|i| {
            let seconds = i as f32 / rate;
            let mut envelope = (-seconds / (fall_ms / 1000.0)).exp();
            if i < edge {
                envelope *= i as f32 / edge as f32;
            }
            if i + edge > n {
                envelope *= (n - i) as f32 / edge as f32;
            }
            envelope * (sine(hz, i) + 0.18 * sine(2.0 * hz, i)) / 1.18
        })
        .collect()
}

/// The samples, scaled so that the loudest is `volume` of the maximum, as s16le.
fn to_pcm(samples: &[f32], volume: f32) -> Vec<u8> {
    let peak = samples.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    let scale = if peak > 0.0 {
        volume.clamp(0.0, 1.0) * f32::from(i16::MAX) / peak
    } else {
        0.0
    };
    samples
        .iter()
        .flat_map(|s| ((s * scale).round() as i16).to_le_bytes())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(pcm: &[u8]) -> Vec<i16> {
        pcm.chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect()
    }

    fn peak(pcm: &[u8]) -> i16 {
        samples(pcm)
            .iter()
            .map(|s| s.saturating_abs())
            .max()
            .unwrap()
    }

    #[test]
    fn the_peak_follows_the_volume() {
        let loud = peak(&tone(Tone::Rising, 0.5));
        let soft = peak(&tone(Tone::Rising, 0.25));
        assert!((loud as i32 - 16_383).abs() < 4, "{loud}");
        assert!((soft as i32 - 8_191).abs() < 4, "{soft}");
        assert!(tone(Tone::Rising, 0.0).chunks(2).all(|b| b == [0, 0]));
        // A volume above 1 does not clip.
        assert_eq!(peak(&tone(Tone::Rising, 3.0)), i16::MAX);
    }

    #[test]
    fn a_tone_lasts_its_notes_and_its_gap_and_a_pulse_is_short_and_quiet() {
        for style in [Tone::Rising, Tone::Bright, Tone::Warm, Tone::Single] {
            let total: u32 = style.notes().iter().map(|n| n.ms).sum::<u32>()
                + NOTE_GAP_MS * (style.notes().len() as u32 - 1);
            assert_eq!(samples(&tone(style, 0.3)).len(), count(total), "{style:?}");
            assert!(total <= 400, "a tone stays short: {style:?}");
        }
        assert_eq!(samples(&pulse(0.3)).len(), count(PULSE_MS));
        assert!(peak(&pulse(0.3)) < peak(&tone(Tone::Rising, 0.3)));
    }

    #[test]
    fn the_sounds_start_and_end_without_a_click() {
        for pcm in [tone(Tone::Rising, 1.0), tone(Tone::Single, 1.0), pulse(1.0)] {
            let s = samples(&pcm);
            let limit = i16::MAX / 20;
            assert!(s.first().unwrap().abs() < limit && s.last().unwrap().abs() < limit);
        }
    }

    #[test]
    fn the_second_note_is_higher_than_the_first() {
        // Counts the zero crossings of each note: more of them, a higher note.
        let s = samples(&tone(Tone::Rising, 1.0));
        let crossings = |part: &[i16]| part.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
        let first = count(Tone::Rising.notes()[0].ms);
        let second_from = first + count(NOTE_GAP_MS);
        let per_second = |part: &[i16]| crossings(part) as f32 / (part.len() as f32 / 24_000.0);
        assert!(per_second(&s[second_from..]) > per_second(&s[..first]) * 1.2);
    }
}
