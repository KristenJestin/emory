//! The LLM straight through a provider's API, with `genai`: `mind` sends the request itself,
//! runs the tools the model asks for in its own process, and sends their results back until the
//! answer.
//!
//! A session is the history of a conversation, kept here: the instructions and Emory's tools go
//! with every request, the history after them. The tools are those of Emory's MCP server, called
//! through the same modules ([`Tools`]) with the key of the conversation, so their rules (the
//! dictionary waits for the user's yes) hold the same way. No MCP server is needed in this mode.
//!
//! The key is read from its file at each request, and goes nowhere else: not in the logs, not in
//! an error, not in the environment.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use anyhow::{Context, anyhow, bail};
use futures_util::StreamExt;
use genai::{
    Client, ModelIden, ServiceTarget,
    adapter::AdapterKind,
    chat::{
        CacheControl, ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent, ContentPart,
        ReasoningEffort, Tool, ToolResponse,
    },
    resolver::{AuthData, Endpoint, ServiceTargetResolver},
};
use rmcp::model::CallToolResult;
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::{
    brain::{Brain, Event, SessionId},
    config::Thinking,
    conversations::ConversationKey,
    tools::Tools,
};

/// A provider `mind` can call, and how.
#[derive(Clone, Copy, Debug)]
pub struct Provider {
    /// Its name in `llm.provider`.
    pub name: &'static str,
    /// The protocol `genai` speaks to it.
    pub kind: AdapterKind,
    /// Its API, when `genai` does not know it (`None`: `llm.base_url` is needed).
    pub endpoint: Option<&'static str>,
    /// Whether a key is needed.
    pub needs_key: bool,
    /// Anthropic's Messages API: prompt caching, effort, fast mode.
    pub anthropic: bool,
}

const fn provider(name: &'static str, kind: AdapterKind) -> Provider {
    Provider {
        name,
        kind,
        endpoint: Some(""),
        needs_key: true,
        anthropic: false,
    }
}

/// The providers of `llm.provider`. An empty endpoint is `genai`'s own; `openai_compatible`
/// is any server that speaks OpenAI's Chat Completions, at `llm.base_url`.
pub const PROVIDERS: [Provider; 14] = [
    Provider {
        anthropic: true,
        ..provider("anthropic", AdapterKind::Anthropic)
    },
    provider("openai", AdapterKind::OpenAI),
    provider("openai_resp", AdapterKind::OpenAIResp),
    provider("gemini", AdapterKind::Gemini),
    provider("groq", AdapterKind::Groq),
    provider("mistral", AdapterKind::Mistral),
    provider("deepseek", AdapterKind::DeepSeek),
    provider("xai", AdapterKind::Xai),
    provider("together", AdapterKind::Together),
    provider("fireworks", AdapterKind::Fireworks),
    provider("open_router", AdapterKind::OpenRouter),
    Provider {
        endpoint: Some("https://api.cerebras.ai/v1/"),
        ..provider("cerebras", AdapterKind::OpenAI)
    },
    Provider {
        needs_key: false,
        ..provider("ollama", AdapterKind::Ollama)
    },
    Provider {
        endpoint: None,
        ..provider("openai_compatible", AdapterKind::OpenAI)
    },
];

impl Provider {
    pub fn named(name: &str) -> Option<Self> {
        PROVIDERS.iter().find(|p| p.name == name).copied()
    }
}

/// The beta header and the body field of Anthropic's fast mode.
const FAST_BETA: &str = "fast-mode-2026-02-01";
/// Tool rounds after which a turn is given up.
const MAX_ROUNDS: usize = 6;

/// How the requests are made: what `[llm]` says in `api` mode.
#[derive(Clone, Debug)]
pub struct ApiSettings {
    pub provider: Provider,
    pub model: String,
    /// Another address for the API (`llm.base_url`, or a fake server in the tests).
    pub base_url: Option<String>,
    pub key_file: Option<PathBuf>,
    pub system_prompt: String,
    pub thinking: Thinking,
    pub effort: Option<String>,
    pub max_tokens: u32,
    /// Prompt caching (Anthropic): the instructions and the tools, and the end of the history.
    pub cache: bool,
    pub fast: bool,
}

