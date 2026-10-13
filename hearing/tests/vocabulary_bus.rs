//! The round trip on a real bus: a word asked on `hearing.vocabulary.add` is written, and the
//! reply says so. Needs a NATS server of its own, never the one a chain runs on:
//! `EMORY_TEST_NATS=nats://127.0.0.1:4226 cargo test -p hearing --test vocabulary_bus`. Without
//! the variable, the test says it is skipped and passes.

use std::{sync::Arc, time::Duration};

use contract::{VocabularyAdd, VocabularyReply, subjects};
use hearing::{hotwords::Vocabulary, vocabulary};

async fn ask(client: &async_nats::Client, word: &str, heard_as: Option<&str>) -> VocabularyReply {
    let request = VocabularyAdd {
        word: word.to_owned(),
        heard_as: heard_as.map(str::to_owned),
    };
    let reply = tokio::time::timeout(
        Duration::from_secs(5),
        client.request(
            subjects::VOCABULARY_ADD,
            serde_json::to_vec(&request).unwrap().into(),
        ),
    )
    .await
    .expect("no reply in time")
    .expect("the request failed");
    serde_json::from_slice(&reply.payload).unwrap()
}

#[tokio::test]
async fn a_word_asked_on_the_bus_is_written_and_answered() {
    let Ok(url) = std::env::var("EMORY_TEST_NATS") else {
        eprintln!("skipped: EMORY_TEST_NATS is not set");
        return;
    };
    let folder = std::env::temp_dir().join(format!("hearing-bus-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    let path = folder.join("hotwords.txt");
    let words = Arc::new(Vocabulary::load(&path).unwrap());

    let server = async_nats::connect(&url).await.unwrap();
    let serving = tokio::spawn(vocabulary::serve(server.clone(), words.clone(), true));
    server.flush().await.unwrap();
    let client = async_nats::connect(&url).await.unwrap();

    assert_eq!(
        ask(&client, "Grafana", Some("gras fana")).await,
        VocabularyReply::success(false, true)
    );
    assert_eq!(
        ask(&client, "grafana", None).await,
        VocabularyReply::success(true, true)
    );
    let refused = ask(&client, "a/b", None).await;
    assert!(!refused.ok && refused.error.is_some());
    assert_eq!(words.words().list, ["Grafana"]);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.ends_with("gras fana »\nGrafana\n"), "{text}");

    serving.abort();
    std::fs::remove_dir_all(&folder).unwrap();
}
