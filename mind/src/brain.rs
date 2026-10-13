//! What the mind asks of the LLM, whatever drives it: a session per conversation, a request per
//! turn that answers in a stream of text, and its cancellation. The ACP adapter is in
//! [`crate::acp`]; the tests drive the conversation with a fake.

use std::future::Future;

use tokio::sync::mpsc;

use crate::conversations::ConversationKey;

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
    /// Opens a session for a conversation; its calls to Emory's MCP server carry the key of the
    /// conversation (see [`crate::conversations`]).
    fn open(
        &self,
        conversation: &ConversationKey,
    ) -> impl Future<Output = anyhow::Result<SessionId>> + Send;

    /// Sends the text of a turn, before returning: a [`Brain::cancel`] made after this call
    /// reaches the LLM after the request. The answer streams on the receiver, ending with
    /// [`Event::Done`]; a request that could not be sent ends at once with `Done(Err)`, and so
    /// does a request on a session whose previous request has not ended.
    fn prompt(&self, session: &SessionId, text: String) -> mpsc::UnboundedReceiver<Event>;

    /// Stops the request in progress on this session, if any: it ends soon with `Done`.
    fn cancel(&self, session: &SessionId);

    /// The conversation is over: the session may be forgotten. A request still running on it
    /// ends with `Done(Err)`.
    fn close(&self, session: &SessionId);
}
