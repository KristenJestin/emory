//! Test tool: presses the button of a device (`Button` `start`, a new turn) on the bus, to check
//! that the mouth falls silent.
//!
//! ```text
//! mouth-press [--nats-url URL] [--device NAME] [--after-ms N]
//! ```

use std::time::Duration;

use anyhow::{Context, bail};
use contract::{Button, Device, State, TurnId, subjects};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut url = "nats://127.0.0.1:4222".to_owned();
    let mut device = Device::new("home")?;
    let mut after = Duration::ZERO;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().with_context(|| format!("{name} expects a value"));
        match arg.as_str() {
            "--nats-url" => url = value("--nats-url")?,
            "--device" => device = Device::new(value("--device")?)?,
            "--after-ms" => after = Duration::from_millis(value("--after-ms")?.parse()?),
            _ => bail!("unknown option: {arg}"),
        }
    }
    let client = async_nats::connect(&url)
        .await
        .with_context(|| format!("connecting to NATS {url}"))?;
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
