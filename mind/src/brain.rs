//! What the mind asks of the LLM, whatever drives it: a session per conversation, a request per
//! turn that answers in a stream of text, and its cancellation. The ACP adapter is in
//! [`crate::acp`]; the tests drive the conversation with a fake.

use std::future::Future;

use tokio::sync::mpsc;

/// A session of the LLM, open for the life of a conversation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What comes back from a request, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A piece of the answer's text.
    Text(String),
    /// The LLM calls a tool of Emory's MCP server (its name, for the logs).
    Tool(String),
    /// The request is over: why it stopped (`end_turn`, `cancelled`…), or what failed.
    Done(Result<String, String>),
}

pub trait Brain: Send + Sync + 'static {
    /// Opens a session for a new conversation.
    fn open(&self) -> impl Future<Output = anyhow::Result<SessionId>> + Send;

    /// Sends the text of a turn; the answer streams on the receiver, ending with
    /// [`Event::Done`].
    fn prompt(
        &self,
        session: &SessionId,
        text: String,
    ) -> impl Future<Output = anyhow::Result<mpsc::UnboundedReceiver<Event>>> + Send;

    /// Stops the request in progress on this session, if any: it ends soon with `Done`.
    fn cancel(&self, session: &SessionId);

    /// The conversation is over: the session may be forgotten.
    fn close(&self, session: &SessionId);
}
