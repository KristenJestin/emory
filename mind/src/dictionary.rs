//! The dictionary: the LLM adds a word to the vocabulary of the hearing, but only once the user
//! has said yes (`specs/mind.md`).
//!
//! Two tools. `propose` notes the word in the conversation and tells the LLM to ask. `add` sends
//! the word to the hearing (`hearing.vocabulary.add`), and is refused unless the word was proposed
//! in an earlier turn of the same conversation: the user must have spoken since. The rule is
//! held here, not left to the LLM. The proposals die with the conversation.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use contract::{TurnId, VocabularyAdd, VocabularyReply, subjects};
use futures_util::future::BoxFuture;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool};
use serde_json::{Value, json};
use tracing::info;

use crate::{
    conversations::{ConversationKey, Conversations},
    tools::Module,
};

/// The longest word, in characters (the hearing checks the rest).
const MAX_WORD: usize = 40;

/// Where the words go: the hearing on the bus, or a fake in the tests.
pub trait Vocabulary: Send + Sync + 'static {
    /// Adds a word; `Err` when the hearing could not be reached.
    fn add(&self, request: VocabularyAdd) -> BoxFuture<'_, Result<VocabularyReply, String>>;
}

/// The hearing, asked on the bus.
pub struct BusVocabulary {
    pub client: async_nats::Client,
    pub timeout: Duration,
}

impl Vocabulary for BusVocabulary {
    fn add(&self, request: VocabularyAdd) -> BoxFuture<'_, Result<VocabularyReply, String>> {
        Box::pin(async move {
            let payload = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
            let asked = self
                .client
                .request(subjects::VOCABULARY_ADD, payload.into());
            let reply = tokio::time::timeout(self.timeout, asked)
                .await
                .map_err(|_| "pas de réponse à temps".to_owned())?
                .map_err(|e| e.to_string())?;
            serde_json::from_slice(&reply.payload).map_err(|e| format!("réponse illisible : {e}"))
        })
    }
}

/// A word the LLM proposed, waiting for the user's yes.
#[derive(Clone, Debug)]
struct Proposal {
    word: String,
    heard_as: Option<String>,
    /// The user's turn when it was proposed: the yes must come in a later one.
    turn: Option<TurnId>,
}

pub struct Dictionary<V> {
    vocabulary: V,
    conversations: Conversations,
    /// By conversation, then by word (in lower case).
    proposals: Mutex<HashMap<ConversationKey, HashMap<String, Proposal>>>,
}

impl<V: Vocabulary> Dictionary<V> {
    pub fn new(vocabulary: V, conversations: Conversations) -> Self {
        Self {
            vocabulary,
            conversations,
            proposals: Mutex::default(),
        }
    }

