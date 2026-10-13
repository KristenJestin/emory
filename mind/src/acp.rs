//! The LLM through ACP: `mind` is the client of an ACP agent (`claude-agent-acp`), started as a
//! child process and driven over its standard input and output.
//!
//! One connection for the life of `mind`, one session per conversation. The settings of a session
//! (`_meta` of `session/new`) are those measured on 2026-10-11: our own system prompt, no tool of
//! Claude Code's own, none of the user's settings or skills, only Emory's MCP server, no thinking
//! unless the configuration asks for it.
//!
//! Fast mode is the session option `fast` of `claude-agent-acp` (offered only for the models
//! that have it), set to `on` by `session/set_config_option` once the session is open.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use agent_client_protocol::{
    AcpAgent, AcpAgentConfig, Agent, Client, ConnectTo, ConnectionTo,
    schema::{
        ProtocolVersion,
        v1::{
            CancelNotification, CloseSessionRequest, ContentBlock, HttpHeader, InitializeRequest,
            McpServer, McpServerHttp, NewSessionRequest, NewSessionResponse, PermissionOptionKind,
            PromptRequest, RequestPermissionOutcome, RequestPermissionRequest,
            RequestPermissionResponse, SelectedPermissionOutcome, SessionConfigOptionValue,
            SessionNotification, SessionUpdate, SetSessionConfigOptionRequest, TextContent,
        },
    },
};
use anyhow::{Context, anyhow};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::{
    brain::{Brain, Event, SessionId},
    config::Thinking,
    conversations::{self, ConversationKey},
};

/// How a session is set up: what goes into `session/new`.
#[derive(Clone, Debug)]
pub struct SessionSettings {
    pub system_prompt: String,
    pub model: String,
    pub effort: String,
    pub thinking: Thinking,
    /// Fast mode: the session option [`FAST_OPTION`] set to `on`.
    pub fast: bool,
    pub workdir: PathBuf,
    /// Emory's MCP server, the only one the LLM gets.
    pub mcp_url: String,
    /// The key of Emory's MCP server, sent by the adapter in `Authorization`.
    pub mcp_key: String,
}

impl SessionSettings {
    /// The `session/new` request of a conversation, with the settings measured on 2026-10-11 in
    /// `_meta`.
    pub fn request(&self, conversation: &ConversationKey) -> NewSessionRequest {
        let server = McpServerHttp::new(MCP_SERVER, &self.mcp_url).headers(vec![
            HttpHeader::new("Authorization", format!("Bearer {}", self.mcp_key)),
            HttpHeader::new(conversations::HEADER, conversation.0.clone()),
        ]);
        NewSessionRequest::new(self.workdir.clone())
            .mcp_servers(vec![McpServer::Http(server)])
            .meta(self.meta())
    }

