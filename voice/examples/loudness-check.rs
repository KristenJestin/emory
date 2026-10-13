//! Measures the loudness stage on files, with no bus and no sound played. Used to choose the
//! settings of `[loudness]` and to check that it does not distort.
//!
//! ```text
//! loudness-check sentences RAW_DIR OUT_DIR [--loudness 'target = -16'] [--keep 0,2,5]
//!     the peak, RMS, loudness and crest of every s*.wav of RAW_DIR as it comes, with `gain` 2, 3
//!     and 5, and with the stage; the WAV of the sentences listed by --keep go to OUT_DIR
//! loudness-check tones [--loudness '…']
//!     distortion of tones and tone bursts: the stage against the old gain with its soft limiter
//! loudness-check timing RAW_DIR [--loudness '…']
//!     the delay of the first sound and the compute time per second of audio
//! ```

use std::{path::Path, time::Instant};

use anyhow::{Context, bail};
use voice::{
    audio::{WavHeader, parse_wav_header},
    config::LoudnessSettings,
    gain::Gain,
    loudness::{KWeighting, Loudness},
};

const RATE: u32 = 24_000;
/// What `voice` sends in chunks of this duration.
const CHUNK_MS: usize = 60;

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut settings = LoudnessSettings {
        enabled: true,
        ..LoudnessSettings::default()
    };
    let mut keep: Vec<usize> = Vec::new();
    while let Some(at) = args.iter().position(|a| a.starts_with("--")) {
        let flag = args.remove(at);
        let value = args.get(at).cloned().context("a flag needs a value")?;
        args.remove(at);
        match flag.as_str() {
            "--loudness" => {
                settings = toml::from_str(&format!("enabled = true\n{value}"))?;
            }
            "--keep" => {
                keep = value.split(',').map(str::parse).collect::<Result<_, _>>()?;
            }
            other => bail!("unknown option {other}"),
        }
    }
    match args.first().map(String::as_str) {
        Some("sentences") => sentences(&args[1..], &settings, &keep),
        Some("tones") => {
            tones(&settings);
            Ok(())
        }
        Some("timing") => timing(&args[1..], &settings),
        _ => bail!("usage: loudness-check sentences|tones|timing …"),
    }
}

fn read_wav(path: &Path) -> anyhow::Result<Vec<i16>> {
    let bytes = std::fs::read(path).with_context(|| path.display().to_string())?;
    let WavHeader::Ready { data_offset, .. } =
        parse_wav_header(&bytes).map_err(anyhow::Error::msg)?
    else {
        bail!("{}: incomplete WAV", path.display());
    };
    Ok(samples_of(&bytes[data_offset..]))
}

