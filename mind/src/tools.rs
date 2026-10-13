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

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

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
        InitializeResult, JsonObject, ListToolsResult, PaginatedRequestParams, ResultType,
        ServerCapabilities, Tool,
    },
    service::RequestContext,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use subtle::ConstantTimeEq;
use tracing::{info, warn};

use crate::{
    conversations::{self, ConversationKey},
    hippocampe::Hippocampe,
};

/// A group of tools.
pub trait Module: Send + Sync + 'static {
    /// The name that prefixes its tools: lowercase letters only.
    fn name(&self) -> &'static str;
    /// Its tools, under their own names.
    fn tools(&self) -> BoxFuture<'_, Vec<Tool>>;
    /// Runs one of its tools. A failure is a tool error, for the LLM to read.
    fn call(&self, tool: &str, arguments: Option<JsonObject>) -> BoxFuture<'_, CallToolResult>;
    /// Runs one of its tools for a conversation (`None` when the call did not say which): for
    /// the modules whose tools depend on it. By default, the conversation does not matter.
    fn call_in(
        &self,
        tool: &str,
        arguments: Option<JsonObject>,
        conversation: Option<ConversationKey>,
    ) -> BoxFuture<'_, CallToolResult> {
        let _ = conversation;
        self.call(tool, arguments)
    }
}

/// Told of each call to a tool, with its arguments, before the tool runs: the waiting cue says
/// what is being searched (see [`crate::waiting`]). It must not wait.
pub trait Observer: Send + Sync + 'static {
    fn called(
        &self,
        conversation: Option<&ConversationKey>,
        tool: &str,
        arguments: Option<&JsonObject>,
    );
}

/// The tools of all the modules.
#[derive(Clone)]
pub struct Tools {
    modules: Arc<Vec<Arc<dyn Module>>>,
    observer: Option<Arc<dyn Observer>>,
}

impl Tools {
    pub fn new(modules: Vec<Arc<dyn Module>>) -> Self {
        Self {
            modules: Arc::new(modules),
            observer: None,
        }
    }

    /// Tells `observer` of each call.
    pub fn observed_by(mut self, observer: Arc<dyn Observer>) -> Self {
        self.observer = Some(observer);
        self
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
        self.call_in(name, arguments, None).await
    }

