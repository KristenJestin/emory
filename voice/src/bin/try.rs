//! Check the voice end to end without an ear: publishes the `Say` messages of a fake turn on a
//! device name, listens to the voice coming back on `mouth.<name>.audio` and writes it to a WAV.
//!
//! ```text
//! voice-try [--nats-url URL] [--device NAME] [--output voice.wav] [--stop-after-first]
//!           [--no-last] [SENTENCE…]
//! ```
//!
//! The last sentence is sent with `last: true`, unless `--no-last` is given (then `Speaking` `end`
//! comes after the grace delay). Once the last sentence is received, the tool waits for the
//! `Speaking` `end` and prints when it came.
//!
//! With `--stop-after-first`, a `Stop` leaves as soon as the first sentence is entirely received;
//! the tool then waits until nothing more arrives, to check that the rest is not said.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use contract::{
    Device, Say, Speaking, State, Stop, TurnId,
    audio::{Format, headers},
    subjects,
};
use futures_util::StreamExt;

const SENTENCES: [&str; 3] = [
    "Bonjour Kris, les oreilles sont spécifiées et la bouche parle enfin.",
    "Je peux dire plusieurs phrases à la suite, sans attendre que tu aies fini de lire.",
    "Et si tu m'interromps, je me tais tout de suite.",
];

struct Args {
    nats_url: String,
    device: Device,
    output: PathBuf,
    stop_after_first: bool,
    no_last: bool,
    sentences: Vec<String>,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut args = Args {
        nats_url: "nats://127.0.0.1:4222".to_owned(),
        device: Device::new("try")?,
        output: PathBuf::from("voice.wav"),
        stop_after_first: false,
        no_last: false,
        sentences: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().with_context(|| format!("{name} expects a value"));
        match arg.as_str() {
            "--nats-url" => args.nats_url = value("--nats-url")?,
            "--device" => args.device = Device::new(value("--device")?)?,
            "--output" => args.output = value("--output")?.into(),
            "--stop-after-first" => args.stop_after_first = true,
            "--no-last" => args.no_last = true,
            s if s.starts_with("--") => bail!("unknown option: {s}"),
            _ => args.sentences.push(arg),
        }
    }
    if args.sentences.is_empty() {
        args.sentences = SENTENCES.map(str::to_owned).to_vec();
    }
    Ok(args)
}

#[derive(Default)]
struct Received {
    bytes: usize,
    chunks: u32,
    first: Option<Duration>,
    done: Option<Duration>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    let nats = voice::nats::connect_to(&args.nats_url, None, None, "voice-try").await?;
    let mut voice = nats.subscribe(subjects::mouth_audio(&args.device)).await?;
    let mut speaking = nats.subscribe(subjects::speaking(&args.device)).await?;
    nats.flush().await?;

    let turn = TurnId::new();
    let start = Instant::now();
    for (index, text) in args.sentences.iter().enumerate() {
        let say = Say {
            turn,
            device: args.device.clone(),
            index: index as u32,
            text: text.clone(),
            last: !args.no_last && index == args.sentences.len() - 1,
        };
        nats.publish(subjects::SAY, serde_json::to_vec(&say)?.into())
            .await?;
    }
    nats.flush().await?;
    println!(
        "turn {turn}: {} sentences published for {}",
        args.sentences.len(),
        args.device
    );

    let mut sentences: Vec<Received> = args.sentences.iter().map(|_| Received::default()).collect();
    let mut pcm = Vec::new();
    let mut format: Option<Format> = None;
    let mut expected_seq = 0u32;
    let mut stopped = false;
    let mut all_received_at = None;
    // Without `--stop…`: done when the last sentence has its `End`. With it: when the silence lasts.
    let quiet = Duration::from_secs(if args.stop_after_first { 4 } else { 20 });
    loop {
        let message = tokio::select! {
            m = voice.next() => m.context("the bus has closed")?,
            Some(m) = speaking.next() => {
                if let Ok(s) = serde_json::from_slice::<Speaking>(&m.payload) {
                    println!("{:>7.0} ms  Speaking {:?}", start.elapsed().as_secs_f64() * 1e3, s.state);
                }
                continue;
            }
            () = tokio::time::sleep(quiet) => {
                if args.stop_after_first { break } else { bail!("no audio for {quiet:?}") }
            }
        };
        let message_headers = message.headers.as_ref().context("chunk without headers")?;
        let header = |name: &str| message_headers.get(name).map(|v| v.as_str().to_owned());
        if header(headers::TURN).as_deref() != Some(&turn.to_string()) {
            continue;
        }
        let seq: u32 = header(headers::SEQ).context("Emory-Seq missing")?.parse()?;
        if seq != expected_seq {
            println!("!! gap in the audio: expected {expected_seq}, received {seq}");
        }
        expected_seq = seq + 1;
        let f: Format = header(headers::FORMAT)
            .context("Emory-Format missing")?
            .parse()?;
        match format {
            Some(known) if known != f => bail!("the format changes during the turn"),
            _ => format = Some(f),
        }
        let index: usize = header(headers::SENTENCE)
            .context("Emory-Sentence missing")?
            .parse()?;
        let last = header(headers::END).as_deref() == Some("1");
        let s = sentences.get_mut(index).context("unknown sentence index")?;
        let at = start.elapsed();
        if !message.payload.is_empty() {
            s.first.get_or_insert(at);
        }
        s.bytes += message.payload.len();
        s.chunks += 1;
        pcm.extend_from_slice(&message.payload);
        if last {
            s.done = Some(at);
            if args.stop_after_first && index == 0 && !stopped {
                stopped = true;
                let stop = Stop {
                    turn,
                    device: args.device.clone(),
                };
                nats.publish(subjects::STOP, serde_json::to_vec(&stop)?.into())
                    .await?;
                nats.flush().await?;
                println!("{:>7.0} ms  Stop sent", at.as_secs_f64() * 1e3);
            }
            if !args.stop_after_first && index == sentences.len() - 1 {
                all_received_at = Some(at);
                break;
            }
        }
    }