fn write_wav(path: &Path, samples: &[i16]) -> anyhow::Result<()> {
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut wav = Vec::new();
    wav.extend(b"RIFF");
    wav.extend((36 + data.len() as u32).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(RATE.to_le_bytes());
    wav.extend((RATE * 2).to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend((data.len() as u32).to_le_bytes());
    wav.extend(data);
    std::fs::write(path, wav)?;
    Ok(())
}

fn bytes_of(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn samples_of(bytes: &[u8]) -> Vec<i16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect()
}

/// The old path: the gain and its soft limiter, chunk by chunk.
fn with_gain(samples: &[i16], gain: f32) -> Vec<i16> {
    let mut bytes = bytes_of(samples);
    for chunk in bytes.chunks_mut(RATE as usize * CHUNK_MS / 1000 * 2) {
        Gain::new(gain).apply(chunk);
    }
    samples_of(&bytes)
}

/// The new path, as `voice` does it: chunk by chunk, then the end of the sentence.
fn with_stage_level(samples: &[i16], settings: &LoudnessSettings) -> (Vec<i16>, Option<f32>, f32) {
    let mut stage = Loudness::new(settings, RATE, 1);
    let mut out = Vec::new();
    for chunk in bytes_of(samples).chunks(RATE as usize * CHUNK_MS / 1000 * 2) {
        out.extend(stage.process(chunk));
    }
    out.extend(stage.finish());
    assert_eq!(stage.clipped(), 0, "the safety clamp was needed");
    (samples_of(&out), stage.level(), stage.limited())
}

fn with_stage(samples: &[i16], settings: &LoudnessSettings) -> Vec<i16> {
    with_stage_level(samples, settings).0
}

struct Measure {
    peak_db: f64,
    rms_db: f64,
    lufs: f64,
    crest: f64,
    at_peak: usize,
    short_term: (f64, f64),
}

fn db(x: f64) -> f64 {
    20.0 * x.max(1e-9).log10()
}

fn measure(samples: &[i16]) -> Measure {
    let x: Vec<f64> = samples.iter().map(|&s| s as f64 / 32768.0).collect();
    let peak = x.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    let at_peak = samples
        .iter()
        .filter(|s| s.unsigned_abs() as f64 >= peak * 32768.0 - 0.5)
        .count();
    // The active RMS: over the blocks of 10 ms above -50 dBFS, like the speech it is.
    let mean_square = x.iter().map(|v| v * v).sum::<f64>() / x.len().max(1) as f64;
    // Integrated loudness of BS.1770: blocks of 400 ms every 100 ms, a gate at -70 then 10 LU
    // under the average of what passed.
    let mut filter = KWeighting::new(RATE as f64);
    let z: Vec<f64> = x.iter().map(|&v| filter.process(v).powi(2)).collect();
    let (block, hop) = (RATE as usize * 4 / 10, RATE as usize / 10);
    let blocks: Vec<f64> = (0..)
        .map(|i| i * hop)
        .take_while(|start| start + block <= z.len())
        .map(|start| z[start..start + block].iter().sum::<f64>() / block as f64)
        .collect();
    let l = |m: f64| -0.691 + 10.0 * m.max(1e-12).log10();
    let mean_of = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
    let passed: Vec<f64> = blocks.iter().copied().filter(|&m| l(m) > -70.0).collect();
    let relative = l(mean_of(&passed)) - 10.0;
    let gated: Vec<f64> = passed
        .iter()
        .copied()
        .filter(|&m| l(m) > relative)
        .collect();
    let lufs = if gated.is_empty() {
        -70.0
    } else {
        l(mean_of(&gated))
    };
    // The spread of the loudness of the 400 ms windows that are speech (10th to 95th percentile).
    let mut levels: Vec<f64> = gated.iter().map(|&m| l(m)).collect();
    levels.sort_by(f64::total_cmp);
    let pick = |q: f64| {
        levels
            .get(((levels.len() as f64 - 1.0) * q) as usize)
            .copied()
            .unwrap_or(-70.0)
    };
    Measure {
        peak_db: db(peak),
        rms_db: 10.0 * mean_square.max(1e-18).log10(),
        lufs,
        crest: db(peak) - lufs,
        at_peak,
        short_term: (pick(0.10), pick(0.95)),
    }
}

fn sentences(args: &[String], settings: &LoudnessSettings, keep: &[usize]) -> anyhow::Result<()> {
    let (raw, out) = (Path::new(&args[0]), Path::new(&args[1]));
    std::fs::create_dir_all(out)?;
    let mut files: Vec<_> = std::fs::read_dir(raw)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "wav"))
        .collect();
    files.sort();
    println!(
        "{:<8} {:<14} {:>6} {:>7} {:>7} {:>7} {:>6} {:>7} {:>10} {:>6}",
        "file", "path", "s", "peak", "RMS", "LUFS", "crest", "@peak", "LU 10-95%", "lim dB"
    );
    let mut totals: Vec<(String, Vec<Measure>)> = Vec::new();
    for (i, file) in files.iter().enumerate() {
        let original = read_wav(file)?;
        let name = file.file_stem().unwrap().to_string_lossy().into_owned();
        let (staged, _, limited) = with_stage_level(&original, settings);
        let variants: Vec<(String, Vec<i16>, f32)> = vec![
            ("gain 1".into(), original.clone(), 0.0),
            ("gain 2".into(), with_gain(&original, 2.0), 0.0),
            ("gain 3".into(), with_gain(&original, 3.0), 0.0),
            ("gain 5".into(), with_gain(&original, 5.0), 0.0),
            ("loudness".into(), staged, limited),
        ];
        for (j, (label, samples, limited)) in variants.iter().enumerate() {
            let m = measure(samples);
            println!(
                "{:<8} {:<14} {:>6.2} {:>7.1} {:>7.1} {:>7.1} {:>6.1} {:>7} {:>5.1} {:>4.1} {:>6.1}",
                name,
                label,
                samples.len() as f64 / RATE as f64,
                m.peak_db,
                m.rms_db,
                m.lufs,
                m.crest,
                m.at_peak,
                m.short_term.0,
                m.short_term.1,
                limited
            );
            if totals.len() <= j {
                totals.push((label.clone(), Vec::new()));
            }
            totals[j].1.push(m);
            if keep.contains(&i) {
                let slug = label.replace(' ', "-");
                write_wav(&out.join(format!("{name}-{slug}.wav")), samples)?;
            }
        }
    }
    println!("\nover {} sentences:", files.len());
    for (label, ms) in &totals {
        let n = ms.len() as f64;
        let lufs: Vec<f64> = ms.iter().map(|m| m.lufs).collect();
        let mean = lufs.iter().sum::<f64>() / n;
        let spread = lufs.iter().cloned().fold(f64::MIN, f64::max)
            - lufs.iter().cloned().fold(f64::MAX, f64::min);
        println!(
            "  {label:<10} LUFS mean {mean:>6.1}, spread between sentences {spread:>4.1} LU, peak max {:>5.1} dBFS, samples at the peak max {}",
            ms.iter().map(|m| m.peak_db).fold(f64::MIN, f64::max),
            ms.iter().map(|m| m.at_peak).max().unwrap()
        );
    }
    Ok(())
}

