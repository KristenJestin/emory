//! `hearing-measure`: the measure of Parakeet and Silero on recorded sentences, one setting per
//! process (`scripts/measure-gpu.sh` runs the matrix and the table).
//!
//! ```text
//! hearing-measure run --parakeet DIR --provider cpu|cuda --precision int8|fp32
//!                     --decoding greedy|beam [--hotwords FILE] [--vad-provider cpu|cuda]
//!                     [--vad FILE] [--threads N] [--rounds N] [--tag NAME]
//!                     [--texts FILE] file.wav            -> one JSON line on stdout
//! hearing-measure table DIR                              -> the Markdown table of the results
//! ```
//!
//! `run` cuts the file with Silero VAD the way the service does (20 ms chunks), then transcribes
//! the sentences with the real engine, `rounds` times over. It never falls back to the CPU: a
//! setting that cannot be had is an error. Nothing of what was said goes to the log; the texts go
//! to `--texts` only, a file the caller owns. The VRAM is not measured here (it is the process's
//! own as `nvidia-smi` sees it, sampled by the script into `<tag>.vram`).
//!
//! The file must be 16 kHz mono 16-bit: `ffmpeg -i in.wav -ar 16000 -ac 1 -c:a pcm_s16le out.wav`.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, Result, bail};
use contract::TurnId;
use hearing::{
    config::{
        ConfigPath, DecodingMethod, ModelsConfig, Precision, Provider, TranscriptionConfig,
        VadConfig,
    },
    engine::{Detector, Transcriber},
    hotwords::Vocabulary,
};
use serde::{Deserialize, Serialize};

/// 20 ms of 16 kHz audio, the chunk an ear sends.
const CHUNK: usize = 320;
/// Linux counts the CPU time of a process in 1/100 s.
const CLOCK_TICKS_PER_SECOND: f64 = 100.0;

/// The result of one `run`.
#[derive(Debug, Serialize, Deserialize)]
struct Measure {
    tag: String,
    file: String,
    provider: String,
    precision: String,
    decoding: String,
    hotwords: usize,
    vad_provider: String,
    threads: i32,
    audio_s: f64,
    speech_s: f64,
    sentences: usize,
    vad_load_ms: u64,
    vad_ms: u64,
    asr_load_ms: u64,
    asr_warm_up_ms: u64,
    /// The wall time of each round over all the sentences, in seconds.
    rounds_s: Vec<f64>,
    /// The time of each sentence in the best round, in milliseconds.
    sentence_ms: Vec<f64>,
    /// The CPU time (user and system, all threads) used by the rounds, in seconds.
    cpu_s: Option<f64>,
    /// The wall time of all the rounds together, in seconds.
    wall_s: f64,
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("run") => run(args.collect()),
        Some("table") => {
            let folder = args.next().context("usage: hearing-measure table DIR")?;
            print!("{}", table(Path::new(&folder))?);
            Ok(())
        }
        _ => bail!("usage: hearing-measure run [options] file.wav | hearing-measure table DIR"),
    }
}

struct Options {
    parakeet: PathBuf,
    vad: PathBuf,
    provider: Provider,
    vad_provider: Provider,
    precision: Precision,
    beam: bool,
    hotwords: Option<PathBuf>,
    threads: i32,
    rounds: usize,
    tag: Option<String>,
    texts: Option<PathBuf>,
    file: PathBuf,
}

fn parse_provider(value: &str) -> Result<Provider> {
    match value {
        "cpu" => Ok(Provider::Cpu),
        "cuda" => Ok(Provider::Cuda),
        _ => bail!("provider: cpu or cuda, not {value}"),
    }
}

