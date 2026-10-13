//! The loudness of the voice: a compressor, a make-up gain that brings the speech to a target
//! level, and a look-ahead peak limiter, applied to the audio of a sentence as it streams.
//!
//! ```text
//! frame -> compressor -> make-up gain -> delay of `lookahead` -> limiter gain -> out
//!              ^               ^                                       ^
//!       level of the input  level of the compressed speech      peaks ahead, seen
//!       (K-weighted)        (K-weighted)                        before they come out
//! ```
//!
//! - **The level** of the speech is measured as a streaming approximation of the integrated
//!   loudness of BS.1770 (K-weighting, 10 ms blocks, a gate at -50 LUFS and one 12 LU below the
//!   speech so far), so pauses and breaths do not count. It is in LUFS, and for speech about the
//!   same number as the RMS in dBFS.
//! - **The compressor** works relative to that level, so the threshold follows the engine and the
//!   target: it lowers the louder syllables towards the average, which is what gives room for the
//!   make-up gain. Its gain is smoothed in dB (attack, release), so it cannot crackle.
//! - **The make-up gain** is the difference between the target and the level of the compressed
//!   speech, smoothed. It only depends on the signal before the make-up (no feedback loop), and it
//!   stays at 0 dB until there is speech to measure.
//! - **The limiter** delays the audio by `lookahead` and lowers the gain *before* a peak reaches
//!   the output: the gain required by each sample goes through a sliding minimum over the
//!   look-ahead and then a moving average of the same length, which makes the gain a smooth ramp
//!   that is never above what any sample in its window requires. Nothing goes past the ceiling,
//!   and nothing is clipped (a hard clamp stays as a safety net and is counted).
//!
//! All the state is sample by sample, so the output is the same whatever the cutting of the audio
//! into chunks. The delay of the look-ahead is not made up with silence: the first call returns
//! `lookahead` frames less than it was given, and [`Loudness::finish`] gives them back at the end
//! of the sentence.

use std::collections::VecDeque;

use crate::config::LoudnessSettings;

/// Blocks quieter than this do not count as speech (LUFS).
const GATE_ABSOLUTE: f64 = -50.0;
/// Nor do blocks this far under the speech measured so far (LU).
const GATE_RELATIVE: f64 = -20.0;
/// The measure is made over blocks of this length.
const BLOCK_SECONDS: f64 = 0.010;
/// What the measure reads above the integrated loudness of BS.1770 on the same speech (dB): it
/// counts only the 10 ms blocks of speech, not the pauses a 400 ms window averages in. Measured
/// on eight sentences of Pocket, mean 1.5 dB (from 0.8 to 3.2), then taken off.
const CALIBRATION: f64 = 1.5;
/// The level is only trusted once this many blocks of speech have been heard (40 ms): the very
/// first block is often the soft start of a breath or a consonant.
const MIN_BLOCKS: u64 = 4;
/// The make-up gain follows the measure with a time constant that starts at this (s) and grows by
/// as much with each block of speech, up to `MAKE_UP_SECONDS`: it settles on the first words.
const MAKE_UP_START: f64 = 0.005;
/// The measure forgets with this time constant (counted in blocks of speech).
const MEMORY_SECONDS: f64 = 4.0;
/// How fast the make-up gain follows the measure.
const MAKE_UP_SECONDS: f64 = 0.15;
/// The make-up gain stays within these (dB).
const MAKE_UP_RANGE: (f32, f32) = (-12.0, 24.0);
/// The width of the soft knee of the compressor (dB).
const KNEE: f32 = 6.0;
/// How fast the limiter lets go once the peak is past.
const LIMITER_RELEASE_SECONDS: f64 = 0.05;

/// Frames of `f32` samples in, bytes of 16-bit little-endian PCM out: the stage of one sentence.
pub struct Loudness {
    channels: usize,
    // Settings, in the units the code uses.
    target: f64,
    ceiling: f32,
    threshold: f32,
    slope: f32,
    attack: f32,
    release: f32,
    // The measures: of what comes in, and of what the compressor sends out.
    input_level: Meter,
    speech_level: Meter,
    // The compressor.
    reduction: f32,
    // The make-up gain, in dB, and how fast it moves.
    make_up: f32,
    make_up_slew: f32,
    slew_blocks: u64,
    rate: f64,
    // The limiter.
    lookahead: usize,
    delay: Vec<f32>,
    at: usize,
    frames_in: u64,
    window: VecDeque<(u64, f32)>,
    released: f32,
    release_step: f32,
    ramp: Vec<f64>,
    ramp_at: usize,
    ramp_sum: f64,
    clipped: u64,
    deepest: f32,
    finished: bool,
    scratch: Vec<f32>,
}