    /// Calls a tool for a conversation.
    pub async fn call_in(
        &self,
        name: &str,
        arguments: Option<JsonObject>,
        conversation: Option<ConversationKey>,
    ) -> CallToolResult {
        let started = Instant::now();
        if let Some(observer) = &self.observer {
            observer.called(conversation.as_ref(), name, arguments.as_ref());
        }
        let routed = self.modules.iter().find_map(|m| {
            name.strip_prefix(m.name())
                .and_then(|rest| rest.strip_prefix('_'))
                .map(|tool| (m, tool))
        });
        let mut result = match routed {
            Some((module, tool)) => module.call_in(tool, arguments, conversation).await,
            None => {
                CallToolResult::error(vec![ContentBlock::text(format!("Outil inconnu : {name}"))])
            }
        };
        // A result relayed from a server that speaks an older revision of MCP has no
        // `resultType`, which the revision this server speaks requires: the adapter would refuse
        // it as malformed.
        result.result_type.get_or_insert(ResultType::COMPLETE);
        // The arguments and the result stay out of the logs: they hold what the user said.
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        // The HTTP request of the call, which carries the key of its conversation.
        let conversation = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.headers.get(conversations::HEADER))
            .and_then(|value| value.to_str().ok())
            .map(|key| ConversationKey(key.to_owned()));
        Ok(CallToolResponse::Complete(
            self.call_in(&request.name, request.arguments, conversation)
                .await,
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
    // Compared in constant time: the time taken says nothing of the key.
    if given.is_some_and(|g| bool::from(g.as_bytes().ct_eq(expected.as_bytes()))) {
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
    /// How long a tool call of the LLM may wait for Hippocampe.
    timeout: Duration,
    /// `None` until Hippocampe answered `tools/list`.
    cache: tokio::sync::Mutex<Option<HashMap<String, Tool>>>,
}

impl HippocampeTools {
    pub fn new(hippocampe: Arc<Hippocampe>, allowed: Vec<String>, timeout: Duration) -> Self {
        Self {
            hippocampe,
            allowed,
            timeout,
            cache: tokio::sync::Mutex::default(),
        }
    }

    /// The tools, asked of Hippocampe the first time, and again as long as it did not answer:
    /// the adapter lists the tools at each session opening, which tries again.
    async fn offered(&self) -> HashMap<String, Tool> {
        let mut cache = self.cache.lock().await;
        if cache.is_none() {
            match self.hippocampe.tools().await {
                Ok(tools) => {
                    let tools: HashMap<String, Tool> = tools
                        .into_iter()
                        .filter(|t| self.allowed.iter().any(|a| a == t.name.as_ref()))
                        .map(|t| (t.name.to_string(), t))
                        .collect();
                    info!(tools = tools.len(), "Hippocampe's tools relayed");
                    *cache = Some(tools);
                }
                Err(e) => warn!("{e:#}"),
            }
        }
        cache.clone().unwrap_or_default()
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
            let call = self.hippocampe.call(&tool, with_defaults(&tool, arguments));
            tokio::time::timeout(self.timeout, call)
                .await
                .unwrap_or_else(|_| {
                    CallToolResult::error(vec![ContentBlock::text(
                        "Hippocampe n'a pas répondu à temps.",
                    )])
                })
        })
    }
}

/// A search asked by the LLM comes without the neighbours of each entry unless it asks for them:
/// with them (3 by default at Hippocampe), a search of 10 entries was measured at about 17 000
/// tokens on 2026-10-12, a second more before the first word. `read` gives them for one entry.
fn with_defaults(tool: &str, arguments: Option<JsonObject>) -> Option<JsonObject> {
    if tool != "search" {
        return arguments;
    }
    let mut arguments = arguments.unwrap_or_default();
    arguments
        .entry("neighbors")
        .or_insert(serde_json::Value::from(0));
    Some(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A module whose results come from an older server, without `resultType`.
    struct Old;

    impl Module for Old {
        fn name(&self) -> &'static str {
            "old"
        }

        fn tools(&self) -> BoxFuture<'_, Vec<Tool>> {
            Box::pin(async { Vec::new() })
        }

        fn call(
            &self,
            _tool: &str,
            _arguments: Option<JsonObject>,
        ) -> BoxFuture<'_, CallToolResult> {
            Box::pin(async {
                serde_json::from_value(serde_json::json!({
                    "content": [{"type": "text", "text": "ok"}],
                    "isError": false
                }))
                .unwrap()
            })
        }
    }

    #[test]
    fn a_search_of_the_llm_comes_without_neighbours_unless_asked() {
        let args = |v: serde_json::Value| v.as_object().cloned();
        assert_eq!(
            with_defaults("search", args(serde_json::json!({"query": "q"}))),
            args(serde_json::json!({"query": "q", "neighbors": 0}))
        );
        assert_eq!(
            with_defaults(
                "search",
                args(serde_json::json!({"query": "q", "neighbors": 2}))
            ),
            args(serde_json::json!({"query": "q", "neighbors": 2}))
        );
        assert_eq!(
            with_defaults("read", args(serde_json::json!({"entry": "e"}))),
            args(serde_json::json!({"entry": "e"}))
        );
    }

    type Call = (Option<ConversationKey>, String, Option<JsonObject>);

    #[derive(Default)]
    struct Calls(std::sync::Mutex<Vec<Call>>);

    impl Observer for Calls {
        fn called(
            &self,
            conversation: Option<&ConversationKey>,
            tool: &str,
            arguments: Option<&JsonObject>,
        ) {
            self.0.lock().unwrap().push((
                conversation.cloned(),
                tool.to_owned(),
                arguments.cloned(),
            ));
        }
    }

    #[tokio::test]
    async fn the_observer_is_told_of_each_call_with_its_arguments() {
        let calls = Arc::new(Calls::default());
        let tools = Tools::new(vec![Arc::new(Old)]).observed_by(calls.clone());
        let arguments = serde_json::json!({"query": "Ali"}).as_object().cloned();
        let key = ConversationKey("k".to_owned());
        tools
            .call_in("old_thing", arguments.clone(), Some(key.clone()))
            .await;
        tools.call("nope", None).await;
        let seen = calls.0.lock().unwrap().clone();
        assert_eq!(
            seen,
            [
                (Some(key), "old_thing".to_owned(), arguments),
                (None, "nope".to_owned(), None)
            ]
        );
    }

    #[tokio::test]
    async fn every_result_says_its_type() {
        let tools = Tools::new(vec![Arc::new(Old)]);
        let relayed = tools.call("old_thing", None).await;
        assert_eq!(relayed.result_type, Some(ResultType::COMPLETE));
        let unknown = tools.call("nope", None).await;
        assert_eq!(unknown.result_type, Some(ResultType::COMPLETE));
        assert_eq!(unknown.is_error, Some(true));
    }
}
