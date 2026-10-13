//! The voice of Emory: receives the sentences to say (`voice.say`), has them synthesised by the
//! voice engine and sends the audio back to the mouth (`mouth.<name>.audio`). See
//! `specs/voice.md`.

pub mod audio;
pub mod config;
pub mod cue;
pub mod device;
pub mod engine;
pub mod gain;
pub mod loudness;
pub mod nats;
pub mod queue;
pub mod text;

use std::collections::HashMap;

use contract::{Cue, Device, Say, Stop, subjects};
use futures_util::StreamExt;
use tracing::{info, warn};

use config::Config;
use device::Input;

/// Everything the mind says to the voice, on one subscription: `voice.say`, `voice.stop` and
/// `voice.cue`.
/// (`voice.<device>.speaking` has three tokens and is not matched.) One subscription keeps the
/// order of the mind: a `Stop` cannot overtake the sentences sent before it.
const VOICE_SUBJECTS: &str = "voice.*";

/// What a message of [`VOICE_SUBJECTS`] is, from its subject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VoiceMessage {
    Say,
    Stop,
    Cue,
}

fn voice_message(subject: &str) -> Option<VoiceMessage> {
    match subject {
        subjects::SAY => Some(VoiceMessage::Say),
        subjects::STOP => Some(VoiceMessage::Stop),
        subjects::CUE => Some(VoiceMessage::Cue),
        _ => None,
    }
}

/// Runs the voice until the program stops (Ctrl-C or SIGTERM). Fails if the bus subscription ends.
pub async fn run(config: Config) -> anyhow::Result<()> {
    if config.loudness.enabled && config.gain != 1.0 {
        warn!(
            gain = config.gain,
            "[loudness] is enabled: gain is ignored, the target sets the level"
        );
    }
    let nats = nats::connect(&config).await?;
    let engine = engine::Engine::new(
        &config.engine_url,
        engine::Request::of(&config),
        config.engine_idle_timeout(),
        config.engine_concurrency,
    )?;
    match engine.warm_up().await {
        Ok(()) => {
            info!(engine = ?config.engine, url = %config.engine_url, voice = %config.voice(), "engine ready")
        }
        Err(e) => warn!("engine not ready yet, the first sentence will be slow or lost: {e:#}"),
    }
    let mut messages = nats.subscribe(VOICE_SUBJECTS).await?;
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
    let ended = loop {
        tokio::select! {
            message = messages.next() => {
                let Some(message) = message else {
                    break Some(anyhow::anyhow!("the bus subscription {VOICE_SUBJECTS} has ended"));
                };
                match voice_message(&message.subject) {
                    Some(VoiceMessage::Say) => match serde_json::from_slice::<Say>(&message.payload) {
                        Ok(s) => { let _ = inbox_of(&s.device).send(Input::Say(s)); }
                        Err(e) => warn!("unreadable Say: {e}"),
                    },
                    Some(VoiceMessage::Stop) => match serde_json::from_slice::<Stop>(&message.payload) {
                        Ok(s) => { let _ = inbox_of(&s.device).send(Input::Stop(s.turn)); }
                        Err(e) => warn!("unreadable Stop: {e}"),
                    },
                    Some(VoiceMessage::Cue) => match serde_json::from_slice::<Cue>(&message.payload) {
                        Ok(c) => { let _ = inbox_of(&c.device).send(Input::Cue(c)); }
                        Err(e) => warn!("unreadable Cue: {e}"),
                    },
                    None => {}
                }
            }
            () = &mut shutdown => break None,
        }
    };
    info!("stopping");
    for (inbox, handle) in devices.into_values() {
        let _ = inbox.send(Input::Shutdown);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }
    nats.flush().await.ok();
    ended.map_or(Ok(()), Err)
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_voice_subscription_is_dispatched_on_the_subject() {
        assert_eq!(voice_message("voice.say"), Some(VoiceMessage::Say));
        assert_eq!(voice_message("voice.stop"), Some(VoiceMessage::Stop));
        assert_eq!(voice_message("voice.cue"), Some(VoiceMessage::Cue));
        assert_eq!(voice_message("voice.other"), None);
        assert_eq!(voice_message("voice.office.speaking"), None);
    }

    #[test]
    fn the_subscription_carries_say_and_stop_and_not_speaking() {
        // `*` stands for exactly one token.
        let carried = |subject: &str| {
            let (pattern, subject): (Vec<_>, Vec<_>) = (
                VOICE_SUBJECTS.split('.').collect(),
                subject.split('.').collect(),
            );
            pattern.len() == subject.len()
                && pattern
                    .iter()
                    .zip(&subject)
                    .all(|(p, s)| *p == "*" || p == s)
        };
        assert!(carried(subjects::SAY) && carried(subjects::STOP) && carried(subjects::CUE));
        let office = "office".parse().unwrap();
        assert!(!carried(&subjects::speaking(&office)));
    }
}