fn options(args: Vec<String>) -> Result<Options> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut options = Options {
        parakeet: home.join(".cache/emory/models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3"),
        vad: home.join(".cache/emory/models/silero_vad.onnx"),
        provider: Provider::Cpu,
        vad_provider: Provider::Cpu,
        precision: Precision::Fp32,
        beam: false,
        hotwords: None,
        threads: 4,
        rounds: 5,
        tag: None,
        texts: None,
        file: PathBuf::new(),
    };
    let mut file = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .with_context(|| format!("{name} expects a value"))
        };
        match arg.as_str() {
            "--parakeet" => options.parakeet = value("--parakeet")?.into(),
            "--vad" => options.vad = value("--vad")?.into(),
            "--provider" => options.provider = parse_provider(&value("--provider")?)?,
            "--vad-provider" => options.vad_provider = parse_provider(&value("--vad-provider")?)?,
            "--precision" => {
                options.precision = match value("--precision")?.as_str() {
                    "int8" => Precision::Int8,
                    "fp32" => Precision::Fp32,
                    other => bail!("precision: int8 or fp32, not {other}"),
                }
            }
            "--decoding" => {
                options.beam = match value("--decoding")?.as_str() {
                    "greedy" => false,
                    "beam" => true,
                    other => bail!("decoding: greedy or beam, not {other}"),
                }
            }
            "--hotwords" => options.hotwords = Some(value("--hotwords")?.into()),
            "--threads" => options.threads = value("--threads")?.parse()?,
            "--rounds" => options.rounds = value("--rounds")?.parse::<usize>()?.max(1),
            "--tag" => options.tag = Some(value("--tag")?),
            "--texts" => options.texts = Some(value("--texts")?.into()),
            _ if arg.starts_with("--") => bail!("unknown option: {arg}"),
            _ => file = Some(arg),
        }
    }
    options.file = file
        .context("usage: hearing-measure run [options] file.wav")?
        .into();
    Ok(options)
}

fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();
    if spec.sample_rate != 16_000
        || spec.channels != 1
        || spec.sample_format != hound::SampleFormat::Int
        || spec.bits_per_sample != 16
    {
        bail!(
            "{}: 16 kHz mono 16-bit is needed (ffmpeg -i in.wav -ar 16000 -ac 1 -c:a pcm_s16le out.wav)",
            path.display()
        );
    }
    reader
        .samples::<i16>()
        .map(|sample| Ok(f32::from(sample?) / 32768.0))
        .collect()
}

/// The user and system CPU time of this process, in seconds, from `/proc/self/stat`.
fn process_cpu_seconds() -> Option<f64> {
    cpu_seconds_from_stat(&std::fs::read_to_string("/proc/self/stat").ok()?)
}

/// Fields 14 and 15 (`utime`, `stime`, in clock ticks). The command name, field 2, may hold
/// spaces and parentheses: the fields are counted after its closing parenthesis.
fn cpu_seconds_from_stat(stat: &str) -> Option<f64> {
    let after_name = &stat[stat.rfind(')')? + 1..];
    let mut fields = after_name.split_whitespace();
    // `after_name` starts at field 3 (the state): utime is the 12th, stime the 13th from there.
    let user: f64 = fields.nth(11)?.parse().ok()?;
    let system: f64 = fields.next()?.parse().ok()?;
    Some((user + system) / CLOCK_TICKS_PER_SECOND)
}

