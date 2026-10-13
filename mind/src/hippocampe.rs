//! Hippocampe, the long-term memory: one MCP connection kept open, for the searches made while
//! the owner speaks, the heads-up of a new conversation, and the tools relayed to the LLM.
//!
//! The key is read from its file each time the connection opens, and never logged.

use std::{
    path::PathBuf,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, anyhow};
use rmcp::{
    RoleClient, ServiceExt,
    model::{CallToolRequestParams, CallToolResult, ContentBlock, JsonObject, Tool},
    service::RunningService,
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::json;
use tracing::{info, warn};

use crate::memory::{Found, Memory, found_in, heads_up_of};

/// How long opening the connection may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

type Connection = Arc<RunningService<RoleClient, ()>>;

pub struct Hippocampe {
    url: String,
    token_file: PathBuf,
    connection: RwLock<Option<Connection>>,
    connecting: AtomicBool,
    /// One opening at a time: a caller that comes during one takes its connection.
    opening: tokio::sync::Mutex<()>,
}

impl Hippocampe {
    pub fn new(url: impl Into<String>, token_file: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            url: url.into(),
            token_file: token_file.into(),
            connection: RwLock::new(None),
            connecting: AtomicBool::new(false),
            opening: tokio::sync::Mutex::new(()),
        })
    }

    /// Opens the connection now, waiting for it; if another call is opening it, waits for that
    /// one instead.
    pub async fn connect(&self) -> anyhow::Result<Connection> {
        let _one = self.opening.lock().await;
        if let Some(c) = self.current() {
            return Ok(c);
        }
        let token = std::fs::read_to_string(&self.token_file)
            .with_context(|| format!("reading the key {}", self.token_file.display()))?;
        let transport = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(self.url.as_str())
                .auth_header(token.trim()),
        );
        let service = tokio::time::timeout(CONNECT_TIMEOUT, ().serve(transport))
            .await
            .map_err(|_| anyhow!("connecting to Hippocampe: no answer"))?
            .map_err(|e| anyhow!("connecting to Hippocampe: {e}"))?;
        let connection = Arc::new(service);
        *self.connection.write().unwrap() = Some(connection.clone());
        info!(url = %self.url, "connected to Hippocampe");
        Ok(connection)
    }

    /// Opens the connection in the background, unless it is already being opened.
    pub fn connect_in_background(self: &Arc<Self>) {
        if self.connecting.swap(true, Ordering::AcqRel) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(e) = this.connect().await {
                warn!("{e:#}");
            }
            this.connecting.store(false, Ordering::Release);
        });
    }

    fn current(&self) -> Option<Connection> {
        self.connection
            .read()
            .unwrap()
            .clone()
            .filter(|c| !c.is_closed())
    }

    /// The open connection; when there is none, `wait` opens one, otherwise it is opened in the
    /// background and this call fails at once.
    async fn connection(self: &Arc<Self>, wait: bool) -> anyhow::Result<Connection> {
        if let Some(c) = self.current() {
            return Ok(c);
        }
        if wait {
            self.connect().await
        } else {
            self.connect_in_background();
            Err(anyhow!("not connected to Hippocampe yet"))
        }
    }

    /// The connection failed: the next call opens another one.
    fn forget(&self, broken: &Connection) {
        let mut connection = self.connection.write().unwrap();
        if connection.as_ref().is_some_and(|c| Arc::ptr_eq(c, broken)) {
            *connection = None;
        }
    }

    async fn call_on(
        self: &Arc<Self>,
        wait: bool,
        name: &str,
        arguments: Option<JsonObject>,
    ) -> anyhow::Result<CallToolResult> {
        let connection = self.connection(wait).await?;
        let mut params = CallToolRequestParams::new(name.to_owned());
        params.arguments = arguments;
        match connection.call_tool(params).await {
            Ok(result) => Ok(result),
            Err(e) => {
                if connection.is_closed() || matches!(e, rmcp::ServiceError::TransportClosed) {
                    self.forget(&connection);
                    if wait {
                        self.connect_in_background();
                    }
                }
                Err(anyhow!("{e}"))
            }
        }
    }

    /// Calls one of Hippocampe's tools for the LLM. A refusal (a tool the key cannot use, a
    /// write with a read-only key) comes back as a tool error the LLM can read, never as a
    /// failure of the server.
    pub async fn call(
        self: &Arc<Self>,
        name: &str,
        arguments: Option<JsonObject>,
    ) -> CallToolResult {
        match self.call_on(true, name, arguments).await {
            Ok(result) => result,
            Err(e) => CallToolResult::error(vec![ContentBlock::text(format!(
                "Hippocampe a refusé ou n'a pas répondu : {e}"
            ))]),
        }
    }

    /// The tools Hippocampe offers with this key.
    pub async fn tools(self: &Arc<Self>) -> anyhow::Result<Vec<Tool>> {
        let connection = self.connection(true).await?;
        connection
            .list_all_tools()
            .await
            .map_err(|e| anyhow!("listing Hippocampe's tools: {e}"))
    }
}

impl Memory for Arc<Hippocampe> {
    async fn search(&self, query: &str, limit: u32) -> anyhow::Result<Vec<Found>> {
        let arguments = json!({ "query": query, "limit": limit, "neighbors": 0 });
        let result = self
            .call_on(false, "search", arguments.as_object().cloned())
            .await?;
        anyhow::ensure!(
            result.is_error != Some(true),
            "Hippocampe refused the search"
        );
        let structured = result
            .structured_content
            .context("Hippocampe's search gave no structured content")?;
        Ok(found_in(&structured))
    }

    async fn heads_up(&self) -> anyhow::Result<Option<String>> {
        let result = self
            .call_on(true, "briefing", Some(JsonObject::new()))
            .await?;
        anyhow::ensure!(
            result.is_error != Some(true),
            "Hippocampe refused the briefing"
        );
        Ok(result.structured_content.as_ref().and_then(heads_up_of))
    }
}
