//! `web_wikipedia`: the summary of the Wikipedia article on a subject (`specs/web.md`).
//!
//! Two requests per edition: a search (`/w/api.php`, three titles) then the summary of the first
//! title (`/api/rest_v1/page/summary/<title>`). The first edition of `languages` that has the
//! subject answers; the next one is asked when it has nothing. A disambiguation page is not
//! summarised: the LLM is given the other titles found, to ask which one is meant.

use std::sync::Arc;

use reqwest::Url;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{info, warn};

use super::{Fetch, Fetched, config::WikipediaConfig};

/// Where an edition is, `{lang}` standing for its code.
const ORIGIN: &str = "https://{lang}.wikipedia.org";
/// How many titles the search keeps: the first is summarised, the others are offered.
const FOUND: usize = 3;
/// The longest subject, in characters.
const MAX_QUERY: usize = 200;

const UNREACHABLE: &str = "Wikipédia ne répond pas pour le moment.";
const UNREADABLE: &str = "Wikipédia a répondu quelque chose d'incompréhensible.";

pub struct Wikipedia {
    fetch: Arc<dyn Fetch>,
    languages: Vec<String>,
    max_chars: usize,
    origin: String,
}

#[derive(Deserialize)]
struct SearchReply {
    query: Option<SearchQuery>,
    /// MediaWiki says a refused request this way, with a 200.
    error: Option<Value>,
}

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default)]
    search: Vec<Hit>,
}

#[derive(Deserialize)]
struct Hit {
    title: String,
}

#[derive(Deserialize)]
struct Summary {
    #[serde(rename = "type")]
    kind: Option<String>,
    title: Option<String>,
    description: Option<String>,
    extract: Option<String>,
    content_urls: Option<ContentUrls>,
}

#[derive(Deserialize)]
struct ContentUrls {
    desktop: Option<PageUrl>,
}

#[derive(Deserialize)]
struct PageUrl {
    page: Option<String>,
}

impl Wikipedia {
    pub fn new(config: &WikipediaConfig, fetch: Arc<dyn Fetch>) -> Self {
        Self {
            fetch,
            languages: config.languages.clone(),
            max_chars: config.max_chars,
            origin: ORIGIN.to_owned(),
        }
    }

    /// Asks at another address (`{lang}` stands for the code), for the tests.
    pub fn at(mut self, origin: &str) -> Self {
        self.origin = origin.to_owned();
        self
    }

    /// The answer for the LLM (compact JSON), or what it must be told when there is none.
    pub async fn lookup(&self, query: &str) -> Result<String, String> {
        if query.chars().count() > MAX_QUERY {
            return Err("Le sujet est trop long pour une recherche.".to_owned());
        }
        for lang in &self.languages {
            let titles = self.search(lang, query).await?;
            let Some((first, others)) = titles.split_first() else {
                continue;
            };
            let Some(summary) = self.summary(lang, first).await? else {
                continue;
            };
            let title = summary.title.unwrap_or_else(|| first.clone());
            if summary.kind.as_deref() == Some("disambiguation") {
                info!(lang, "web: wikipedia, a disambiguation page");
                return Ok(json!({
                    "lang": lang,
                    "title": title,
                    "disambiguation": true,
                    "options": others,
                    "note": "Plusieurs sujets portent ce nom : demande à l'utilisateur lequel il veut.",
                })
                .to_string());
            }
            let extract = shorten(
                &remove_pronunciation(summary.extract.as_deref().unwrap_or_default()),
                self.max_chars,
            );
            if extract.is_empty() {
                continue;
            }
            info!(lang, others = others.len(), "web: wikipedia, an article");
            let mut answer = json!({"lang": lang, "title": title, "extract": extract});
            let description = summary.description.as_deref().map(str::trim);
            if let Some(description) = description.filter(|d| !d.is_empty()) {
                answer["description"] = json!(description);
            }
            if !others.is_empty() {
                answer["others"] = json!(others);
            }
            // For `web_read`, when the summary is not enough (the figures of an infobox).
            let page = summary.content_urls.and_then(|u| u.desktop?.page);
            if let Some(url) = page.filter(|u| Url::parse(u).is_ok()) {
                answer["url"] = json!(url);
            }
            return Ok(answer.to_string());
        }
        info!("web: wikipedia, nothing found");
        Err(format!("Wikipédia n'a pas d'article sur « {query} »."))
    }

