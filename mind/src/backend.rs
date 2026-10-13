//! The LLM `[llm]` chooses: through an ACP agent, or straight through a provider's API. Both are
//! a [`Brain`]; the mind does not know which one it talks to.

use std::future::Future;

use anyhow::Context;
use tokio::sync::mpsc;
use tracing::info;

use crate::{
    acp::{AcpBrain, SessionSettings},
    api::{ApiBrain, ApiSettings, Provider},
    brain::{Brain, Event, SessionId},
    config::{LlmConfig, LlmMode, ToolsConfig},
    conversations::ConversationKey,
    tools::{self, Tools},
};

/// The longest answer asked of an API when `llm.max_tokens` is absent, thinking included.
const DEFAULT_MAX_TOKENS: u32 = 2048;

pub enum Backend {
    Acp(AcpBrain),
    Api(ApiBrain),
}

impl Backend {
    /// Starts the LLM of the configuration (checked already) with Emory's tools: for ACP, they
    /// are served on the loopback to the adapter; for an API, called in the process.
    pub async fn start(
        llm: &LlmConfig,
        tools_config: &ToolsConfig,
        system_prompt: String,
        tools: Tools,
    ) -> anyhow::Result<Self> {
        llm.check_files()?;
        match llm.mode {
            LlmMode::Acp => {
                let server = tools::serve(tools, tools_config.bind).await?;
                let workdir = match &llm.workdir {
                    Some(dir) => dir.0.clone(),
                    None => std::env::temp_dir().join("emory-mind"),
                };
                std::fs::create_dir_all(&workdir).with_context(|| {
                    format!("creating the working folder {}", workdir.display())
                })?;
                let effort = llm.acp_effort();
                let brain = AcpBrain::start(
                    llm.command.as_deref().unwrap_or_default(),
                    &llm.args,
                    SessionSettings {
                        system_prompt,
                        model: llm.model.clone(),
                        effort: effort.clone(),
                        thinking: llm.thinking,
                        fast: llm.fast,
                        workdir,
                        mcp_url: server.url,
                        mcp_key: server.key,
                    },
                )
                .await?;
                info!(model = %llm.model, %effort, fast = llm.fast, "LLM adapter started (ACP)");
                Ok(Self::Acp(brain))
            }
            LlmMode::Api => {
                let provider = llm
                    .provider
                    .as_deref()
                    .and_then(Provider::named)
                    .context("llm.provider")?;
                let brain = ApiBrain::new(
                    ApiSettings {
                        provider,
                        model: llm.model.clone(),
                        base_url: llm.base_url.clone(),
                        key_file: llm.key_file.as_ref().map(|k| k.0.clone()),
                        system_prompt,
                        thinking: llm.thinking,
                        effort: llm.effort.clone(),
                        max_tokens: llm.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
                        cache: llm.cache.unwrap_or(true),
                        fast: llm.fast,
                    },
                    tools,
                )?;
                info!(
                    provider = provider.name,
                    model = %llm.model,
                    thinking = ?llm.thinking,
                    fast = llm.fast,
                    "LLM ready (API)"
                );
                Ok(Self::Api(brain))
            }
        }
    }

    /// Resolves when the LLM is gone for good: the ACP adapter died. An API never is.
    pub async fn closed(&self) {
        match self {
            Self::Acp(brain) => brain.closed().await,
            Self::Api(_) => std::future::pending().await,
        }
    }
}

impl Brain for Backend {
    fn open(
        &self,
        conversation: &ConversationKey,
    ) -> impl Future<Output = anyhow::Result<SessionId>> + Send {
        let conversation = conversation.clone();
        async move {
            match self {
                Self::Acp(brain) => brain.open(&conversation).await,
                Self::Api(brain) => brain.open(&conversation).await,
            }
        }
    }

    fn prompt(&self, session: &SessionId, text: String) -> mpsc::UnboundedReceiver<Event> {
        match self {
            Self::Acp(brain) => brain.prompt(session, text),
            Self::Api(brain) => brain.prompt(session, text),
        }
    }

    fn cancel(&self, session: &SessionId) {
        match self {
            Self::Acp(brain) => brain.cancel(session),
            Self::Api(brain) => brain.cancel(session),
        }
    }

    fn close(&self, session: &SessionId) {
        match self {
            Self::Acp(brain) => brain.close(session),
            Self::Api(brain) => brain.close(session),
        }
    }
}