impl ApiSettings {
    /// The options of every request: what to capture of the stream, the thinking, the effort,
    /// fast mode.
    pub fn options(&self) -> ChatOptions {
        let mut options = ChatOptions::default()
            .with_capture_usage(true)
            .with_capture_content(true)
            .with_capture_tool_calls(true)
            .with_max_tokens(self.max_tokens);
        let anthropic = self.provider.anthropic;
        let mut body = Map::new();
        match self.thinking {
            Thinking::Off if anthropic => {
                // Sonnet 5.5 refuses `thinking: disabled` (measured on 2026-10-13): its way of
                // not thinking before it answers is `between_tools`.
                if self.model.contains("sonnet-5") {
                    body.insert("thinking".into(), json!({"type": "between_tools"}));
                } else {
                    options = options.with_reasoning_effort(ReasoningEffort::Zero);
                }
                if let Some(effort) = &self.effort {
                    body.insert("output_config".into(), json!({"effort": effort}));
                }
            }
            // Gemini thinks by default; `genai` turns `Zero` into its budget of 0.
            Thinking::Off if self.provider.kind == AdapterKind::Gemini => {
                options = options.with_reasoning_effort(ReasoningEffort::Zero);
            }
            // The other providers: no parameter, which models without reasoning would refuse.
            Thinking::Off => {}
            Thinking::Adaptive => match &self.effort {
                Some(effort) => options = options.with_reasoning_effort(reasoning(effort)),
                None => {
                    body.insert("thinking".into(), json!({"type": "adaptive"}));
                }
            },
            Thinking::Budget(n) => {
                options = options.with_reasoning_effort(ReasoningEffort::Budget(n))
            }
        }
        if self.thinking != Thinking::Off {
            // The signed thinking goes back with the tool results, or Anthropic refuses them.
            options = options.with_capture_reasoning_content(true);
        }
        if self.fast {
            options = options.with_extra_headers([("anthropic-beta", FAST_BETA)]);
            body.insert("speed".into(), json!("fast"));
        }
        if !body.is_empty() {
            options = options.with_extra_body(Value::Object(body));
        }
        options
    }

    /// The request of one round: the instructions, the history, the tools. With the cache, a
    /// point on the instructions (which covers the tools before them) and one on the last
    /// message, which moves with the conversation: the next round reads all of it from the cache.
    fn request(&self, history: &[ChatMessage], tools: &[Tool]) -> ChatRequest {
        let cache = self.cache && self.provider.anthropic;
        let mut system = ChatMessage::system(self.system_prompt.clone());
        if cache {
            system = system.with_options(CacheControl::Ephemeral);
        }
        let mut messages = Vec::with_capacity(history.len() + 1);
        messages.push(system);
        messages.extend(history.iter().cloned());
        if cache && let Some(last) = messages.last_mut().filter(|_| !history.is_empty()) {
            *last = last.clone().with_options(CacheControl::Ephemeral);
        }
        let request = ChatRequest::new(messages);
        if tools.is_empty() {
            request
        } else {
            request.with_tools(tools.to_vec())
        }
    }

    /// The name `genai` resolves the provider's protocol and address from.
    fn model_name(&self) -> String {
        format!("{}::{}", self.provider.kind.as_lower_str(), self.model)
    }
}

fn reasoning(effort: &str) -> ReasoningEffort {
    match effort {
        "minimal" => ReasoningEffort::Minimal,
        "low" => ReasoningEffort::Low,
        "medium" => ReasoningEffort::Medium,
        "xhigh" => ReasoningEffort::XHigh,
        "max" => ReasoningEffort::Max,
        _ => ReasoningEffort::High,
    }
}

fn read_key(path: &PathBuf) -> anyhow::Result<String> {
    let key = std::fs::read_to_string(path)
        .with_context(|| format!("reading the key file {}", path.display()))?;
    Ok(key.trim().to_owned())
}