impl Loudness {
    /// The stage for a stream of this many channels at this sample rate.
    pub fn new(settings: &LoudnessSettings, sample_rate: u32, channels: u16) -> Self {
        let rate = sample_rate as f64;
        let channels = channels.max(1) as usize;
        let lookahead = ((settings.lookahead_ms as f64 / 1000.0 * rate).round() as usize).max(1);
        let one_pole = |ms: f32| 1.0 - (-1.0 / (ms as f64 / 1000.0 * rate)).exp() as f32;
        Self {
            channels,
            target: settings.target as f64,
            ceiling: 10f32.powf(settings.ceiling / 20.0),
            threshold: settings.threshold,
            slope: 1.0 - 1.0 / settings.ratio,
            attack: one_pole(settings.attack_ms),
            release: one_pole(settings.release_ms),
            input_level: Meter::new(rate, channels),
            speech_level: Meter::new(rate, channels),
            reduction: 0.0,
            make_up: 0.0,
            make_up_slew: 1.0,
            slew_blocks: 0,
            rate,
            lookahead,
            delay: vec![0.0; (lookahead + 1) * channels],
            at: 0,
            frames_in: 0,
            window: VecDeque::new(),
            released: 1.0,
            release_step: (1.0 - (-1.0 / (LIMITER_RELEASE_SECONDS * rate)).exp()) as f32,
            ramp: vec![1.0; lookahead + 1],
            ramp_at: 0,
            ramp_sum: (lookahead + 1) as f64,
            clipped: 0,
            deepest: 1.0,
            finished: false,
            scratch: vec![0.0; channels],
        }
    }

    /// The time the limiter looks ahead, in frames: what the first sound is delayed by.
    pub fn lookahead(&self) -> usize {
        self.lookahead
    }

    /// How many samples had to be cut at the ceiling by the safety net: 0 when the limiter did
    /// its work.
    pub fn clipped(&self) -> u64 {
        self.clipped
    }

    /// The most the limiter lowered the gain so far, in dB (0 when it never acted).
    pub fn limited(&self) -> f32 {
        20.0 * (1.0 / self.deepest).log10()
    }

    /// The level of the speech so far, in LUFS, after the compressor and the make-up gain.
    pub fn level(&self) -> Option<f32> {
        self.speech_level
            .level()
            .map(|l| (l + self.make_up as f64 - CALIBRATION) as f32)
    }

    /// Processes a piece of the stream (whole frames; a trailing incomplete frame is dropped) and
    /// returns what is ready: `lookahead` frames less than it was given at the start of the
    /// sentence, as many afterwards.
    pub fn process(&mut self, pcm: &[u8]) -> Vec<u8> {
        let frame_bytes = 2 * self.channels;
        let mut out = Vec::with_capacity(pcm.len());
        for frame in pcm.chunks_exact(frame_bytes) {
            for (slot, sample) in self.scratch.iter_mut().zip(frame.as_chunks::<2>().0) {
                *slot = i16::from_le_bytes(*sample) as f32 / 32768.0;
            }
            self.step(&mut out);
        }
        out
    }