    fn base(&self, lang: &str) -> Result<Url, String> {
        Url::parse(&self.origin.replace("{lang}", lang)).map_err(|e| {
            warn!("web: wikipedia, the address of the edition is wrong: {e}");
            UNREACHABLE.to_owned()
        })
    }

    async fn get(&self, url: Url) -> Result<Fetched, String> {
        self.fetch
            .get(url)
            .await
            .map_err(|_| UNREACHABLE.to_owned())
    }

    /// The titles found for `query`, best first (none: the edition has nothing).
    async fn search(&self, lang: &str, query: &str) -> Result<Vec<String>, String> {
        let mut url = self.base(lang)?;
        url.set_path("/w/api.php");
        url.query_pairs_mut()
            .append_pair("action", "query")
            .append_pair("list", "search")
            .append_pair("srsearch", query)
            .append_pair("srlimit", &FOUND.to_string())
            // Only the titles: the extracts of the search come balisés and the summary is read next.
            .append_pair("srprop", "")
            .append_pair("format", "json")
            .append_pair("formatversion", "2");
        let fetched = self.get(url).await?;
        if fetched.status != 200 {
            warn!(
                lang,
                status = fetched.status,
                "web: wikipedia, search refused"
            );
            return Err(UNREACHABLE.to_owned());
        }
        let reply: SearchReply = serde_json::from_slice(&fetched.body).map_err(|e| {
            warn!(lang, "web: wikipedia, unreadable search answer: {e}");
            UNREADABLE.to_owned()
        })?;
        if reply.error.is_some() {
            warn!(lang, "web: wikipedia, the search was refused in the answer");
            return Err(UNREACHABLE.to_owned());
        }
        Ok(reply
            .query
            .map(|q| q.search.into_iter().map(|h| h.title).collect())
            .unwrap_or_default())
    }

    /// The summary of an article (none: it is not there, which a search that just found it makes
    /// rare).
    async fn summary(&self, lang: &str, title: &str) -> Result<Option<Summary>, String> {
        let mut url = self.base(lang)?;
        url.path_segments_mut()
            .map_err(|()| UNREACHABLE.to_owned())?
            .clear()
            .extend(["api", "rest_v1", "page", "summary"])
            // `push` encodes what must be: a `/` or a `?` of a title stays in the title.
            .push(&title.replace(' ', "_"));
        let fetched = self.get(url).await?;
        match fetched.status {
            200 => {}
            404 => return Ok(None),
            status => {
                warn!(lang, status, "web: wikipedia, summary refused");
                return Err(UNREACHABLE.to_owned());
            }
        }
        serde_json::from_slice(&fetched.body)
            .map(Some)
            .map_err(|e| {
                warn!(lang, "web: wikipedia, unreadable summary: {e}");
                UNREADABLE.to_owned()
            })
    }
}

/// Whether a character belongs to the phonetic alphabet (or to its modifiers), which no sentence
/// of a summary uses outside a pronunciation.
fn is_phonetic(c: char) -> bool {
    matches!(c,
        '\u{0250}'..='\u{02FF}' // IPA extensions, modifier letters
        | '\u{0300}'..='\u{036F}' // combining marks (nasal vowels)
        | '\u{1D00}'..='\u{1DBF}' // phonetic extensions
        | 'ð' | 'ŋ' | 'θ')
}