/// The client: the provider's address, and the key read from its file at each request.
fn client(settings: &ApiSettings) -> anyhow::Result<Client> {
    let provider = settings.provider;
    let base_url = settings.base_url.clone().or_else(|| {
        provider
            .endpoint
            .filter(|e| !e.is_empty())
            .map(str::to_owned)
    });
    let key_file = settings.key_file.clone();
    let model = settings.model.clone();
    let resolver = ServiceTargetResolver::from_resolver_fn(
        move |target: ServiceTarget| -> Result<ServiceTarget, genai::resolver::Error> {
            let auth = match &key_file {
                Some(file) => AuthData::from_single(
                    read_key(file).map_err(|e| genai::resolver::Error::Custom(format!("{e:#}")))?,
                ),
                // A local server: no key, and none taken from the environment.
                None => AuthData::from_single(""),
            };
            let endpoint = match &base_url {
                Some(url) => Endpoint::from_owned(url.clone()),
                None => target.endpoint,
            };
            Ok(ServiceTarget {
                endpoint,
                auth,
                model: ModelIden::new(provider.kind, model.clone()),
            })
        },
    );
    Client::builder()
        .with_service_target_resolver(resolver)
        .build()
        .map_err(|e| anyhow!("the API client: {e}"))
}

/// The text of a tool result, as the model reads it.
fn result_text(result: &CallToolResult) -> String {
    let mut text: String = result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty()
        && let Some(structured) = &result.structured_content
    {
        text = structured.to_string();
    }
    if result.is_error == Some(true) {
        text = format!("Erreur : {text}");
    }
    text
}

fn genai_tool(tool: &rmcp::model::Tool) -> Tool {
    Tool::new(tool.name.to_string())
        .with_description(tool.description.as_deref().unwrap_or_default().to_owned())
        .with_schema(Value::Object((*tool.input_schema).clone()))
}

/// A conversation: its key for the tools, its history, the tools offered when it opened.
struct Session {
    conversation: ConversationKey,
    history: Vec<ChatMessage>,
    tools: Arc<Vec<Tool>>,
    /// The request in progress: its number, and what stops it (taken by the cancel: the
    /// session stays busy until the request has ended).
    running: Option<(u64, Option<oneshot::Sender<()>>)>,
}

struct Inner {
    client: Client,
    settings: ApiSettings,
    options: ChatOptions,
    tools: Tools,
    sessions: Mutex<HashMap<String, Session>>,
    requests: AtomicU64,
}

/// The LLM through a provider's API. Cheap to clone.
#[derive(Clone)]
pub struct ApiBrain {
    inner: Arc<Inner>,
}

impl ApiBrain {
    pub fn new(settings: ApiSettings, tools: Tools) -> anyhow::Result<Self> {
        Ok(Self {
            inner: Arc::new(Inner {
                client: client(&settings)?,
                options: settings.options(),
                settings,
                tools,
                sessions: Mutex::default(),
                requests: AtomicU64::new(0),
            }),
        })
    }
}

impl Brain for ApiBrain {
    async fn open(&self, conversation: &ConversationKey) -> anyhow::Result<SessionId> {
        // Asked at each opening, as the ACP adapter does: a module that did not answer the first
        // time is asked again.
        let tools: Vec<Tool> = self
            .inner
            .tools
            .list()
            .await
            .iter()
            .map(genai_tool)
            .collect();
        let id = uuid::Uuid::new_v4().simple().to_string();
        self.inner.sessions.lock().unwrap().insert(
            id.clone(),
            Session {
                conversation: conversation.clone(),
                history: Vec::new(),
                tools: Arc::new(tools),
                running: None,
            },
        );
        Ok(SessionId(id))
    }

    fn prompt(&self, session: &SessionId, text: String) -> mpsc::UnboundedReceiver<Event> {
        let (events, receiver) = mpsc::unbounded_channel();
        let number = self.inner.requests.fetch_add(1, Ordering::Relaxed);
        let (stop, stopped) = oneshot::channel();
        let started = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            match sessions.get_mut(&session.0) {
                None => Err("no such session"),
                Some(s) if s.running.is_some() => Err("a request is still running on this session"),
                Some(s) => {
                    s.running = Some((number, Some(stop)));
                    Ok((s.history.clone(), s.tools.clone(), s.conversation.clone()))
                }
            }
        };
        match started {
            Err(e) => {
                let _ = events.send(Event::Done(Err(e.to_owned())));
            }
            Ok((history, tools, conversation)) => {
                let turn = Turn {
                    inner: self.inner.clone(),
                    session: session.0.clone(),
                    number,
                    conversation,
                    tools,
                    events,
                };
                tokio::spawn(turn.run(history, text, stopped));
            }
        }
        receiver
    }

    fn cancel(&self, session: &SessionId) {
        let mut sessions = self.inner.sessions.lock().unwrap();
        if let Some(stop) = sessions
            .get_mut(&session.0)
            .and_then(|s| s.running.as_mut())
            .and_then(|(_, stop)| stop.take())
        {
            let _ = stop.send(());
        }
    }

    fn close(&self, session: &SessionId) {
        // Its request, if any, sees its stop dropped and ends.
        self.inner.sessions.lock().unwrap().remove(&session.0);
    }
}

