//! `web_search`: the search of the web through a provider that can be replaced (`specs/web.md`).
//!
//! The provider is a [`SearchProvider`]; the only one built is DuckDuckGo's light HTML page, read
//! with no key and no account, at the price of a page that changes without notice. What this
//! module adds around it: three ways of failing that are told apart ([`SearchError`]), a state
//! that is logged when it changes and never at every call, a pause after a block or a changed page
//! (the tool answers at once, the provider is left alone), the least time between two requests,
//! and a cache of the answers. The logs say the provider, the case, the status, the time; never
//! the question.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use dom_query::{Document, Selection};
use futures_util::future::BoxFuture;
use reqwest::Url;
use serde_json::json;
use tokio::time::Instant;
use tracing::{info, warn};

use super::{Fetch, FetchError, config::SearchConfig};

const DUCKDUCKGO_ORIGIN: &str = "https://html.duckduckgo.com";
/// The longest question sent to a provider, in characters.
const MAX_QUERY: usize = 300;
/// How many answers are kept.
const KEPT: usize = 100;
/// The longest title and snippet given to the LLM, in characters.
const MAX_TITLE: usize = 150;
const MAX_SNIPPET: usize = 300;

const UNREACHABLE: &str = "La recherche web ne répond pas.";
const BLOCKED: &str = "La recherche web est bloquée pour le moment.";
const QUERY_TOO_LONG: &str = "La question est trop longue pour une recherche.";
const NO_RESULT: &str = "Aucun résultat.";

/// A result: where it is, what it is called, what it says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub title: String,
    pub url: Url,
    /// The domain name without `www.`.
    pub site: String,
    pub snippet: String,
}

/// Why a search gave nothing. The status is the HTTP code the provider answered, when it did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchError {
    /// The network, a time-out, a server error.
    Unreachable(Option<u16>),
    /// The provider took the program for a robot (a captcha, a refusal).
    Blocked(u16),
    /// The provider answered a page that is not the one that was read: it has changed.
    PageChanged(u16),
    /// The provider cannot search for this; the sentence is told to the LLM.
    Refused(String),
}

/// A search engine: a question in, a few results out. Another provider (SearXNG, an API) is one
/// implementation and one value of `[web.search] provider`.
pub trait SearchProvider: Send + Sync + 'static {
    /// The value of `provider` that selects it.
    fn name(&self) -> &'static str;
    /// How the LLM is told about it (« DuckDuckGo a changé sa page »).
    fn label(&self) -> &'static str;
    fn search(&self, query: &str, limit: usize) -> BoxFuture<'_, Result<Vec<Hit>, SearchError>>;
}

/// DuckDuckGo's light page: `GET <origin>/html/?q=<question>&kl=<region>`.
pub struct DuckDuckGo {
    fetch: Arc<dyn Fetch>,
    origin: String,
    region: String,
}

impl DuckDuckGo {
    pub fn new(config: &SearchConfig, fetch: Arc<dyn Fetch>) -> Self {
        Self {
            fetch,
            origin: DUCKDUCKGO_ORIGIN.to_owned(),
            region: config.region.clone(),
        }
    }

    /// Asks at another address: for the tests, with a server of their own.
    pub fn at(mut self, origin: &str) -> Self {
        self.origin = origin.trim_end_matches('/').to_owned();
        self
    }
}

impl SearchProvider for DuckDuckGo {
    fn name(&self) -> &'static str {
        "duckduckgo"
    }

    fn label(&self) -> &'static str {
        "DuckDuckGo"
    }

    fn search(&self, query: &str, limit: usize) -> BoxFuture<'_, Result<Vec<Hit>, SearchError>> {
        let query = query.to_owned();
        Box::pin(async move {
            let query = query.trim();
            if query.is_empty() || query.chars().count() > MAX_QUERY {
                return Err(SearchError::Refused(QUERY_TOO_LONG.to_owned()));
            }
            let mut url = Url::parse(&format!("{}/html/", self.origin))
                .map_err(|_| SearchError::Unreachable(None))?;
            url.query_pairs_mut()
                .append_pair("q", query)
                .append_pair("kl", &self.region);
            let fetched = self
                .fetch
                .get(url)
                .await
                .map_err(|_: FetchError| SearchError::Unreachable(None))?;
            let page = String::from_utf8_lossy(&fetched.body);
            read_page(fetched.status, &page).map(|mut hits| {
                hits.truncate(limit);
                hits
            })
        })
    }
}

