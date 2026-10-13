//! The API brain against a fake Anthropic Messages API on the loopback: the streamed text, a tool
//! round run in the process, the history kept, the cancellation that drops the connection, the
//! failures, the dictionary's rule, the key read from its file, fast mode.

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
    routing::post,
};
use contract::{TurnId, VocabularyAdd, VocabularyReply};
use futures_util::{StreamExt, future::BoxFuture, stream};
use mind::{
    api::{ApiBrain, ApiSettings, Provider},
    brain::{Brain, Event},
    config::Thinking,
    conversations::{ConversationKey, Conversations},
    dictionary::{Dictionary, Vocabulary},
    tools::{Module, Tools},
};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedReceiver;

/// What the fake API answers to a request.
enum Script {
    /// Text, in pieces.
    Text(Vec<&'static str>),
    /// Some text, then a call to a tool.
    Tool(&'static str, &'static str, Value),
    /// Some text, then nothing until the client goes.
    Hang(&'static str),
    /// An HTTP error.
    Fail(u16),
}

#[derive(Default)]
struct Fake {
    scripts: Mutex<VecDeque<Script>>,
    /// The bodies of the requests, and their `x-api-key` and `anthropic-beta`.
    bodies: Mutex<Vec<Value>>,
    keys: Mutex<Vec<String>>,
    betas: Mutex<Vec<Option<String>>>,
    /// Set when the client dropped a hanging answer.
    dropped: Arc<AtomicBool>,
}

fn sse(event: &str, data: Value) -> Result<Bytes, std::io::Error> {
    Ok(Bytes::from(format!("event: {event}\ndata: {data}\n\n")))
}

fn start(id: usize) -> Vec<Result<Bytes, std::io::Error>> {
    vec![sse(
        "message_start",
        json!({"type": "message_start", "message": {"id": format!("m{id}"), "type": "message",
               "role": "assistant", "model": "fake", "content": [], "stop_reason": null,
               "usage": {"input_tokens": 10, "output_tokens": 1}}}),
    )]
}

fn text_block(index: u32, pieces: &[&str]) -> Vec<Result<Bytes, std::io::Error>> {
    let mut events = vec![sse(
        "content_block_start",
        json!({"type": "content_block_start", "index": index,
               "content_block": {"type": "text", "text": ""}}),
    )];
    for piece in pieces {
        events.push(sse(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": index,
                   "delta": {"type": "text_delta", "text": piece}}),
        ));
    }
    events.push(sse(
        "content_block_stop",
        json!({"type": "content_block_stop", "index": index}),
    ));
    events
}

fn end(reason: &str) -> Vec<Result<Bytes, std::io::Error>> {
    vec![
        sse(
            "message_delta",
            json!({"type": "message_delta", "delta": {"stop_reason": reason},
                   "usage": {"output_tokens": 5}}),
        ),
        sse("message_stop", json!({"type": "message_stop"})),
    ]
}

/// Sets a flag when dropped: the answer's stream went with the connection.
struct Dropped(Arc<AtomicBool>);

impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

