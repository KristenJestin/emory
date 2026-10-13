//! Probe of today's path, for the talker bench: plays turns on a bus like the hearing would
//! (`hearing.turn`) and measures how long the running `mind` takes to say its first sentence
//! (`voice.say`). Run against a `mind` on a bus of its own, never the development one.
//!
//! ```sh
//! cargo run -p mind --example acp-probe -- nats://127.0.0.1:4227 "first question" "second one"
//! ```

use std::time::{Duration, Instant};

use contract::{Device, Say, Turn, TurnId, Wake, subjects};
use futures_util::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let url = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("a bus url is needed"))?;
    let client = async_nats::connect(url).await?;
    let mut says = client.subscribe(subjects::SAY).await?;
    for text in args {
        let id = TurnId::new();
        let now = contract::now();
        let turn = Turn {
            turn: id,
            device: Device::new("home")?,
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
                sentences += 1;
                first.get_or_insert(started.elapsed().as_millis());
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
