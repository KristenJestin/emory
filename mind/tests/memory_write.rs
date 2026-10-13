//! Writing to the memory on request, from the LLM to Hippocampe: a fake ACP agent (the LLM) opens
//! the session `AcpBrain` asks for, reaches Emory's MCP server with the address and the key it was
//! given, and calls `hippocampe_*` tools; a fake Hippocampe (never the real one) records what
//! arrives. What is checked is the plumbing: the arguments reach Hippocampe as the LLM wrote them
//! (provenance included), a refusal reaches the LLM as an error, and the search of what Emory
//! noted goes through with `by` and `since`. What the LLM decides to write is its consigne's
//! business, not a test's.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_client_protocol::{
    Agent, Channel, Client, ConnectionTo,
    schema::v1::{
        AgentCapabilities, ContentBlock, ContentChunk, InitializeRequest, InitializeResponse,
        NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, SessionNotification,
        SessionUpdate, StopReason, TextContent,
    },
};
use futures_util::future::BoxFuture;
use mind::{
    acp::{AcpBrain, SessionSettings},
    brain::{Brain, Event},
    config::Thinking,
    conversations::ConversationKey,
    hippocampe::Hippocampe,
    tools::{self, HippocampeTools, Tools},
};
use rmcp::{
    RoleClient, ServiceExt,
    model::{CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock as McpContent},
    service::RunningService,
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::{Value, json};

const FAKE_KEY: &str = "fake-emory-key";

// ---- The fake Hippocampe -------------------------------------------------------------------

/// Records every call it receives, in order, with its arguments.
#[derive(Clone, Default)]
struct FakeHippocampe {
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    /// `write` answers with an error, as a key that cannot write does.
    refuse_writes: bool,
}

impl FakeHippocampe {
    fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().unwrap().clone()
    }

    fn calls_to(&self, tool: &str) -> Vec<Value> {
        self.calls()
            .into_iter()
            .filter(|(name, _)| name == tool)
            .map(|(_, arguments)| arguments)
            .collect()
    }
}

fn open_schema() -> Arc<rmcp::model::JsonObject> {
    Arc::new(
        json!({"type": "object", "additionalProperties": true})
            .as_object()
            .unwrap()
            .clone(),
    )
}

impl rmcp::ServerHandler for FakeHippocampe {
    fn get_info(&self) -> rmcp::model::InitializeResult {
        rmcp::model::InitializeResult::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        Ok(rmcp::model::ListToolsResult::with_all_items(
            ["search", "read", "write", "link", "types"]
                .map(|n| rmcp::model::Tool::new(n, format!("The {n} tool."), open_schema()))
                .to_vec(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        let arguments = Value::Object(request.arguments.clone().unwrap_or_default());
        self.calls
            .lock()
            .unwrap()
            .push((request.name.to_string(), arguments.clone()));
        let result = match request.name.as_ref() {
            "write" if self.refuse_writes => {
                CallToolResult::error(vec![McpContent::text("this key cannot write (read-only)")])
            }
            "write" => CallToolResult::structured(json!({"entry": {"slug": "perceuse"}})),
            // What this key wrote: found by `by`, never by anything else.
            "search" if arguments["by"] == "emory" => CallToolResult::structured(json!({
                "results": [
                    {"slug": "perceuse", "title": "Perceuse", "by": "emory"},
                    {"slug": "portail", "title": "Portail", "by": "emory"}
                ],
                "total": 2
            })),
            "search" => CallToolResult::structured(json!({"results": [], "total": 0})),
            other => CallToolResult::success(vec![McpContent::text(format!("{other} done"))]),
        };
        Ok(CallToolResponse::Complete(result))
    }
}

/// Serves the fake Hippocampe on the loopback, behind its key.
async fn serve_hippocampe(fake: FakeHippocampe) -> std::net::SocketAddr {
    use axum::{
        extract::Request,
        http::StatusCode,
        middleware::{self, Next},
    };
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };
    let service = StreamableHttpService::new(
        move || Ok(fake.clone()),
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

// ---- The fake LLM: an ACP agent that calls Emory's tools ------------------------------------

/// What the fake LLM does for a turn: it is given the text of the turn and a client of Emory's
/// MCP server, and answers with the text it says.
type Script = Arc<dyn Fn(String, Llm) -> BoxFuture<'static, String> + Send + Sync>;

/// The LLM's side of Emory's MCP server.
#[derive(Clone)]
struct Llm(Arc<RunningService<RoleClient, ()>>);

impl Llm {
    async fn call(&self, tool: &str, arguments: Value) -> CallToolResult {
        let mut request = CallToolRequestParams::new(tool.to_owned());
        request.arguments = arguments.as_object().cloned();
        self.0.call_tool(request).await.unwrap()
    }
}

fn text_of(result: &CallToolResult) -> String {
    serde_json::to_string(&result.content).unwrap()
}

fn chunk(session: &str, text: &str) -> SessionNotification {
    SessionNotification::new(
        session.to_owned(),
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            text,
        )))),
    )
}

