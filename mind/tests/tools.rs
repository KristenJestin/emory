//! Emory's MCP server and the Hippocampe module against a fake Hippocampe (never the real one):
//! the search made while Kris speaks, the heads-up, the relayed tools under their prefix, a
//! refused write surfaced as a tool error, the key of the server.

use std::{net::SocketAddr, sync::Arc};

use mind::{
    hippocampe::Hippocampe,
    memory::Memory,
    tools::{self, HippocampeTools, Tools},
};
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, InitializeResult,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, Tool,
    },
    service::RequestContext,
    transport::{
        StreamableHttpClientTransport,
        streamable_http_client::StreamableHttpClientTransportConfig,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        },
    },
};
use serde_json::json;

const FAKE_KEY: &str = "fake-read-only-key";

/// A fake Hippocampe with a read-only key: it lists `write`, and refuses it.
#[derive(Clone)]
struct FakeHippocampe;

fn schema() -> Arc<rmcp::model::JsonObject> {
    Arc::new(
        json!({"type": "object", "properties": {"query": {"type": "string"}}})
            .as_object()
            .unwrap()
            .clone(),
    )
}

impl ServerHandler for FakeHippocampe {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            ["search", "read", "write", "briefing", "slow"]
                .map(|n| Tool::new(n, format!("The {n} tool."), schema()))
                .to_vec(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let result = match request.name.as_ref() {
            "search" => {
                let query = request.arguments.as_ref().unwrap()["query"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                CallToolResult::structured(json!({"results": [
                    {"id": "1", "slug": "musique", "type": "decision", "title": "Musique",
                     "summary": format!("Trouvé pour {query}."), "excerpt": "la <mark>musique</mark>",
                     "score": 0.9}
                ], "total": 1}))
            }
            "briefing" => CallToolResult::structured(json!({
                "upcoming": [{"slug": "dentiste", "title": "Dentiste", "days_left": 1}],
                "waiting": {"supposed": [{"slug": "x"}]}
            })),
            "write" => CallToolResult::error(vec![ContentBlock::text(
                "this key cannot write (read-only)",
            )]),
            "slow" => {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                CallToolResult::success(vec![ContentBlock::text("too late")])
            }
            other => CallToolResult::success(vec![ContentBlock::text(format!("{other} done"))]),
        };
        Ok(CallToolResponse::Complete(result))
    }
}

/// Serves the fake Hippocampe on the loopback, behind its key.
async fn serve_fake() -> SocketAddr {
    use axum::{
        extract::Request,
        http::StatusCode,
        middleware::{self, Next},
    };
    let service = StreamableHttpService::new(
        || Ok(FakeHippocampe),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let app = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn(
            |request: Request, next: Next| async move {
                let expected = format!("Bearer {FAKE_KEY}");
                match request.headers().get("authorization") {
                    Some(v) if v == expected.as_str() => Ok(next.run(request).await),
                    _ => Err(StatusCode::UNAUTHORIZED),
                }
            },
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    address
}

/// The client of the fake Hippocampe, with the key in a file.
async fn hippocampe() -> (Arc<Hippocampe>, tempdir::Dir) {
    let address = serve_fake().await;
    let dir = tempdir::Dir::new();
    let token = dir.0.join("hippocampe.token");
    std::fs::write(&token, format!("{FAKE_KEY}\n")).unwrap();
    (Hippocampe::new(format!("http://{address}/mcp"), token), dir)
}

mod tempdir {
    /// A temporary folder, removed at the end of the test.
    pub struct Dir(pub std::path::PathBuf);

    impl Dir {
        pub fn new() -> Self {
            let path = std::env::temp_dir().join(format!("mind-test-{}", uuid()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn uuid() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{}-{nanos}", std::process::id())
    }
}

#[tokio::test]
async fn the_search_and_the_heads_up_read_the_memory() {
    let (hippocampe, _dir) = hippocampe().await;
    // Not connected yet: the search made while Kris speaks does not wait, it fails at once and
    // the connection opens in the background.
    assert!(hippocampe.search("la musique", 5).await.is_err());
    hippocampe.connect().await.unwrap();
    let found = hippocampe.search("la musique", 5).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].slug, "musique");
    assert_eq!(found[0].summary, "Trouvé pour la musique.");
    assert_eq!(found[0].excerpt, "la musique");
    let heads_up = hippocampe.heads_up().await.unwrap().unwrap();
    assert!(heads_up.contains("dentiste") && !heads_up.contains("supposed"));
}

#[tokio::test]
async fn emory_relays_the_allowed_tools_and_surfaces_a_refusal() {
    let (hippocampe, _dir) = hippocampe().await;
    let module = HippocampeTools::new(
        hippocampe,
        vec!["search".to_owned(), "write".to_owned(), "link".to_owned()],
        std::time::Duration::from_secs(10),
    );
    let server = tools::serve(
        Tools::new(vec![Arc::new(module)]),
        ([127, 0, 0, 1], 0).into(),
    )
    .await
    .unwrap();

    // Without the key, nothing.
    let stranger = ().serve(StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(server.url.as_str()),
    ));
    assert!(stranger.await.is_err());

    let client = ()
        .serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(server.url.as_str())
                .auth_header(server.key.clone()),
        ))
        .await
        .unwrap();
    let mut names: Vec<String> = client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort();
    // `link` is allowed but not offered by Hippocampe; `read` is offered but not allowed.
    assert_eq!(names, ["hippocampe_search", "hippocampe_write"]);

    let mut write = CallToolRequestParams::new("hippocampe_write");
    write.arguments = json!({"query": "note que"}).as_object().cloned();
    let refused = client.call_tool(write).await.unwrap();
    assert_eq!(refused.is_error, Some(true));
    let said = serde_json::to_string(&refused.content).unwrap();
    assert!(said.contains("read-only"), "{said}");

    let read = client
        .call_tool(CallToolRequestParams::new("hippocampe_read"))
        .await
        .unwrap();
    assert_eq!(read.is_error, Some(true));

    let mut search = CallToolRequestParams::new("hippocampe_search");
    search.arguments = json!({"query": "musique"}).as_object().cloned();
    let found = client.call_tool(search).await.unwrap();
    assert_ne!(found.is_error, Some(true));
    assert_eq!(
        found.structured_content.unwrap()["results"][0]["slug"],
        "musique"
    );
}

#[tokio::test]
async fn a_slow_tool_call_is_an_error_in_time() {
    let (hippocampe, _dir) = hippocampe().await;
    let module = HippocampeTools::new(
        hippocampe,
        vec!["slow".to_owned()],
        std::time::Duration::from_millis(200),
    );
    let tools = Tools::new(vec![Arc::new(module)]);
    let started = std::time::Instant::now();
    let result = tools.call("hippocampe_slow", None).await;
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    assert_eq!(result.is_error, Some(true));
    let said = serde_json::to_string(&result.content).unwrap();
    assert!(said.contains("à temps"), "{said}");
}

#[tokio::test]
async fn the_tools_are_asked_again_when_hippocampe_did_not_answer() {
    let (hippocampe, dir) = hippocampe().await;
    // The key is not there yet: Hippocampe cannot be reached at the first session.
    let token = dir.0.join("hippocampe.token");
    let key = std::fs::read_to_string(&token).unwrap();
    std::fs::remove_file(&token).unwrap();
    let module = HippocampeTools::new(
        hippocampe,
        vec!["search".to_owned()],
        std::time::Duration::from_secs(10),
    );
    let tools = Tools::new(vec![Arc::new(module)]);
    assert!(tools.list().await.is_empty());
    // At the next session, it answers.
    std::fs::write(&token, key).unwrap();
    let names: Vec<String> = tools
        .list()
        .await
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    assert_eq!(names, ["hippocampe_search"]);
}

#[tokio::test]
async fn concurrent_reconnections_open_one_connection() {
    let (hippocampe, _dir) = hippocampe().await;
    let (a, b) = tokio::join!(hippocampe.connect(), hippocampe.connect());
    assert!(Arc::ptr_eq(&a.unwrap(), &b.unwrap()));
}