/// What the answer of DuckDuckGo says, by its code and its page.
///
/// - 5xx: the server is down (`Unreachable`).
/// - 202, 403, 429, or `anomaly-modal` in the page (the captcha): `Blocked`.
/// - 200 with `a.result__a` links: the results, advertisements left out.
/// - 200 with none, and the « no results » mark of DuckDuckGo: no result, which is an answer.
/// - 200 with none and no such mark, or any other code, or links that lead nowhere:
///   `PageChanged`.
pub fn read_page(status: u16, page: &str) -> Result<Vec<Hit>, SearchError> {
    match status {
        500..=599 => return Err(SearchError::Unreachable(Some(status))),
        202 | 403 | 429 => return Err(SearchError::Blocked(status)),
        200 => {}
        _ => return Err(SearchError::PageChanged(status)),
    }
    if page.contains("anomaly-modal") {
        return Err(SearchError::Blocked(status));
    }
    let document = Document::from(page);
    let links = document.select("a.result__a");
    if !links.exists() {
        return if has_no_result_mark(&document) {
            Ok(Vec::new())
        } else {
            Err(SearchError::PageChanged(status))
        };
    }
    let base = Url::parse(DUCKDUCKGO_ORIGIN).expect("a valid origin");
    let mut hits: Vec<Hit> = Vec::new();
    let mut organic = 0;
    for link in links.iter() {
        if is_advertisement(&link) {
            continue;
        }
        organic += 1;
        let Some(url) = link.attr("href").and_then(|h| target(&base, &h)) else {
            continue;
        };
        if hits.iter().any(|h| h.url == url) {
            continue;
        }
        let title = clean(&link.text());
        if title.is_empty() {
            continue;
        }
        let snippet = result_of(&link)
            .map(|r| clean(&r.select(".result__snippet").text()))
            .unwrap_or_default();
        hits.push(Hit {
            site: site_of(&url),
            title,
            url,
            snippet,
        });
    }
    // Links, but none that can be used: the page is not the one that was read.
    if organic > 0 && hits.is_empty() {
        return Err(SearchError::PageChanged(status));
    }
    Ok(hits)
}

/// DuckDuckGo's « no results » mark: a `no-results` block, or its sentence in the page.
fn has_no_result_mark(document: &Document) -> bool {
    document.select(".no-results").exists()
        || clean(&document.select("body").text()).contains("No results")
}

/// Whether a result is a paid one: it sits in a block marked `result--ad`.
fn is_advertisement(link: &Selection) -> bool {
    link.nodes()
        .iter()
        .any(|n| n.ancestors_it(None).any(|a| a.has_class("result--ad")))
}

/// The block of a result that holds its link, its address and its snippet.
fn result_of<'a>(link: &Selection<'a>) -> Option<Selection<'a>> {
    let node = link
        .nodes()
        .first()?
        .ancestors_it(None)
        .find(|a| a.has_class("result"))?;
    Some(Selection::from(node))
}

/// The address a result leads to. DuckDuckGo's links go through `//duckduckgo.com/l/?uddg=<the
/// address, encoded>&rut=…`: the real address is in `uddg`. An address that already leaves
/// DuckDuckGo is taken as it is; any other link to DuckDuckGo (its own pages, its advertisement
/// redirections) is no result.
fn target(base: &Url, href: &str) -> Option<Url> {
    let link = base.join(href.trim()).ok()?;
    let url = if is_duckduckgo(&link) {
        if link.path() != "/l/" {
            return None;
        }
        let (_, encoded) = link.query_pairs().find(|(k, _)| k == "uddg")?;
        Url::parse(&encoded).ok()?
    } else {
        link
    };
    (matches!(url.scheme(), "http" | "https") && url.host_str().is_some() && !is_duckduckgo(&url))
        .then_some(url)
}

fn is_duckduckgo(url: &Url) -> bool {
    url.host_str()
        .is_some_and(|h| h == "duckduckgo.com" || h.ends_with(".duckduckgo.com"))
}

/// The domain name without `www.`.
fn site_of(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    host.strip_prefix("www.").unwrap_or(host).to_owned()
}

