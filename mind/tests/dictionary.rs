//! The dictionary's tools through Emory's MCP server, with a fake hearing: a word is added only
//! once the user has spoken after it was proposed, in the same conversation.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::http::{HeaderName, HeaderValue};
use contract::{TurnId, VocabularyAdd, VocabularyReply};
use futures_util::future::BoxFuture;
use mind::{
    conversations::{self, ConversationKey, Conversations},
    dictionary::{Dictionary, Vocabulary},
    tools::{self, Tools},
};
use rmcp::{
    RoleClient, ServiceExt,
    model::{CallToolRequestParams, CallToolResult},
    service::RunningService,
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::json;

/// The hearing: records what it is asked, answers what the test says.
#[derive(Clone, Default)]
struct FakeHearing {
    asked: Arc<Mutex<Vec<VocabularyAdd>>>,
    /// `None`: the hearing cannot be reached.
    reply: Arc<Mutex<Option<VocabularyReply>>>,
}

impl Vocabulary for FakeHearing {
    fn add(&self, request: VocabularyAdd) -> BoxFuture<'_, Result<VocabularyReply, String>> {
        Box::pin(async move {
            self.asked.lock().unwrap().push(request);
            self.reply
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| "no responders".to_owned())
        })
    }
}

struct Bench {
    hearing: FakeHearing,
    conversations: Conversations,
    server: tools::Server,
}

async fn bench() -> Bench {
    let hearing = FakeHearing::default();
    *hearing.reply.lock().unwrap() = Some(VocabularyReply::success(false, true));
    let conversations = Conversations::default();
    let module = Dictionary::new(hearing.clone(), conversations.clone());
    let server = tools::serve(
        Tools::new(vec![Arc::new(module)]),
        ([127, 0, 0, 1], 0).into(),
    )
    .await
    .unwrap();
    Bench {
        hearing,
        conversations,
        server,
    }
}

impl Bench {
    /// The MCP connection of a conversation's session, as the adapter opens it.
    async fn client(
        &self,
        conversation: Option<&ConversationKey>,
    ) -> RunningService<RoleClient, ()> {
        let mut headers = HashMap::new();
        if let Some(key) = conversation {
            headers.insert(
                HeaderName::from_static(conversations::HEADER),
                HeaderValue::from_str(&key.0).unwrap(),
            );
        }
        ().serve(StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(self.server.url.as_str())
                .auth_header(self.server.key.clone())
                .custom_headers(headers),
        ))
        .await
        .unwrap()
    }

    /// A conversation where the user has spoken once.
    fn conversation(&self) -> ConversationKey {
        let key = self.conversations.open();
        self.conversations.spoke(&key, TurnId::new());
        key
    }

    fn asked(&self) -> Vec<VocabularyAdd> {
        self.hearing.asked.lock().unwrap().clone()
    }
}

async fn call(
    client: &RunningService<RoleClient, ()>,
    tool: &str,
    arguments: serde_json::Value,
) -> (bool, String) {
    let mut request = CallToolRequestParams::new(tool.to_owned());
    request.arguments = arguments.as_object().cloned();
    let result: CallToolResult = client.call_tool(request).await.unwrap();
    let text = result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join(" ");
    (result.is_error != Some(true), text)
}

#[tokio::test]
async fn the_tools_are_offered_under_their_module() {
    let b = bench().await;
    let client = b.client(None).await;
    let mut names: Vec<String> = client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["dictionary_add", "dictionary_propose"]);
}