// ---- tones -------------------------------------------------------------------------------------

/// The harmonics 2 to 8 against the fundamental in total (THD, dB), and harmonics 2 to 5 one by
/// one. The fundamental and the harmonics are fitted by least squares over `samples`.
fn distortion(samples: &[i16], frequency: f64) -> (f64, Vec<f64>) {
    let x: Vec<f64> = samples.iter().map(|&s| s as f64).collect();
    let n = x.len() as f64;
    let component = |f: f64| {
        let w = 2.0 * std::f64::consts::PI * f / RATE as f64;
        let (mut c, mut s) = (0.0, 0.0);
        for (i, v) in x.iter().enumerate() {
            c += v * (w * i as f64).cos();
            s += v * (w * i as f64).sin();
        }
        (2.0 * c / n, 2.0 * s / n)
    };
    let power = |f: f64| {
        let (c, s) = component(f);
        (c * c + s * s) / 2.0
    };
    let fundamental = power(frequency);
    let harmonics: Vec<f64> = (2..=8)
        .filter(|k| frequency * *k as f64 <= 0.45 * RATE as f64)
        .map(|k| power(frequency * k as f64))
        .collect();
    let total: f64 = harmonics.iter().sum();
    (
        10.0 * (total.max(1e-18) / fundamental).log10(),
        (0..4)
            .map(|i| {
                10.0 * (harmonics.get(i).copied().unwrap_or(0.0).max(1e-18) / fundamental).log10()
            })
            .collect(),
    )
}

fn tone(frequency: f64, amplitude: f64, seconds: f64) -> Vec<f64> {
    (0..(seconds * RATE as f64) as usize)
        .map(|i| {
            amplitude * (2.0 * std::f64::consts::PI * frequency * i as f64 / RATE as f64).sin()
        })
        .collect()
}