/// The text with its blanks (and line breaks) reduced to single spaces.
fn clean(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// At most `max` characters, cut after a word with « … ».
fn short(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let cut: String = text.chars().take(max).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    format!("{}…", cut.trim_end_matches([',', ';', ':', '.', ' ']))
}

/// How the provider is doing, as far as the last request tells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Standing {
    Working,
    Unreachable,
    Blocked,
    PageChanged,
}

impl Standing {
    fn of(error: &SearchError) -> Option<Self> {
        match error {
            SearchError::Unreachable(_) => Some(Self::Unreachable),
            SearchError::Blocked(_) => Some(Self::Blocked),
            SearchError::PageChanged(_) => Some(Self::PageChanged),
            SearchError::Refused(_) => None,
        }
    }

    fn case(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Unreachable => "unreachable",
            Self::Blocked => "blocked",
            Self::PageChanged => "page changed",
        }
    }
}

#[derive(Default)]
struct Kept {
    standing: Option<Standing>,
    /// Until when the provider is left alone.
    paused_until: Option<Instant>,
    answers: HashMap<String, (Instant, String)>,
}

/// The `web_search` tool: a provider, and what is kept around it.
pub struct Search {
    provider: Box<dyn SearchProvider>,
    max_results: usize,
    min_interval: Duration,
    cache: Duration,
    backoff: Duration,
    kept: Mutex<Kept>,
    /// Held while a request is made: the requests go one at a time, `min_interval` apart. The
    /// instant is the end of the last one.
    gate: tokio::sync::Mutex<Option<Instant>>,
}

impl Search {
    /// The search of this configuration, or `None` when `provider` is `"none"`.
    pub fn new(config: &SearchConfig, fetch: Arc<dyn Fetch>) -> Option<Self> {
        match config.provider.as_str() {
            "duckduckgo" => Some(Self::with_provider(
                config,
                Box::new(DuckDuckGo::new(config, fetch)),
            )),
            _ => None,
        }
    }

    /// The search through `provider`.
    pub fn with_provider(config: &SearchConfig, provider: Box<dyn SearchProvider>) -> Self {
        Self {
            provider,
            max_results: config.max_results,
            min_interval: config.min_interval(),
            cache: config.cache(),
            backoff: config.backoff(),
            kept: Mutex::new(Kept::default()),
            gate: tokio::sync::Mutex::new(None),
        }
    }

    /// The results of a question as the LLM gets them (JSON), or the sentence that says why not.
    pub async fn search(&self, query: &str) -> Result<String, String> {
        let query = clean(query);
        if query.is_empty() {
            return Err("« query » manque.".to_owned());
        }
        if query.chars().count() > MAX_QUERY {
            return Err(QUERY_TOO_LONG.to_owned());
        }
        let key = query.to_lowercase();
        if let Some(answer) = self.answer_without_asking(&key) {
            return answer;
        }
        let mut last = self.gate.lock().await;
        // Another call may have asked, or been blocked, while this one waited its turn.
        if let Some(answer) = self.answer_without_asking(&key) {
            return answer;
        }
        if let Some(last) = *last {
            tokio::time::sleep_until(last + self.min_interval).await;
        }
        let started = Instant::now();
        let outcome = self.provider.search(&query, self.max_results).await;
        *last = Some(Instant::now());
        drop(last);
        match outcome {
            Ok(hits) => {
                info!(
                    provider = self.provider.name(),
                    results = hits.len(),
                    ms = started.elapsed().as_millis() as u64,
                    "web: search"
                );
                let answer = tell(&hits);
                self.succeeded(key, &answer);
                Ok(answer)
            }
            Err(error) => Err(self.failed(&error)),
        }
    }

    /// The answer kept for this question, or the error of the pause in course: what can be said
    /// without asking the provider.
    fn answer_without_asking(&self, key: &str) -> Option<Result<String, String>> {
        let kept = self.kept.lock().unwrap();
        let now = Instant::now();
        if let Some((at, answer)) = kept.answers.get(key) {
            if now < *at + self.cache {
                info!(
                    provider = self.provider.name(),
                    "web: search from the cache"
                );
                return Some(Ok(answer.clone()));
            }
        }
        match kept.paused_until {
            Some(until) if now < until => Some(Err(
                self.sentence(kept.standing.unwrap_or(Standing::Blocked))
            )),
            _ => None,
        }
    }