    fn meta(&self) -> serde_json::Map<String, Value> {
        let meta = json!({
            "systemPrompt": self.system_prompt,
            "claudeCode": {
                "options": {
                    "model": self.model,
                    "effort": self.effort,
                    "tools": [],
                    "settingSources": [],
                    "skills": [],
                    "strictMcpConfig": true,
                    "thinking": self.thinking_meta(),
                }
            }
        });
        match meta {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    /// The thinking of the Agent SDK's options.
    fn thinking_meta(&self) -> Value {
        match self.thinking {
            Thinking::Off => json!({ "type": "disabled" }),
            Thinking::Adaptive => json!({ "type": "adaptive" }),
            Thinking::Budget(n) => json!({ "type": "enabled", "budgetTokens": n }),
        }
    }
}

/// The session option of `claude-agent-acp` for fast mode, and its value when on.
pub const FAST_OPTION: &str = "fast";
const FAST_ON: &str = "on";

/// Turns fast mode on in a session just opened, if the agent offers it.
async fn fast_mode(cx: &ConnectionTo<Agent>, opened: &NewSessionResponse) -> anyhow::Result<()> {
    let offered = opened
        .config_options
        .iter()
        .flatten()
        .any(|o| o.id.0.as_ref() == FAST_OPTION);
    anyhow::ensure!(
        offered,
        "fast mode asked, but the agent offers no `{FAST_OPTION}` option for this model"
    );
    cx.send_request(SetSessionConfigOptionRequest::new(
        opened.session_id.clone(),
        FAST_OPTION,
        SessionConfigOptionValue::value_id(FAST_ON),
    ))
    .block_task()
    .await
    .map_err(|e| anyhow!("ACP session/set_config_option {FAST_OPTION}: {e}"))?;
    info!(session = %opened.session_id.0, "fast mode on");
    Ok(())
}

/// The streams of the requests in progress, by session.
type Streams = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Event>>>>;

enum Command {
    Open(ConversationKey, oneshot::Sender<anyhow::Result<SessionId>>),
    Prompt {
        session: SessionId,
        text: String,
        events: mpsc::UnboundedSender<Event>,
    },
    Cancel(SessionId),
    Close(SessionId),
}

/// The LLM behind an ACP agent. Cheap to clone; the connection lives in a task of its own.
#[derive(Clone)]
pub struct AcpBrain {
    commands: mpsc::UnboundedSender<Command>,
}

impl AcpBrain {
    /// Starts the adapter (`command args…`) and initialises the connection. The environment is
    /// passed on as it is: in development the adapter uses the local Claude Code login, and
    /// `CLAUDE_CODE_OAUTH_TOKEN`, when set, reaches it that way.
    pub async fn start(
        command: &str,
        args: &[String],
        settings: SessionSettings,
    ) -> anyhow::Result<Self> {
        let agent = AcpAgent::new(AcpAgentConfig::new(command).args(args));
        Self::connect(agent, settings).await
    }

    /// Initialises the connection over any transport (the adapter's process, or a fake agent in
    /// the tests).
    pub async fn connect(
        transport: impl ConnectTo<Client> + 'static,
        settings: SessionSettings,
    ) -> anyhow::Result<Self> {
        let (commands, receiver) = mpsc::unbounded_channel();
        let (ready, is_ready) = oneshot::channel();
        tokio::spawn(run(transport, settings, receiver, ready));
        is_ready
            .await
            .map_err(|_| anyhow!("the ACP connection ended during its start"))??;
        Ok(Self { commands })
    }

    /// Resolves when the connection to the agent has ended (the adapter died).
    pub async fn closed(&self) {
        self.commands.closed().await
    }

    fn send(&self, command: Command) -> anyhow::Result<()> {
        self.commands
            .send(command)
            .map_err(|_| anyhow!("the ACP connection is closed"))
    }
}

impl Brain for AcpBrain {
    async fn open(&self, conversation: &ConversationKey) -> anyhow::Result<SessionId> {
        let (reply, answer) = oneshot::channel();
        self.send(Command::Open(conversation.clone(), reply))?;
        answer.await.context("the ACP connection is closed")?
    }

    fn prompt(&self, session: &SessionId, text: String) -> mpsc::UnboundedReceiver<Event> {
        let (events, receiver) = mpsc::unbounded_channel();
        if let Err(e) = self.send(Command::Prompt {
            session: session.clone(),
            text,
            events: events.clone(),
        }) {
            let _ = events.send(Event::Done(Err(e.to_string())));
        }
        receiver
    }

    fn cancel(&self, session: &SessionId) {
        let _ = self.send(Command::Cancel(session.clone()));
    }

    fn close(&self, session: &SessionId) {
        let _ = self.send(Command::Close(session.clone()));
    }
}

/// The connection: its handlers route the updates to the stream of their session; the commands
/// of the mind become requests.
async fn run(
    transport: impl ConnectTo<Client> + 'static,
    settings: SessionSettings,
    mut commands: mpsc::UnboundedReceiver<Command>,
    ready: oneshot::Sender<anyhow::Result<()>>,
) {
    let streams: Streams = Arc::default();
    let updates = streams.clone();
    let ready = Arc::new(Mutex::new(Some(ready)));
    let started = ready.clone();
    let connection_streams = streams.clone();
    let result = Client
        .builder()
        .name("emory-mind")
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                route(&updates, notification);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _cx| {
                responder.respond(RequestPermissionResponse::new(permission(&request)))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport, async move |cx: ConnectionTo<Agent>| {
            let init = cx
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await;
            let init = match init {
                Ok(init) => init,
                Err(e) => {
                    if let Some(r) = started.lock().unwrap().take() {
                        let _ = r.send(Err(anyhow!("ACP initialize: {e}")));
                    }
                    return Ok(());
                }
            };
            info!(
                agent = ?init.agent_info.as_ref().map(|i| format!("{} {}", i.name, i.version)),
                "ACP agent ready"
            );
            if let Some(r) = started.lock().unwrap().take() {
                let _ = r.send(Ok(()));
            }
            while let Some(command) = commands.recv().await {
                handle(&cx, &settings, &connection_streams, command);
            }
            Ok(())
        })
        .await;
    let error = match result {
        Ok(()) => anyhow!("the ACP connection ended"),
        Err(e) => anyhow!("the ACP connection failed: {e}"),
    };
    warn!("{error}");
    if let Some(r) = ready.lock().unwrap().take() {
        let _ = r.send(Err(error));
    }
    // The requests in progress end with the connection.
    for (_, events) in streams.lock().unwrap().drain() {
        let _ = events.send(Event::Done(Err("the ACP connection ended".to_owned())));
    }
}

/// The name of Emory's MCP server in `session/new`: Claude Code calls its tools
/// `mcp__emory__<tool>`.
const MCP_SERVER: &str = "emory";

/// The answer to a permission request: once, for a tool of Emory's MCP server, which `mind`
/// controls itself; refused for anything else (Claude Code should have no tool of its own).
fn permission(request: &RequestPermissionRequest) -> RequestPermissionOutcome {
    let call = &request.tool_call;
    let name = call
        .meta
        .as_ref()
        .and_then(|m| m.get("claudeCode"))
        .and_then(|c| c.get("toolName"))
        .and_then(Value::as_str)
        .or(call.fields.name.as_deref())
        .or(call.fields.title.as_deref())
        .unwrap_or_default();
    let ours = name
        .strip_prefix("mcp__")
        .and_then(|rest| rest.strip_prefix(MCP_SERVER))
        .is_some_and(|rest| rest.starts_with("__"));
    let pick = |kind: PermissionOptionKind| {
        request
            .options
            .iter()
            .find(|o| o.kind == kind)
            .map(|o| o.option_id.clone())
    };
    let chosen = if ours {
        pick(PermissionOptionKind::AllowOnce)
    } else {
        warn!(
            tool = name,
            "permission refused: not a tool of Emory's MCP server"
        );
        pick(PermissionOptionKind::RejectOnce)
    };
    match chosen {
        Some(id) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)),
        None => {
            if ours {
                warn!(
                    tool = name,
                    "permission refused: no option to allow it once"
                );
            }
            RequestPermissionOutcome::Cancelled
        }
    }
}

