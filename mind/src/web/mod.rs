//! The web: a module of Emory's MCP server (`web_<tool>`), for what the owner asks about the
//! world (`specs/web.md`): the weather (`web_weather`), Wikipedia (`web_wikipedia`), the search
//! of the web (`web_search`) and the reading of a page (`web_read`).
//!
//! `web_read` reads only an address that `web_search` or `web_wikipedia` gave in the same
//! conversation (`only_found_urls`): a page read may hold orders, and the LLM must not be able to
//! send what it knows to an address of its own making. The addresses found are kept here, by
//! conversation, and go with it.
//!
//! The module listens to nothing and publishes nothing on the bus: it is called by the LLM only.
//! Its HTTP clients are kept open: one for the APIs, one for the search, and the guarded one of
//! the pages read ([`read`]). The logs say the host, the status, the size and the time of each
//! request to a service, never the question; nothing of a page's address.

pub mod config;
pub mod read;
pub mod search;
pub mod weather;
pub mod wikipedia;

use std::{
    collections::{HashMap, VecDeque},
    net::IpAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use reqwest::Url;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool};
use serde_json::{Value, json};
use tracing::{info, warn};

pub use config::{ReadConfig, SearchConfig, WebConfig};
use read::{Lookup, Reader};
use search::Search;
use weather::Weather;
use wikipedia::Wikipedia;

use crate::{
    conversations::{ConversationKey, Conversations},
    tools::Module,
};

/// The most an API answer may weigh, in bytes: the answers used are a few kilobytes.
const MAX_ANSWER_BYTES: usize = 1_000_000;

/// What an API answered.
pub struct Fetched {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Why nothing was fetched. The details are in the logs; the LLM is only told the service does
/// not answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchError {
    /// The network, a time-out, a broken answer.
    Unreachable,
    /// The answer is heavier than expected.
    TooBig,
}

/// Where the requests go: the internet through [`Http`], or recorded answers in the tests.
pub trait Fetch: Send + Sync + 'static {
    fn get(&self, url: Url) -> BoxFuture<'_, Result<Fetched, FetchError>>;
}

/// The client of the module: one for every tool, with the time-out and the `User-Agent` of the
/// configuration.
pub struct Http {
    client: reqwest::Client,
    accept: &'static str,
    max_bytes: usize,
}

impl Http {
    /// The client of the APIs (Wikipedia, Open-Meteo): they answer JSON.
    pub fn new(config: &WebConfig) -> Result<Self> {
        Self::build(config.user_agent(), config.timeout(), "application/json")
    }

    /// The client of the searches: the `User-Agent` of a browser, and a page that is HTML.
    pub fn for_search(config: &SearchConfig) -> Result<Self> {
        Self::build(
            config.user_agent.clone(),
            config.timeout(),
            "text/html,application/xhtml+xml;q=0.9,*/*;q=0.8",
        )
    }

    fn build(user_agent: String, timeout: Duration, accept: &'static str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(user_agent)
            .timeout(timeout)
            // The APIs used answer where they are asked, at most a canonical title away.
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .context("building the HTTP client of the web module")?;
        Ok(Self {
            client,
            accept,
            max_bytes: MAX_ANSWER_BYTES,
        })
    }

    /// With another cap on the size of an answer.
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }
}

impl Fetch for Http {
    fn get(&self, url: Url) -> BoxFuture<'_, Result<Fetched, FetchError>> {
        Box::pin(async move {
            let started = Instant::now();
            // The path and the query hold what was asked: only the host is logged.
            let host = url.host_str().unwrap_or_default().to_owned();
            let failed = |e: reqwest::Error| {
                warn!(
                    host,
                    timeout = e.is_timeout(),
                    connect = e.is_connect(),
                    ms = started.elapsed().as_millis() as u64,
                    "web: request failed"
                );
                FetchError::Unreachable
            };
            let mut response = self
                .client
                .get(url)
                .header(reqwest::header::ACCEPT, self.accept)
                .send()
                .await
                .map_err(failed)?;
            let status = response.status().as_u16();
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(failed)? {
                if body.len() + chunk.len() > self.max_bytes {
                    warn!(host, status, "web: answer too big, given up");
                    return Err(FetchError::TooBig);
                }
                body.extend_from_slice(&chunk);
            }
            info!(
                host,
                status,
                bytes = body.len(),
                ms = started.elapsed().as_millis() as u64,
                "web: request"
            );
            Ok(Fetched { status, body })
        })
    }
}

/// The most addresses kept for one conversation: the oldest go first.
const MAX_FOUND: usize = 200;

/// The addresses the tools gave, by conversation.
struct Found {
    conversations: Conversations,
    urls: Mutex<HashMap<ConversationKey, VecDeque<String>>>,
}

impl Found {
    fn new(conversations: Conversations) -> Self {
        Self {
            conversations,
            urls: Mutex::default(),
        }
    }