    /// The end of the sentence: the frames still held for the look-ahead. Once only; the stage is
    /// not used after it.
    pub fn finish(&mut self) -> Vec<u8> {
        if std::mem::replace(&mut self.finished, true) {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(self.lookahead * 2 * self.channels);
        self.scratch.fill(0.0);
        for _ in 0..self.lookahead {
            self.step(&mut out);
        }
        out
    }

    /// One frame (in `scratch`) through the stage; the frame that leaves, if any, goes to `out`.
    fn step(&mut self, out: &mut Vec<u8>) {
        let channels = self.channels;
        // The compressor, on the level of the loudest channel against the speech so far.
        let peak = self.scratch.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        let wanted = match self.input_level.level() {
            Some(level) if peak > 0.0 => {
                let over = 20.0 * peak.log10() - level as f32 - self.threshold;
                self.slope * soft_knee(over)
            }
            _ => 0.0,
        };
        let coefficient = if wanted > self.reduction {
            self.attack
        } else {
            self.release
        };
        self.reduction += (wanted - self.reduction) * coefficient;
        let compressed = db_to_amplitude(-self.reduction);
        self.input_level.push(&self.scratch);
        for sample in &mut self.scratch {
            *sample *= compressed;
        }
        self.speech_level.push(&self.scratch);

        // The make-up gain towards the target: quick to settle on the first words, then slower.
        if let Some(level) = self.speech_level.level() {
            if self.speech_level.blocks != self.slew_blocks {
                self.slew_blocks = self.speech_level.blocks;
                let seconds = (MAKE_UP_START * (self.slew_blocks + 1 - MIN_BLOCKS) as f64)
                    .min(MAKE_UP_SECONDS);
                self.make_up_slew = (1.0 - (-1.0 / (seconds * self.rate)).exp()) as f32;
            }
            let wanted = (self.target + CALIBRATION - level)
                .clamp(MAKE_UP_RANGE.0 as f64, MAKE_UP_RANGE.1 as f64)
                as f32;
            self.make_up += (wanted - self.make_up) * self.make_up_slew;
        }
        let make_up = db_to_amplitude(self.make_up);
        let slot = self.at * channels;
        let mut loudest = 0.0f32;
        for (stored, sample) in self.delay[slot..slot + channels]
            .iter_mut()
            .zip(&self.scratch)
        {
            *stored = sample * make_up;
            loudest = loudest.max(stored.abs());
        }
        self.at = (self.at + 1) % (self.lookahead + 1);

        // The limiter: the gain this frame asks for, its minimum over the look-ahead, a release,
        // then a moving average over the look-ahead.
        let required = if loudest > self.ceiling {
            self.ceiling / loudest
        } else {
            1.0
        };
        let now = self.frames_in;
        self.frames_in += 1;
        while self.window.back().is_some_and(|&(_, g)| g >= required) {
            self.window.pop_back();
        }
        self.window.push_back((now, required));
        while self
            .window
            .front()
            .is_some_and(|&(at, _)| now - at > self.lookahead as u64)
        {
            self.window.pop_front();
        }
        let minimum = self.window.front().map_or(1.0, |&(_, g)| g);
        self.released = if minimum < self.released {
            minimum
        } else {
            self.released + (minimum - self.released) * self.release_step
        };
        self.ramp_sum += self.released as f64 - self.ramp[self.ramp_at];
        self.ramp[self.ramp_at] = self.released as f64;
        self.ramp_at = (self.ramp_at + 1) % (self.lookahead + 1);
        if now < self.lookahead as u64 {
            return;
        }
        let gain = (self.ramp_sum / (self.lookahead + 1) as f64) as f32;
        self.deepest = self.deepest.min(gain);
        // `at` has moved on: it now points at the oldest frame, the one that leaves.
        let slot = self.at * channels;
        for stored in &self.delay[slot..slot + channels] {
            let mut value = stored * gain;
            if value.abs() > self.ceiling * 1.000_001 {
                self.clipped += 1;
                value = value.clamp(-self.ceiling, self.ceiling);
            }
            let sample = (value * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
            out.extend_from_slice(&sample.to_le_bytes());
        }
    }
}

/// The gain reduction curve before the ratio: nothing below the knee, the excess above it, and a
/// parabola in between.
fn soft_knee(over: f32) -> f32 {
    if 2.0 * over < -KNEE {
        0.0
    } else if 2.0 * over.abs() <= KNEE {
        (over + KNEE / 2.0).powi(2) / (2.0 * KNEE)
    } else {
        over
    }
}

fn db_to_amplitude(db: f32) -> f32 {
    (db * (std::f32::consts::LN_10 / 20.0)).exp()
}

/// A streaming approximation of the integrated loudness of BS.1770: K-weighting, short blocks and
/// the two gates, with a memory that fades over seconds.
struct Meter {
    filters: Vec<KWeighting>,
    block_frames: usize,
    frames: usize,
    energy: f64,
    sum: f64,
    weight: f64,
    decay: f64,
    /// How many blocks of speech have been counted.
    blocks: u64,
}

impl Meter {
    fn new(rate: f64, channels: usize) -> Self {
        Self {
            filters: (0..channels).map(|_| KWeighting::new(rate)).collect(),
            block_frames: (BLOCK_SECONDS * rate).round() as usize,
            frames: 0,
            energy: 0.0,
            sum: 0.0,
            weight: 0.0,
            blocks: 0,
            decay: (-BLOCK_SECONDS / MEMORY_SECONDS).exp(),
        }
    }

    fn push(&mut self, frame: &[f32]) {
        for (filter, &sample) in self.filters.iter_mut().zip(frame) {
            let z = filter.process(sample as f64);
            self.energy += z * z;
        }
        self.frames += 1;
        if self.frames < self.block_frames {
            return;
        }
        let mean = self.energy / self.frames as f64;
        self.frames = 0;
        self.energy = 0.0;
        let loudness = loudness_of(mean);
        let relative = self
            .level()
            .map_or(f64::NEG_INFINITY, |l| l + GATE_RELATIVE);
        if loudness > GATE_ABSOLUTE && loudness > relative {
            self.sum = self.sum * self.decay + mean;
            self.weight = self.weight * self.decay + 1.0;
            self.blocks += 1;
        }
    }

    /// The loudness of the speech so far, in LUFS, once there is enough of it to tell.
    fn level(&self) -> Option<f64> {
        (self.blocks >= MIN_BLOCKS).then(|| loudness_of(self.sum / self.weight))
    }
}

fn loudness_of(mean_square: f64) -> f64 {
    -0.691 + 10.0 * mean_square.max(1e-12).log10()
}

/// The two filters of the K-weighting of BS.1770 (a shelf at high frequencies, then a high-pass),
/// for any sample rate. In `f64`: a high-pass at 38 Hz is too close to the unit circle for `f32`.
pub struct KWeighting {
    stages: [Biquad; 2],
}

impl KWeighting {
    pub fn new(rate: f64) -> Self {
        // The coefficients are those of the reference, written for any sample rate.
        let (f0, gain_db, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196f64);
        let k = (std::f64::consts::PI * f0 / rate).tan();
        let vh = 10f64.powf(gain_db / 20.0);
        let vb = vh.powf(0.4996667741545416);
        let a0 = 1.0 + k / q + k * k;
        let shelf = Biquad {
            b: [
                (vh + vb * k / q + k * k) / a0,
                2.0 * (k * k - vh) / a0,
                (vh - vb * k / q + k * k) / a0,
            ],
            a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
            s: [0.0; 2],
        };
        let (f0, q) = (38.13547087602444, 0.5003270373238773f64);
        let k = (std::f64::consts::PI * f0 / rate).tan();
        let a0 = 1.0 + k / q + k * k;
        let high_pass = Biquad {
            b: [1.0, -2.0, 1.0],
            a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
            s: [0.0; 2],
        };
        Self {
            stages: [shelf, high_pass],
        }
    }

    pub fn process(&mut self, x: f64) -> f64 {
        let [shelf, high_pass] = &mut self.stages;
        high_pass.process(shelf.process(x))
    }
}

/// A biquad in transposed direct form II.
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    s: [f64; 2],
}

impl Biquad {
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.s[0];
        self.s[0] = self.b[1] * x - self.a[0] * y + self.s[1];
        self.s[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 24_000;

    fn settings() -> LoudnessSettings {
        LoudnessSettings {
            enabled: true,
            ..LoudnessSettings::default()
        }
    }

    fn bytes(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    fn samples(bytes: &[u8]) -> Vec<i16> {
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect()
    }

    /// The stage over a whole sentence cut in pieces of `piece` frames (all of it when 0).
    fn run(settings: &LoudnessSettings, channels: u16, input: &[i16], piece: usize) -> Vec<i16> {
        let mut stage = Loudness::new(settings, RATE, channels);
        let piece = if piece == 0 {
            input.len()
        } else {
            piece * channels as usize
        };
        let mut out = Vec::new();
        for part in input.chunks(piece.max(1)) {
            out.extend(stage.process(&bytes(part)));
        }
        out.extend(stage.finish());
        assert_eq!(stage.clipped(), 0, "the safety net had to cut a sample");
        samples(&out)
    }

    /// A deterministic signal that behaves like speech for a loudness stage: syllables of voiced
    /// harmonics at a pitch, pauses between them, uneven strengths, an occasional sharp peak.
    fn speech_like(seconds: f64, rms_db: f64) -> Vec<i16> {
        let mut seed = 12345u32;
        let mut random = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f64 / (1u64 << 24) as f64
        };
        let frames = (seconds * RATE as f64) as usize;
        let mut x = vec![0.0f64; frames];
        let (mut at, mut phase) = (0usize, 0.0f64);
        while at < frames {
            let length = ((0.12 + 0.16 * random()) * RATE as f64) as usize;
            let strength = 0.4 + 0.6 * random();
            let pitch = 110.0 + 60.0 * random();
            for i in 0..length.min(frames - at) {
                let t = i as f64 / length as f64;
                let envelope = (std::f64::consts::PI * t).sin().powf(0.7) * strength;
                phase += 2.0 * std::f64::consts::PI * pitch / RATE as f64;
                let voiced: f64 = (1..=12).map(|k| (phase * k as f64).sin() / k as f64).sum();
                x[at + i] = envelope * voiced;
            }
            if random() < 0.25 && at + length / 3 < frames {
                x[at + length / 3] *= 1.8;
            }
            at += length + ((0.03 + 0.1 * random()) * RATE as f64) as usize;
        }
        let mean = x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64;
        let scale = 10f64.powf(rms_db / 20.0) / mean.sqrt();
        x.iter()
            .map(|v| (v * scale * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
            .collect()
    }

    /// The loudness of BS.1770 (400 ms blocks every 100 ms, the two gates), mono.
    fn lufs(samples: &[i16]) -> f64 {
        let mut filter = KWeighting::new(RATE as f64);
        let z: Vec<f64> = samples
            .iter()
            .map(|&s| filter.process(s as f64 / 32768.0).powi(2))
            .collect();
        let block = RATE as usize * 4 / 10;
        let blocks: Vec<f64> = (0..)
            .map(|i| i * block / 4)
            .take_while(|start| start + block <= z.len())
            .map(|start| z[start..start + block].iter().sum::<f64>() / block as f64)
            .collect();
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        let passed: Vec<f64> = blocks
            .iter()
            .copied()
            .filter(|&m| loudness_of(m) > -70.0)
            .collect();
        let relative = loudness_of(mean(&passed)) - 10.0;
        let gated: Vec<f64> = passed
            .into_iter()
            .filter(|&m| loudness_of(m) > relative)
            .collect();
        loudness_of(mean(&gated))
    }

    fn peak(samples: &[i16]) -> f64 {
        samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0) as f64 / 32768.0
    }

    #[test]
    fn the_output_is_the_same_whatever_the_cutting_of_the_audio() {
        let input = speech_like(3.0, -26.0);
        let whole = run(&settings(), 1, &input, 0);
        // Pieces smaller and larger than the look-ahead (120 frames), and the sizes the chunker
        // makes (60 ms = 1440 frames).
        for piece in [1, 7, 119, 120, 121, 480, 1440, 5000] {
            assert_eq!(
                run(&settings(), 1, &input, piece),
                whole,
                "pieces of {piece}"
            );
        }
    }

    #[test]
    fn nothing_is_lost_or_added_the_tail_is_drained() {
        let input = speech_like(1.5, -26.0);
        let mut stage = Loudness::new(&settings(), RATE, 1);
        let lookahead = stage.lookahead();
        assert_eq!(lookahead, 120, "5 ms at 24 kHz");
        let first = stage.process(&bytes(&input[..1440]));
        assert_eq!(
            first.len() / 2,
            1440 - lookahead,
            "the first call holds the look-ahead"
        );
        let second = stage.process(&bytes(&input[1440..]));
        let rest = stage.finish();
        assert_eq!(rest.len() / 2, lookahead);
        assert_eq!(first.len() + second.len() + rest.len(), input.len() * 2);
        assert!(stage.finish().is_empty(), "the end is given once");
    }

    #[test]
    fn a_sound_that_stops_abruptly_keeps_its_last_samples() {
        // A tone that is still at full strength on the last sample.
        let tone: Vec<i16> = (0..RATE as usize)
            .map(|i| (8000.0 * (i as f64 * 0.2).sin()) as i16)
            .collect();
        let out = run(&settings(), 1, &tone, 1440);
        assert_eq!(out.len(), tone.len());
        let end = &out[out.len() - 240..];
        assert!(
            peak(end) > 0.05,
            "the tail of the sound is there: {}",
            peak(end)
        );
    }

    #[test]
    fn nothing_goes_past_the_ceiling_not_even_a_hard_blow() {
        let ceiling = 10f64.powf(settings().ceiling as f64 / 20.0);
        let mut input = speech_like(2.0, -26.0);
        // A burst 30 dB louder, then a full-scale click, then noise at full scale.
        for (i, s) in input[24_000..30_000].iter_mut().enumerate() {
            *s =
                ((*s as f64 * 30.0).clamp(-32768.0, 32767.0) as i16).saturating_add((i % 3) as i16);
        }
        input[40_000] = i16::MAX;
        input[40_001] = i16::MIN;
        let mut seed = 1u32;
        for s in &mut input[44_000..46_000] {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *s = if seed >> 31 == 0 { i16::MAX } else { i16::MIN };
        }
        for target in [-30.0, -16.0, -8.0] {
            let settings = LoudnessSettings {
                target,
                ..settings()
            };
            let out = run(&settings, 1, &input, 1440);
            assert!(
                peak(&out) <= ceiling + 1.0 / 32768.0,
                "target {target}: peak {} over the ceiling {ceiling}",
                peak(&out)
            );
        }
    }

    #[test]
    fn the_speech_reaches_the_target_whatever_its_level() {
        let target = settings().target as f64;
        let mut reached = Vec::new();
        for level in [-36.0, -30.0, -24.0, -18.0] {
            let out = run(&settings(), 1, &speech_like(5.0, level), 1440);
            let measured = lufs(&out);
            assert!(
                (measured - target).abs() < 2.0,
                "speech at {level} dBFS RMS came out at {measured:.1} LUFS, target {target}"
            );
            reached.push(measured);
        }
        let spread = reached.iter().cloned().fold(f64::MIN, f64::max)
            - reached.iter().cloned().fold(f64::MAX, f64::min);
        assert!(spread < 1.5, "levels reached {reached:?}");
    }

    #[test]
    fn another_target_is_reached() {
        for target in [-22.0f32, -12.0] {
            let settings = LoudnessSettings {
                target,
                ..settings()
            };
            let measured = lufs(&run(&settings, 1, &speech_like(5.0, -30.0), 1440));
            assert!(
                (measured - target as f64).abs() < 2.0,
                "{measured:.1} for a target of {target}"
            );
        }
    }

    #[test]
    fn the_compressor_brings_the_peaks_closer_to_the_average() {
        let input = speech_like(5.0, -26.0);
        let crest = |s: &[i16]| 20.0 * peak(s).log10() - lufs(s);
        let flat = LoudnessSettings {
            ratio: 1.0,
            ..settings()
        };
        let compressed = run(&settings(), 1, &input, 1440);
        let uncompressed = run(&flat, 1, &input, 1440);
        assert!(
            crest(&compressed) + 1.0 < crest(&uncompressed),
            "crest {:.1} against {:.1} without compression",
            crest(&compressed),
            crest(&uncompressed)
        );
    }

    #[test]
    fn silence_and_a_noise_floor_are_not_amplified() {
        let silence = vec![0i16; 24_000];
        assert!(run(&settings(), 1, &silence, 1440).iter().all(|&s| s == 0));
        // About -75 dBFS: under the gate, so no make-up gain.
        let mut seed = 7u32;
        let hiss: Vec<i16> = (0..48_000)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 20) as i16 % 5) - 2
            })
            .collect();
        let out = run(&settings(), 1, &hiss, 1440);
        assert!(peak(&out) <= 3.0 / 32768.0, "peak {}", peak(&out));
    }

    #[test]
    fn stereo_channels_move_together() {
        let mono = speech_like(2.0, -26.0);
        let stereo: Vec<i16> = mono.iter().flat_map(|&s| [s & !1, (s & !1) / 2]).collect();
        let out = run(&settings(), 2, &stereo, 1440);
        assert_eq!(out.len(), stereo.len());
        // The gain is the same on both channels: the ratio between them is kept.
        for frame in out.as_chunks::<2>().0.iter().skip(2000).step_by(97) {
            let (l, r) = (frame[0] as i32, frame[1] as i32);
            assert!((l - 2 * r).abs() <= 2, "{l} {r}");
        }
    }

    #[test]
    fn an_incomplete_last_frame_is_dropped_not_misread() {
        let mut stage = Loudness::new(&settings(), RATE, 1);
        let mut input = bytes(&speech_like(0.5, -26.0));
        input.push(0x55);
        let out = stage.process(&input).len() + stage.finish().len();
        assert_eq!(out, input.len() - 1);
    }

    #[test]
    fn the_gain_changes_gently_no_step_in_the_sound() {
        // A steady tone: the output may not jump from one sample to the next more than a tone
        // of that level and pitch does (no click when the make-up gain or the limiter moves).
        let tone: Vec<i16> = (0..RATE as usize * 2)
            .map(|i| (2000.0 * (i as f64 * 0.05).sin()) as i16)
            .collect();
        let out = run(&settings(), 1, &tone, 1440);
        let step = out
            .windows(2)
            .map(|w| (w[1] as i32 - w[0] as i32).abs())
            .max()
            .unwrap();
        let loudest = out.iter().map(|s| s.unsigned_abs()).max().unwrap() as f64;
        assert!(
            step as f64 <= loudest * 0.05 * 1.25 + 2.0,
            "step {step} for a tone of {loudest}"
        );
    }

    #[test]
    fn the_limiter_is_clean_on_a_loud_tone_burst() {
        // 1 kHz, 1 s quiet then 0.25 s that is 20 dB louder: the limiter holds the burst at the
        // ceiling. Harmonics must stay far below what a clip would make (about -11 dB).
        let tone = |amplitude: f64, frames: usize| -> Vec<i16> {
            (0..frames)
                .map(|i| {
                    (amplitude
                        * 32768.0
                        * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / RATE as f64).sin())
                        as i16
                })
                .collect()
        };
        let mut input = tone(0.1, RATE as usize);
        input.extend(tone(1.0, RATE as usize / 4));
        let out = run(&settings(), 1, &input, 1440);
        let burst = &out[RATE as usize + RATE as usize / 10..RATE as usize + RATE as usize / 5];
        let power = |f: f64| {
            let w = 2.0 * std::f64::consts::PI * f / RATE as f64;
            let (mut c, mut s) = (0.0, 0.0);
            for (i, &v) in burst.iter().enumerate() {
                c += v as f64 * (w * i as f64).cos();
                s += v as f64 * (w * i as f64).sin();
            }
            (c * c + s * s) / (burst.len() as f64).powi(2) * 2.0
        };
        let fundamental = power(1000.0);
        for k in 2..=5 {
            let harmonic = 10.0 * (power(1000.0 * k as f64) / fundamental).log10();
            assert!(harmonic < -50.0, "harmonic {k} at {harmonic:.1} dB");
        }
        assert!(
            peak(&out) > 0.8,
            "the burst was held at the ceiling, not crushed"
        );
    }

    #[test]
    fn the_settings_are_used() {
        // A lower ceiling is kept; a longer look-ahead is a longer delay.
        let low = LoudnessSettings {
            ceiling: -6.0,
            target: -12.0,
            ..settings()
        };
        let out = run(&low, 1, &speech_like(4.0, -26.0), 1440);
        assert!(peak(&out) <= 10f64.powf(-6.0 / 20.0) + 1.0 / 32768.0);
        let stage = Loudness::new(
            &LoudnessSettings {
                lookahead_ms: 10.0,
                ..settings()
            },
            RATE,
            1,
        );
        assert_eq!(stage.lookahead(), 240);
    }
}
