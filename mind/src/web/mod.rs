//! The web: a module of Emory's MCP server (`web_<tool>`), for what the owner asks about the
//! world (`specs/web.md`). In this version, the weather (`web_weather`), Wikipedia
//! (`web_wikipedia`) and the search of the web (`web_search`); the reading of a page comes as a
//! tool of the same module.
//!
//! The module listens to nothing and publishes nothing on the bus: it is called by the LLM only.
//! One HTTP client is kept open for all its tools. The logs say the host, the status, the size and
//! the time of each request, never the question.

pub mod config;
pub mod search;
pub mod weather;
pub mod wikipedia;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use reqwest::Url;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool};
use serde_json::{Value, json};
use tracing::{info, warn};

pub use config::{SearchConfig, WebConfig};
use search::Search;
use weather::Weather;
use wikipedia::Wikipedia;

use crate::tools::Module;

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

/// The `web` module: the tools that are enabled.
pub struct Web {
    weather: Option<Weather>,
    wikipedia: Option<Wikipedia>,
    search: Option<Search>,
    /// What the search was built from, to build it again elsewhere (`search_at`).
    search_from: (SearchConfig, Arc<dyn Fetch>),
}

impl Web {
    /// The module of this configuration, or `None` when it has no tool left.
    pub fn new(config: &WebConfig) -> Result<Option<Self>> {
        if !config.any_tool() {
            return Ok(None);
        }
        // The search has a client of its own: another `User-Agent`, another time-out.
        let search = Arc::new(Http::for_search(&config.search)?);
        Ok(Self::with_fetches(
            config,
            Arc::new(Http::new(config)?),
            search,
        ))
    }

    /// The same, fetching through `fetch` (the search too).
    pub fn with_fetch(config: &WebConfig, fetch: Arc<dyn Fetch>) -> Option<Self> {
        Self::with_fetches(config, fetch.clone(), fetch)
    }

    /// The same, the APIs fetched through `api` and the search through `search`.
    pub fn with_fetches(
        config: &WebConfig,
        api: Arc<dyn Fetch>,
        search: Arc<dyn Fetch>,
    ) -> Option<Self> {
        config.any_tool().then(|| Self {
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
        })
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
            tools
        })
    }

    fn call(&self, tool: &str, arguments: Option<JsonObject>) -> BoxFuture<'_, CallToolResult> {
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
                _ => Err(format!("Outil inconnu : web_{tool}")),
            };
            match outcome {
                Ok(answer) => CallToolResult::success(vec![ContentBlock::text(answer)]),
                Err(e) => CallToolResult::error(vec![ContentBlock::text(e)]),
            }
        })
    }
}
