//! `hearing-replay [--nats URL] [--ca PEM] [--nkey SEED] [--device NAME] [--no-end] file.wav`
//!
//! Plays a WAV file on the bus the way an ear would (`start`, 20 ms chunks at real-time pace,
//! `end`; with `--no-end`, the ear "dies" without sending it), and prints the sentences and the
//! turn that the hearing sends back. It is the end-to-end try of the hearing. The file must be
//! 16 kHz mono 16-bit: `ffmpeg -i input.wav -ar 16000 -ac 1 -c:a pcm_s16le output.wav`.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use async_nats::HeaderMap;
use contract::{Button, Device, Sentence, State, Turn, TurnId, audio, subjects};
use futures_util::StreamExt;
use hearing::{
    bus::{self, publish},
    config::{ConfigPath, NatsConfig},
};
use tokio::time::sleep_until;

/// How long we wait for the turn after the end.
const TURN_TIMEOUT: Duration = Duration::from_secs(60);

struct Arguments {
    nats: NatsConfig,
    device: Device,
    file: String,
    no_end: bool,
}

fn arguments() -> Result<Arguments> {
    let mut nats = NatsConfig::default();
    let mut device = "office".parse()?;
    let mut file = None;
    let mut no_end = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .with_context(|| format!("{name} expects a value"))
        };
        match arg.as_str() {
            "--nats" => nats.url = value("--nats")?,
            "--ca" => nats.ca = Some(ConfigPath(value("--ca")?.into())),
            "--nkey" => nats.nkey = Some(ConfigPath(value("--nkey")?.into())),
            "--device" => device = value("--device")?.parse()?,
            "--no-end" => no_end = true,
            _ if arg.starts_with("--") => bail!("unknown option: {arg}"),
            _ => file = Some(arg),
        }
    }
    let file = file.context(
        "usage: hearing-replay [--nats URL] [--ca PEM] [--nkey SEED] [--device NAME] [--no-end] file.wav",
    )?;
    Ok(Arguments {
        nats,
        device,
        file,
        no_end,
    })
}

fn read_wav(path: &str) -> Result<Vec<i16>> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("opening {path}"))?;
    let spec = reader.spec();
    if spec.sample_rate != audio::MIC.sample_rate
        || spec.channels != audio::MIC.channels
        || spec.sample_format != hound::SampleFormat::Int
        || spec.bits_per_sample != 16
    {
        bail!(
            "{path}: {} Hz, {} channel, {} bits; 16 kHz mono 16-bit is needed \
             (ffmpeg -i input.wav -ar 16000 -ac 1 -c:a pcm_s16le output.wav)",
            spec.sample_rate,
            spec.channels,
            spec.bits_per_sample
        );
    }
    Ok(reader.samples::<i16>().collect::<Result<_, _>>()?)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = arguments()?;
    let wav = read_wav(&args.file)?;
    let chunk = audio::MIC_CHUNK_BYTES / 2;
    println!(
        "{}: {:.1} s, {} chunks of 20 ms",
        args.file,
        wav.len() as f64 / audio::MIC.sample_rate as f64,
        wav.len().div_ceil(chunk)
    );

    let client = bus::connect(&args.nats, "hearing-replay", false).await?;
    let turn = TurnId::new();
    let mut sentences = client.subscribe(subjects::SENTENCE).await?;
    let mut turns = client.subscribe(subjects::TURN).await?;
    client.flush().await?;

    let started = Instant::now();
    let clock = move || format!("{:>6.2} s", started.elapsed().as_secs_f64());

    // What the hearing sends back, printed as it comes.
    let listening = tokio::spawn({
        async move {
            loop {
                tokio::select! {
                    biased;
                    Some(m) = sentences.next() => {
                        let Ok(s) = serde_json::from_slice::<Sentence>(&m.payload) else { continue };
                        if s.turn != turn { continue }
                        println!("{}  sentence {} [{} → {}]: {}", clock(), s.index,
                            s.start.format("%H:%M:%S%.3f"), s.end.format("%H:%M:%S%.3f"), s.text);
                    }
                    Some(m) = turns.next() => {
                        let Ok(t) = serde_json::from_slice::<Turn>(&m.payload) else { continue };
                        if t.turn != turn { continue }
                        return Ok((t, Instant::now()));
                    }
                    else => bail!("the bus has closed"),
                }
            }
        }
    });

    let button = |state| Button {
        turn,
        state,
        at: contract::now(),
    };
    publish(
        &client,
        subjects::button(&args.device),
        &button(State::Start),
    )
    .await?;
    println!("{}  turn started {turn}", clock());

    let audio_subject = subjects::ear_audio(&args.device);
    let begin = tokio::time::Instant::now();
    for (seq, samples) in wav.chunks(chunk).enumerate() {
        sleep_until(begin + audio::MIC_CHUNK * seq as u32).await;
        let mut headers = HeaderMap::new();
        headers.insert(audio::headers::TURN, turn.to_string());
        headers.insert(audio::headers::SEQ, seq.to_string());
        headers.insert(audio::headers::FORMAT, audio::MIC.to_string());
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        client
            .publish_with_headers(audio_subject.clone(), headers, bytes.into())
            .await?;
    }
    sleep_until(begin + audio::MIC_CHUNK * wav.len().div_ceil(chunk) as u32).await;
    let end_instant = Instant::now();
    if args.no_end {
        println!("{}  the ear dies: no end", clock());
    } else {
        publish(&client, subjects::button(&args.device), &button(State::End)).await?;
        println!("{}  turn ended", clock());
    }

    let (received_turn, received) = tokio::time::timeout(TURN_TIMEOUT, listening)
        .await
        .context("no turn received")???;
    println!("{}  turn: {:?}", clock(), received_turn.text);
    println!(
        "speaker {:?}, wake {:?}",
        received_turn.speaker.map(|s| (s.voice, s.origin)),
        received_turn.wake
    );
    println!(
        "from the end to the turn: {:.0} ms",
        received.duration_since(end_instant).as_secs_f64() * 1000.0
    );
    Ok(())
}