fn run(args: Vec<String>) -> Result<()> {
    let options = options(args)?;
    let samples = read_wav(&options.file)?;
    let audio_s = samples.len() as f64 / 16_000.0;

    // The cut, as the service does it.
    let started = Instant::now();
    let mut detector = Detector::new(
        &options.vad,
        &VadConfig {
            provider: options.vad_provider,
            ..VadConfig::default()
        },
    )?;
    let vad_load_ms = started.elapsed().as_millis() as u64;
    let started = Instant::now();
    let mut segments = Vec::new();
    for chunk in samples.chunks(CHUNK) {
        segments.extend(detector.feed(chunk));
    }
    segments.extend(detector.flush());
    let vad_ms = started.elapsed().as_millis() as u64;
    let speech_s: f64 = segments.iter().map(|s| s.duration().as_secs_f64()).sum();
    anyhow::ensure!(
        !segments.is_empty(),
        "the VAD found no sentence in the file"
    );
    eprintln!(
        "vad on {}: {} sentences, {speech_s:.1} s of speech in {audio_s:.1} s, {vad_ms} ms",
        options.vad_provider.name(),
        segments.len()
    );

    // The engine, with no fallback: what is measured is what was asked for.
    let vocabulary = Arc::new(match (&options.hotwords, options.beam) {
        (Some(path), true) => Vocabulary::load(path)?,
        // No file: an empty vocabulary, on a path that is never written (nothing is added).
        _ => Vocabulary::load(&std::env::temp_dir().join("hearing-measure-no-hotwords.txt"))?,
    });
    let models = ModelsConfig {
        vad: ConfigPath(options.vad.clone()),
        parakeet: ConfigPath(options.parakeet.clone()),
    };
    let settings = TranscriptionConfig {
        threads: options.threads,
        decoding_method: Some(if options.beam {
            DecodingMethod::ModifiedBeamSearch
        } else {
            DecodingMethod::GreedySearch
        }),
        provider: options.provider,
        precision: Some(options.precision),
        fallback_to_cpu: false,
        ..TranscriptionConfig::default()
    };
    let hotwords = vocabulary.words().list.len();
    let transcriber = Transcriber::start(&models, &settings, vocabulary)?;
    let ready = transcriber.ready();
    eprintln!(
        "parakeet {} {}: load {} ms, warm-up {} ms",
        ready.provider.name(),
        ready.precision.name(),
        ready.load.as_millis(),
        ready.warm_up.as_millis()
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let cpu_before = process_cpu_seconds();
    let wall = Instant::now();
    let mut rounds_s = Vec::new();
    let mut per_round: Vec<Vec<f64>> = Vec::new();
    let mut texts = Vec::new();
    for round in 0..options.rounds {
        let started = Instant::now();
        let mut sentence_ms = Vec::new();
        for segment in &segments {
            let reply = transcriber.transcribe(TurnId::new(), segment.samples.clone());
            let done = runtime
                .block_on(reply)?
                .map_err(|e| anyhow::anyhow!("transcription: {e}"))?;
            sentence_ms.push(done.duration.as_secs_f64() * 1000.0);
            if round == 0 {
                texts.push(done.text);
            }
        }
        rounds_s.push(started.elapsed().as_secs_f64());
        per_round.push(sentence_ms);
    }
    let wall_s = wall.elapsed().as_secs_f64();
    let cpu_s = cpu_before.zip(process_cpu_seconds()).map(|(a, b)| b - a);

    let best = rounds_s
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.total_cmp(b.1))
        .map_or(0, |(index, _)| index);
    let tag = options.tag.clone().unwrap_or_else(|| {
        format!(
            "{}-{}-{}",
            ready.provider.name(),
            ready.precision.name(),
            if options.beam { "beam" } else { "greedy" }
        )
    });
    if let Some(path) = &options.texts {
        let lines: Vec<String> = texts
            .iter()
            .enumerate()
            .map(|(n, text)| format!("{n}\t{}", text.trim()))
            .collect();
        std::fs::write(path, lines.join("\n") + "\n")
            .with_context(|| format!("writing {}", path.display()))?;
    }
    let measure = Measure {
        tag,
        file: options
            .file
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        provider: ready.provider.name().to_owned(),
        precision: ready.precision.name().to_owned(),
        decoding: if options.beam { "beam" } else { "greedy" }.to_owned(),
        hotwords,
        vad_provider: options.vad_provider.name().to_owned(),
        threads: options.threads,
        audio_s,
        speech_s,
        sentences: segments.len(),
        vad_load_ms,
        vad_ms,
        asr_load_ms: ready.load.as_millis() as u64,
        asr_warm_up_ms: ready.warm_up.as_millis() as u64,
        rounds_s,
        sentence_ms: per_round.swap_remove(best),
        cpu_s,
        wall_s,
    };
    println!("{}", serde_json::to_string(&measure)?);
    Ok(())
}

// ---- the table ----

/// What `nvidia-smi` showed while a run lasted: the lines of `<tag>.vram`, `process total`, in
/// MiB; the first is taken before the process starts (the card as the others leave it).
#[derive(Debug, PartialEq)]
struct Vram {
    baseline_total: u64,
    process_peak: u64,
    total_peak: u64,
}

fn parse_vram(text: &str) -> Option<Vram> {
    let samples: Vec<(u64, u64)> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect();
    Some(Vram {
        baseline_total: samples.first()?.1,
        process_peak: samples.iter().map(|s| s.0).max()?,
        total_peak: samples.iter().map(|s| s.1).max()?,
    })
}

/// A text without case, punctuation or spacing differences: what changes the words.
fn words_of(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2]
}

