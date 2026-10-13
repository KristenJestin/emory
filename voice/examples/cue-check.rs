//! Checks the waiting cue end to end on a bus of its own: publishes `voice.cue` and `voice.say`
//! for a fake turn and reads what the voice sends to the mouth, with its timeline. Needs a running
//! `voice` (and its engine, for a phrase or an answer) on the same bus. Nothing is played.
//!
//! ```text
//! cue-check [--nats-url nats://127.0.0.1:4228] [--device try] SCENARIO
//!   tone-pulse-answer   a tone, a pulse (the voice repeats it), then the answer: no pulse after it
//!   phrase-answer       a phrase, then the answer arrives while it is said: the answer follows
//!   stop                a pulse, then voice.stop: no pulse after the stop
//! ```

use std::time::{Duration, Instant};

use anyhow::{Context, bail, ensure};
use contract::{
    Cue, CueKind, Device, Say, Stop, TurnId,
    audio::{Format, headers},
    subjects,
};
use futures_util::StreamExt;

struct Chunk {
    at: Duration,
    seq: u32,
    sentence: u32,
    end: bool,
    bytes: usize,
    format: Format,
}

async fn publish<T: serde::Serialize>(
    nats: &async_nats::Client,
    subject: &str,
    message: &T,
) -> anyhow::Result<()> {
    nats.publish(subject.to_owned(), serde_json::to_vec(message)?.into())
        .await?;
    nats.flush().await?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut nats_url = "nats://127.0.0.1:4228".to_owned();
    let mut device = Device::new("try")?;
    let mut scenario = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--nats-url" => nats_url = args.next().context("--nats-url needs a value")?,
            "--device" => device = Device::new(args.next().context("--device needs a value")?)?,
            other if other.starts_with("--") => bail!("unknown option {other}"),
            other => scenario = Some(other.to_owned()),
        }
    }
    let scenario = scenario.context("give a scenario")?;
    let nats = voice::nats::connect_to(&nats_url, None, None, "cue-check").await?;
    let mut audio = nats.subscribe(subjects::mouth_audio(&device)).await?;
    nats.flush().await?;

    let turn = TurnId::new();
    let start = Instant::now();
    // Read as it comes, so that the times are those of the arrival.
    let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let collector = tokio::spawn({
        let received = received.clone();
        async move {
            while let Some(m) = audio.next().await {
                let at = start.elapsed();
                let Some(h) = m.headers.as_ref() else {
                    continue;
                };
                let get = |name: &str| h.get(name).map(|v| v.as_str().to_owned());
                if get(headers::TURN).as_deref() != Some(&turn.to_string()) {
                    continue;
                }
                let chunk = (|| -> anyhow::Result<Chunk> {
                    Ok(Chunk {
                        at,
                        seq: get(headers::SEQ).context("seq")?.parse()?,
                        sentence: get(headers::SENTENCE).context("sentence")?.parse()?,
                        end: get(headers::END).as_deref() == Some("1"),
                        bytes: m.payload.len(),
                        format: get(headers::FORMAT).context("format")?.parse()?,
                    })
                })();
                if let Ok(chunk) = chunk {
                    received.lock().unwrap().push(chunk);
                }
            }
        }
    });
    let cue = |kind, text: Option<&str>| Cue {
        turn,
        device: device.clone(),
        kind,
        text: text.map(str::to_owned),
    };
    let say = |text: &str| Say {
        turn,
        device: device.clone(),
        index: 0,
        text: text.to_owned(),
        last: true,
    };
    let mut marks: Vec<(&str, Duration)> = Vec::new();
    let listen = match scenario.as_str() {
        "tone-pulse-answer" => {
            publish(&nats, subjects::CUE, &cue(CueKind::Tone, None)).await?;
            tokio::time::sleep(Duration::from_millis(500)).await;
            publish(&nats, subjects::CUE, &cue(CueKind::Pulse, None)).await?;
            tokio::time::sleep(Duration::from_millis(4000)).await;
            marks.push(("answer sent", start.elapsed()));
            publish(&nats, subjects::SAY, &say("Voilà ce que j'ai trouvé.")).await?;
            Duration::from_secs(6)
        }
        "phrase-answer" => {
            publish(
                &nats,
                subjects::CUE,
                &cue(CueKind::Speech, Some("Je regarde ce que j'ai.")),
            )
            .await?;
            // Long enough for the first sound of the phrase to be out, short of its end.
            tokio::time::sleep(Duration::from_millis(900)).await;
            marks.push(("answer sent", start.elapsed()));
            publish(&nats, subjects::SAY, &say("Voilà ce que j'ai trouvé.")).await?;
            Duration::from_secs(8)
        }
        "stop" => {
            publish(&nats, subjects::CUE, &cue(CueKind::Pulse, None)).await?;
            tokio::time::sleep(Duration::from_millis(2000)).await;
            marks.push(("stop sent", start.elapsed()));
            publish(
                &nats,
                subjects::STOP,
                &Stop {
                    turn,
                    device: device.clone(),
                },
            )
            .await?;
            Duration::from_secs(4)
        }
        other => bail!("unknown scenario {other}"),
    };

    tokio::time::sleep(listen).await;
    collector.abort();
    let chunks = std::mem::take(&mut *received.lock().unwrap());

    for (name, at) in &marks {
        println!("{:>7.0} ms  {name}", at.as_secs_f64() * 1e3);
    }
    for c in &chunks {
        let seconds = c.bytes as f64 / (2.0 * f64::from(c.format.sample_rate));
        let who = if c.sentence == voice::cue::SENTENCE {
            "cue".to_owned()
        } else {
            format!("sentence {}", c.sentence)
        };
        println!(
            "{:>7.0} ms  seq {:>2}  {who:<10} {seconds:.2} s{}",
            c.at.as_secs_f64() * 1e3,
            c.seq,
            if c.end { "  end" } else { "" }
        );
    }
    ensure!(!chunks.is_empty(), "nothing came back");
    for (i, c) in chunks.iter().enumerate() {
        ensure!(c.seq == i as u32, "the numbering has a gap at chunk {i}");
    }
    let answer = chunks.iter().find(|c| c.sentence != voice::cue::SENTENCE);
    let cues = || chunks.iter().filter(|c| c.sentence == voice::cue::SENTENCE);
    match scenario.as_str() {
        "tone-pulse-answer" => {
            ensure!(
                cues().count() >= 3,
                "a tone and at least two pulses expected"
            );
            let answer = answer.context("the answer never came")?;
            let after = cues().filter(|c| c.seq > answer.seq).count();
            ensure!(after == 0, "{after} cue chunk(s) after the answer");
            let last = cues().next_back().unwrap();
            println!(
                "\nlast pulse {:.0} ms before the first sound of the answer",
                (answer.at - last.at).as_secs_f64() * 1e3
            );
        }
        "phrase-answer" => {
            let answer = answer.context("the answer never came")?;
            let phrase_end = cues().find(|c| c.end).context("the phrase never ended")?;
            ensure!(
                phrase_end.seq < answer.seq,
                "the answer started before the phrase ended"
            );
            println!("\nthe phrase is whole, then the answer");
        }
        "stop" => {
            ensure!(cues().count() >= 2, "two pulses expected before the stop");
            let (_, stop_at) = marks[0];
            let late = cues()
                .filter(|c| c.at > stop_at + Duration::from_millis(300))
                .count();
            ensure!(late == 0, "{late} pulse(s) after the stop");
            println!("\nno pulse after the stop");
        }
        _ => {}
    }
    println!("OK");
    Ok(())
}