/// Replaces the groups `open … close` (no other `open` inside, at most 120 characters) for which
/// `rewrite` gives a text; the others stay.
fn rewrite_groups(
    text: &str,
    open: char,
    close: char,
    rewrite: impl Fn(&str) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(open) {
        let after = &rest[start + open.len_utf8()..];
        let replaced = after.find(close).and_then(|end| {
            let inside = &after[..end];
            if inside.contains(open) || inside.chars().count() > 120 {
                return None;
            }
            rewrite(inside).map(|new| (new, end + close.len_utf8()))
        });
        match replaced {
            Some((new, consumed)) => {
                out.push_str(&rest[..start]);
                out.push_str(&new);
                rest = &after[consumed..];
            }
            None => {
                out.push_str(&rest[..start + open.len_utf8()]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// What is left of a parenthesis once the link to the recording (« écouter », « listen ») that
/// Wikipedia puts at its start is taken off. Only when the word stands alone: « (écouter de la
/// musique) » is a sentence.
fn without_listen_link(inside: &str) -> &str {
    for word in ["écouter", "listen"] {
        let Some(head) = inside.get(..word.len()) else {
            continue;
        };
        let rest = &inside[word.len()..];
        let alone = rest.trim_start().is_empty() || rest.trim_start().starts_with([';', ',']);
        if alone && head.to_lowercase() == word {
            return rest;
        }
    }
    inside
}

fn is_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, ';' | ',' | ':')
}

/// The summary without what a voice cannot say: the pronunciation in brackets or between slashes
/// (`[tuʁɛfɛl]`, `/ˈaɪfəl/`), the link to the recording (`( écouter)`), and the parentheses and
/// spaces that this leaves empty or loose.
pub fn remove_pronunciation(text: &str) -> String {
    let phonetic = |inside: &str| inside.chars().any(is_phonetic).then(String::new);
    let text = rewrite_groups(text, '[', ']', phonetic);
    let text = rewrite_groups(&text, '/', '/', phonetic);
    // Again until nothing changes: a parenthesis emptied can leave another one empty around it.
    let tidy_parentheses = |text: String| {
        rewrite_groups(&text, '(', ')', |inside| {
            let tidy = |s: &str| s.trim_matches(is_separator).to_owned();
            let kept = tidy(without_listen_link(tidy(inside).as_str()));
            if !kept.chars().any(char::is_alphanumeric) {
                Some(String::new())
            } else if kept != inside {
                Some(format!("({kept})"))
            } else {
                None
            }
        })
    };
    let mut text = text;
    for _ in 0..3 {
        let next = tidy_parentheses(text.clone());
        if next == text {
            break;
        }
        text = next;
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    // A space is left before the sign that followed a removed group.
    [(" ,", ","), (" .", "."), (" )", ")"), ("( ", "(")]
        .iter()
        .fold(text, |text, (from, to)| text.replace(from, to))
}

/// At most `max` characters: cut after a sentence if one ends in the second half, else after a
/// word, with « … ».
fn shorten(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_owned();
    }
    let sentence_end = (max / 2..max).rev().find(|&i| {
        matches!(chars[i], '.' | '!' | '?') && chars.get(i + 1).is_none_or(|c| c.is_whitespace())
    });
    if let Some(end) = sentence_end {
        return chars[..=end].iter().collect();
    }
    // Room for the « … ».
    let room = max.saturating_sub(1);
    let mut head: String = chars[..room].iter().collect();
    if !chars[room].is_whitespace()
        && let Some(space) = head.rfind(char::is_whitespace)
    {
        head.truncate(space);
    }
    format!(
        "{}…",
        head.trim_end_matches(|c: char| is_separator(c) || c == '-')
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::FetchError;
    use futures_util::future::BoxFuture;
    use std::{collections::HashMap, sync::Mutex};

    const SEARCH_EIFFEL: &str = include_str!("../../tests/fixtures/web/search-fr-tour-eiffel.json");
    const SUMMARY_EIFFEL: &str =
        include_str!("../../tests/fixtures/web/summary-fr-tour-eiffel.json");

    /// Recorded answers by address; whatever else is asked is unreachable. What was asked is kept.
    #[derive(Default)]
    struct Recorded {
        answers: Mutex<HashMap<String, (u16, String)>>,
        asked: Mutex<Vec<String>>,
    }

    impl Recorded {
        fn on(self: &Arc<Self>, url: &str, status: u16, body: &str) -> &Arc<Self> {
            self.answers
                .lock()
                .unwrap()
                .insert(url.to_owned(), (status, body.to_owned()));
            self
        }

        fn asked(&self) -> Vec<String> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl Fetch for Recorded {
        fn get(&self, url: Url) -> BoxFuture<'_, Result<Fetched, FetchError>> {
            self.asked.lock().unwrap().push(url.to_string());
            let answer = self.answers.lock().unwrap().get(url.as_str()).cloned();
            Box::pin(async move {
                answer
                    .map(|(status, body)| Fetched {
                        status,
                        body: body.into_bytes(),
                    })
                    .ok_or(FetchError::Unreachable)
            })
        }
    }

    fn wikipedia(recorded: &Arc<Recorded>, languages: &[&str]) -> Wikipedia {
        Wikipedia::new(
            &WikipediaConfig {
                languages: languages.iter().map(|l| (*l).to_owned()).collect(),
                ..WikipediaConfig::default()
            },
            recorded.clone(),
        )
    }

    fn search_url(lang: &str, query: &str) -> String {
        format!(
            "https://{lang}.wikipedia.org/w/api.php?action=query&list=search&srsearch={query}&srlimit=3&srprop=&format=json&formatversion=2"
        )
    }

    fn summary_url(lang: &str, title: &str) -> String {
        format!("https://{lang}.wikipedia.org/api/rest_v1/page/summary/{title}")
    }

    fn found(titles: &[&str]) -> String {
        json!({"batchcomplete": true, "query": {"search": titles.iter().map(|t| json!({"ns": 0, "title": t})).collect::<Vec<_>>()}}).to_string()
    }

    const NOTHING: &str =
        r#"{"batchcomplete":true,"query":{"searchinfo":{"totalhits":0},"search":[]}}"#;

    fn answer(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[tokio::test]
    async fn the_summary_of_an_article_comes_without_its_pronunciation() {
        let recorded = Arc::new(Recorded::default());
        recorded
            .on(&search_url("fr", "tour+eiffel"), 200, SEARCH_EIFFEL)
            .on(&summary_url("fr", "Tour_Eiffel"), 200, SUMMARY_EIFFEL);
        let text = wikipedia(&recorded, &["fr", "en"])
            .lookup("tour eiffel")
            .await
            .unwrap();
        let a = answer(&text);
        assert_eq!(a["title"], "Tour Eiffel");
        assert_eq!(a["lang"], "fr");
        assert_eq!(
            a["description"],
            "tour de fer puddlé de 330 mètres à Paris, France"
        );
        assert_eq!(
            a["extract"],
            "La tour Eiffel est une tour autoportante de fer puddlé de 330 m de hauteur située à Paris, à l’extrémité nord-ouest du parc du Champ-de-Mars en bordure de la Seine dans le 7e arrondissement. Son adresse officielle est 5, avenue Anatole-France."
        );
        // The two other titles are offered, and the address of the article for web_read; no
        // identifier, nothing else to read out.
        assert_eq!(
            a["others"],
            json!(["Histoire de la tour Eiffel", "Gustave Eiffel"])
        );
        assert_eq!(a["url"], "https://fr.wikipedia.org/wiki/Tour_Eiffel");
        assert_eq!(a.as_object().unwrap().len(), 6, "{text}");
        // One search, one summary, in the first language only.
        assert_eq!(
            recorded.asked(),
            [
                search_url("fr", "tour+eiffel"),
                summary_url("fr", "Tour_Eiffel")
            ]
        );
    }

    #[tokio::test]
    async fn a_disambiguation_page_is_not_summarised_the_other_titles_are_offered() {
        let recorded = Arc::new(Recorded::default());
        recorded
            .on(
                &search_url("fr", "mercure+homonymie"),
                200,
                include_str!("../../tests/fixtures/web/search-fr-mercure-homonymie.json"),
            )
            .on(
                &summary_url("fr", "Daniel_Mercure_(homonymie)"),
                200,
                include_str!("../../tests/fixtures/web/summary-fr-disambiguation.json"),
            );
        let text = wikipedia(&recorded, &["fr", "en"])
            .lookup("mercure homonymie")
            .await
            .unwrap();
        let a = answer(&text);
        assert_eq!(a["disambiguation"], true);
        assert_eq!(a["title"], "Daniel Mercure (homonymie)");
        assert_eq!(a["options"], json!(["Mercure", "Mercure de France"]));
        // Its own text is not given: the LLM asks which one is meant.
        assert!(a.get("extract").is_none() && !text.contains("pianiste"));
        assert!(a["note"].as_str().unwrap().contains("demande"));
        assert_eq!(recorded.asked().len(), 2, "no other edition is asked");
    }

    #[tokio::test]
    async fn an_edition_with_nothing_gives_way_to_the_next() {
        let recorded = Arc::new(Recorded::default());
        recorded
            .on(&search_url("fr", "eiffel+tower"), 200, NOTHING)
            .on(
                &search_url("en", "eiffel+tower"),
                200,
                include_str!("../../tests/fixtures/web/search-en-eiffel-tower.json"),
            )
            .on(
                &summary_url("en", "Eiffel_Tower"),
                200,
                include_str!("../../tests/fixtures/web/summary-en-eiffel-tower.json"),
            );
        let text = wikipedia(&recorded, &["fr", "en"])
            .lookup("eiffel tower")
            .await
            .unwrap();
        let a = answer(&text);
        assert_eq!(a["lang"], "en", "the LLM must know the text is in English");
        assert_eq!(a["title"], "Eiffel Tower");
        let extract = a["extract"].as_str().unwrap();
        assert!(extract.starts_with("The Eiffel Tower"), "{extract}");
        assert!(
            !extract.contains(['[', '/', 'ˈ']) && !extract.to_lowercase().contains("listen"),
            "{extract}"
        );
        assert_eq!(
            recorded.asked(),
            [
                search_url("fr", "eiffel+tower"),
                search_url("en", "eiffel+tower"),
                summary_url("en", "Eiffel_Tower")
            ]
        );
    }

    #[tokio::test]
    async fn a_page_that_is_not_found_gives_way_to_the_next_edition_too() {
        let recorded = Arc::new(Recorded::default());
        recorded
            .on(&search_url("fr", "mot"), 200, &found(&["Zzqxv inexistant"]))
            .on(
                &summary_url("fr", "Zzqxv_inexistant"),
                404,
                include_str!("../../tests/fixtures/web/summary-fr-not-found.json"),
            )
            .on(&search_url("en", "mot"), 200, &found(&["Mot"]))
            .on(
                &summary_url("en", "Mot"),
                200,
                &json!({"type": "standard", "title": "Mot", "extract": "A mot is a word."})
                    .to_string(),
            );
        let text = wikipedia(&recorded, &["fr", "en"])
            .lookup("mot")
            .await
            .unwrap();
        assert_eq!(answer(&text)["lang"], "en");
    }

    #[tokio::test]
    async fn nothing_in_any_edition_is_said_simply() {
        let recorded = Arc::new(Recorded::default());
        recorded.on(&search_url("fr", "zzqxv"), 200, NOTHING).on(
            &search_url("en", "zzqxv"),
            200,
            NOTHING,
        );
        let error = wikipedia(&recorded, &["fr", "en"])
            .lookup("zzqxv")
            .await
            .unwrap_err();
        assert_eq!(error, "Wikipédia n'a pas d'article sur « zzqxv ».");
        assert_eq!(recorded.asked().len(), 2);
        // A subject asked in one edition only is not looked for in another.
        let recorded = Arc::new(Recorded::default());
        recorded.on(&search_url("fr", "zzqxv"), 200, NOTHING);
        assert!(wikipedia(&recorded, &["fr"]).lookup("zzqxv").await.is_err());
        assert_eq!(recorded.asked().len(), 1);
    }

    #[tokio::test]
    async fn a_service_that_fails_is_said_simply_and_not_retried_elsewhere() {
        // Unreachable (the fake knows nothing of the address).
        let recorded = Arc::new(Recorded::default());
        let error = wikipedia(&recorded, &["fr", "en"])
            .lookup("tour eiffel")
            .await
            .unwrap_err();
        assert_eq!(error, UNREACHABLE);
        assert_eq!(recorded.asked().len(), 1, "no other edition is tried");
        // A refusal (too many requests), at the search then at the summary.
        let recorded = Arc::new(Recorded::default());
        recorded.on(&search_url("fr", "a"), 429, "slow down");
        assert_eq!(
            wikipedia(&recorded, &["fr"]).lookup("a").await.unwrap_err(),
            UNREACHABLE
        );
        let recorded = Arc::new(Recorded::default());
        recorded.on(&search_url("fr", "a"), 200, &found(&["A"])).on(
            &summary_url("fr", "A"),
            503,
            "",
        );
        assert_eq!(
            wikipedia(&recorded, &["fr"]).lookup("a").await.unwrap_err(),
            UNREACHABLE
        );
        // An answer that is not what the API gives, or an error in a 200.
        for body in ["<html>maintenance</html>", ""] {
            let recorded = Arc::new(Recorded::default());
            recorded.on(&search_url("fr", "a"), 200, body);
            assert_eq!(
                wikipedia(&recorded, &["fr"]).lookup("a").await.unwrap_err(),
                UNREADABLE
            );
        }
        let recorded = Arc::new(Recorded::default());
        recorded.on(
            &search_url("fr", "a"),
            200,
            r#"{"error":{"code":"badvalue","info":"x"}}"#,
        );
        assert_eq!(
            wikipedia(&recorded, &["fr"]).lookup("a").await.unwrap_err(),
            UNREACHABLE
        );
    }

    #[tokio::test]
    async fn a_title_is_encoded_in_the_address_and_a_query_too() {
        let recorded = Arc::new(Recorded::default());
        recorded
            .on(
                &search_url("fr", "ac%2Fdc+%26+%C3%A7a%3F"),
                200,
                &found(&["AC/DC", "Qu'est-ce que ça ?"]),
            )
            .on(
                &summary_url("fr", "AC%2FDC"),
                200,
                r#"{"type":"standard","title":"AC/DC","extract":"AC/DC est un groupe."}"#,
            );
        let text = wikipedia(&recorded, &["fr"])
            .lookup("ac/dc & ça?")
            .await
            .unwrap();
        assert_eq!(answer(&text)["title"], "AC/DC");
        assert_eq!(recorded.asked().len(), 2, "{:?}", recorded.asked());
    }

    #[tokio::test]
    async fn a_summary_without_text_is_skipped_and_a_long_subject_refused() {
        let recorded = Arc::new(Recorded::default());
        recorded.on(&search_url("fr", "x"), 200, &found(&["X"])).on(
            &summary_url("fr", "X"),
            200,
            r#"{"type":"no-extract","title":"X"}"#,
        );
        assert!(wikipedia(&recorded, &["fr"]).lookup("x").await.is_err());
        let long = "a".repeat(MAX_QUERY + 1);
        let error = wikipedia(&recorded, &["fr"])
            .lookup(&long)
            .await
            .unwrap_err();
        assert!(error.contains("trop long"), "{error}");
        assert_eq!(recorded.asked().len(), 2, "the long subject is not sent");
    }

    #[tokio::test]
    async fn the_summary_is_cut_to_the_size_of_the_configuration() {
        let recorded = Arc::new(Recorded::default());
        recorded
            .on(&search_url("fr", "tour+eiffel"), 200, SEARCH_EIFFEL)
            .on(&summary_url("fr", "Tour_Eiffel"), 200, SUMMARY_EIFFEL);
        let short = Wikipedia::new(
            &WikipediaConfig {
                max_chars: 150,
                ..WikipediaConfig::default()
            },
            recorded.clone(),
        );
        let a = answer(&short.lookup("tour eiffel").await.unwrap());
        let extract = a["extract"].as_str().unwrap();
        assert!(extract.chars().count() <= 150, "{extract}");
        assert!(extract.starts_with("La tour Eiffel est une tour autoportante"));
        assert!(extract.ends_with('…'), "{extract}");
    }

    #[test]
    fn the_pronunciation_goes_and_the_sentence_closes_up() {
        for (before, after) in [
            // The recorded one.
            (
                "La tour Eiffel [tuʁɛfɛl] est une tour.",
                "La tour Eiffel est une tour.",
            ),
            // Before a comma, nasal vowels, a long pronunciation.
            ("Bonjour [bɔ̃ʒuʁ], monde.", "Bonjour, monde."),
            // Between slashes, English style, with the link to the recording.
            (
                "Paris (/paʁi/ ( écouter)) est une ville.",
                "Paris est une ville.",
            ),
            ("Paris (écouter) est une ville.", "Paris est une ville."),
            ("Rome (/ˈroʊm/ ; Listen) is a city.", "Rome is a city."),
            // What is left of a parenthesis is kept, loose separators go.
            (
                "Gustave Eiffel ([ɡystav ɛfɛl] ; né en 1832) est un ingénieur.",
                "Gustave Eiffel (né en 1832) est un ingénieur.",
            ),
            // Nothing phonetic: left alone, brackets, slashes and parentheses.
            (
                "Le [sic] et/ou le texte (1832-1923) restent, 3/4 aussi.",
                "Le [sic] et/ou le texte (1832-1923) restent, 3/4 aussi.",
            ),
            // A bracket that never closes.
            ("Ouvert [tuʁ sans fin.", "Ouvert [tuʁ sans fin."),
            // Spaces, including the non-breaking ones, are made plain.
            ("Il fait\u{a0}froid  ici .", "Il fait froid ici."),
            ("", ""),
        ] {
            assert_eq!(remove_pronunciation(before), after, "{before}");
        }
        // The word « écouter » in a sentence is left.
        assert_eq!(
            remove_pronunciation("On aime écouter (de la musique)."),
            "On aime écouter (de la musique)."
        );
    }

    #[test]
    fn a_text_is_cut_after_a_sentence_when_it_can_else_after_a_word() {
        assert_eq!(shorten("Court.", 100), "Court.");
        let two = "Première phrase assez longue. Seconde phrase qui déborde largement.";
        assert_eq!(shorten(two, 45), "Première phrase assez longue.");
        // No sentence end in the second half of the limit: after a word.
        let one = "Une phrase unique qui ne finit jamais, tant elle est longue et sans point";
        let cut = shorten(one, 30);
        assert_eq!(cut, "Une phrase unique qui ne…");
        assert!(cut.chars().count() <= 30);
        // A cut in the first half is not taken for a sentence (« Dr. » and the like).
        assert_eq!(
            shorten("Dr. Alice Martin dirige le laboratoire de physique.", 30),
            "Dr. Alice Martin dirige le…"
        );
        // Exactly the limit: untouched; characters, not bytes.
        assert_eq!(shorten("éééé", 4), "éééé");
        assert_eq!(shorten("éé ééé", 4), "éé…");
    }
}