fn route(streams: &Streams, notification: SessionNotification) {
    let session = notification.session_id.0.to_string();
    let event = match notification.update {
        SessionUpdate::AgentMessageChunk(chunk) => match chunk.content {
            ContentBlock::Text(text) => Event::Text(text.text),
            _ => return,
        },
        SessionUpdate::ToolCall(call) => Event::Tool(call.title),
        _ => return,
    };
    if let Some(events) = streams.lock().unwrap().get(&session) {
        let _ = events.send(event);
    }
}

fn handle(
    cx: &ConnectionTo<Agent>,
    settings: &SessionSettings,
    streams: &Streams,
    command: Command,
) {
    match command {
        Command::Open(conversation, reply) => {
            let cx = cx.clone();
            let request = settings.request(&conversation);
            let fast = settings.fast;
            tokio::spawn(async move {
                let result = async {
                    let opened = cx
                        .send_request(request)
                        .block_task()
                        .await
                        .map_err(|e| anyhow!("ACP session/new: {e}"))?;
                    if fast {
                        fast_mode(&cx, &opened).await?;
                    }
                    Ok(SessionId(opened.session_id.0.to_string()))
                }
                .await;
                let _ = reply.send(result);
            });
        }
        Command::Prompt {
            session,
            text,
            events,
        } => {
            // One request at a time on a session: the text of a request still running would be
            // taken for the answer of the new one.
            {
                let mut streams = streams.lock().unwrap();
                if streams.contains_key(&session.0) {
                    warn!(%session, "a request is still running on this session: refused");
                    let _ = events.send(Event::Done(Err(
                        "a request is still running on this session".to_owned(),
                    )));
                    return;
                }
                streams.insert(session.0.clone(), events.clone());
            }
            let request = PromptRequest::new(
                session.0.clone(),
                vec![ContentBlock::Text(TextContent::new(text))],
            );
            let failed = (streams.clone(), session.0.clone(), events.clone());
            let streams = streams.clone();
            // The answer is handled in order with the notifications: every update of the request
            // reaches the stream before its `Done`.
            let published = cx
                .prepare_request(request)
                .on_receiving_result(async move |result| {
                    let done = match result {
                        Ok(response) => Ok(stop_reason(&response.stop_reason)),
                        Err(e) => Err(e.to_string()),
                    };
                    forget(&streams, &session.0, &events);
                    let _ = events.send(Event::Done(done));
                    Ok(())
                });
            if let Err(e) = published {
                let (streams, session, events) = failed;
                forget(&streams, &session, &events);
                let _ = events.send(Event::Done(Err(format!("ACP session/prompt: {e}"))));
            }
        }
        Command::Cancel(session) => {
            debug!(%session, "cancelling the request in progress");
            if let Err(e) = cx.send_notification(CancelNotification::new(session.0)) {
                warn!("ACP session/cancel: {e}");
            }
        }
        Command::Close(session) => {
            // A request the agent never ended (it did not answer its cancel) ends here: the
            // session is given up.
            if let Some(events) = streams.lock().unwrap().remove(&session.0) {
                let _ = events.send(Event::Done(Err("the session was closed".to_owned())));
            }
            let cx = cx.clone();
            tokio::spawn(async move {
                if let Err(e) = cx
                    .send_request(CloseSessionRequest::new(session.0.clone()))
                    .block_task()
                    .await
                {
                    debug!(%session, "ACP session/close: {e}");
                }
            });
        }
    }
}