async fn messages(State(fake): State<Arc<Fake>>, headers: HeaderMap, body: Bytes) -> Response {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    fake.keys
        .lock()
        .unwrap()
        .push(header("x-api-key").unwrap_or_default());
    fake.betas.lock().unwrap().push(header("anthropic-beta"));
    fake.bodies
        .lock()
        .unwrap()
        .push(serde_json::from_slice(&body).unwrap());
    let id = fake.bodies.lock().unwrap().len();
    let script = fake
        .scripts
        .lock()
        .unwrap()
        .pop_front()
        .expect("a request the test did not expect");
    let events = match script {
        Script::Fail(status) => {
            return (
                StatusCode::from_u16(status).unwrap(),
                json!({"type": "error", "error": {"type": "api_error", "message": "boom"}})
                    .to_string(),
            )
                .into_response();
        }
        Script::Text(pieces) => [start(id), text_block(0, &pieces), end("end_turn")]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
        Script::Tool(said, name, input) => [
            start(id),
            text_block(0, &[said]),
            vec![
                sse(
                    "content_block_start",
                    json!({"type": "content_block_start", "index": 1,
                       "content_block": {"type": "tool_use", "id": format!("call-{id}"),
                                         "name": name, "input": {}}}),
                ),
                sse(
                    "content_block_delta",
                    json!({"type": "content_block_delta", "index": 1,
                       "delta": {"type": "input_json_delta", "partial_json": input.to_string()}}),
                ),
                sse(
                    "content_block_stop",
                    json!({"type": "content_block_stop", "index": 1}),
                ),
            ],
            end("tool_use"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>(),
        Script::Hang(said) => {
            let guard = Dropped(fake.dropped.clone());
            let first = [
                start(id),
                vec![
                    sse(
                        "content_block_start",
                        json!({"type": "content_block_start", "index": 0,
                           "content_block": {"type": "text", "text": ""}}),
                    ),
                    sse(
                        "content_block_delta",
                        json!({"type": "content_block_delta", "index": 0,
                           "delta": {"type": "text_delta", "text": said}}),
                    ),
                ],
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            let body = stream::iter(first)
                .chain(stream::pending())
                .map(move |event| {
                    let _ = &guard;
                    event
                });
            return (
                [(CONTENT_TYPE, "text/event-stream")],
                Body::from_stream(body),
            )
                .into_response();
        }
    };
    (
        [(CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(stream::iter(events)),
    )
        .into_response()
}

/// A module that tells the time, and which conversation asked.
#[derive(Default)]
struct Clock {
    asked_by: Mutex<Vec<Option<ConversationKey>>>,
}

impl Module for Clock {
    fn name(&self) -> &'static str {
        "clock"
    }

    fn tools(&self) -> BoxFuture<'_, Vec<Tool>> {
        Box::pin(async {
            let schema = json!({"type": "object", "properties": {}});
            vec![Tool::new(
                "now",
                "L'heure qu'il est.",
                Arc::new(schema.as_object().unwrap().clone()),
            )]
        })
    }

    fn call(&self, _tool: &str, _arguments: Option<JsonObject>) -> BoxFuture<'_, CallToolResult> {
        unreachable!("called with its conversation")
    }

    fn call_in(
        &self,
        _tool: &str,
        _arguments: Option<JsonObject>,
        conversation: Option<ConversationKey>,
    ) -> BoxFuture<'_, CallToolResult> {
        self.asked_by.lock().unwrap().push(conversation);
        Box::pin(async { CallToolResult::success(vec![ContentBlock::text("midi")]) })
    }
}

#[derive(Clone, Default)]
struct Hearing {
    asked: Arc<Mutex<Vec<VocabularyAdd>>>,
}

impl Vocabulary for Hearing {
    fn add(&self, request: VocabularyAdd) -> BoxFuture<'_, Result<VocabularyReply, String>> {
        self.asked.lock().unwrap().push(request);
        Box::pin(async { Ok(VocabularyReply::success(false, true)) })
    }
}

struct Bench {
    fake: Arc<Fake>,
    brain: ApiBrain,
    clock: Arc<Clock>,
    hearing: Hearing,
    conversations: Conversations,
    key_file: PathBuf,
}

async fn bench(scripts: Vec<Script>, configure: impl FnOnce(&mut ApiSettings)) -> Bench {
    let fake = Arc::new(Fake::default());
    *fake.scripts.lock().unwrap() = scripts.into();
    let app = Router::new()
        .route("/v1/messages", post(messages))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });

    let key_file =
        std::env::temp_dir().join(format!("emory-api-test-{}.key", uuid::Uuid::new_v4()));
    std::fs::write(&key_file, "first-test-key\n").unwrap();
    let clock = Arc::new(Clock::default());
    let hearing = Hearing::default();
    let conversations = Conversations::default();
    let tools = Tools::new(vec![
        clock.clone(),
        Arc::new(Dictionary::new(hearing.clone(), conversations.clone())),
    ]);
    let mut settings = ApiSettings {
        provider: Provider::named("anthropic").unwrap(),
        model: "claude-haiku-5-5".to_owned(),
        base_url: Some(format!("http://{address}/v1/")),
        key_file: Some(key_file.clone()),
        system_prompt: "Tu es Emory.".to_owned(),
        thinking: Thinking::Off,
        effort: None,
        max_tokens: 1024,
        cache: true,
        fast: false,
    };
    configure(&mut settings);
    Bench {
        brain: ApiBrain::new(settings, tools).unwrap(),
        fake,
        clock,
        hearing,
        conversations,
        key_file,
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.key_file);
    }
}

impl Bench {
    fn body(&self, index: usize) -> Value {
        self.fake.bodies.lock().unwrap()[index].clone()
    }

