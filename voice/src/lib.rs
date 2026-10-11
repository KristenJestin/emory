//! The voice of Emory: receives the sentences to say (`voice.say`), has them synthesised by the
//! voice engine and sends the audio back to the mouth (`mouth.<name>.audio`). See
//! `specs/voice.md`.

pub mod audio;
pub mod config;
pub mod device;
pub mod engine;
pub mod nats;
pub mod queue;
pub mod text;

use std::collections::HashMap;

use contract::{Device, Say, Stop, subjects};
use futures_util::StreamExt;
use tracing::{info, warn};

use config::Config;
use device::Input;

/// Runs the voice until the program stops (Ctrl-C or SIGTERM).
pub async fn run(config: Config) -> anyhow::Result<()> {
    let nats = nats::connect(&config).await?;
    let engine = engine::Engine::new(&config.engine_url, &config.voice)?;
    match engine.warm_up().await {
        Ok(()) => info!(engine = %config.engine_url, voice = %config.voice, "engine ready"),
        Err(e) => warn!("engine not ready yet, the first sentence will be slow or lost: {e:#}"),
    }
    let mut say = nats.subscribe(subjects::SAY).await?;
    let mut stop = nats.subscribe(subjects::STOP).await?;
    info!(nats = %config.nats_url, "the voice is listening");

    let mut devices: HashMap<Device, (device::Inbox, tokio::task::JoinHandle<()>)> = HashMap::new();
    let mut inbox_of = |name: &Device| {
        devices
            .entry(name.clone())
            .or_insert_with(|| {
                device::spawn(name.clone(), nats.clone(), engine.clone(), config.clone())
            })
            .0
            .clone()
    };
    let mut shutdown = std::pin::pin!(shutdown_signal());
    loop {
        tokio::select! {
            Some(message) = say.next() => match serde_json::from_slice::<Say>(&message.payload) {
                Ok(s) => { let _ = inbox_of(&s.device).send(Input::Say(s)); }
                Err(e) => warn!("unreadable Say: {e}"),
            },
            Some(message) = stop.next() => match serde_json::from_slice::<Stop>(&message.payload) {
                Ok(s) => { let _ = inbox_of(&s.device).send(Input::Stop(s.turn)); }
                Err(e) => warn!("unreadable Stop: {e}"),
            },
            () = &mut shutdown => break,
            else => break,
        }
    }
    info!("stopping");
    for (inbox, handle) in devices.into_values() {
        let _ = inbox.send(Input::Shutdown);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }
    nats.flush().await.ok();
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}
