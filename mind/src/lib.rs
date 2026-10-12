//! The mind of Emory: receives what the ears heard (`hearing.sentence`, `hearing.turn`, the
//! presses of `ear.<name>.button`), keeps the conversation, asks the LLM through ACP with what the
//! memory holds, and sends the answer sentence by sentence to the voice (`voice.say`,
//! `voice.stop`). See `specs/mind.md`.

pub mod acp;
pub mod brain;
pub mod bus;
pub mod config;
pub mod hippocampe;
pub mod memory;
pub mod mind;
pub mod request;
pub mod sentences;
pub mod tools;

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use contract::{Button, Sentence, Turn, subjects};
use futures_util::StreamExt;
use tracing::{info, warn};

use crate::{
    acp::{AcpBrain, SessionSettings},
    config::Config,
    hippocampe::Hippocampe,
    mind::{Input, Mind, Settings},
    tools::{HippocampeTools, Tools},
};

/// What the hearing says, on one subscription, so that the sentences of a turn come before it:
/// `hearing.sentence` and `hearing.turn`.
const HEARING_SUBJECTS: &str = "hearing.*";
/// How often the idle conversations are looked for.
const TICK: Duration = Duration::from_secs(10);

/// What a message of [`HEARING_SUBJECTS`] is, from its subject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HearingMessage {
    Sentence,
    Turn,
}

fn hearing_message(subject: &str) -> Option<HearingMessage> {
    match subject {
        subjects::SENTENCE => Some(HearingMessage::Sentence),
        subjects::TURN => Some(HearingMessage::Turn),
        _ => None,
    }
}

/// Runs the mind until the program stops (Ctrl-C or SIGTERM). Fails if the adapter of the LLM or a
/// bus subscription ends.
pub async fn run(config: Config) -> Result<()> {
    let system_prompt = std::fs::read_to_string(&config.llm.system_prompt).with_context(|| {
        format!(
            "reading the system prompt {}",
            config.llm.system_prompt.0.display()
        )
    })?;

    let hippocampe = Hippocampe::new(
        config.hippocampe.url.clone(),
        config.hippocampe.token_file.0.clone(),
    );
    hippocampe.connect_in_background();
    let tools = Tools::new(vec![Arc::new(HippocampeTools::new(
        hippocampe.clone(),
        config.hippocampe.tools.clone(),
    ))]);
    let server = tools::serve(tools, config.tools.bind).await?;

    let workdir = match &config.llm.workdir {
        Some(dir) => dir.0.clone(),
        None => std::env::temp_dir().join("emory-mind"),
    };
    std::fs::create_dir_all(&workdir)
        .with_context(|| format!("creating the working folder {}", workdir.display()))?;
    let brain = AcpBrain::start(
        &config.llm.command,
        &config.llm.args,
        SessionSettings {
            system_prompt,
            model: config.llm.model.clone(),
            effort: config.llm.effort.clone(),
            workdir,
            mcp_url: server.url,
            mcp_key: server.key,
        },
    )
    .await?;
    info!(model = %config.llm.model, effort = %config.llm.effort, "LLM adapter started");

    let client = bus::connect(&config.nats, "mind", true).await?;
    info!(url = %config.nats.url, "connected to the bus");
    let mut hearing = client.subscribe(HEARING_SUBJECTS).await?;
    let mut buttons = client.subscribe(subjects::ALL_BUTTONS).await?;

    let brain = Arc::new(brain);
    let mut mind = Mind::new(
        brain.clone(),
        Arc::new(hippocampe),
        Arc::new(bus::Bus(client.clone())),
        Settings {
            idle_timeout: config.conversation.idle_timeout(),
            search_timeout: config.hippocampe.search_timeout(),
            search_limit: config.hippocampe.search_limit,
        },
    );
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut shutdown = std::pin::pin!(shutdown_signal());
    info!("the mind is listening");
    let ended = loop {
        let input = tokio::select! {
            message = hearing.next() => {
                let Some(message) = message else {
                    break Some(anyhow::anyhow!("the bus subscription {HEARING_SUBJECTS} has ended"));
                };
                match hearing_message(&message.subject) {
                    Some(HearingMessage::Sentence) => match serde_json::from_slice::<Sentence>(&message.payload) {
                        Ok(s) => Input::Sentence(s),
                        Err(e) => { warn!("unreadable Sentence: {e}"); continue; }
                    },
                    Some(HearingMessage::Turn) => match serde_json::from_slice::<Turn>(&message.payload) {
                        Ok(t) => Input::Turn(t),
                        Err(e) => { warn!("unreadable Turn: {e}"); continue; }
                    },
                    None => continue,
                }
            }
            message = buttons.next() => {
                let Some(message) = message else {
                    break Some(anyhow::anyhow!("the bus subscription {} has ended", subjects::ALL_BUTTONS));
                };
                let Some(device) = subjects::device_of_subject(&message.subject) else { continue };
                match serde_json::from_slice::<Button>(&message.payload) {
                    Ok(b) => Input::Button(device, b),
                    Err(e) => { warn!("unreadable Button: {e}"); continue; }
                }
            }
            _ = tick.tick() => Input::Tick,
            () = brain.closed() => break Some(anyhow::anyhow!("the LLM adapter has stopped")),
            () = &mut shutdown => break None,
        };
        mind.handle(input).await;
    };
    info!("stopping");
    client.flush().await.ok();
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
    fn the_hearing_subscription_is_dispatched_on_the_subject() {
        assert_eq!(
            hearing_message("hearing.sentence"),
            Some(HearingMessage::Sentence)
        );
        assert_eq!(hearing_message("hearing.turn"), Some(HearingMessage::Turn));
        assert_eq!(hearing_message("hearing.other"), None);
    }
}