    let format = format.context("no audio received")?;
    write_wav(&args.output, &pcm, format)?;
    println!(
        "\nfile: {} ({:.2} s of voice, {})",
        args.output.display(),
        seconds(pcm.len(), format),
        format
    );
    for (i, s) in sentences.iter().enumerate() {
        let audio = seconds(s.bytes, format);
        match (s.first, s.done) {
            (Some(first), Some(done)) => println!(
                "sentence {i}: first sound {:>4.0} ms after sending, {} chunks, {:.2} s of voice, done at {:>5.0} ms ({}× faster than real time)",
                first.as_secs_f64() * 1e3,
                s.chunks,
                audio,
                done.as_secs_f64() * 1e3,
                if done > first {
                    format!("{:.1}", audio / (done - first).as_secs_f64())
                } else {
                    "?".to_owned()
                },
            ),
            (None, None) if s.chunks == 0 => println!("sentence {i}: nothing received"),
            _ => println!(
                "sentence {i}: {} chunks, {:.2} s of voice, no end announced",
                s.chunks, audio
            ),
        }
    }
    if let Some(received_at) = all_received_at {
        // The `Speaking` `end` comes when the mouth has finished playing: wait for it.
        let first_sound = sentences.first().and_then(|s| s.first);
        let wait = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(m) = speaking.next().await {
                if let Ok(s) = serde_json::from_slice::<Speaking>(&m.payload) {
                    let at = start.elapsed();
                    println!("{:>7.0} ms  Speaking {:?}", at.as_secs_f64() * 1e3, s.state);
                    if s.state == State::End {
                        return at;
                    }
                }
            }
            Duration::MAX
        })
        .await;
        match wait {
            Ok(at) if at != Duration::MAX => {
                println!(
                    "\nSpeaking end: {:.0} ms after the last chunk was received",
                    at.saturating_sub(received_at).as_secs_f64() * 1e3
                );
                if let Some(first) = first_sound {
                    let played_until = first + Duration::from_secs_f64(seconds(pcm.len(), format));
                    println!(
                        "              {:+.0} ms compared with the end of the audio played from the first sound",
                        (at.as_secs_f64() - played_until.as_secs_f64()) * 1e3
                    );
                }
            }
            _ => println!("\nSpeaking end: not received within 10 s"),
        }
    }
    if args.stop_after_first {
        let said = sentences.iter().skip(1).filter(|s| s.chunks > 0).count();
        println!(
            "\nStop: {}",
            if said == 0 {
                "the following sentences are not said".to_owned()
            } else {
                format!("ERROR, {said} sentence(s) said despite the Stop")
            }
        );
        if said > 0 {
            std::process::exit(1);
        }
    }
    Ok(())
}

fn seconds(bytes: usize, format: Format) -> f64 {
    bytes as f64 / (2.0 * format.channels as f64 * format.sample_rate as f64)
}

fn write_wav(path: &std::path::Path, pcm: &[u8], format: Format) -> anyhow::Result<()> {
    let rate = format.sample_rate;
    let block = format.channels * 2;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend(b"RIFF");
    wav.extend((36 + pcm.len() as u32).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(format.channels.to_le_bytes());
    wav.extend(rate.to_le_bytes());
    wav.extend((rate * block as u32).to_le_bytes());
    wav.extend(block.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend((pcm.len() as u32).to_le_bytes());
    wav.extend(pcm);
    std::fs::write(path, wav).with_context(|| format!("writing {}", path.display()))
}