    fn urls(&self) -> std::sync::MutexGuard<'_, HashMap<ConversationKey, VecDeque<String>>> {
        let mut urls = self
            .urls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // The addresses of the conversations that closed go with them.
        urls.retain(|key, _| self.conversations.is_open(key));
        urls
    }

    /// Keeps the addresses of an answer of a tool: its `url`, and that of each of its `results`.
    fn note(&self, conversation: &ConversationKey, answer: &str) {
        if !self.conversations.is_open(conversation) {
            return;
        }
        let Ok(answer) = serde_json::from_str::<Value>(answer) else {
            return;
        };
        let results = answer["results"].as_array().into_iter().flatten();
        let given = std::iter::once(&answer["url"])
            .chain(results.map(|r| &r["url"]))
            .filter_map(Value::as_str)
            .filter_map(|u| Url::parse(u).ok());
        let mut urls = self.urls();
        let known = urls.entry(conversation.clone()).or_default();
        for url in given {
            let key = found_key(&url);
            if !known.contains(&key) {
                known.push_back(key);
            }
        }
        while known.len() > MAX_FOUND {
            known.pop_front();
        }
    }

    fn has(&self, conversation: &ConversationKey, url: &Url) -> bool {
        self.urls()
            .get(conversation)
            .is_some_and(|known| known.contains(&found_key(url)))
    }
}

/// An address as it is compared: in its normal form (`Url` writes it so), without the part
/// after `#`, which the server never sees.
fn found_key(url: &Url) -> String {
    let mut url = url.clone();
    url.set_fragment(None);
    url.into()
}

/// The `web` module: the tools that are enabled.
pub struct Web {
    weather: Option<Weather>,
    wikipedia: Option<Wikipedia>,
    search: Option<Search>,
    /// What the search was built from, to build it again elsewhere (`search_at`).
    search_from: (SearchConfig, Arc<dyn Fetch>),
    reader: Option<Reader>,
    /// What the reader was built from, to build it again (`read_through`).
    read_from: (ReadConfig, String),
    found: Found,
}

impl Web {
    /// The module of this configuration, or `None` when it has no tool left. The addresses
    /// found are kept for the open conversations of `conversations`.
    pub fn new(config: &WebConfig, conversations: Conversations) -> Result<Option<Self>> {
        if !config.any_tool() {
            return Ok(None);
        }
        // The search has a client of its own: another `User-Agent`, another time-out.
        let search = Arc::new(Http::for_search(&config.search)?);
        Self::with_fetches(config, Arc::new(Http::new(config)?), search, conversations)
    }

    /// The same, the APIs fetched through `api` and the search through `search`. A page read
    /// is always fetched by the guarded client of [`Reader`].
    pub fn with_fetches(
        config: &WebConfig,
        api: Arc<dyn Fetch>,
        search: Arc<dyn Fetch>,
        conversations: Conversations,
    ) -> Result<Option<Self>> {
        if !config.any_tool() {
            return Ok(None);
        }
        let reader = match config.reads() {
            true => Some(Reader::new(&config.read, config.user_agent())?),
            false => None,
        };
        Ok(Some(Self {
            weather: config
                .weather
                .enabled
                .then(|| Weather::new(&config.weather, api.clone())),
            wikipedia: config
                .wikipedia
                .enabled
                .then(|| Wikipedia::new(&config.wikipedia, api)),
            search: Search::new(&config.search, search.clone()),
            search_from: (config.search.clone(), search),
            reader,
            read_from: (config.read.clone(), config.user_agent()),
            found: Found::new(conversations),
        }))
    }

    /// Reads the pages with the names found by `lookup`, and `exception` taken as a public
    /// address: for the tests, whose server is on the loopback.
    pub fn read_through(mut self, lookup: Arc<dyn Lookup>, exception: IpAddr) -> Result<Self> {
        if self.reader.is_some() {
            let (config, user_agent) = &self.read_from;
            self.reader = Some(Reader::with_lookup(
                config,
                user_agent.clone(),
                lookup,
                Some(exception),
            )?);
        }
        Ok(self)
    }

    /// Whether `web_read` may read `url` in `conversation`.
    fn may_read(&self, conversation: Option<&ConversationKey>, url: &Url) -> Result<(), String> {
        if !self.read_from.0.only_found_urls {
            return Ok(());
        }
        match conversation {
            None => Err("Cet outil ne sert que dans une conversation.".to_owned()),
            Some(c) if self.found.has(c, url) => Ok(()),
            Some(_) => {
                info!("web: read refused, an address not found");
                Err(NOT_FOUND.to_owned())
            }
        }
    }

    async fn read(
        &self,
        conversation: Option<&ConversationKey>,
        arguments: &JsonObject,
    ) -> Result<String, String> {
        let given = text(arguments, "url")?;
        let url = Url::parse(&given).map_err(|_| "Cette adresse est illisible.".to_owned())?;
        self.may_read(conversation, &url)?;
        let page = self.reader.as_ref().unwrap().read(url).await?;
        Ok(page.told())
    }

    /// Searches at another address (the pages of DuckDuckGo): for the tests, with a server of
    /// their own.
    pub fn search_at(mut self, origin: &str) -> Self {
        let (config, fetch) = &self.search_from;
        self.search = self.search.is_some().then(|| {
            Search::with_provider(
                config,
                Box::new(search::DuckDuckGo::new(config, fetch.clone()).at(origin)),
            )
        });
        self
    }