#[tokio::test]
async fn a_word_is_added_only_after_the_user_spoke_again() {
    let b = bench().await;
    let key = b.conversation();
    let client = b.client(Some(&key)).await;

    let (ok, said) = call(
        &client,
        "dictionary_propose",
        json!({"word": " Grafana ", "heard_as": "gras fana"}),
    )
    .await;
    assert!(ok, "{said}");
    assert!(said.contains("Demande"), "{said}");

    // Same turn: the user has not answered.
    let (ok, said) = call(&client, "dictionary_add", json!({"word": "Grafana"})).await;
    assert!(!ok);
    assert!(said.contains("pas encore répondu"), "{said}");
    assert!(b.asked().is_empty());

    // The user speaks (« oui »): the word goes to the hearing, with what was heard.
    b.conversations.spoke(&key, TurnId::new());
    let (ok, said) = call(&client, "dictionary_add", json!({"word": "grafana"})).await;
    assert!(ok, "{said}");
    assert!(said.contains("ajouté"), "{said}");
    assert_eq!(
        b.asked(),
        [VocabularyAdd {
            word: "Grafana".to_owned(),
            heard_as: Some("gras fana".to_owned()),
        }]
    );

    // Once added, the proposal is used up.
    let (ok, _) = call(&client, "dictionary_add", json!({"word": "Grafana"})).await;
    assert!(!ok);
    assert_eq!(b.asked().len(), 1);
}

#[tokio::test]
async fn a_word_never_proposed_is_refused() {
    let b = bench().await;
    let key = b.conversation();
    let client = b.client(Some(&key)).await;
    b.conversations.spoke(&key, TurnId::new());
    let (ok, said) = call(&client, "dictionary_add", json!({"word": "Nextcloud"})).await;
    assert!(!ok);
    assert!(said.contains("dictionary_propose"), "{said}");
    assert!(b.asked().is_empty());
}

#[tokio::test]
async fn a_proposal_belongs_to_its_conversation_and_dies_with_it() {
    let b = bench().await;
    let (mine, other) = (b.conversation(), b.conversation());
    let client = b.client(Some(&mine)).await;
    let stranger = b.client(Some(&other)).await;
    assert!(
        call(&client, "dictionary_propose", json!({"word": "Grafana"}))
            .await
            .0
    );
    b.conversations.spoke(&mine, TurnId::new());
    b.conversations.spoke(&other, TurnId::new());
    // Another conversation cannot use it.
    assert!(
        !call(&stranger, "dictionary_add", json!({"word": "Grafana"}))
            .await
            .0
    );
    // Closed, the conversation takes its proposals with it.
    b.conversations.close(&mine);
    let (ok, said) = call(&client, "dictionary_add", json!({"word": "Grafana"})).await;
    assert!(!ok, "{said}");
    assert!(b.asked().is_empty());
}

#[tokio::test]
async fn a_call_without_its_conversation_is_refused() {
    let b = bench().await;
    let client = b.client(None).await;
    assert!(
        !call(&client, "dictionary_propose", json!({"word": "Grafana"}))
            .await
            .0
    );
    let unknown = b.client(Some(&ConversationKey("forged".to_owned()))).await;
    assert!(
        !call(&unknown, "dictionary_propose", json!({"word": "Grafana"}))
            .await
            .0
    );
}

#[tokio::test]
async fn what_the_hearing_says_reaches_the_llm() {
    let b = bench().await;
    let key = b.conversation();
    let client = b.client(Some(&key)).await;
    for word in ["Grafana", "Nextcloud", "Syncthing", "a/b"] {
        assert!(
            call(&client, "dictionary_propose", json!({"word": word}))
                .await
                .0
        );
    }
    b.conversations.spoke(&key, TurnId::new());

    // Unreachable: an error, and the proposal stays for another try.
    *b.hearing.reply.lock().unwrap() = None;
    let (ok, said) = call(&client, "dictionary_add", json!({"word": "Grafana"})).await;
    assert!(!ok && said.contains("pas ajouté"), "{said}");
    *b.hearing.reply.lock().unwrap() = Some(VocabularyReply::success(true, true));
    let (ok, said) = call(&client, "dictionary_add", json!({"word": "Grafana"})).await;
    assert!(ok && said.contains("déjà"), "{said}");

    *b.hearing.reply.lock().unwrap() = Some(VocabularyReply::success(false, false));
    let (ok, said) = call(&client, "dictionary_add", json!({"word": "Nextcloud"})).await;
    assert!(ok && said.contains("prochain démarrage"), "{said}");

    *b.hearing.reply.lock().unwrap() = Some(VocabularyReply::failure("caractère refusé"));
    let (ok, said) = call(&client, "dictionary_add", json!({"word": "a/b"})).await;
    assert!(!ok && said.contains("caractère refusé"), "{said}");
}