/// The request of this stream is over: the session may take another one.
fn forget(streams: &Streams, session: &str, events: &mpsc::UnboundedSender<Event>) {
    let mut streams = streams.lock().unwrap();
    if streams
        .get(session)
        .is_some_and(|current| current.same_channel(events))
    {
        streams.remove(session);
    }
}

fn stop_reason(reason: &impl serde::Serialize) -> String {
    match serde_json::to_value(reason) {
        Ok(Value::String(s)) => s,
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_carries_the_measured_settings() {
        let settings = SessionSettings {
            system_prompt: "Tu es Emory.".to_owned(),
            model: "claude-haiku-5-5".to_owned(),
            effort: "low".to_owned(),
            thinking: Thinking::Off,
            fast: false,
            workdir: "/tmp/emory".into(),
            mcp_url: "http://127.0.0.1:4321/mcp".to_owned(),
            mcp_key: "k".to_owned(),
        };
        let request =
            serde_json::to_value(settings.request(&ConversationKey("c-7".to_owned()))).unwrap();
        let meta = &request["_meta"];
        assert_eq!(meta["systemPrompt"], "Tu es Emory.");
        let options = &meta["claudeCode"]["options"];
        assert_eq!(options["model"], "claude-haiku-5-5");
        assert_eq!(options["effort"], "low");
        assert_eq!(options["tools"], json!([]));
        assert_eq!(options["settingSources"], json!([]));
        assert_eq!(options["skills"], json!([]));
        assert_eq!(options["strictMcpConfig"], true);
        assert_eq!(options["thinking"], json!({"type": "disabled"}));
        let servers = request["mcpServers"].as_array().unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0]["type"], "http");
        assert_eq!(servers[0]["name"], "emory");
        assert_eq!(servers[0]["url"], "http://127.0.0.1:4321/mcp");
        assert_eq!(servers[0]["headers"][0]["value"], "Bearer k");
        // The tools know which conversation calls them.
        assert_eq!(servers[0]["headers"][1]["name"], "emory-conversation");
        assert_eq!(servers[0]["headers"][1]["value"], "c-7");
    }

    #[test]
    fn the_thinking_of_the_configuration_reaches_the_sdk() {
        let mut settings = SessionSettings {
            system_prompt: String::new(),
            model: "m".to_owned(),
            effort: "low".to_owned(),
            thinking: Thinking::Adaptive,
            fast: false,
            workdir: "/tmp".into(),
            mcp_url: String::new(),
            mcp_key: String::new(),
        };
        assert_eq!(settings.thinking_meta(), json!({"type": "adaptive"}));
        settings.thinking = Thinking::Budget(3000);
        assert_eq!(
            settings.thinking_meta(),
            json!({"type": "enabled", "budgetTokens": 3000})
        );
    }

    fn asked(tool_call: Value) -> RequestPermissionRequest {
        serde_json::from_value(json!({
            "sessionId": "s",
            "toolCall": tool_call,
            "options": [
                {"optionId": "always", "name": "Always", "kind": "allow_always"},
                {"optionId": "once", "name": "Once", "kind": "allow_once"},
                {"optionId": "no", "name": "No", "kind": "reject_once"}
            ]
        }))
        .unwrap()
    }

    fn chosen(outcome: RequestPermissionOutcome) -> Option<String> {
        match outcome {
            RequestPermissionOutcome::Selected(s) => Some(s.option_id.0.to_string()),
            _ => None,
        }
    }

    #[test]
    fn only_the_tools_of_emory_are_allowed_and_only_once() {
        let meta = |name: &str| json!({"claudeCode": {"toolName": name}});
        let ours = asked(json!({"toolCallId": "t", "title": "x",
                                "_meta": meta("mcp__emory__hippocampe_search")}));
        assert_eq!(chosen(permission(&ours)).as_deref(), Some("once"));
        let by_title = asked(json!({"toolCallId": "t", "title": "mcp__emory__hippocampe_read"}));
        assert_eq!(chosen(permission(&by_title)).as_deref(), Some("once"));
        for other in [
            "Bash",
            "mcp__emoryx__search",
            "mcp__other__emory",
            "mcp__emory",
        ] {
            let request = asked(json!({"toolCallId": "t", "title": "x", "_meta": meta(other)}));
            assert_eq!(
                chosen(permission(&request)).as_deref(),
                Some("no"),
                "{other}"
            );
        }
        let untitled = asked(json!({"toolCallId": "t"}));
        assert_eq!(chosen(permission(&untitled)).as_deref(), Some("no"));
        // Without an option to allow once, nothing is allowed.
        let mut always_only = ours.clone();
        always_only
            .options
            .retain(|o| o.kind == PermissionOptionKind::AllowAlways);
        assert_eq!(chosen(permission(&always_only)), None);
    }
}