fn pcm(x: &[f64]) -> Vec<i16> {
    x.iter()
        .map(|v| (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
        .collect()
}

fn tones(settings: &LoudnessSettings) {
    println!("THD in dB against the fundamental (lower is cleaner), then harmonics 2 to 5.\n");
    let report = |label: &str, samples: &[i16], f: f64| {
        let (thd, h) = distortion(samples, f);
        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0) as f64 / 32768.0;
        println!(
            "  {label:<34} peak {:>5.2}  THD {thd:>7.1} dB   H2..H5 {:>6.1} {:>6.1} {:>6.1} {:>6.1}",
            peak, h[0], h[1], h[2], h[3]
        );
    };
    let ceiling = 10f64.powf(settings.ceiling as f64 / 20.0);
    println!("1. a steady 1 kHz tone at 0.35 (about what Pocket peaks at), whole signal");
    let steady = pcm(&tone(1000.0, 0.35, 10.0));
    report("as it comes (reference)", &steady, 1000.0);
    for g in [2.0, 3.0, 5.0] {
        report(
            &format!("gain {g} (tanh limiter)"),
            &with_gain(&steady, g),
            1000.0,
        );
    }
    let out = with_stage(&steady, settings);
    report(
        "stage (last 2 s of 10: settled)",
        &out[out.len() - 2 * RATE as usize..],
        1000.0,
    );

    println!(
        "\n2. a 1 kHz burst 20 dB louder than the 1 s of tone before it (0.1 to 1.0 peak input)"
    );
    for f in [200.0, 1000.0, 3000.0] {
        let mut x = tone(f, 0.1, 1.0);
        x.extend(tone(f, 1.0, 0.25));
        x.extend(tone(f, 0.1, 0.5));
        let input = pcm(&x);
        let burst = RATE as usize..(RATE as usize + RATE as usize / 4);
        let from = burst.start + RATE as usize / 100; // settled: past the 10 ms of attack
        let to = burst.end - RATE as usize / 100;
        println!(" at {f} Hz:");
        let hard: Vec<i16> = input
            .iter()
            .map(|&s| ((s as f64 * 3.0).clamp(-32768.0, 32767.0)) as i16)
            .collect();
        report("gain 3 hard clipped (naive)", &hard[burst.clone()], f);
        let soft = with_gain(&input, 3.0);
        report("gain 3 (tanh limiter)", &soft[burst.clone()], f);
        let out = with_stage(&input, settings);
        // The stage delays by the look-ahead: the burst is `lookahead` frames earlier in `out`.
        report("stage, whole burst", &out[burst.clone()], f);
        report("stage, burst without its edges", &out[from..to], f);
        report(
            "stage, last 50 ms of the burst",
            &out[burst.end - RATE as usize / 20..burst.end],
            f,
        );
        let peak = out.iter().map(|s| s.unsigned_abs()).max().unwrap() as f64 / 32768.0;
        println!(
            "    stage peak over the file {peak:.4} (ceiling {ceiling:.4}); the limiter did not clamp"
        );
    }
}

// ---- timing ------------------------------------------------------------------------------------

fn timing(args: &[String], settings: &LoudnessSettings) -> anyhow::Result<()> {
    let mut all = Vec::new();
    for entry in std::fs::read_dir(&args[0])? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "wav") {
            all.extend(read_wav(&path)?);
        }
    }
    let seconds = all.len() as f64 / RATE as f64;
    let bytes = bytes_of(&all);
    let chunk = RATE as usize * CHUNK_MS / 1000 * 2;
    let mut best = f64::MAX;
    for _ in 0..5 {
        let mut stage = Loudness::new(settings, RATE, 1);
        let start = Instant::now();
        let mut produced = 0;
        for c in bytes.chunks(chunk) {
            produced += stage.process(c).len();
        }
        produced += stage.finish().len();
        best = best.min(start.elapsed().as_secs_f64());
        assert_eq!(produced, bytes.len());
    }
    println!(
        "{seconds:.1} s of audio in {:.1} ms (best of 5): {:.2} ms of compute per second of audio, {:.3} % of real time",
        best * 1e3,
        best * 1e3 / seconds,
        best / seconds * 100.0
    );
    // The delay of the first sound: the look-ahead, and the compute for the first chunk.
    let stage = Loudness::new(settings, RATE, 1);
    println!(
        "look-ahead {} frames = {:.2} ms: the first {} frames of the stream do not come out of the first chunk",
        stage.lookahead(),
        stage.lookahead() as f64 * 1e3 / RATE as f64,
        stage.lookahead()
    );
    let mut worst = 0.0f64;
    for _ in 0..200 {
        let mut stage = Loudness::new(settings, RATE, 1);
        let start = Instant::now();
        let out = stage.process(&bytes[..chunk]);
        worst = worst.max(start.elapsed().as_secs_f64());
        std::hint::black_box(out);
    }
    println!(
        "compute for the first 60 ms chunk: up to {:.3} ms (200 runs)",
        worst * 1e3
    );
    Ok(())
}