    /// A search that worked: the state is back to normal (and the log says so), the answer kept.
    fn succeeded(&self, key: String, answer: &str) {
        let mut kept = self.kept.lock().unwrap();
        let was = kept.standing.replace(Standing::Working);
        if let Some(was) = was.filter(|s| *s != Standing::Working) {
            info!(
                provider = self.provider.name(),
                was = was.case(),
                "web: search works again"
            );
        }
        kept.paused_until = None;
        if self.cache.is_zero() {
            return;
        }
        let now = Instant::now();
        kept.answers.retain(|_, (at, _)| now < *at + self.cache);
        while kept.answers.len() >= KEPT {
            let oldest = kept.answers.iter().min_by_key(|(_, (at, _))| *at);
            let Some(oldest) = oldest.map(|(k, _)| k.clone()) else {
                break;
            };
            kept.answers.remove(&oldest);
        }
        kept.answers.insert(key, (now, answer.to_owned()));
    }

    /// A search that failed: the state, logged when it is not the one it already was; the pause
    /// after a block or a changed page; the sentence for the LLM.
    fn failed(&self, error: &SearchError) -> String {
        let Some(standing) = Standing::of(error) else {
            let SearchError::Refused(sentence) = error else {
                unreachable!("only a refusal has no standing");
            };
            return sentence.clone();
        };
        let status = match error {
            SearchError::Unreachable(status) => *status,
            SearchError::Blocked(status) | SearchError::PageChanged(status) => Some(*status),
            SearchError::Refused(_) => None,
        };
        let mut kept = self.kept.lock().unwrap();
        // A pause only for what asking again would make worse.
        let pause = standing != Standing::Unreachable;
        if kept.standing != Some(standing) {
            warn!(
                provider = self.provider.name(),
                case = standing.case(),
                status,
                pause_minutes = if pause {
                    self.backoff.as_secs() / 60
                } else {
                    0
                },
                "web: search does not work"
            );
        }
        kept.standing = Some(standing);
        kept.paused_until = pause.then(|| Instant::now() + self.backoff);
        self.sentence(standing)
    }

    /// What the LLM is told for each case.
    fn sentence(&self, standing: Standing) -> String {
        match standing {
            Standing::Blocked => BLOCKED.to_owned(),
            Standing::PageChanged => format!(
                "La recherche web ne marche plus : {} a changé sa page.",
                self.provider.label()
            ),
            Standing::Unreachable | Standing::Working => UNREACHABLE.to_owned(),
        }
    }
}

