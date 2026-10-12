//! Emory's MCP server: the only tools the LLM sees, served by `mind` over HTTP on the loopback.
//!
//! Each group of tools is a [`Module`]; a tool is offered to the LLM as `<module>_<tool>`
//! (`hippocampe_search`), so that modules never collide. In this version: Hippocampe, whose tools
//! are relayed. The time, the web and the hands become modules of their own.
//!
//! Served in the process of `mind`, rather than as a child of the adapter: the tools then share
//! the state of the mind (the conversations, later the timers and the hands on the bus) without
//! any other channel. The server listens on the loopback only, and wants a key drawn at start-up,
//! which only the adapter is given.

use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Instant};

use anyhow::Context;
use axum::{
    Router,
    extract::Request,
    http::{StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::Response,
};
use futures_util::future::BoxFuture;
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        InitializeResult, JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
        Tool,
    },
    service::RequestContext,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use tracing::{info, warn};

use crate::hippocampe::Hippocampe;

/// A group of tools.
pub trait Module: Send + Sync + 'static {
    /// The name that prefixes its tools: lowercase letters only.
    fn name(&self) -> &'static str;
    /// Its tools, under their own names.
    fn tools(&self) -> BoxFuture<'_, Vec<Tool>>;
    /// Runs one of its tools. A failure is a tool error, for the LLM to read.
    fn call(&self, tool: &str, arguments: Option<JsonObject>) -> BoxFuture<'_, CallToolResult>;
}

/// The tools of all the modules.
#[derive(Clone)]
pub struct Tools {
    modules: Arc<Vec<Arc<dyn Module>>>,
}

impl Tools {
    pub fn new(modules: Vec<Arc<dyn Module>>) -> Self {
        Self {
            modules: Arc::new(modules),
        }
    }

    pub async fn list(&self) -> Vec<Tool> {
        let mut all = Vec::new();
        for module in self.modules.iter() {
            for mut tool in module.tools().await {
                tool.name = format!("{}_{}", module.name(), tool.name).into();
                all.push(tool);
            }
        }
        all
    }

    pub async fn call(&self, name: &str, arguments: Option<JsonObject>) -> CallToolResult {
        let started = Instant::now();
        let routed = self.modules.iter().find_map(|m| {
            name.strip_prefix(m.name())
                .and_then(|rest| rest.strip_prefix('_'))
                .map(|tool| (m, tool))
        });
        let result = match routed {
            Some((module, tool)) => module.call(tool, arguments).await,
            None => {
                CallToolResult::error(vec![ContentBlock::text(format!("Outil inconnu : {name}"))])
            }
        };
        // The arguments and the result stay out of the logs: they hold what Kris said.
        info!(
            tool = name,
            error = result.is_error == Some(true),
            ms = started.elapsed().as_millis() as u64,
            "tool called"
        );
        result
    }
}

impl ServerHandler for Tools {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("emory", env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.list().await))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        Ok(CallToolResponse::Complete(
            self.call(&request.name, request.arguments).await,
        ))
    }
}

/// The server, listening.
pub struct Server {
    /// The address of its MCP endpoint (`http://127.0.0.1:<port>/mcp`).
    pub url: String,
    /// The key to give in `Authorization: Bearer`.
    pub key: String,
}

/// Starts serving the tools on `bind` (a loopback address).
pub async fn serve(tools: Tools, bind: SocketAddr) -> anyhow::Result<Server> {
    let key = uuid::Uuid::new_v4().simple().to_string();
    let service = StreamableHttpService::new(
        move || Ok(tools.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let expected = format!("Bearer {key}");
    let app = Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn(move |request: Request, next: Next| {
            let expected = expected.clone();
            async move { authorize(&expected, request, next).await }
        }));
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("listening on {bind}"))?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            warn!("Emory's MCP server stopped: {e}");
        }
    });
    info!(%address, "Emory's MCP server is listening");
    Ok(Server {
        url: format!("http://{address}/mcp"),
        key,
    })
}

async fn authorize(expected: &str, request: Request, next: Next) -> Result<Response, StatusCode> {
    let given = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if given == Some(expected) {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Hippocampe's tools, relayed: those of the configured list that Hippocampe offers with
/// Emory's key.
pub struct HippocampeTools {
    hippocampe: Arc<Hippocampe>,
    allowed: Vec<String>,
    cache: tokio::sync::Mutex<HashMap<String, Tool>>,
}

impl HippocampeTools {
    pub fn new(hippocampe: Arc<Hippocampe>, allowed: Vec<String>) -> Self {
        Self {
            hippocampe,
            allowed,
            cache: tokio::sync::Mutex::default(),
        }
    }

    /// The tools, asked of Hippocampe the first time (and again as long as it did not answer).
    async fn offered(&self) -> HashMap<String, Tool> {
        let mut cache = self.cache.lock().await;
        if cache.is_empty() {
            match self.hippocampe.tools().await {
                Ok(tools) => {
                    *cache = tools
                        .into_iter()
                        .filter(|t| self.allowed.iter().any(|a| a == t.name.as_ref()))
                        .map(|t| (t.name.to_string(), t))
                        .collect();
                    info!(tools = cache.len(), "Hippocampe's tools relayed");
                }
                Err(e) => warn!("{e:#}"),
            }
        }
        cache.clone()
    }
}

impl Module for HippocampeTools {
    fn name(&self) -> &'static str {
        "hippocampe"
    }

    fn tools(&self) -> BoxFuture<'_, Vec<Tool>> {
        Box::pin(async move {
            let mut tools: Vec<Tool> = self.offered().await.into_values().collect();
            tools.sort_by(|a, b| a.name.cmp(&b.name));
            tools
        })
    }

    fn call(&self, tool: &str, arguments: Option<JsonObject>) -> BoxFuture<'_, CallToolResult> {
        let tool = tool.to_owned();
        Box::pin(async move {
            if !self.offered().await.contains_key(&tool) {
                return CallToolResult::error(vec![ContentBlock::text(format!(
                    "Hippocampe n'offre pas l'outil {tool} à Emory."
                ))]);
            }
            self.hippocampe.call(&tool, arguments).await
        })
    }
}