    /// Asks Open-Meteo at other addresses (the geocoding, then the forecast): for the tests, with
    /// a server of their own.
    pub fn weather_at(mut self, geocoding: &str, forecast: &str) -> Self {
        self.weather = self.weather.map(|w| w.at(geocoding, forecast));
        self
    }

    /// Asks Wikipedia at another address (`{lang}` stands for the language): for the tests, with
    /// a server of their own.
    pub fn wikipedia_at(mut self, origin: &str) -> Self {
        self.wikipedia = self.wikipedia.map(|w| w.at(origin));
        self
    }
}

const NOT_FOUND: &str = "Cette adresse n'a été donnée ni par web_search ni par web_wikipedia dans \
                         cette conversation : cherche la page d'abord.";

fn schema(value: Value) -> Arc<JsonObject> {
    match value {
        Value::Object(map) => Arc::new(map),
        _ => unreachable!(),
    }
}

fn text(arguments: &JsonObject, name: &str) -> Result<String, String> {
    let value = arguments
        .get(name)
        .and_then(Value::as_str)
        .map(|v| v.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    if value.is_empty() {
        return Err(format!("« {name} » manque."));
    }
    Ok(value)
}

impl Module for Web {
    fn name(&self) -> &'static str {
        "web"
    }

    // Short: the descriptions go with every request.
    fn tools(&self) -> BoxFuture<'_, Vec<Tool>> {
        Box::pin(async {
            let mut tools = Vec::new();
            if let Some(weather) = &self.weather {
                tools.push(Tool::new(
                    "weather",
                    if weather.has_default() {
                        "La météo, maintenant et les jours qui viennent. Sans lieu : chez l'utilisateur."
                    } else {
                        "La météo d'un lieu, maintenant et les jours qui viennent."
                    },
                    schema(json!({
                        "type": "object",
                        "properties": {
                            "place": {
                                "type": "string",
                                "description": "le lieu : une ville, suivie du pays si le nom est ambigu (« Lyon, France »)"
                            },
                            "days": {
                                "type": "integer",
                                "minimum": 1,
                                "maximum": 7,
                                "description": "combien de jours, aujourd'hui compris (2 par défaut)"
                            }
                        }
                    })),
                ));
            }
            if self.wikipedia.is_some() {
                tools.push(Tool::new(
                    "wikipedia",
                    "Le résumé de l'article Wikipédia sur un sujet.",
                    schema(json!({
                        "type": "object",
                        "properties": {"query": {"type": "string", "description": "le sujet"}},
                        "required": ["query"]
                    })),
                ));
            }
            if self.search.is_some() {
                tools.push(Tool::new(
                    "search",
                    "Cherche sur le web. Pour l'actualité ou ce que tu ne sais pas. Les adresses servent à web_read, ne les lis jamais.",
                    schema(json!({
                        "type": "object",
                        "properties": {"query": {"type": "string", "description": "la recherche"}},
                        "required": ["query"]
                    })),
                ));
            }
            if self.reader.is_some() {
                tools.push(Tool::new(
                    "read",
                    "Lit le texte d'une page trouvée par web_search ou web_wikipedia.",
                    schema(json!({
                        "type": "object",
                        "properties": {"url": {"type": "string", "description": "l'adresse, telle que l'outil l'a donnée"}},
                        "required": ["url"]
                    })),
                ));
            }
            tools
        })
    }

    fn call(&self, tool: &str, arguments: Option<JsonObject>) -> BoxFuture<'_, CallToolResult> {
        self.call_in(tool, arguments, None)
    }

    fn call_in(
        &self,
        tool: &str,
        arguments: Option<JsonObject>,
        conversation: Option<ConversationKey>,
    ) -> BoxFuture<'_, CallToolResult> {
        let tool = tool.to_owned();
        Box::pin(async move {
            let arguments = arguments.unwrap_or_default();
            let outcome = match tool.as_str() {
                "weather" if self.weather.is_some() => {
                    self.weather.as_ref().unwrap().report(&arguments).await
                }
                "wikipedia" if self.wikipedia.is_some() => match text(&arguments, "query") {
                    Ok(query) => self.wikipedia.as_ref().unwrap().lookup(&query).await,
                    Err(e) => Err(e),
                },
                "search" if self.search.is_some() => match text(&arguments, "query") {
                    Ok(query) => self.search.as_ref().unwrap().search(&query).await,
                    Err(e) => Err(e),
                },
                "read" if self.reader.is_some() => {
                    self.read(conversation.as_ref(), &arguments).await
                }
                _ => Err(format!("Outil inconnu : web_{tool}")),
            };
            // The addresses given are those `web_read` may read next, in this conversation.
            if let (Ok(answer), Some(conversation)) = (&outcome, &conversation)
                && matches!(tool.as_str(), "wikipedia" | "search")
            {
                self.found.note(conversation, answer);
            }
            match outcome {
                Ok(answer) => CallToolResult::success(vec![ContentBlock::text(answer)]),
                Err(e) => CallToolResult::error(vec![ContentBlock::text(e)]),
            }
        })
    }
}
