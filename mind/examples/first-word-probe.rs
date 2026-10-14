//! Probe of the first word, without a bus: opens one conversation on the LLM of a configuration
//! (ACP or API) with Emory's tools, plays the questions one after another as `mind` would (memory
//! search, request text, the heads-up at the first request) and prints, per turn, how long the
//! memory search took and how long the LLM took to its first text.
//!
//! The dictionary gets a vocabulary that answers nothing (no hearing without a bus); the timers
//! never ring.
//!
//! ```sh
//! cargo run -p mind --example first-word-probe -- dev/mind.toml "first question" "second one"
//! ```
//!
//! `PROBE_TOOLS=none` serves no tool; `PROBE_SEARCH=0` sends no entry found.

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use contract::{VocabularyAdd, VocabularyReply};
use futures_util::future::BoxFuture;
use mind::{
    backend::Backend,
    brain::{Brain, Event},
    config::Config,
    conversations::{ConversationKey, Conversations},
    dictionary::{Dictionary, Vocabulary},
    hippocampe::Hippocampe,
    memory::Memory,
    prompt, request,
    tools::{self, HippocampeTools, Tools},
    web,
};

/// A hearing that never answers.
struct NoHearing;

impl Vocabulary for NoHearing {
    fn add(&self, _: VocabularyAdd) -> BoxFuture<'_, Result<VocabularyReply, String>> {
        Box::pin(async { Err("no hearing in the probe".to_owned()) })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".to_owned()))
        .with_writer(std::io::stderr)
        .init();
    let mut args = std::env::args().skip(1);
    let path = args.next().context("a configuration is needed")?;
    let questions: Vec<String> = args.collect();
    let config = Config::read(Path::new(&path))?;
    let with_tools = std::env::var("PROBE_TOOLS").as_deref() != Ok("none");
    let with_search = std::env::var("PROBE_SEARCH").as_deref() != Ok("0");

    let system_prompt = prompt::render(
        &std::fs::read_to_string(&config.llm.system_prompt)?,
        &config.owner,
    );
    let hippocampe = Hippocampe::new(
        config.hippocampe.url.clone(),
        config.hippocampe.token_file.0.clone(),
    );
    hippocampe.connect_in_background();
    let conversations = Conversations::default();
    let mut modules: Vec<Arc<dyn tools::Module>> = Vec::new();
    if with_tools {
        modules.push(Arc::new(HippocampeTools::new(
            hippocampe.clone(),
            config.hippocampe.tools.clone(),
            config.hippocampe.tool_timeout(),
        )));
        if config.dictionary.enabled {
            modules.push(Arc::new(Dictionary::new(NoHearing, conversations.clone())));
        }
        if let Some(web) = web::Web::new(&config.web)? {
            modules.push(Arc::new(web));
        }
        if let Some(time) =
            mind::time::Time::new(&config.time, &config.data, conversations.clone())?
        {
            modules.push(Arc::new(time));
        }
    }
    let brain = Backend::start(
        &config.llm,
        &config.tools,
        system_prompt,
        Tools::new(modules),
    )
    .await?;

    let device = contract::Device::new("probe")?;
    let key: ConversationKey = conversations.open_on(device);
    let opened = Instant::now();
    let (session, heads_up) = tokio::join!(brain.open(&key), hippocampe.heads_up());
    let session = session?;
    let heads_up = heads_up.ok().flatten();
    println!("opened ms={}", opened.elapsed().as_millis());
    for (n, question) in questions.iter().enumerate() {
        let received = Instant::now();
        let found = if with_search {
            tokio::time::timeout(Duration::from_secs(2), hippocampe.search(question, 5))
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let searched = received.elapsed().as_millis();
        let heads_up = heads_up.as_deref().filter(|_| n == 0);
        let text = request::build(contract::now(), heads_up, &[], &found, question);
        let sent = Instant::now();
        let mut events = brain.prompt(&session, text);
        let (mut first, mut tools, mut chars) = (None, 0, 0);
        let reason = loop {
            match events.recv().await {
                Some(Event::Text(t)) => {
                    first.get_or_insert(sent.elapsed().as_millis());
                    chars += t.chars().count();
                }
                Some(Event::Tool(_)) => tools += 1,
                Some(Event::Done(r)) => break r.unwrap_or_else(|e| format!("error: {e}")),
                None => break "closed".to_owned(),
            }
        };
        println!(
            "turn={} search_ms={searched} found={} first_word_llm_ms={} first_word_total_ms={} end_ms={} tools={tools} chars={chars} reason={reason}",
            n + 1,
            found.len(),
            first.map_or(-1, |f| f as i64),
            first.map_or(-1, |f| f as i64 + searched as i64),
            sent.elapsed().as_millis(),
        );
    }
    brain.close(&session);
    tokio::time::sleep(Duration::from_millis(300)).await;
    Ok(())
}