/// The agent: at `session/new` it keeps the address and the key of Emory's MCP server; at each
/// turn it connects to it, runs the script and streams what the script says.
fn start_agent(script: Script) -> Channel {
    let (client_end, agent_end) = Channel::duplex();
    let server: Arc<Mutex<Option<(String, String)>>> = Arc::default();
    let on_new = server.clone();
    tokio::spawn(async move {
        let _ = Agent
            .builder()
            .name("fake-llm")
            .on_receive_request(
                async move |init: InitializeRequest, responder, _cx| {
                    responder.respond(
                        InitializeResponse::new(init.protocol_version)
                            .agent_capabilities(AgentCapabilities::new()),
                    )
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: NewSessionRequest, responder, _cx| {
                    let request = serde_json::to_value(&request).unwrap();
                    let server = &request["mcpServers"][0];
                    let url = server["url"].as_str().unwrap().to_owned();
                    let key = server["headers"][0]["value"]
                        .as_str()
                        .unwrap()
                        .strip_prefix("Bearer ")
                        .unwrap()
                        .to_owned();
                    *on_new.lock().unwrap() = Some((url, key));
                    responder.respond(NewSessionResponse::new("session-1"))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                async move |request: PromptRequest, responder, cx: ConnectionTo<Client>| {
                    let text = match &request.prompt[0] {
                        ContentBlock::Text(t) => t.text.clone(),
                        _ => String::new(),
                    };
                    let session = request.session_id.0.to_string();
                    let (url, key) = server.lock().unwrap().clone().unwrap();
                    let script = script.clone();
                    let sender = cx.clone();
                    cx.spawn(async move {
                        let client = ()
                            .serve(StreamableHttpClientTransport::from_config(
                                StreamableHttpClientTransportConfig::with_uri(url.as_str())
                                    .auth_header(key),
                            ))
                            .await
                            .unwrap();
                        let said = script(text, Llm(Arc::new(client))).await;
                        sender.send_notification(chunk(&session, &said))?;
                        responder.respond(PromptResponse::new(StopReason::EndTurn))
                    })
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(agent_end)
            .await;
    });
    client_end
}

// ---- The scene -----------------------------------------------------------------------------

/// A turn in Emory's chain: the fake LLM, Emory's MCP server (every tool relayed) and the fake
/// Hippocampe behind it. Returns the text said and the fake Hippocampe, to look at its calls.
async fn one_turn(
    fake: FakeHippocampe,
    script: Script,
    turn: &str,
) -> (Vec<Event>, FakeHippocampe) {
    let address = serve_hippocampe(fake.clone()).await;
    let token = std::env::temp_dir().join(format!(
        "mind-write-test-{}-{}.token",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&token, format!("{FAKE_KEY}\n")).unwrap();
    let hippocampe = Hippocampe::new(format!("http://{address}/mcp"), &token);
    let module = HippocampeTools::new(
        hippocampe,
        ["search", "read", "write", "link", "types"]
            .map(str::to_owned)
            .to_vec(),
        Duration::from_secs(10),
    );
    let server = tools::serve(
        Tools::new(vec![Arc::new(module)]),
        ([127, 0, 0, 1], 0).into(),
    )
    .await
    .unwrap();
    let settings = SessionSettings {
        system_prompt: "Tu es Emory.".to_owned(),
        model: "claude-haiku-5-5".to_owned(),
        effort: "low".to_owned(),
        thinking: Thinking::Off,
        fast: false,
        workdir: "/tmp".into(),
        mcp_url: server.url.clone(),
        mcp_key: server.key.clone(),
    };
    let brain = AcpBrain::connect(start_agent(script), settings)
        .await
        .unwrap();
    let session = brain.open(&ConversationKey("c1".to_owned())).await.unwrap();
    let mut events = brain.prompt(&session, turn.to_owned());
    let mut all = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("the turn never ended")
            .expect("the stream closed before Done");
        let done = matches!(event, Event::Done(_));
        all.push(event);
        if done {
            break;
        }
    }
    let _ = std::fs::remove_file(&token);
    (all, fake)
}

fn said(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_note_asked_by_the_owner_reaches_hippocampe_as_the_llm_wrote_it() {
    // Three writes with what the memory's rules ask for: a creation (known, said by the owner), a
    // change of a few words, and what the LLM only supposes. The shape of the arguments is
    // Hippocampe's; here only their passing through matters, so they carry odd values too
    // (accents, nesting, numbers, null).
    let creation = json!({
        "type": "item",
        "title": "Perceuse",
        "body": "Prêtée à Marc, à récupérer d'ici la fin du mois.",
        "provenance": "extracted",
        "source": {"said_by": "owner", "on": "2026-10-13"},
        "fields": {"quantity": 1, "serial": null}
    });
    let few_words = json!({
        "entry": "perceuse",
        "edits": [{"old": "d'ici la fin du mois", "new": "avant le premier novembre"}],
        "source": {"said_by": "owner", "on": "2026-10-13"}
    });
    let supposed = json!({
        "entry": "perceuse",
        "provenance": "inferred",
        "fields": {"lent_to": "Marc"}
    });
    let script: Script = {
        let writes = [creation.clone(), few_words.clone(), supposed.clone()];
        Arc::new(move |_turn, llm| {
            let writes = writes.clone();
            Box::pin(async move {
                let found = llm
                    .call("hippocampe_search", json!({"query": "perceuse"}))
                    .await;
                assert_ne!(found.is_error, Some(true));
                for write in writes {
                    let result = llm.call("hippocampe_write", write).await;
                    assert_ne!(result.is_error, Some(true), "{}", text_of(&result));
                }
                "C'est noté dans ta fiche sur la perceuse.".to_owned()
            })
        })
    };
    let (events, fake) = one_turn(
        FakeHippocampe::default(),
        script,
        "Note que j'ai prêté la perceuse à Marc.",
    )
    .await;

    assert_eq!(
        events,
        [
            Event::Text("C'est noté dans ta fiche sur la perceuse.".into()),
            Event::Done(Ok("end_turn".into()))
        ]
    );
    // The search comes first, with the neighbours left out (Emory's default for a search) ...
    let order: Vec<String> = fake.calls().into_iter().map(|(tool, _)| tool).collect();
    assert_eq!(order, ["search", "write", "write", "write"]);
    assert_eq!(
        fake.calls_to("search"),
        [json!({"query": "perceuse", "neighbors": 0})]
    );
    // ... and the writes arrive untouched, no default added, nothing dropped.
    assert_eq!(fake.calls_to("write"), [creation, few_words, supposed]);
}

#[tokio::test]
async fn an_undo_is_a_write_like_another_and_passes_unchanged() {
    let archive = json!({
        "entry": "perceuse",
        "archive": true,
        "reason": "annulé à la demande"
    });
    let script: Script = {
        let archive = archive.clone();
        Arc::new(move |_turn, llm| {
            let archive = archive.clone();
            Box::pin(async move {
                let result = llm.call("hippocampe_write", archive).await;
                assert_ne!(result.is_error, Some(true), "{}", text_of(&result));
                "C'est annulé.".to_owned()
            })
        })
    };
    let (events, fake) = one_turn(FakeHippocampe::default(), script, "Annule ça.").await;
    assert_eq!(said(&events), "C'est annulé.");
    assert_eq!(fake.calls_to("write"), [archive]);
}

/// What the LLM saw of a tool call: whether it was an error, and the text.
type Refusal = (Option<bool>, String);

#[tokio::test]
async fn a_refused_write_reaches_the_llm_as_an_error_and_is_said_in_one_sentence() {
    let seen_by_llm: Arc<Mutex<Vec<Refusal>>> = Arc::default();
    let script: Script = {
        let seen_by_llm = seen_by_llm.clone();
        Arc::new(move |_turn, llm| {
            let seen_by_llm = seen_by_llm.clone();
            Box::pin(async move {
                let result = llm
                    .call("hippocampe_write", json!({"title": "Portail"}))
                    .await;
                seen_by_llm
                    .lock()
                    .unwrap()
                    .push((result.is_error, text_of(&result)));
                if result.is_error == Some(true) {
                    "La mémoire a refusé de noter ça.".to_owned()
                } else {
                    "C'est noté.".to_owned()
                }
            })
        })
    };
    let fake = FakeHippocampe {
        refuse_writes: true,
        ..FakeHippocampe::default()
    };
    let (events, fake) = one_turn(fake, script, "Retiens que le code du portail change.").await;

    let seen = seen_by_llm.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, Some(true));
    assert!(seen[0].1.contains("read-only"), "{}", seen[0].1);
    // Tried once, not again behind the LLM's back; and said in one sentence.
    assert_eq!(fake.calls_to("write").len(), 1);
    let sentence = said(&events);
    assert_eq!(sentence, "La mémoire a refusé de noter ça.");
    assert_eq!(sentence.matches(['.', '!', '?']).count(), 1);
}

#[tokio::test]
async fn what_emory_noted_is_searched_by_its_key_and_the_day() {
    let script: Script = Arc::new(|_turn, llm| {
        Box::pin(async move {
            let result = llm
                .call(
                    "hippocampe_search",
                    json!({"by": "emory", "since": "2026-10-13"}),
                )
                .await;
            let titles: Vec<String> = result.structured_content.as_ref().unwrap()["results"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["title"].as_str().unwrap().to_owned())
                .collect();
            format!("Tu m'as fait noter {}.", titles.join(" et "))
        })
    });
    let (events, fake) = one_turn(
        FakeHippocampe::default(),
        script,
        "Qu'est-ce que tu as noté aujourd'hui ?",
    )
    .await;
    assert_eq!(said(&events), "Tu m'as fait noter Perceuse et Portail.");
    // `by` and `since` arrive as the LLM wrote them; the neighbours are left out, as for any
    // search of the LLM.
    assert_eq!(
        fake.calls_to("search"),
        [json!({"by": "emory", "since": "2026-10-13", "neighbors": 0})]
    );
    assert!(fake.calls_to("write").is_empty());
}
