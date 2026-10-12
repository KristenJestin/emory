//! The level of the audio received in a turn, in dBFS (decibels relative to full scale: 0 is the
//! loudest a sample can be). It tells a headset that sent near-silence (the mute button, a bad
//! wireless link) from a voice that nothing detected.

/// The level reported for a perfect silence (and below this, for anything quieter): dBFS of a
/// zero sample is minus infinity, which logs badly.
pub const FLOOR_DBFS: f32 = -120.0;

/// What the audio of a turn has been so far: its loudest sample and its mean power.
#[derive(Clone, Copy, Debug, Default)]
pub struct Level {
    peak: f32,
    sum_squares: f64,
    samples: u64,
}

impl Level {
    pub fn add(&mut self, samples: &[f32]) {
        for &sample in samples {
            self.peak = self.peak.max(sample.abs());
            self.sum_squares += f64::from(sample) * f64::from(sample);
        }
        self.samples += samples.len() as u64;
    }

    /// The loudest sample, in dBFS to a tenth.
    pub fn peak_dbfs(&self) -> f32 {
        dbfs(f64::from(self.peak))
    }

    /// The root mean square of all the samples, in dBFS to a tenth.
    pub fn rms_dbfs(&self) -> f32 {
        match self.samples {
            0 => FLOOR_DBFS,
            n => dbfs((self.sum_squares / n as f64).sqrt()),
        }
    }
}

fn dbfs(amplitude: f64) -> f32 {
    let db = if amplitude > 0.0 {
        20.0 * amplitude.log10()
    } else {
        f64::from(FLOOR_DBFS)
    };
    ((db.max(f64::from(FLOOR_DBFS)) * 10.0).round() / 10.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_scale_square_wave_is_zero_dbfs() {
        let mut level = Level::default();
        level.add(&[1.0, -1.0, 1.0, -1.0]);
        assert_eq!((level.peak_dbfs(), level.rms_dbfs()), (0.0, 0.0));
    }

    #[test]
    fn a_half_amplitude_peak_is_minus_six_dbfs_and_the_rms_is_lower() {
        let mut level = Level::default();
        level.add(&[0.5, 0.0, 0.0, 0.0]);
        assert_eq!(level.peak_dbfs(), -6.0);
        // sqrt(0.25 / 4) = 0.25
        assert_eq!(level.rms_dbfs(), -12.0);
    }

    #[test]
    fn the_level_accumulates_over_chunks() {
        let mut level = Level::default();
        level.add(&[0.1, -0.1]);
        level.add(&[0.01; 2]);
        level.add(&[]);
        assert_eq!(level.peak_dbfs(), -20.0);
    }

    #[test]
    fn silence_and_nothing_at_all_are_at_the_floor() {
        let mut level = Level::default();
        assert_eq!((level.peak_dbfs(), level.rms_dbfs()), (-120.0, -120.0));
        level.add(&[0.0; 320]);
        assert_eq!((level.peak_dbfs(), level.rms_dbfs()), (-120.0, -120.0));
        // Quieter than the floor is the floor.
        level.add(&[1e-9]);
        assert_eq!(level.peak_dbfs(), -120.0);
    }
}