/// One request of a session, from the text of the turn to the answer.
struct Turn {
    inner: Arc<Inner>,
    session: String,
    number: u64,
    conversation: ConversationKey,
    tools: Arc<Vec<Tool>>,
    events: mpsc::UnboundedSender<Event>,
}

enum Ending {
    Answered(anyhow::Result<String>),
    Cancelled,
    Closed,
}

impl Turn {
    async fn run(self, before: Vec<ChatMessage>, text: String, stopped: oneshot::Receiver<()>) {
        let started = Instant::now();
        let mut history = before.clone();
        history.push(ChatMessage::user(text.clone()));
        // What the model wrote, all rounds together: what the owner may have heard.
        let mut spoken = String::new();
        let ending = tokio::select! {
            answered = self.rounds(&mut history, &mut spoken) => Ending::Answered(answered),
            stop = stopped => if stop.is_ok() { Ending::Cancelled } else { Ending::Closed },
        };
        let (kept, done) = match ending {
            Ending::Answered(Ok(reason)) => (Some(history), Ok(reason)),
            // The turn stays, with what was said of it: no tool call is left without its result.
            Ending::Cancelled => {
                let mut kept = before;
                kept.push(ChatMessage::user(text));
                if !spoken.is_empty() {
                    kept.push(ChatMessage::assistant(spoken));
                }
                (Some(kept), Ok("cancelled".to_owned()))
            }
            Ending::Closed => (None, Err("the session was closed".to_owned())),
            // The turn is dropped: the history stays as it was before it.
            Ending::Answered(Err(e)) => (Some(before), Err(self.redact(format!("{e:#}")))),
        };
        {
            let mut sessions = self.inner.sessions.lock().unwrap();
            if let Some(session) = sessions.get_mut(&self.session) {
                if let Some(history) = kept {
                    session.history = history;
                }
                if session
                    .running
                    .as_ref()
                    .is_some_and(|(n, _)| *n == self.number)
                {
                    session.running = None;
                }
            }
        }
        if let Err(e) = &done {
            warn!(
                ms = started.elapsed().as_millis() as u64,
                "API request failed: {e}"
            );
        }
        let _ = self.events.send(Event::Done(done));
    }

