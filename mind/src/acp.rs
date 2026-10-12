//! The LLM through ACP: `mind` is the client of an ACP agent (`claude-agent-acp`), started as a
//! child process and driven over its standard input and output.
//!
//! One connection for the life of `mind`, one session per conversation. The settings of a session
//! (`_meta` of `session/new`) are those measured on 2026-10-11: our own system prompt, no tool of
//! Claude Code's own, none of the user's settings or skills, only Emory's MCP server, no thinking.

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
            McpServer, McpServerHttp, NewSessionRequest, PermissionOptionKind, PromptRequest,
            RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
            SelectedPermissionOutcome, SessionNotification, SessionUpdate, TextContent,
        },
    },
};
use anyhow::{Context, anyhow};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::brain::{Brain, Event, SessionId};

/// How a session is set up: what goes into `session/new`.
#[derive(Clone, Debug)]
pub struct SessionSettings {
    pub system_prompt: String,
    pub model: String,
    pub effort: String,
    pub workdir: PathBuf,
    /// Emory's MCP server, the only one the LLM gets.
    pub mcp_url: String,
    /// The key of Emory's MCP server, sent by the adapter in `Authorization`.
    pub mcp_key: String,
}

impl SessionSettings {
    /// The `session/new` request, with the settings measured on 2026-10-11 in `_meta`.
    pub fn request(&self) -> NewSessionRequest {
        let server = McpServerHttp::new("emory", &self.mcp_url).headers(vec![HttpHeader::new(
            "Authorization",
            format!("Bearer {}", self.mcp_key),
        )]);
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
                    "thinking": { "type": "disabled" },
                }
            }
        });
        match meta {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }
}

/// The streams of the requests in progress, by session.
type Streams = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Event>>>>;

enum Command {
    Open(oneshot::Sender<anyhow::Result<SessionId>>),
    Prompt {
        session: SessionId,
        text: String,
        events: mpsc::UnboundedSender<Event>,
        sent: oneshot::Sender<anyhow::Result<()>>,
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
    async fn open(&self) -> anyhow::Result<SessionId> {
        let (reply, answer) = oneshot::channel();
        self.send(Command::Open(reply))?;
        answer.await.context("the ACP connection is closed")?
    }

    async fn prompt(
        &self,
        session: &SessionId,
        text: String,
    ) -> anyhow::Result<mpsc::UnboundedReceiver<Event>> {
        let (events, receiver) = mpsc::unbounded_channel();
        let (sent, is_sent) = oneshot::channel();
        self.send(Command::Prompt {
            session: session.clone(),
            text,
            events,
            sent,
        })?;
        is_sent.await.context("the ACP connection is closed")??;
        Ok(receiver)
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
                // Claude Code has no tool of its own: whatever asks is a tool of Emory's server,
                // which `mind` controls itself.
                let allow = request
                    .options
                    .iter()
                    .find(|o| o.kind == PermissionOptionKind::AllowOnce)
                    .or(request.options.first())
                    .map(|o| o.option_id.clone());
                responder.respond(RequestPermissionResponse::new(match allow {
                    Some(id) => {
                        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id))
                    }
                    None => RequestPermissionOutcome::Cancelled,
                }))
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
        Command::Open(reply) => {
            let cx = cx.clone();
            let request = settings.request();
            tokio::spawn(async move {
                let result = cx
                    .send_request(request)
                    .block_task()
                    .await
                    .map(|r| SessionId(r.session_id.0.to_string()))
                    .map_err(|e| anyhow!("ACP session/new: {e}"));
                let _ = reply.send(result);
            });
        }
        Command::Prompt {
            session,
            text,
            events,
            sent,
        } => {
            streams
                .lock()
                .unwrap()
                .insert(session.0.clone(), events.clone());
            let request = PromptRequest::new(
                session.0.clone(),
                vec![ContentBlock::Text(TextContent::new(text))],
            );
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
                    let mut streams = streams.lock().unwrap();
                    if let Some(current) = streams.get(&session.0)
                        && current.same_channel(&events)
                    {
                        streams.remove(&session.0);
                    }
                    let _ = events.send(Event::Done(done));
                    Ok(())
                });
            let _ = sent.send(published.map_err(|e| anyhow!("ACP session/prompt: {e}")));
        }
        Command::Cancel(session) => {
            debug!(%session, "cancelling the request in progress");
            if let Err(e) = cx.send_notification(CancelNotification::new(session.0)) {
                warn!("ACP session/cancel: {e}");
            }
        }
        Command::Close(session) => {
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
            workdir: "/tmp/emory".into(),
            mcp_url: "http://127.0.0.1:4321/mcp".to_owned(),
            mcp_key: "k".to_owned(),
        };
        let request = serde_json::to_value(settings.request()).unwrap();
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
    }
}
