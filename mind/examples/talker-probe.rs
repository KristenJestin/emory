//! Probe of the talker, whichever way `mind` reaches the LLM (ACP or an API): plays turns on a
//! bus like an ear and the hearing would (the press, the sentence while the owner speaks, then
//! `hearing.turn`) and measures how long the running `mind` takes to say its first sentence
//! (`voice.say`). Run against a `mind` on a bus of its own, never the development one.
//!
//! A question written `!question` is interrupted: once its first sentence comes, the probe
//! presses again (a barge-in) and measures how long the `voice.stop` of the turn takes.
//!
//! ```sh
//! cargo run -p mind --example talker-probe -- nats://127.0.0.1:4229 "first question" "!second one"
//! ```

use std::time::{Duration, Instant};

use contract::{Button, Device, Say, Sentence, State, Stop, Turn, TurnId, Wake, subjects};
use futures_util::StreamExt;

/// How long the owner speaks before the turn ends: the searches of the sentence run meanwhile.
const SPEAKING: Duration = Duration::from_millis(600);

fn press(turn: TurnId) -> anyhow::Result<Vec<u8>> {
    Ok(serde_json::to_vec(&Button {
        turn,
        state: State::Start,
        at: contract::now(),
    })?)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let url = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("a bus url is needed"))?;
    let client = async_nats::connect(url).await?;
    let device = Device::new("home")?;
    let mut says = client.subscribe(subjects::SAY).await?;
    let mut stops = client.subscribe(subjects::STOP).await?;
    for arg in args {
        let (text, barge_in) = match arg.strip_prefix('!') {
            Some(text) => (text.to_owned(), true),
            None => (arg, false),
        };
        let id = TurnId::new();
        client
            .publish(subjects::button(&device), press(id)?.into())
            .await?;
        let now = contract::now();
        let sentence = Sentence {
            turn: id,
            device: device.clone(),
            index: 0,
            text: text.clone(),
            start: now,
            end: now,
        };
        client
            .publish(subjects::SENTENCE, serde_json::to_vec(&sentence)?.into())
            .await?;
        tokio::time::sleep(SPEAKING).await;
        let now = contract::now();
        let turn = Turn {
            turn: id,
            device: device.clone(),
            start: now,
            end: now,
            text,
            wake: Wake::Button,
            speaker: None,
            transcription_confidence: None,
            context: Vec::new(),
        };
        let started = Instant::now();
        client
            .publish(subjects::TURN, serde_json::to_vec(&turn)?.into())
            .await?;
        let (mut first, mut sentences) = (None, 0);
        loop {
            let Ok(Some(message)) =
                tokio::time::timeout(Duration::from_secs(90), says.next()).await
            else {
                println!("turn: no end after 90 s");
                break;
            };
            let say: Say = serde_json::from_slice(&message.payload)?;
            if say.turn != id {
                continue;
            }
            if !say.text.is_empty() {
                println!("  « {} »", say.text);
                sentences += 1;
                first.get_or_insert(started.elapsed().as_millis());
            }
            if barge_in && first.is_some() {
                let pressed = Instant::now();
                client
                    .publish(subjects::button(&device), press(TurnId::new())?.into())
                    .await?;
                loop {
                    let Ok(Some(message)) =
                        tokio::time::timeout(Duration::from_secs(10), stops.next()).await
                    else {
                        println!("barge-in: no voice.stop after 10 s");
                        break;
                    };
                    let stop: Stop = serde_json::from_slice(&message.payload)?;
                    if stop.turn == id {
                        println!(
                            "first sentence {} ms, barge-in: voice.stop {} ms after the press",
                            first.unwrap_or_default(),
                            pressed.elapsed().as_millis()
                        );
                        break;
                    }
                }
                break;
            }
            if say.last {
                println!(
                    "first sentence {} ms, end {} ms, {} sentences",
                    first.map_or("-".to_owned(), |f| f.to_string()),
                    started.elapsed().as_millis(),
                    sentences
                );
                break;
            }
        }
        // The next turn comes as the owner would speak again, a moment later.
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Ok(())
}
