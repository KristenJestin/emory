//! The words the mind adds to the vocabulary, from the bus (`hearing.vocabulary.add`, a request
//! with a reply): written to the file and used from the next sentence (see [`crate::hotwords`]).

use std::sync::Arc;

use anyhow::{Context, Result};
use async_nats::{Client, Message};
use contract::{VocabularyAdd, VocabularyReply, subjects};
use futures_util::StreamExt;
use tracing::{info, warn};

use crate::hotwords::{Added, Vocabulary};

/// The reply to a request: the word written (or already there), or why not.
pub fn answer(vocabulary: &Vocabulary, takes_words: bool, payload: &[u8]) -> VocabularyReply {
    let request: VocabularyAdd = match serde_json::from_slice(payload) {
        Ok(request) => request,
        Err(e) => return VocabularyReply::failure(format!("demande illisible : {e}")),
    };
    match vocabulary.add(&request.word, request.heard_as.as_deref()) {
        Ok(added) => {
            // The word itself stays out of the logs, like everything the user said.
            info!(
                known = added == Added::Known,
                in_use = takes_words,
                words = vocabulary.words().list.len(),
                "word added to the vocabulary"
            );
            if !takes_words && added == Added::New {
                warn!(
                    "the greedy search takes no words: the word counts from the next start"
                );
            }
            VocabularyReply::success(added == Added::Known, takes_words)
        }
        Err(error) => {
            warn!("word refused for the vocabulary: {error}");
            VocabularyReply::failure(error)
        }
    }
}

/// Answers a request in a task of its own: the file is written away from the bus loop.
pub fn handle(client: Client, vocabulary: Arc<Vocabulary>, takes_words: bool, message: Message) {
    tokio::spawn(async move {
        let Some(reply_to) = message.reply.clone() else {
            warn!("vocabulary: a request without a reply subject, ignored");
            return;
        };
        let reply =
            tokio::task::spawn_blocking(move || answer(&vocabulary, takes_words, &message.payload))
                .await
                .unwrap_or_else(|e| VocabularyReply::failure(format!("erreur interne : {e}")));
        let payload = match serde_json::to_vec(&reply) {
            Ok(payload) => payload,
            Err(e) => {
                warn!("vocabulary: reply not serialised: {e}");
                return;
            }
        };
        if let Err(e) = client.publish(reply_to, payload.into()).await {
            warn!("vocabulary: reply not sent: {e}");
        }
    });
}

/// Serves the requests until the subscription ends (the service does it in its own loop; this is
/// for the tests and the tools).
pub async fn serve(client: Client, vocabulary: Arc<Vocabulary>, takes_words: bool) -> Result<()> {
    let mut requests = client
        .subscribe(subjects::VOCABULARY_ADD)
        .await
        .context("subscribing to the vocabulary")?;
    while let Some(message) = requests.next().await {
        handle(client.clone(), vocabulary.clone(), takes_words, message);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_answered_from_the_vocabulary() {
        let folder =
            std::env::temp_dir().join(format!("hearing-vocabulary-bus-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let vocabulary = Vocabulary::load(&folder.join("hotwords.txt")).unwrap();
        let ask = |json: &str, takes_words| answer(&vocabulary, takes_words, json.as_bytes());
        assert_eq!(
            ask(r#"{"word":"Grafana","heard_as":"gras fana"}"#, true),
            VocabularyReply::success(false, true)
        );
        assert_eq!(
            ask(r#"{"word":"grafana"}"#, true),
            VocabularyReply::success(true, true)
        );
        assert_eq!(
            ask(r#"{"word":"Nextcloud"}"#, false),
            VocabularyReply::success(false, false)
        );
        let refused = ask(r#"{"word":"a/b"}"#, true);
        assert!(!refused.ok && refused.error.is_some());
        assert!(!ask("not json", true).ok);
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