/// One run with what sits beside its JSON file.
struct Run {
    measure: Measure,
    vram: Option<Vram>,
    texts: Option<Vec<String>>,
}

fn load_runs(folder: &Path) -> Result<Vec<Run>> {
    let mut runs = Vec::new();
    for entry in
        std::fs::read_dir(folder).with_context(|| format!("reading {}", folder.display()))?
    {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let measure: Measure = serde_json::from_str(&std::fs::read_to_string(&path)?)
            .with_context(|| format!("reading {}", path.display()))?;
        let vram = std::fs::read_to_string(path.with_extension("vram"))
            .ok()
            .and_then(|text| parse_vram(&text));
        let texts = std::fs::read_to_string(path.with_extension("texts"))
            .ok()
            .map(|text| {
                text.lines()
                    .map(|line| words_of(line.split_once('\t').map_or(line, |(_, t)| t)))
                    .collect()
            });
        runs.push(Run {
            measure,
            vram,
            texts,
        });
    }
    Ok(runs)
}

/// The Markdown table, one section per recorded file; the order of the runs is the one of the
/// matrix (CPU before GPU, int8 before fp32, greedy before beam). The last column counts the
/// sentences whose words differ from the CPU int8 greedy run of the same file.
fn table(folder: &Path) -> Result<String> {
    let mut by_file: BTreeMap<String, Vec<Run>> = BTreeMap::new();
    for run in load_runs(folder)? {
        by_file
            .entry(run.measure.file.clone())
            .or_default()
            .push(run);
    }
    anyhow::ensure!(
        !by_file.is_empty(),
        "no result (*.json) in {}",
        folder.display()
    );
    let mut out = String::new();
    for (file, mut runs) in by_file {
        runs.sort_by_key(|r| {
            (
                r.measure.provider.clone(),
                std::cmp::Reverse(r.measure.precision.clone()),
                r.measure.decoding == "beam",
                r.measure.vad_provider.clone(),
            )
        });
        let baseline = runs.iter().find(|r| {
            (
                r.measure.provider.as_str(),
                r.measure.precision.as_str(),
                r.measure.decoding.as_str(),
            ) == ("cpu", "int8", "greedy")
        });
        let first = &runs[0].measure;
        out.push_str(&format!(
            "### {file}: {} sentences, {:.1} s of speech in {:.1} s of audio, {} rounds\n\n",
            first.sentences,
            first.speech_s,
            first.audio_s,
            first.rounds_s.len()
        ));
        out.push_str(
            "| Run | Load (s) | Warm-up (s) | Best round (s) | Median round (s) | Per sentence (ms) \
             | Slowest sentence (ms) | CPU used (cores) | VRAM process (MiB) | VRAM card, peak (MiB) \
             | Silero (ms per s of audio) | Sentences with other words |\n\
             |---|---|---|---|---|---|---|---|---|---|---|---|\n",
        );
        for run in &runs {
            let m = &run.measure;
            let best = m.rounds_s.iter().copied().fold(f64::INFINITY, f64::min);
            let slowest = m.sentence_ms.iter().copied().fold(0.0, f64::max);
            let cores = m
                .cpu_s
                .map_or("?".to_owned(), |c| format!("{:.1}", c / m.wall_s));
            let (process, card) = match &run.vram {
                Some(v) => (v.process_peak.to_string(), v.total_peak.to_string()),
                None => ("-".to_owned(), "-".to_owned()),
            };
            let differing = match (&run.texts, baseline.and_then(|b| b.texts.as_ref())) {
                (Some(texts), Some(base)) if texts.len() == base.len() => texts
                    .iter()
                    .zip(base)
                    .filter(|(a, b)| a != b)
                    .count()
                    .to_string(),
                _ => "-".to_owned(),
            };
            let words = if m.hotwords > 0 {
                format!(" + {} hotwords", m.hotwords)
            } else {
                String::new()
            };
            let vad = if m.vad_provider == "cuda" {
                ", Silero on CUDA"
            } else {
                ""
            };
            out.push_str(&format!(
                "| {} {} {}{words}{vad} | {:.1} | {:.2} | {best:.2} | {:.2} | {:.0} | {slowest:.0} \
                 | {cores} | {process} | {card} | {:.1} | {differing} |\n",
                m.provider,
                m.precision,
                m.decoding,
                m.asr_load_ms as f64 / 1000.0,
                m.asr_warm_up_ms as f64 / 1000.0,
                median(&m.rounds_s),
                best * 1000.0 / m.sentences as f64,
                m.vad_ms as f64 / m.audio_s,
            ));
        }
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measure(provider: &str, precision: &str, decoding: &str, rounds: &[f64]) -> Measure {
        Measure {
            tag: format!("{provider}-{precision}-{decoding}"),
            file: "a.wav".to_owned(),
            provider: provider.to_owned(),
            precision: precision.to_owned(),
            decoding: decoding.to_owned(),
            hotwords: 0,
            vad_provider: "cpu".to_owned(),
            threads: 4,
            audio_s: 10.0,
            speech_s: 8.0,
            sentences: 2,
            vad_load_ms: 5,
            vad_ms: 40,
            asr_load_ms: 2500,
            asr_warm_up_ms: 300,
            rounds_s: rounds.to_vec(),
            sentence_ms: vec![400.0, 600.0],
            cpu_s: Some(8.0),
            wall_s: rounds.iter().sum(),
        }
    }

    #[test]
    fn the_cpu_time_is_read_after_the_command_name() {
        // A name with spaces and a parenthesis, as a thread or a program can have.
        let stat = "123 (hearing (x) y) S 1 123 123 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 5 0";
        assert_eq!(cpu_seconds_from_stat(stat), Some(3.0));
        assert_eq!(cpu_seconds_from_stat("garbage"), None);
    }

    #[test]
    fn the_vram_lines_give_the_peaks_and_the_baseline() {
        let text = "0 1200\n0 1200\n750 1950\n3100 4300\n3100 4300\n0 1200\n";
        assert_eq!(
            parse_vram(text),
            Some(Vram {
                baseline_total: 1200,
                process_peak: 3100,
                total_peak: 4300
            })
        );
        assert_eq!(parse_vram(""), None);
    }

    #[test]
    fn the_words_ignore_case_and_punctuation() {
        assert_eq!(
            words_of("Emory, rappelle-moi ça !"),
            words_of("emory rappelle moi ça")
        );
        assert_ne!(words_of("un kit"), words_of("akit"));
    }

    #[test]
    fn the_table_compares_each_run_with_the_cpu_int8_greedy_one() {
        let folder = std::env::temp_dir().join(format!("measure-table-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let runs = [
            (
                "cpu-int8-greedy",
                measure("cpu", "int8", "greedy", &[5.0, 4.5, 4.8]),
                "0\tUn kit, deux.\n1\tBonjour\n",
                None,
            ),
            (
                "cuda-fp32-greedy",
                measure("cuda", "fp32", "greedy", &[2.0, 1.5, 1.8]),
                "0\tun kit deux\n1\tbonjour tout\n",
                Some("0 1200\n3100 4300\n"),
            ),
        ];
        for (name, run, texts, vram) in runs {
            std::fs::write(
                folder.join(format!("{name}.json")),
                serde_json::to_string(&run).unwrap(),
            )
            .unwrap();
            std::fs::write(folder.join(format!("{name}.texts")), texts).unwrap();
            if let Some(vram) = vram {
                std::fs::write(folder.join(format!("{name}.vram")), vram).unwrap();
            }
        }
        let table = table(&folder).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();
        assert!(table.contains("### a.wav: 2 sentences"), "{table}");
        let lines: Vec<&str> = table.lines().collect();
        let cpu = lines
            .iter()
            .find(|l| l.starts_with("| cpu int8 greedy"))
            .unwrap();
        let gpu = lines
            .iter()
            .find(|l| l.starts_with("| cuda fp32 greedy"))
            .unwrap();
        // Best round 4.50 s for 2 sentences, 1.7 cores (8 s of CPU in 14.3 s of wall).
        assert!(
            cpu.contains("| 4.50 |") && cpu.contains("| 2250 |"),
            "{cpu}"
        );
        assert!(
            cpu.contains("| - | - |") && cpu.trim_end().ends_with("| 0 |"),
            "{cpu}"
        );
        assert!(
            gpu.contains("| 3100 | 4300 |") && gpu.trim_end().ends_with("| 1 |"),
            "{gpu}"
        );
    }
}
