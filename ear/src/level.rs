//! The level of the sound captured during a turn, in dBFS (0 is the loudest a sample can be).
//! The hearing measures the same on its side; comparing both tells where a quiet turn lost its
//! sound.

/// The level reported for a perfect silence, and for anything quieter.
const FLOOR_DBFS: f32 = -120.0;

/// The loudest sample and the mean power of the sound so far.
#[derive(Default)]
pub struct Level {
    peak: u16,
    sum_squares: f64,
    samples: u64,
}

impl Level {
    /// Adds a chunk of 16-bit little-endian PCM (an odd last byte is ignored).
    pub fn add(&mut self, bytes: &[u8]) {
        for pair in bytes.as_chunks::<2>().0 {
            let sample = i16::from_le_bytes(*pair);
            self.peak = self.peak.max(sample.unsigned_abs());
            self.sum_squares += f64::from(sample) * f64::from(sample);
            self.samples += 1;
        }
    }

    pub fn peak_dbfs(&self) -> f32 {
        dbfs(f64::from(self.peak))
    }

    pub fn rms_dbfs(&self) -> f32 {
        match self.samples {
            0 => FLOOR_DBFS,
            n => dbfs((self.sum_squares / n as f64).sqrt()),
        }
    }
}

/// A level in dBFS to a tenth, from an amplitude in units of the 16-bit scale.
fn dbfs(amplitude: f64) -> f32 {
    let db = if amplitude > 0.0 {
        20.0 * (amplitude / 32768.0).log10()
    } else {
        f64::from(FLOOR_DBFS)
    };
    ((db.max(f64::from(FLOOR_DBFS)) * 10.0).round() / 10.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn half_scale_is_minus_six_dbfs() {
        let mut level = Level::default();
        level.add(&pcm(&[16384, 0, 0, 0]));
        assert_eq!(level.peak_dbfs(), -6.0);
        assert_eq!(level.rms_dbfs(), -12.0);
    }

    #[test]
    fn the_most_negative_sample_is_full_scale() {
        let mut level = Level::default();
        level.add(&pcm(&[i16::MIN]));
        assert_eq!(level.peak_dbfs(), 0.0);
    }

    #[test]
    fn the_level_accumulates_and_silence_is_the_floor() {
        let mut level = Level::default();
        assert_eq!((level.peak_dbfs(), level.rms_dbfs()), (-120.0, -120.0));
        level.add(&pcm(&[0; 320]));
        assert_eq!(level.peak_dbfs(), -120.0);
        level.add(&pcm(&[3277, -3277]));
        level.add(&[]);
        assert_eq!(level.peak_dbfs(), -20.0);
    }
}