    fn proposals(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<ConversationKey, HashMap<String, Proposal>>> {
        let mut proposals = self
            .proposals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // The proposals of the conversations that closed go with them.
        proposals.retain(|key, _| self.conversations.is_open(key));
        proposals
    }

    fn propose(
        &self,
        conversation: &ConversationKey,
        arguments: &JsonObject,
    ) -> Result<String, String> {
        let word = word(arguments, "word")?;
        let heard_as = arguments
            .get("heard_as")
            .and_then(Value::as_str)
            .map(tidy)
            .filter(|h| !h.is_empty());
        let Some(turn) = self.conversations.turn(conversation) else {
            return Err(NO_CONVERSATION.to_owned());
        };
        self.proposals()
            .entry(conversation.clone())
            .or_default()
            .insert(
                word.to_lowercase(),
                Proposal {
                    word: word.clone(),
                    heard_as,
                    turn,
                },
            );
        info!("dictionary: word proposed");
        Ok(format!(
            "Proposition notée. Demande à l'utilisateur s'il veut ajouter « {word} » au \
             dictionnaire, puis attends sa réponse : dictionary_add n'est accepté qu'après son oui."
        ))
    }

    async fn add(
        &self,
        conversation: &ConversationKey,
        arguments: &JsonObject,
    ) -> Result<String, String> {
        let word = word(arguments, "word")?;
        let Some(now) = self.conversations.turn(conversation) else {
            return Err(NO_CONVERSATION.to_owned());
        };
        let proposal = self
            .proposals()
            .get(conversation)
            .and_then(|p| p.get(&word.to_lowercase()))
            .cloned();
        let Some(proposal) = proposal else {
            info!("dictionary: add refused, never proposed");
            return Err(format!(
                "« {word} » n'a pas été proposé. Appelle d'abord dictionary_propose, demande à \
                 l'utilisateur, et n'ajoute qu'après son oui."
            ));
        };
        if proposal.turn == now {
            info!("dictionary: add refused, the user has not answered yet");
            return Err(format!(
                "L'utilisateur n'a pas encore répondu. Demande-lui s'il veut ajouter « {word} », \
                 et n'ajoute qu'après son oui."
            ));
        }
        let reply = self
            .vocabulary
            .add(VocabularyAdd {
                word: proposal.word.clone(),
                heard_as: proposal.heard_as.clone(),
            })
            .await
            .map_err(|e| {
                format!("L'écoute n'a pas pu être jointe ({e}) : le mot n'est pas ajouté.")
            })?;
        if !reply.ok {
            let why = reply
                .error
                .unwrap_or_else(|| "sans raison donnée".to_owned());
            return Err(format!("L'écoute a refusé le mot : {why}."));
        }
        if let Some(words) = self.proposals().get_mut(conversation) {
            words.remove(&word.to_lowercase());
        }
        info!(
            known = reply.known,
            in_use = reply.in_use,
            "dictionary: word added"
        );
        let word = proposal.word;
        Ok(match (reply.known, reply.in_use) {
            (true, _) => format!("« {word} » était déjà dans le dictionnaire."),
            (false, true) => format!("« {word} » est ajouté au dictionnaire."),
            (false, false) => format!(
                "« {word} » est ajouté au dictionnaire ; il servira au prochain démarrage de l'écoute."
            ),
        })
    }
}

const NO_CONVERSATION: &str = "Cet outil ne sert que dans une conversation.";

/// Spaces tidied: around and between the words.
fn tidy(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn word(arguments: &JsonObject, name: &str) -> Result<String, String> {
    let word = arguments
        .get(name)
        .and_then(Value::as_str)
        .map(tidy)
        .unwrap_or_default();
    if word.is_empty() {
        return Err(format!("« {name} » manque."));
    }
    if word.chars().count() > MAX_WORD {
        return Err(format!("Le mot dépasse {MAX_WORD} caractères."));
    }
    Ok(word)
}

fn schema(value: Value) -> Arc<JsonObject> {
    match value {
        Value::Object(map) => Arc::new(map),
        _ => unreachable!(),
    }
}

fn result(outcome: Result<String, String>) -> CallToolResult {
    match outcome {
        Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
        Err(text) => CallToolResult::error(vec![ContentBlock::text(text)]),
    }
}

impl<V: Vocabulary> Module for Dictionary<V> {
    fn name(&self) -> &'static str {
        "dictionary"
    }

    // Short: the descriptions go with every request.
    fn tools(&self) -> BoxFuture<'_, Vec<Tool>> {
        Box::pin(async {
            vec![
                Tool::new(
                    "propose",
                    "Un nom propre revient mal transcrit : propose-le pour le dictionnaire de la \
                     transcription, puis demande à l'utilisateur s'il veut l'ajouter.",
                    schema(json!({
                        "type": "object",
                        "properties": {
                            "word": {"type": "string", "description": "écrit comme il doit sortir"},
                            "heard_as": {"type": "string", "description": "ce que la transcription a donné"}
                        },
                        "required": ["word"]
                    })),
                ),
                Tool::new(
                    "add",
                    "Ajoute un mot proposé, seulement après le oui de l'utilisateur.",
                    schema(json!({
                        "type": "object",
                        "properties": {"word": {"type": "string"}},
                        "required": ["word"]
                    })),
                ),
            ]
        })
    }

    fn call(&self, _tool: &str, _arguments: Option<JsonObject>) -> BoxFuture<'_, CallToolResult> {
        Box::pin(async { result(Err(NO_CONVERSATION.to_owned())) })
    }

    fn call_in(
        &self,
        tool: &str,
        arguments: Option<JsonObject>,
        conversation: Option<ConversationKey>,
    ) -> BoxFuture<'_, CallToolResult> {
        let tool = tool.to_owned();
        Box::pin(async move {
            let Some(conversation) = conversation else {
                return result(Err(NO_CONVERSATION.to_owned()));
            };
            let arguments = arguments.unwrap_or_default();
            result(match tool.as_str() {
                "propose" => self.propose(&conversation, &arguments),
                "add" => self.add(&conversation, &arguments).await,
                _ => Err(format!("Outil inconnu : dictionary_{tool}")),
            })
        })
    }
}