    /// The messages of a request, as role and text (a tool call or result as its name or content).
    fn messages(&self, index: usize) -> Vec<(String, String)> {
        self.body(index)["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                let content = match &m["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(parts) => parts
                        .iter()
                        .map(|p| match p["type"].as_str() {
                            Some("text") => p["text"].as_str().unwrap_or_default().to_owned(),
                            Some("tool_use") => format!("[{}]", p["name"].as_str().unwrap()),
                            Some("tool_result") => match &p["content"] {
                                Value::String(s) => format!("=> {s}"),
                                other => format!("=> {}", other[0]["text"].as_str().unwrap()),
                            },
                            _ => String::new(),
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                    _ => String::new(),
                };
                (m["role"].as_str().unwrap().to_owned(), content)
            })
            .collect()
    }
}

async fn collect(events: &mut UnboundedReceiver<Event>) -> Vec<Event> {
    let mut all = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("the request never ended")
            .expect("the stream closed before Done");
        let done = matches!(event, Event::Done(_));
        all.push(event);
        if done {
            return all;
        }
    }
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(r, t)| (r.to_string(), t.to_string()))
        .collect()
}

#[tokio::test]
async fn the_answer_streams_and_the_conversation_is_kept() {
    let b = bench(
        vec![
            Script::Text(vec!["Un. ", "Deux", " trois."]),
            Script::Text(vec!["Quatre."]),
        ],
        |_| {},
    )
    .await;
    let session = b.brain.open(&ConversationKey("c1".into())).await.unwrap();
    let mut events = b.brain.prompt(&session, "dis trois".into());
    assert_eq!(
        collect(&mut events).await,
        [
            Event::Text("Un. ".into()),
            Event::Text("Deux".into()),
            Event::Text(" trois.".into()),
            Event::Done(Ok("end_turn".into()))
        ]
    );
    // The key is read from its file at each request.
    std::fs::write(&b.key_file, "second-test-key").unwrap();
    let mut events = b.brain.prompt(&session, "et après ?".into());
    collect(&mut events).await;
    assert_eq!(
        b.messages(1),
        pairs(&[
            ("user", "dis trois"),
            ("assistant", "Un. Deux trois."),
            ("user", "et après ?")
        ])
    );
    assert_eq!(
        *b.fake.keys.lock().unwrap(),
        ["first-test-key", "second-test-key"]
    );
    let body = b.body(1);
    assert_eq!(body["model"], "claude-haiku-5-5");
    assert_eq!(body["stream"], true);
    assert_eq!(body["max_tokens"], 1024);
    // The instructions and the tools are cached, and so is the end of the history.
    assert_eq!(body["system"][0]["text"], "Tu es Emory.");
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    let last = body["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["content"][0]["cache_control"]["type"], "ephemeral");
    let first = &body["messages"][0];
    assert!(first["content"].is_string() || first["content"][0]["cache_control"].is_null());
    let tools: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(tools, ["clock_now", "dictionary_propose", "dictionary_add"]);
    // No beta header unless fast mode is asked.
    assert_eq!(*b.fake.betas.lock().unwrap(), [None, None]);
}

#[tokio::test]
async fn a_tool_runs_in_the_process_for_its_conversation_and_the_answer_follows() {
    let b = bench(
        vec![
            Script::Tool("Voyons. ", "clock_now", json!({})),
            Script::Text(vec!["Il est midi."]),
        ],
        |_| {},
    )
    .await;
    let key = ConversationKey("c-tool".into());
    let session = b.brain.open(&key).await.unwrap();
    let mut events = b.brain.prompt(&session, "quelle heure ?".into());
    assert_eq!(
        collect(&mut events).await,
        [
            Event::Text("Voyons. ".into()),
            Event::Tool("clock_now".into()),
            Event::Text("Il est midi.".into()),
            Event::Done(Ok("end_turn".into()))
        ]
    );
    assert_eq!(*b.clock.asked_by.lock().unwrap(), [Some(key)]);
    assert_eq!(
        b.messages(1),
        pairs(&[
            ("user", "quelle heure ?"),
            ("assistant", "Voyons.  [clock_now]"),
            ("user", "=> midi")
        ])
    );
}

#[tokio::test]
async fn a_cancel_drops_the_connection_and_the_turn_stays_with_what_was_said() {
    let b = bench(
        vec![Script::Hang("Début. "), Script::Text(vec!["Oui."])],
        |_| {},
    )
    .await;
    let session = b.brain.open(&ConversationKey("c1".into())).await.unwrap();
    let mut events = b.brain.prompt(&session, "attends".into());
    assert_eq!(events.recv().await, Some(Event::Text("Début. ".into())));
    // Busy: another request is refused, and never reaches the API.
    let mut refused = b.brain.prompt(&session, "encore".into());
    assert!(matches!(
        collect(&mut refused).await[..],
        [Event::Done(Err(_))]
    ));
    b.brain.cancel(&session);
    assert_eq!(
        collect(&mut events).await,
        [Event::Done(Ok("cancelled".into()))]
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !b.fake.dropped.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the connection was never dropped");
    let mut events = b.brain.prompt(&session, "alors ?".into());
    collect(&mut events).await;
    assert_eq!(
        b.messages(1),
        pairs(&[
            ("user", "attends"),
            ("assistant", "Début. "),
            ("user", "alors ?")
        ])
    );
}

#[tokio::test]
async fn a_failed_request_leaves_the_history_as_it_was_and_a_close_ends_the_request() {
    let b = bench(vec![Script::Fail(500), Script::Hang("Début. ")], |_| {}).await;
    let session = b.brain.open(&ConversationKey("c1".into())).await.unwrap();
    let mut events = b.brain.prompt(&session, "première".into());
    let done = collect(&mut events).await;
    assert!(matches!(&done[..], [Event::Done(Err(_))]), "{done:?}");
    if let [Event::Done(Err(e))] = &done[..] {
        assert!(!e.contains("first-test-key"), "{e}");
    }
    let mut events = b.brain.prompt(&session, "seconde".into());
    assert_eq!(events.recv().await, Some(Event::Text("Début. ".into())));
    assert_eq!(b.messages(1), pairs(&[("user", "seconde")]));
    b.brain.close(&session);
    assert!(matches!(
        collect(&mut events).await[..],
        [Event::Done(Err(_))]
    ));
    let mut events = b.brain.prompt(&session, "fermée".into());
    assert!(matches!(
        collect(&mut events).await[..],
        [Event::Done(Err(_))]
    ));
}

#[tokio::test]
async fn the_dictionary_adds_a_word_only_after_the_user_said_yes_in_a_later_turn() {
    let b = bench(
        vec![
            // First turn: the model proposes, then adds at once: refused.
            Script::Tool("", "dictionary_propose", json!({"word": "Hemera"})),
            Script::Tool("", "dictionary_add", json!({"word": "Hemera"})),
            Script::Text(vec!["Tu confirmes ?"]),
            // Second turn, after the user's yes: added.
            Script::Tool("", "dictionary_add", json!({"word": "Hemera"})),
            Script::Text(vec!["C'est ajouté."]),
        ],
        |_| {},
    )
    .await;
    let key = b.conversations.open();
    let session = b.brain.open(&key).await.unwrap();
    b.conversations.spoke(&key, TurnId::new());
    let mut events = b.brain.prompt(&session, "ajoute Hemera".into());
    collect(&mut events).await;
    assert!(b.hearing.asked.lock().unwrap().is_empty());
    let refused = b.messages(2).last().unwrap().1.clone();
    assert!(refused.starts_with("=> Erreur"), "{refused}");

    b.conversations.spoke(&key, TurnId::new());
    let mut events = b.brain.prompt(&session, "oui".into());
    let done = collect(&mut events).await;
    assert_eq!(done.last(), Some(&Event::Done(Ok("end_turn".into()))));
    let asked = b.hearing.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].word, "Hemera");
}

#[tokio::test]
async fn fast_mode_sends_its_beta_header_and_speed() {
    let b = bench(vec![Script::Text(vec!["Vite."])], |s| {
        s.model = "claude-opus-5-5".to_owned();
        s.fast = true;
    })
    .await;
    let session = b.brain.open(&ConversationKey("c1".into())).await.unwrap();
    let mut events = b.brain.prompt(&session, "vite".into());
    collect(&mut events).await;
    assert_eq!(
        *b.fake.betas.lock().unwrap(),
        [Some("fast-mode-2026-02-01".to_owned())]
    );
    assert_eq!(b.body(0)["speed"], "fast");
}

#[tokio::test]
async fn without_the_cache_no_point_is_marked() {
    let b = bench(vec![Script::Text(vec!["Oui."])], |s| s.cache = false).await;
    let session = b.brain.open(&ConversationKey("c1".into())).await.unwrap();
    let mut events = b.brain.prompt(&session, "bonjour".into());
    collect(&mut events).await;
    assert!(!b.body(0).to_string().contains("cache_control"));
}
