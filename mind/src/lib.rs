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

/// The dependencies whose logs, below these levels, carry the messages themselves: the requests
/// to the LLM (what Kris said, the key of Emory's MCP server in `session/new`), the MCP calls and
/// their headers, the bus payloads. Pinned whatever `RUST_LOG` says.
const PINNED_LOGS: [(&str, &str); 7] = [
    ("agent_client_protocol", "warn"),
    ("rmcp", "warn"),
    ("reqwest", "warn"),
    ("hyper", "warn"),
    ("hyper_util", "warn"),
    ("h2", "warn"),
    ("async_nats", "info"),
];

/// The log filter: `RUST_LOG` (`mind=info` without it), less what it says of the pinned
/// dependencies, plus their pinned levels.
pub fn log_filter(env: Option<&str>) -> String {
    let pinned = |directive: &str| {
        let target = directive
            .split(['=', '['])
            .next()
            .unwrap_or_default()
            .trim();
        PINNED_LOGS.iter().any(|(name, _)| {
            target == *name
                || target
                    .strip_prefix(name)
                    .is_some_and(|rest| rest.starts_with("::"))
        })
    };
    let mut directives: Vec<String> = env
        .filter(|e| !e.trim().is_empty())
        .unwrap_or("mind=info")
        .split(',')
        .map(str::trim)
        .filter(|d| !d.is_empty() && !pinned(d))
        .map(str::to_owned)
        .collect();
    directives.extend(
        PINNED_LOGS
            .iter()
            .map(|(name, level)| format!("{name}={level}")),
    );
    directives.join(",")
}

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
        config.hippocampe.tool_timeout(),
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
            nothing_heard: config.conversation.nothing_heard.clone(),
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
    fn the_payload_logs_stay_pinned_whatever_rust_log_says() {
        let filter = log_filter(Some(
            "debug,agent_client_protocol=trace,rmcp::service=debug,mind=debug,rmcpx=debug",
        ));
        let directives: Vec<&str> = filter.split(',').collect();
        assert!(directives.contains(&"debug") && directives.contains(&"mind=debug"));
        assert!(directives.contains(&"rmcpx=debug"));
        assert!(!filter.contains("agent_client_protocol=trace"));
        assert!(!filter.contains("rmcp::service"));
        assert!(directives.contains(&"agent_client_protocol=warn"));
        assert!(directives.contains(&"rmcp=warn"));
        assert!(log_filter(None).starts_with("mind=info,"));
        // The filter parses, and the pinned level wins over a broad one.
        let parsed: tracing_subscriber::EnvFilter = filter.parse().unwrap();
        assert!(parsed.to_string().contains("agent_client_protocol=warn"));
    }

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
