//! The ACP client against a fake ACP agent, in the same process: the settings of `session/new`,
//! the streamed text before the end of the request, the cancellation, the close.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_client_protocol::{
    Agent, Channel, Client, ConnectionTo,
    schema::v1::{
        AgentCapabilities, CancelNotification, CloseSessionRequest, CloseSessionResponse,
        ContentBlock, ContentChunk, InitializeRequest, InitializeResponse, NewSessionRequest,
        NewSessionResponse, PromptRequest, PromptResponse, SessionNotification, SessionUpdate,
        StopReason, TextContent,
    },
};
use mind::{
    acp::{AcpBrain, SessionSettings},
    brain::{Brain, Event},
};
use serde_json::Value;
use tokio::sync::Notify;

#[derive(Default)]
struct Seen {
    new_session: Mutex<Option<Value>>,
    prompts: Mutex<Vec<String>>,
    closed: Mutex<Vec<String>>,
    cancelled: Notify,
}

fn chunk(session: &str, text: &str) -> SessionNotification {
    SessionNotification::new(
        session.to_owned(),
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            text,
        )))),
    )
}

/// The fake agent: `stream` answers in pieces, `wait` starts then waits for `session/cancel`.
fn start_agent(seen: Arc<Seen>) -> Channel {
    let (client_end, agent_end) = Channel::duplex();
    let on_new = seen.clone();
    let on_prompt = seen.clone();
    let on_cancel = seen.clone();
    let on_close = seen;
    tokio::spawn(async move {
        let _ = Agent
            .builder()
            .name("fake-agent")
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
                    *on_new.new_session.lock().unwrap() =
                        Some(serde_json::to_value(&request).unwrap());
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
                    on_prompt.prompts.lock().unwrap().push(text.clone());
                    let session = request.session_id.0.to_string();
                    let seen = on_prompt.clone();
                    let sender = cx.clone();
                    cx.spawn(async move {
                        if text.contains("wait") {
                            let cancelled = seen.cancelled.notified();
                            sender.send_notification(chunk(&session, "Début. "))?;
                            cancelled.await;
                            responder.respond(PromptResponse::new(StopReason::Cancelled))
                        } else {
                            for piece in ["Un. ", "Deux", " trois."] {
                                sender.send_notification(chunk(&session, piece))?;
                            }
                            responder.respond(PromptResponse::new(StopReason::EndTurn))
                        }
                    })
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_notification(
                async move |_: CancelNotification, _cx| {
                    on_cancel.cancelled.notify_waiters();
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                async move |request: CloseSessionRequest, responder, _cx| {
                    on_close
                        .closed
                        .lock()
                        .unwrap()
                        .push(request.session_id.0.to_string());
                    responder.respond(CloseSessionResponse::new())
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(agent_end)
            .await;
    });
    client_end
}

fn settings() -> SessionSettings {
    SessionSettings {
        system_prompt: "Tu es Emory.".to_owned(),
        model: "claude-haiku-5-5".to_owned(),
        effort: "low".to_owned(),
        workdir: "/tmp".into(),
        mcp_url: "http://127.0.0.1:1/mcp".to_owned(),
        mcp_key: "key".to_owned(),
    }
}

async fn collect(events: &mut tokio::sync::mpsc::UnboundedReceiver<Event>) -> Vec<Event> {
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

#[tokio::test]
async fn a_session_streams_its_answer_then_ends() {
    let seen = Arc::new(Seen::default());
    let brain = AcpBrain::connect(start_agent(seen.clone()), settings())
        .await
        .unwrap();
    let session = brain.open().await.unwrap();
    assert_eq!(session.0, "session-1");
    let request = seen.new_session.lock().unwrap().clone().unwrap();
    assert_eq!(request["_meta"]["systemPrompt"], "Tu es Emory.");
    assert_eq!(
        request["_meta"]["claudeCode"]["options"]["tools"],
        serde_json::json!([])
    );
    assert_eq!(request["mcpServers"][0]["url"], "http://127.0.0.1:1/mcp");

    let mut events = brain.prompt(&session, "dis trois".to_owned());
    // Every piece arrives before the end of the request, in order.
    assert_eq!(
        collect(&mut events).await,
        [
            Event::Text("Un. ".into()),
            Event::Text("Deux".into()),
            Event::Text(" trois.".into()),
            Event::Done(Ok("end_turn".into()))
        ]
    );
    assert_eq!(*seen.prompts.lock().unwrap(), ["dis trois"]);

    brain.close(&session);
    tokio::time::timeout(Duration::from_secs(5), async {
        while seen.closed.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("session/close never came");
}

#[tokio::test]
async fn a_cancelled_request_ends_cancelled_and_the_session_goes_on() {
    let seen = Arc::new(Seen::default());
    let brain = AcpBrain::connect(start_agent(seen.clone()), settings())
        .await
        .unwrap();
    let session = brain.open().await.unwrap();
    let mut events = brain.prompt(&session, "wait".to_owned());
    assert_eq!(events.recv().await, Some(Event::Text("Début. ".into())));
    brain.cancel(&session);
    assert_eq!(
        collect(&mut events).await,
        [Event::Done(Ok("cancelled".into()))]
    );
    let mut events = brain.prompt(&session, "encore".to_owned());
    assert_eq!(
        collect(&mut events).await.last(),
        Some(&Event::Done(Ok("end_turn".into())))
    );
}

#[tokio::test]
async fn a_request_on_a_busy_session_is_refused_and_takes_none_of_its_text() {
    let seen = Arc::new(Seen::default());
    let brain = AcpBrain::connect(start_agent(seen.clone()), settings())
        .await
        .unwrap();
    let session = brain.open().await.unwrap();
    let mut first = brain.prompt(&session, "wait".to_owned());
    assert_eq!(first.recv().await, Some(Event::Text("Début. ".into())));
    let mut second = brain.prompt(&session, "encore".to_owned());
    assert!(matches!(
        collect(&mut second).await[..],
        [Event::Done(Err(_))]
    ));
    // The refused request never reached the agent; the first one still gets its end.
    brain.cancel(&session);
    assert_eq!(
        collect(&mut first).await,
        [Event::Done(Ok("cancelled".into()))]
    );
    assert_eq!(*seen.prompts.lock().unwrap(), ["wait"]);
}

#[tokio::test]
async fn closing_a_session_ends_its_request() {
    let seen = Arc::new(Seen::default());
    let brain = AcpBrain::connect(start_agent(seen.clone()), settings())
        .await
        .unwrap();
    let session = brain.open().await.unwrap();
    let mut events = brain.prompt(&session, "wait".to_owned());
    assert_eq!(events.recv().await, Some(Event::Text("Début. ".into())));
    // The agent does not answer: the close gives the session up.
    brain.close(&session);
    assert!(matches!(
        collect(&mut events).await[..],
        [Event::Done(Err(_))]
    ));
}