/// The results as JSON for the LLM: few fields, nothing to read out but the title and the snippet.
fn tell(hits: &[Hit]) -> String {
    if hits.is_empty() {
        return json!({"results": [], "note": NO_RESULT}).to_string();
    }
    let results: Vec<_> = hits
        .iter()
        .map(|h| {
            json!({
                "title": short(&h.title, MAX_TITLE),
                "site": h.site,
                "snippet": short(&h.snippet, MAX_SNIPPET),
                "url": h.url.as_str(),
            })
        })
        .collect();
    json!({ "results": results }).to_string()
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        io,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use tracing_subscriber::fmt::MakeWriter;

    use super::*;

    /// Pages recorded from DuckDuckGo with neutral questions (`tests/fixtures/web/`).
    const RESULTS: &str = include_str!("../../tests/fixtures/web/search-ddg-results.html");
    const CAPTCHA: &str = include_str!("../../tests/fixtures/web/search-ddg-captcha.html");
    /// Not recorded (DuckDuckGo blocked that request): see the comment at the top of the file.
    const NO_RESULT_PAGE: &str = include_str!("../../tests/fixtures/web/search-ddg-no-result.html");

    #[test]
    fn a_normal_page_gives_its_results() {
        let hits = read_page(200, RESULTS).unwrap();
        assert_eq!(hits.len(), 10);
        let first = &hits[0];
        assert_eq!(first.title, "Rust Programming Language");
        assert_eq!(first.url.as_str(), "https://rust-lang.org/");
        assert_eq!(first.site, "rust-lang.org");
        assert!(
            first.snippet.starts_with("In 2018, the Rust community"),
            "{first:?}"
        );
        for hit in &hits {
            // The redirection of DuckDuckGo is gone, and so are the tags.
            assert!(!hit.url.as_str().contains("duckduckgo"), "{hit:?}");
            assert!(matches!(hit.url.scheme(), "http" | "https"));
            assert!(
                !hit.site.starts_with("www.") && !hit.site.is_empty(),
                "{hit:?}"
            );
            assert!(
                !hit.title.is_empty() && !hit.snippet.contains('<'),
                "{hit:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_provider_gives_at_most_the_limit() {
        struct Fixed;
        impl Fetch for Fixed {
            fn get(&self, url: Url) -> BoxFuture<'_, Result<super::super::Fetched, FetchError>> {
                assert_eq!(url.path(), "/html/");
                let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
                assert_eq!(query["q"], "rust & more");
                assert_eq!(query["kl"], "fr-fr");
                Box::pin(async {
                    Ok(super::super::Fetched {
                        status: 200,
                        body: RESULTS.as_bytes().to_vec(),
                    })
                })
            }
        }
        let provider = DuckDuckGo::new(&SearchConfig::default(), Arc::new(Fixed));
        assert_eq!(provider.search("rust & more", 3).await.unwrap().len(), 3);
    }

    #[test]
    fn the_address_is_the_one_inside_uddg() {
        let base = Url::parse(DUCKDUCKGO_ORIGIN).unwrap();
        for (href, expected) in [
            (
                "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.org%2Fa%3Fb%3D1%26c%3D2%23x&rut=abc",
                Some("https://example.org/a?b=1&c=2#x"),
            ),
            (
                "/l/?uddg=http%3A%2F%2Fwww.example.org%2F",
                Some("http://www.example.org/"),
            ),
            // Already out of DuckDuckGo.
            ("https://example.org/page", Some("https://example.org/page")),
            // Not a result: DuckDuckGo's own pages, its advertisement redirection, no address,
            // an address that is no web page, a redirection back to DuckDuckGo.
            ("//duckduckgo.com/y.js?ad_domain=example.org&u3=x", None),
            ("/html/?q=more", None),
            ("//duckduckgo.com/l/?rut=abc", None),
            ("//duckduckgo.com/l/?uddg=javascript%3Aalert(1)", None),
            ("//duckduckgo.com/l/?uddg=mailto%3Aa%40example.org", None),
            (
                "//duckduckgo.com/l/?uddg=https%3A%2F%2Fduckduckgo.com%2F",
                None,
            ),
            ("//duckduckgo.com/l/?uddg=not+an+address", None),
            ("javascript:void(0)", None),
        ] {
            assert_eq!(
                target(&base, href).as_ref().map(Url::as_str),
                expected,
                "{href}"
            );
        }
    }

    #[test]
    fn advertisements_are_left_out() {
        let page = r#"<html><body><div id="links" class="results">
          <div class="result results_links result--ad">
            <h2 class="result__title"><a class="result__a" href="//duckduckgo.com/y.js?ad_domain=ads.example&amp;u3=x">Buy now</a></h2>
            <a class="result__snippet" href="//duckduckgo.com/y.js?ad_domain=ads.example">An advertisement</a>
          </div>
          <div class="result results_links result--ad">
            <h2 class="result__title"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fads.example%2F">Paid, with a plain address</a></h2>
          </div>
          <div class="result results_links web-result">
            <h2 class="result__title"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.example.org%2Fdoc">The  <b>real</b>
               page</a></h2>
            <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.example.org%2Fdoc">Some <b>words</b>
               on it &amp; more.</a>
          </div></div></body></html>"#;
        let hits = read_page(200, page).unwrap();
        assert_eq!(
            hits,
            [Hit {
                title: "The real page".to_owned(),
                url: Url::parse("https://www.example.org/doc").unwrap(),
                site: "example.org".to_owned(),
                snippet: "Some words on it & more.".to_owned(),
            }]
        );
        // Nothing but advertisements: no result, not a changed page.
        let only_ads = page
            .split(r#"<div class="result results_links web-result">"#)
            .next()
            .unwrap();
        assert_eq!(read_page(200, only_ads).unwrap(), []);
    }

    #[test]
    fn a_page_with_the_no_result_mark_is_no_result() {
        assert_eq!(read_page(200, NO_RESULT_PAGE).unwrap(), []);
        // The mark as words only, in another block.
        let words = "<html><body><div id=\"links\"><p>No  results.</p></div></body></html>";
        assert_eq!(read_page(200, words).unwrap(), []);
    }

    #[test]
    fn a_captcha_is_a_block() {
        // As recorded: code 202 and the captcha.
        assert_eq!(read_page(202, CAPTCHA), Err(SearchError::Blocked(202)));
        // The captcha under a 200 is a block too; so are the codes of a refusal.
        assert_eq!(read_page(200, CAPTCHA), Err(SearchError::Blocked(200)));
        assert_eq!(read_page(202, ""), Err(SearchError::Blocked(202)));
        assert_eq!(read_page(429, ""), Err(SearchError::Blocked(429)));
        assert_eq!(
            read_page(403, "<html></html>"),
            Err(SearchError::Blocked(403))
        );
        // The page of results, with the captcha added to it, is still a block.
        let both = RESULTS.replace("</body>", "<div class=\"anomaly-modal\"></div></body>");
        assert_eq!(read_page(200, &both), Err(SearchError::Blocked(200)));
    }

    #[test]
    fn a_page_without_result_links_is_a_changed_page() {
        // The class of the links renamed, as DuckDuckGo might do.
        let renamed = RESULTS.replace("result__a", "result__link");
        assert_eq!(read_page(200, &renamed), Err(SearchError::PageChanged(200)));
        assert_eq!(read_page(200, ""), Err(SearchError::PageChanged(200)));
        assert_eq!(
            read_page(200, "<html><body><p>Hello</p></body></html>"),
            Err(SearchError::PageChanged(200))
        );
        // Links, but none that leads anywhere.
        let no_address = RESULTS.replace("uddg=", "to=");
        assert_eq!(
            read_page(200, &no_address),
            Err(SearchError::PageChanged(200))
        );
        // A code that is not an answer to a search.
        assert_eq!(read_page(404, RESULTS), Err(SearchError::PageChanged(404)));
        assert_eq!(read_page(301, ""), Err(SearchError::PageChanged(301)));
    }

    #[test]
    fn a_server_error_is_unreachable() {
        for status in [500, 502, 503, 599] {
            assert_eq!(
                read_page(status, "<html>oops</html>"),
                Err(SearchError::Unreachable(Some(status)))
            );
        }
    }

    // The state, the pause, the interval and the cache, with a provider that answers from a list
    // and a clock that only moves when the test sleeps.

    #[derive(Clone, Default)]
    struct Script(Arc<ScriptInner>);

    #[derive(Default)]
    struct ScriptInner {
        answers: Mutex<VecDeque<Result<Vec<Hit>, SearchError>>>,
        calls: AtomicUsize,
        at: Mutex<Vec<Instant>>,
    }

    impl Script {
        fn say(&self, answer: Result<Vec<Hit>, SearchError>) {
            self.0.answers.lock().unwrap().push_back(answer);
        }

        fn calls(&self) -> usize {
            self.0.calls.load(Ordering::SeqCst)
        }
    }

    impl SearchProvider for Script {
        fn name(&self) -> &'static str {
            "script"
        }

        fn label(&self) -> &'static str {
            "Script"
        }

        fn search(&self, _: &str, limit: usize) -> BoxFuture<'_, Result<Vec<Hit>, SearchError>> {
            self.0.calls.fetch_add(1, Ordering::SeqCst);
            self.0.at.lock().unwrap().push(Instant::now());
            let answer = self.0.answers.lock().unwrap().pop_front();
            Box::pin(async move {
                answer
                    .expect("the test says what the provider answers")
                    .map(|mut hits| {
                        hits.truncate(limit);
                        hits
                    })
            })
        }
    }

    fn hit(n: usize) -> Hit {
        Hit {
            title: format!("Title {n}"),
            url: Url::parse(&format!("https://www.example.org/{n}")).unwrap(),
            site: "example.org".to_owned(),
            snippet: format!("Snippet {n}"),
        }
    }

    fn search_with(script: &Script, tweak: impl FnOnce(&mut SearchConfig)) -> Search {
        let mut config = SearchConfig::default();
        tweak(&mut config);
        Search::with_provider(&config, Box::new(script.clone()))
    }

    const MINUTE: Duration = Duration::from_secs(60);

    #[tokio::test(start_paused = true)]
    async fn the_results_are_told_in_json_and_kept() {
        let script = Script::default();
        let search = search_with(&script, |c| c.max_results = 2);
        script.say(Ok(vec![hit(1), hit(2), hit(3)]));
        let said = search.search("  Some   Question ").await.unwrap();
        let said: serde_json::Value = serde_json::from_str(&said).unwrap();
        assert_eq!(
            said,
            json!({"results": [
                {"title": "Title 1", "site": "example.org", "snippet": "Snippet 1", "url": "https://www.example.org/1"},
                {"title": "Title 2", "site": "example.org", "snippet": "Snippet 2", "url": "https://www.example.org/2"},
            ]})
        );
        // The same question (in other letters and blanks) is not asked again, until it expires.
        assert!(search.search("some question").await.is_ok());
        assert_eq!(script.calls(), 1);
        tokio::time::sleep(9 * MINUTE).await;
        assert!(search.search("some question").await.is_ok());
        assert_eq!(script.calls(), 1);
        tokio::time::sleep(2 * MINUTE).await;
        script.say(Ok(vec![hit(4)]));
        assert!(
            search
                .search("some question")
                .await
                .unwrap()
                .contains("Title 4")
        );
        assert_eq!(script.calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_cache_every_question_is_asked() {
        let script = Script::default();
        let search = search_with(&script, |c| c.cache_minutes = 0);
        for _ in 0..3 {
            script.say(Ok(vec![hit(1)]));
            search.search("q").await.unwrap();
        }
        assert_eq!(script.calls(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn no_result_is_an_answer_not_an_error() {
        let script = Script::default();
        let search = search_with(&script, |_| {});
        script.say(Ok(vec![]));
        let said: serde_json::Value =
            serde_json::from_str(&search.search("nothing").await.unwrap()).unwrap();
        assert_eq!(said, json!({"results": [], "note": "Aucun résultat."}));
    }

    #[tokio::test(start_paused = true)]
    async fn the_requests_are_spaced() {
        let script = Script::default();
        let search = search_with(&script, |c| c.min_interval_ms = 1500);
        for _ in 0..3 {
            script.say(Ok(vec![hit(1)]));
        }
        let (a, b, c) = tokio::join!(search.search("a"), search.search("b"), search.search("c"));
        assert!(a.is_ok() && b.is_ok() && c.is_ok());
        let at = script.0.at.lock().unwrap().clone();
        assert_eq!(at.len(), 3);
        for pair in at.windows(2) {
            assert!(pair[1] - pair[0] >= Duration::from_millis(1500), "{pair:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_same_question_at_once_is_asked_once() {
        let script = Script::default();
        let search = search_with(&script, |_| {});
        script.say(Ok(vec![hit(1)]));
        let (a, b) = tokio::join!(search.search("same"), search.search("same"));
        assert_eq!(a, b);
        assert_eq!(script.calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_block_or_a_changed_page_pauses_the_provider() {
        for (error, sentence) in [
            (
                SearchError::Blocked(202),
                "La recherche web est bloquée pour le moment.",
            ),
            (
                SearchError::PageChanged(200),
                "La recherche web ne marche plus : Script a changé sa page.",
            ),
        ] {
            let script = Script::default();
            let search = search_with(&script, |c| c.backoff_minutes = 30);
            script.say(Err(error));
            assert_eq!(search.search("first").await.unwrap_err(), sentence);
            // During the pause: the same sentence at once, whatever the question, and the
            // provider is not asked.
            tokio::time::sleep(29 * MINUTE).await;
            assert_eq!(search.search("second").await.unwrap_err(), sentence);
            assert_eq!(search.search("first").await.unwrap_err(), sentence);
            assert_eq!(script.calls(), 1);
            // After it: asked again, and a success ends it.
            tokio::time::sleep(2 * MINUTE).await;
            script.say(Ok(vec![hit(1)]));
            assert!(search.search("third").await.is_ok());
            assert_eq!(script.calls(), 2);
            script.say(Ok(vec![hit(2)]));
            assert!(search.search("fourth").await.is_ok());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_block_that_lasts_starts_the_pause_again() {
        let script = Script::default();
        let search = search_with(&script, |c| c.backoff_minutes = 10);
        script.say(Err(SearchError::Blocked(202)));
        script.say(Err(SearchError::Blocked(202)));
        search.search("a").await.unwrap_err();
        tokio::time::sleep(11 * MINUTE).await;
        search.search("a").await.unwrap_err();
        assert_eq!(script.calls(), 2);
        tokio::time::sleep(5 * MINUTE).await;
        search.search("a").await.unwrap_err();
        assert_eq!(script.calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn an_unreachable_provider_is_asked_again_at_once() {
        let script = Script::default();
        let search = search_with(&script, |_| {});
        script.say(Err(SearchError::Unreachable(None)));
        script.say(Err(SearchError::Unreachable(Some(503))));
        script.say(Ok(vec![hit(1)]));
        for _ in 0..2 {
            assert_eq!(
                search.search("q").await.unwrap_err(),
                "La recherche web ne répond pas."
            );
        }
        assert!(search.search("q").await.is_ok());
        assert_eq!(script.calls(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refusal_is_told_and_changes_nothing() {
        let script = Script::default();
        let search = search_with(&script, |_| {});
        script.say(Err(SearchError::Refused("Pas cette question.".to_owned())));
        script.say(Ok(vec![hit(1)]));
        assert_eq!(search.search("q").await.unwrap_err(), "Pas cette question.");
        assert!(search.search("q").await.is_ok());
        // Nothing to ask, or too much: said without asking.
        assert!(search.search("   ").await.is_err());
        assert_eq!(
            search.search(&"x".repeat(MAX_QUERY + 1)).await.unwrap_err(),
            QUERY_TOO_LONG
        );
        assert_eq!(script.calls(), 2);
    }

    /// What the logs say, kept in memory.
    #[derive(Clone, Default)]
    struct Logs(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Logs {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Logs {
        type Writer = Logs;

        fn make_writer(&'a self) -> Logs {
            self.clone()
        }
    }

    impl Logs {
        fn lines(&self) -> Vec<String> {
            String::from_utf8_lossy(&self.0.lock().unwrap())
                .lines()
                .map(str::to_owned)
                .collect()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_log_speaks_when_the_state_changes_only() {
        let logs = Logs::default();
        let _guard = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_writer(logs.clone())
                .with_ansi(false)
                .with_max_level(tracing::Level::INFO)
                .finish(),
        );
        let script = Script::default();
        let search = search_with(&script, |c| c.backoff_minutes = 10);
        let worrying = |logs: &Logs| {
            logs.lines()
                .into_iter()
                .filter(|l| l.contains("does not work") || l.contains("works again"))
                .collect::<Vec<_>>()
        };
        // Working from the start says nothing about the state.
        script.say(Ok(vec![hit(1)]));
        search.search("secret question one").await.unwrap();
        assert!(worrying(&logs).is_empty());
        // A block: one warning with the case and the code, however many calls and pauses.
        for _ in 0..3 {
            script.say(Err(SearchError::Blocked(202)));
            search.search("secret question two").await.unwrap_err();
            search.search("secret question two").await.unwrap_err();
            tokio::time::sleep(11 * MINUTE).await;
        }
        let lines = worrying(&logs);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("WARN") && lines[0].contains("blocked"),
            "{lines:?}"
        );
        assert!(
            lines[0].contains("202") && lines[0].contains("script"),
            "{lines:?}"
        );
        // Another case is a change; so is the return to normal, said once.
        script.say(Err(SearchError::PageChanged(200)));
        search.search("secret question three").await.unwrap_err();
        script.say(Ok(vec![hit(2)]));
        tokio::time::sleep(11 * MINUTE).await;
        search.search("secret question four").await.unwrap();
        script.say(Ok(vec![hit(3)]));
        search.search("secret question five").await.unwrap();
        let lines = worrying(&logs);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(
            lines[1].contains("page changed") && lines[1].contains("200"),
            "{lines:?}"
        );
        assert!(
            lines[2].contains("INFO") && lines[2].contains("again"),
            "{lines:?}"
        );
        // Never the question, whatever the level.
        assert!(
            logs.lines().iter().all(|l| !l.contains("secret")),
            "{:?}",
            logs.lines()
        );
    }
}
