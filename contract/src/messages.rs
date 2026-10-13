//! The JSON messages. Optional fields have a default value, so that a message from an older
//! version can still be read.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Device, Timestamp, TurnId};

/// The start or the end of something: a button press, Emory's speech.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Start,
    End,
}

/// A press on the button of an ear (`ear.<name>.button`). The audio of the turn goes between the
/// start and the end.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Button {
    pub turn: TurnId,
    pub state: State,
    pub at: Timestamp,
}

/// A sentence closed by the VAD and transcribed while the owner is speaking (`hearing.sentence`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sentence {
    pub turn: TurnId,
    pub device: Device,
    /// The number of the sentence in the turn, starting at 0.
    pub index: u32,
    pub text: String,
    pub start: Timestamp,
    pub end: Timestamp,
}

/// The whole turn, transcribed at the second press (`hearing.turn`). `text` is empty when nothing
/// was heard: the mind still knows that the turn is over.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub turn: TurnId,
    pub device: Device,
    pub start: Timestamp,
    pub end: Timestamp,
    pub text: String,
    pub wake: Wake,
    /// `None` as long as the voices are not recognised.
    #[serde(default)]
    pub speaker: Option<Speaker>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcription_confidence: Option<f32>,
    /// The sentences of the hearing buffer, when the mind asks for them.
    #[serde(default)]
    pub context: Vec<ContextSentence>,
}

/// What woke Emory up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wake {
    /// The button ear.
    Button,
    /// The wake word.
    Word,
    /// Within the follow-up window.
    FollowUp,
}

/// Who spoke, when the voices are recognised.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Speaker {
    /// The stable label of the voiceprint (`alex`, `voix-7`).
    pub voice: String,
    /// The slug of the person in Hippocampe, when the voice has a name.
    #[serde(default)]
    pub person: Option<String>,
    pub origin: Origin,
    pub confidence: f32,
}

/// Where the name of a voice comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// The owner gave the name.
    Known,
    /// The system deduced it: what the mind writes about it is marked `inferred`.
    Inferred,
}

/// A sentence heard before the turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextSentence {
    pub start: Timestamp,
    pub voice: String,
    #[serde(default)]
    pub person: Option<String>,
    pub text: String,
}

/// A sentence of the answer to say on a device (`voice.say`), numbered in the turn; the voice
/// says them in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Say {
    pub turn: TurnId,
    pub device: Device,
    pub index: u32,
    pub text: String,
    /// `true` on the last sentence of the answer: the voice then knows that nothing more comes.
    #[serde(default)]
    pub last: bool,
}

/// Throws away everything the voice has not said yet for this turn (`voice.stop`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stop {
    pub turn: TurnId,
    pub device: Device,
}

/// The voice starts or finishes speaking on a device (`voice.<name>.speaking`): the hands of the
/// device lower the music, then raise it again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Speaking {
    pub state: State,
}

/// What an ear, a mouth or some hands can do (`ear.<name>.announce`, `mouth.<name>.announce`,
/// `hands.<name>.announce`), published at start-up and in answer to `announce.request`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Announce {
    pub device: Device,
    pub program: Program,
    pub version: String,
    pub modules: Vec<String>,
    /// The actions of some hands; an ear or a mouth has none.
    #[serde(default)]
    pub actions: Vec<Action>,
}

/// The program that announces itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Program {
    Ear,
    Mouth,
    Hands,
}

/// An action of some hands, which the mind turns into a tool of the LLM.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    /// `mpris.pause`: the subject is `hands.<name>.mpris.pause`.
    pub name: String,
    pub description: String,
    /// The JSON schema of the arguments.
    pub arguments: Value,
}

/// The answer of some hands to an action: `ok` with a result, or an error.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionReply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ActionReply {
    pub fn success(result: Value) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(error.into()),
        }
    }

    pub fn into_result(self) -> Result<Value, String> {
        if self.ok {
            Ok(self.result.unwrap_or(Value::Null))
        } else {
            Err(self
                .error
                .unwrap_or_else(|| "error without a message".to_owned()))
        }
    }
}

/// A word to add to the vocabulary of the hearing (`hearing.vocabulary.add`), a request with a
/// reply ([`VocabularyReply`]). The mind sends it only once the user has confirmed the word.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VocabularyAdd {
    /// The word or name, written as it should come out (`Grafana`).
    pub word: String,
    /// What the transcription gave instead, when it is known (`et mera`): kept beside the word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heard_as: Option<String>,
}

/// The answer of the hearing to [`VocabularyAdd`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VocabularyReply {
    pub ok: bool,
    /// The word was there already: nothing was written, and it is still a success.
    #[serde(default)]
    pub known: bool,
    /// The transcription uses the word from the next sentence on. `false` when the hearing runs
    /// the greedy search, which takes no vocabulary: the word is used from its next start.
    #[serde(default)]
    pub in_use: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl VocabularyReply {
    pub fn success(known: bool, in_use: bool) -> Self {
        Self {
            ok: true,
            known,
            in_use,
            error: None,
        }
    }

    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            known: false,
            in_use: false,
            error: Some(error.into()),
        }
    }
}
