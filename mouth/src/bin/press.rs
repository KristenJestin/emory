//! Test tool: presses the button of a device (`Button` `start`, a new turn) on the bus, to check
//! that the mouth falls silent.
//!
//! With `--announce` it presses nothing: it sends `announce.request` and prints the answer of the
//! mouth.
//!
//! ```text
//! mouth-press [--nats-url URL] [--device NAME] [--after-ms N] [--announce]
//! ```

use std::time::Duration;

use anyhow::{Context, bail};
use contract::{Button, Device, State, TurnId, subjects};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut url = "nats://127.0.0.1:4222".to_owned();
    let mut device = Device::new("home")?;
    let mut after = Duration::ZERO;
    let mut announce = false;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().with_context(|| format!("{name} expects a value"));
        match arg.as_str() {
            "--nats-url" => url = value("--nats-url")?,
            "--device" => device = Device::new(value("--device")?)?,
            "--announce" => announce = true,
            "--after-ms" => after = Duration::from_millis(value("--after-ms")?.parse()?),
            _ => bail!("unknown option: {arg}"),
        }
    }
    let client = async_nats::connect(&url)
        .await
        .with_context(|| format!("connecting to NATS {url}"))?;
    if announce {
        use futures_util::StreamExt;
        let mut announces = client.subscribe(subjects::mouth_announce(&device)).await?;
        client.flush().await?;
        client
            .publish(subjects::ANNOUNCE_REQUEST, Vec::new().into())
            .await?;
        client.flush().await?;
        let message = tokio::time::timeout(Duration::from_secs(2), announces.next())
            .await
            .context("no announcement within 2 s")?
            .context("the bus has closed")?;
        println!("{}", String::from_utf8_lossy(&message.payload));
        return Ok(());
    }
    tokio::time::sleep(after).await;
    let button = Button {
        turn: TurnId::new(),
        state: State::Start,
        at: contract::now(),
    };
    client
        .publish(
            subjects::button(&device),
            serde_json::to_vec(&button)?.into(),
        )
        .await?;
    client.flush().await?;
    println!("Button start of turn {} sent for {device}", button.turn);
    Ok(())
}
