//! The open conversations, as the tools see them: a key per conversation, and the turn it is at.
//!
//! The LLM's calls reach Emory's MCP server without saying which conversation they belong to.
//! Each conversation therefore draws a key, which its session gives to the server in a header
//! ([`HEADER`]); a tool finds there the conversation it works for, and the user's last turn in it.
//! Tools that must wait for the user (the dictionary asks before it adds a word) rely on it.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use contract::TurnId;

/// The header in which the LLM's MCP connection carries the key of its conversation.
pub const HEADER: &str = "emory-conversation";

/// The key of a conversation: drawn at random, never shown to the LLM.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConversationKey(pub String);

impl ConversationKey {
    fn draw() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }
}

impl std::fmt::Display for ConversationKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The open conversations and the user's last turn in each. Cheap to clone, shared by the mind
/// (which opens, advances and closes them) and the tools (which read them).
#[derive(Clone, Default)]
pub struct Conversations(Arc<Mutex<HashMap<ConversationKey, Option<TurnId>>>>);

impl Conversations {
    /// A new conversation, before any turn.
    pub fn open(&self) -> ConversationKey {
        let key = ConversationKey::draw();
        self.lock().insert(key.clone(), None);
        key
    }

    /// The user has spoken: `turn` is the conversation's turn from now on.
    pub fn spoke(&self, key: &ConversationKey, turn: TurnId) {
        if let Some(current) = self.lock().get_mut(key) {
            *current = Some(turn);
        }
    }

    /// The conversation is over: what the tools kept for it goes with it.
    pub fn close(&self, key: &ConversationKey) {
        self.lock().remove(key);
    }

    /// `None` if the conversation is not open; else the user's last turn, if any yet.
    pub fn turn(&self, key: &ConversationKey) -> Option<Option<TurnId>> {
        self.lock().get(key).copied()
    }

    pub fn is_open(&self, key: &ConversationKey) -> bool {
        self.lock().contains_key(key)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ConversationKey, Option<TurnId>>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_conversation_follows_the_turns_until_it_closes() {
        let conversations = Conversations::default();
        let key = conversations.open();
        assert_eq!(conversations.turn(&key), Some(None));
        let (first, second) = (TurnId::new(), TurnId::new());
        conversations.spoke(&key, first);
        assert_eq!(conversations.turn(&key), Some(Some(first)));
        conversations.spoke(&key, second);
        assert_eq!(conversations.turn(&key), Some(Some(second)));
        conversations.close(&key);
        assert_eq!(conversations.turn(&key), None);
        assert!(!conversations.is_open(&key));
        // A turn for a closed conversation does not open it again.
        conversations.spoke(&key, first);
        assert!(!conversations.is_open(&key));
        assert_ne!(conversations.open(), conversations.open());
    }
}
