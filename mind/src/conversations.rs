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

use contract::{Device, TurnId};

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

/// What is known of an open conversation.
#[derive(Clone, Debug, Default)]
struct Open {
    /// The user's last turn, if any yet.
    turn: Option<TurnId>,
    /// The device it takes place on, when the mind said.
    device: Option<Device>,
}

#[derive(Default)]
struct Inner {
    open: HashMap<ConversationKey, Open>,
    /// The device of the user's last turn, in any conversation.
    last_device: Option<Device>,
}

/// The open conversations and the user's last turn in each. Cheap to clone, shared by the mind
/// (which opens, advances and closes them) and the tools (which read them).
#[derive(Clone, Default)]
pub struct Conversations(Arc<Mutex<Inner>>);

impl Conversations {
    /// A new conversation, before any turn.
    pub fn open(&self) -> ConversationKey {
        let key = ConversationKey::draw();
        self.lock().open.insert(key.clone(), Open::default());
        key
    }

    /// A new conversation on a device, before any turn: the tools that act where the user is (a
    /// timer rings on the device it was set on) find it with [`Self::device`].
    pub fn open_on(&self, device: Device) -> ConversationKey {
        let key = ConversationKey::draw();
        self.lock().open.insert(
            key.clone(),
            Open {
                turn: None,
                device: Some(device),
            },
        );
        key
    }

    /// The user has spoken: `turn` is the conversation's turn from now on.
    pub fn spoke(&self, key: &ConversationKey, turn: TurnId) {
        let mut inner = self.lock();
        if let Some(open) = inner.open.get_mut(key) {
            open.turn = Some(turn);
            if let Some(device) = open.device.clone() {
                inner.last_device = Some(device);
            }
        }
    }

    /// The conversation is over: what the tools kept for it goes with it.
    pub fn close(&self, key: &ConversationKey) {
        self.lock().open.remove(key);
    }

    /// `None` if the conversation is not open; else the user's last turn, if any yet.
    pub fn turn(&self, key: &ConversationKey) -> Option<Option<TurnId>> {
        self.lock().open.get(key).map(|open| open.turn)
    }

    /// The device of the conversation, if it is open and was opened with one.
    pub fn device(&self, key: &ConversationKey) -> Option<Device> {
        self.lock()
            .open
            .get(key)
            .and_then(|open| open.device.clone())
    }

    /// The device the user last spoke on, if any since the start.
    pub fn last_device(&self) -> Option<Device> {
        self.lock().last_device.clone()
    }

    pub fn is_open(&self, key: &ConversationKey) -> bool {
        self.lock().open.contains_key(key)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
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

    #[test]
    fn a_conversation_knows_its_device_when_opened_with_one() {
        let conversations = Conversations::default();
        let office = Device::new("office").unwrap();
        let on = conversations.open_on(office.clone());
        assert_eq!(conversations.device(&on), Some(office));
        assert_eq!(conversations.turn(&on), Some(None));
        assert_eq!(conversations.device(&conversations.open()), None);
        assert_eq!(conversations.last_device(), None);
        conversations.spoke(&on, TurnId::new());
        conversations.close(&on);
        assert_eq!(conversations.device(&on), None);
        // The last device spoken on outlives its conversation.
        assert_eq!(
            conversations.last_device(),
            Some(Device::new("office").unwrap())
        );
    }
}