    /// The rounds of a turn: a request, the tools it asks for, again, until the answer.
    async fn rounds(
        &self,
        history: &mut Vec<ChatMessage>,
        spoken: &mut String,
    ) -> anyhow::Result<String> {
        let settings = &self.inner.settings;
        for round in 0..MAX_ROUNDS {
            let request = settings.request(history, &self.tools);
            let sent = Instant::now();
            let mut stream = self
                .inner
                .client
                .exec_chat_stream(&settings.model_name(), request, Some(&self.inner.options))
                .await
                .map_err(|e| anyhow!("{e}"))?;
            let mut first_text = None;
            let mut content = None;
            let mut stop = None;
            let mut usage = None;
            while let Some(event) = stream.stream.next().await {
                match event.map_err(|e| anyhow!("{e}"))? {
                    ChatStreamEvent::Chunk(chunk) if !chunk.content.is_empty() => {
                        first_text.get_or_insert(sent.elapsed().as_millis() as u64);
                        spoken.push_str(&chunk.content);
                        let _ = self.events.send(Event::Text(chunk.content));
                    }
                    ChatStreamEvent::End(end) => {
                        content = end.captured_content;
                        stop = end.captured_stop_reason;
                        usage = end.captured_usage;
                    }
                    _ => {}
                }
            }
            let calls: Vec<_> = content
                .as_ref()
                .map(|c| {
                    c.parts()
                        .iter()
                        .filter_map(|p| match p {
                            ContentPart::ToolCall(call) => Some(call.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            // The sizes and times only: what was said stays out of the logs.
            let usage = usage.unwrap_or_default();
            let details = usage.prompt_tokens_details.unwrap_or_default();
            info!(
                round,
                first_text_ms = first_text,
                ms = sent.elapsed().as_millis() as u64,
                input = usage.prompt_tokens.unwrap_or(0),
                cache_read = details.cached_tokens.unwrap_or(0),
                cache_write = details.cache_creation_tokens.unwrap_or(0),
                output = usage.completion_tokens.unwrap_or(0),
                tools = calls.len(),
                "API call over"
            );
            if calls.is_empty() {
                let text = content
                    .and_then(|c| c.into_joined_texts())
                    .unwrap_or_default();
                history.push(ChatMessage::assistant(text));
                return Ok(stop.map_or_else(|| "end_turn".to_owned(), |s| stop_reason(&s)));
            }
            history.push(ChatMessage::assistant(content.unwrap_or_default()));
            for call in calls {
                let _ = self.events.send(Event::Tool(call.fn_name.clone()));
                let result = self
                    .inner
                    .tools
                    .call_in(
                        &call.fn_name,
                        call.fn_arguments.as_object().cloned(),
                        Some(self.conversation.clone()),
                    )
                    .await;
                history.push(ChatMessage::from(ToolResponse::new(
                    call.call_id,
                    result_text(&result),
                )));
            }
        }
        bail!("more than {MAX_ROUNDS} tool rounds in one turn")
    }

    /// Hides the key if an error message ever holds it.
    fn redact(&self, text: String) -> String {
        match self.inner.settings.key_file.as_ref().map(read_key) {
            Some(Ok(key)) if key.len() >= 8 => text.replace(&key, "[key]"),
            _ => text,
        }
    }
}

fn stop_reason(reason: &genai::chat::StopReason) -> String {
    use genai::chat::StopReason as S;
    match reason {
        S::Completed(r)
        | S::MaxTokens(r)
        | S::ToolCall(r)
        | S::ContentFilter(r)
        | S::StopSequence(r)
        | S::Other(r) => r.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(model: &str, thinking: Thinking, effort: Option<&str>) -> ApiSettings {
        ApiSettings {
            provider: Provider::named("anthropic").unwrap(),
            model: model.to_owned(),
            base_url: None,
            key_file: None,
            system_prompt: "Tu es Emory.".to_owned(),
            thinking,
            effort: effort.map(str::to_owned),
            max_tokens: 2048,
            cache: true,
            fast: false,
        }
    }

    #[test]
    fn every_provider_has_a_way_to_its_api() {
        for p in PROVIDERS {
            assert_eq!(Provider::named(p.name).unwrap().name, p.name);
        }
        assert!(
            Provider::named("openai_compatible")
                .unwrap()
                .endpoint
                .is_none()
        );
        assert!(Provider::named("nope").is_none());
    }

    #[test]
    fn thinking_off_is_said_the_way_each_model_takes_it() {
        let haiku = settings("claude-haiku-5-5", Thinking::Off, None).options();
        assert!(matches!(
            haiku.reasoning_effort,
            Some(ReasoningEffort::Zero)
        ));
        assert!(haiku.extra_body.is_none());
        let sonnet = settings("claude-sonnet-5-5", Thinking::Off, Some("low")).options();
        assert!(sonnet.reasoning_effort.is_none());
        assert_eq!(
            sonnet.extra_body,
            Some(
                json!({"thinking": {"type": "between_tools"}, "output_config": {"effort": "low"}})
            )
        );
        let adaptive = settings("claude-haiku-5-5", Thinking::Adaptive, Some("medium")).options();
        assert!(matches!(
            adaptive.reasoning_effort,
            Some(ReasoningEffort::Medium)
        ));
        assert_eq!(adaptive.capture_reasoning_content, Some(true));
        let budget = settings("claude-haiku-5-5", Thinking::Budget(2000), None).options();
        assert!(matches!(
            budget.reasoning_effort,
            Some(ReasoningEffort::Budget(2000))
        ));
    }

    #[test]
    fn fast_mode_is_a_beta_header_and_a_field_of_the_body() {
        let mut fast = settings("claude-opus-5-5", Thinking::Off, None);
        fast.fast = true;
        let options = fast.options();
        assert_eq!(options.extra_body, Some(json!({"speed": "fast"})));
        let headers = format!("{:?}", options.extra_headers.unwrap());
        assert!(headers.contains("anthropic-beta") && headers.contains(FAST_BETA));
    }
}
