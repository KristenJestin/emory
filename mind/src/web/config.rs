//! The `[web]` section of the configuration (`specs/web.md`). Every key has a default: a missing
//! section gives the module with these values.

use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WebConfig {
    /// `false` takes every web tool away.
    pub enabled: bool,
    /// The `User-Agent` of the API calls. Wikimedia wants one that lets them reach the author:
    /// the owner completes it with a contact. By default it names Emory and its version only.
    pub user_agent: Option<String>,
    /// Milliseconds for one call to an API.
    pub timeout_ms: u64,
    pub weather: WeatherConfig,
    pub wikipedia: WikipediaConfig,
    pub search: SearchConfig,
    pub read: ReadConfig,
}

/// `web_read`: the text of a page found by `web_search` or `web_wikipedia`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ReadConfig {
    pub enabled: bool,
    /// Only an address that a tool of the module gave in the same conversation: a page read
    /// may hold orders, and the LLM must not be able to send what it knows anywhere.
    pub only_found_urls: bool,
    /// Milliseconds for the whole reading: the redirections and the page.
    pub timeout_ms: u64,
    /// The most read of a page, in bytes; the rest is dropped.
    pub max_bytes: usize,
    /// The longest text given to the LLM, in characters.
    pub max_chars: usize,
    /// How many redirections are followed at most.
    pub max_redirects: usize,
}

/// `web_search`: the search of the web, through a provider that can be replaced.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SearchConfig {
    /// `"duckduckgo"` (its light HTML page, no key), or `"none"`, which takes the tool away.
    pub provider: String,
    /// The region of the results, as DuckDuckGo spells it (`fr-fr`, `us-en`, `wt-wt`).
    pub region: String,
    /// How many results the LLM gets at most.
    pub max_results: usize,
    /// Milliseconds for one search.
    pub timeout_ms: u64,
    /// The least time between two requests to the provider, in milliseconds.
    pub min_interval_ms: u64,
    /// How long the answer to a query is kept, in minutes (0: never).
    pub cache_minutes: u64,
    /// The pause after a block or a changed page, in minutes: the tool answers at once, without
    /// asking the provider again.
    pub backoff_minutes: u64,
    /// The `User-Agent` of the searches: the page refuses a robot more willingly than a browser.
    pub user_agent: String,
}

/// `web_weather`: the weather now and in the next days, from Open-Meteo.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WeatherConfig {
    pub enabled: bool,
    /// The place of the owner, used when the LLM gives none: a name, which Open-Meteo finds
    /// (`"Lyon"`; a name shared by cities of several countries takes the country after a comma,
    /// `"Lyon, France"`). Empty: the LLM must be given a place.
    pub place: String,
    /// The coordinates of `place`, which save the lookup of its name. Both or none.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// How long a forecast is kept, in minutes (0: never).
    pub cache_minutes: u64,
}

/// `web_wikipedia`: the summary of an article.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WikipediaConfig {
    pub enabled: bool,
    /// The editions to try, in this order: the next one is asked when the previous has nothing.
    /// Language codes of Wikipedia (`fr`, `en`, `pt-br`…).
    pub languages: Vec<String>,
    /// The longest summary given to the LLM, in characters.
    pub max_chars: usize,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            user_agent: None,
            timeout_ms: 5000,
            weather: WeatherConfig::default(),
            wikipedia: WikipediaConfig::default(),
            search: SearchConfig::default(),
            read: ReadConfig::default(),
        }
    }
}

impl Default for ReadConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            only_found_urls: true,
            timeout_ms: 8000,
            max_bytes: 2_000_000,
            max_chars: 6000,
            max_redirects: 5,
        }
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            provider: "duckduckgo".to_owned(),
            region: "fr-fr".to_owned(),
            max_results: 5,
            timeout_ms: 5000,
            min_interval_ms: 1000,
            cache_minutes: 10,
            backoff_minutes: 30,
            user_agent: "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0"
                .to_owned(),
        }
    }
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            place: String::new(),
            latitude: None,
            longitude: None,
            cache_minutes: 10,
        }
    }
}

impl Default for WikipediaConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            languages: vec!["fr".to_owned(), "en".to_owned()],
            max_chars: 1200,
        }
    }
}

impl WebConfig {
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }

    /// The `User-Agent` sent: the configured one, or Emory's name and version.
    pub fn user_agent(&self) -> String {
        self.user_agent
            .clone()
            .unwrap_or_else(|| format!("Emory/{} (assistant vocal)", env!("CARGO_PKG_VERSION")))
    }

    /// Whether any web tool is left.
    pub fn any_tool(&self) -> bool {
        self.enabled && (self.weather.enabled || self.finds() || self.reads())
    }

    /// Whether a tool gives addresses (`web_wikipedia`, `web_search`).
    fn finds(&self) -> bool {
        self.wikipedia.enabled || self.search.enabled()
    }

    /// Whether `web_read` is there: enabled, and with something to read when it may only read
    /// the addresses found.
    pub fn reads(&self) -> bool {
        self.enabled && self.read.enabled && (self.finds() || !self.read.only_found_urls)
    }

    /// Refuses absurd values, naming the key.
    pub fn check(&self) -> Result<()> {
        anyhow::ensure!(
            (100..=60_000).contains(&self.timeout_ms),
            "web.timeout_ms must be between 100 and 60000"
        );
        if let Some(agent) = &self.user_agent {
            anyhow::ensure!(
                !agent.trim().is_empty() && reqwest::header::HeaderValue::from_str(agent).is_ok(),
                "web.user_agent must be a non-blank header value (no line break)"
            );
        }
        self.weather.check()?;
        self.wikipedia.check()?;
        self.search.check()?;
        self.read.check()
    }
}

impl ReadConfig {
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }

    fn check(&self) -> Result<()> {
        anyhow::ensure!(
            (100..=60_000).contains(&self.timeout_ms),
            "web.read.timeout_ms must be between 100 and 60000"
        );
        anyhow::ensure!(
            (10_000..=20_000_000).contains(&self.max_bytes),
            "web.read.max_bytes must be between 10000 and 20000000"
        );
        anyhow::ensure!(
            (500..=50_000).contains(&self.max_chars),
            "web.read.max_chars must be between 500 and 50000"
        );
        anyhow::ensure!(
            self.max_redirects <= 10,
            "web.read.max_redirects must be 10 at most"
        );
        Ok(())
    }
}

impl SearchConfig {
    /// Whether `web_search` is there: a provider is named.
    pub fn enabled(&self) -> bool {
        self.provider != "none"
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }

    pub fn min_interval(&self) -> Duration {
        Duration::from_millis(self.min_interval_ms)
    }

    pub fn cache(&self) -> Duration {
        Duration::from_secs(self.cache_minutes * 60)
    }

    pub fn backoff(&self) -> Duration {
        Duration::from_secs(self.backoff_minutes * 60)
    }

    fn check(&self) -> Result<()> {
        anyhow::ensure!(
            matches!(self.provider.as_str(), "duckduckgo" | "none"),
            "web.search.provider must be \"duckduckgo\" or \"none\", not \"{}\"",
            self.provider
        );
        // The region goes into the address as it is: nothing else than a region may pass.
        anyhow::ensure!(
            is_region(&self.region),
            "web.search.region \"{}\" is not a region like \"fr-fr\", \"us-en\" or \"wt-wt\"",
            self.region
        );
        anyhow::ensure!(
            (1..=10).contains(&self.max_results),
            "web.search.max_results must be between 1 and 10"
        );
        anyhow::ensure!(
            (100..=60_000).contains(&self.timeout_ms),
            "web.search.timeout_ms must be between 100 and 60000"
        );
        anyhow::ensure!(
            self.min_interval_ms <= 60_000,
            "web.search.min_interval_ms must be 60000 at most"
        );
        anyhow::ensure!(
            self.cache_minutes <= 1440,
            "web.search.cache_minutes must be 1440 at most (0: no cache)"
        );
        // Hammering a blocked page makes the block worse: a pause is always there.
        anyhow::ensure!(
            (1..=1440).contains(&self.backoff_minutes),
            "web.search.backoff_minutes must be between 1 and 1440"
        );
        anyhow::ensure!(
            !self.user_agent.trim().is_empty()
                && reqwest::header::HeaderValue::from_str(&self.user_agent).is_ok(),
            "web.search.user_agent must be a non-blank header value (no line break)"
        );
        Ok(())
    }
}

/// Two letters, a hyphen, two letters: `fr-fr`, `us-en`, `wt-wt`.
fn is_region(region: &str) -> bool {
    let b = region.as_bytes();
    b.len() == 5 && b[2] == b'-' && [0, 1, 3, 4].iter().all(|&i| b[i].is_ascii_lowercase())
}

impl WeatherConfig {
    fn check(&self) -> Result<()> {
        anyhow::ensure!(
            self.place.chars().count() <= 100 && !self.place.contains(['\n', '\r']),
            "web.weather.place must be a name of 100 characters at most"
        );
        anyhow::ensure!(
            self.latitude.is_some() == self.longitude.is_some(),
            "web.weather: latitude and longitude go together"
        );
        if let Some(latitude) = self.latitude {
            anyhow::ensure!(
                (-90.0..=90.0).contains(&latitude),
                "web.weather.latitude must be between -90 and 90"
            );
        }
        if let Some(longitude) = self.longitude {
            anyhow::ensure!(
                (-180.0..=180.0).contains(&longitude),
                "web.weather.longitude must be between -180 and 180"
            );
        }
        anyhow::ensure!(
            self.cache_minutes <= 1440,
            "web.weather.cache_minutes must be 1440 at most (0: no cache)"
        );
        Ok(())
    }
}

impl WikipediaConfig {
    fn check(&self) -> Result<()> {
        anyhow::ensure!(
            !self.languages.is_empty(),
            "web.wikipedia.languages must name at least one language"
        );
        anyhow::ensure!(
            self.languages.len() <= 5,
            "web.wikipedia.languages: five languages at most (each one is another request)"
        );
        for (i, language) in self.languages.iter().enumerate() {
            // The code becomes the first part of a host name: nothing else than a code may pass.
            anyhow::ensure!(
                is_language_code(language),
                "web.wikipedia.languages: \"{language}\" is not a language code like \"fr\" or \"pt-br\""
            );
            anyhow::ensure!(
                !self.languages[..i].contains(language),
                "web.wikipedia.languages: \"{language}\" is given twice"
            );
        }
        anyhow::ensure!(
            (100..=5000).contains(&self.max_chars),
            "web.wikipedia.max_chars must be between 100 and 5000"
        );
        Ok(())
    }
}

/// Lower-case letters and digits, in groups joined by hyphens, starting with a letter: `fr`,
/// `simple`, `zh-min-nan`.
fn is_language_code(code: &str) -> bool {
    code.len() <= 12
        && code.starts_with(|c: char| c.is_ascii_lowercase())
        && code.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> Result<WebConfig> {
        let config: WebConfig = toml::from_str(toml)?;
        config.check()?;
        Ok(config)
    }

    #[test]
    fn an_empty_section_gives_the_defaults() {
        let c = parse("").unwrap();
        assert!(c.enabled && c.any_tool() && c.wikipedia.enabled && c.weather.enabled);
        assert!(c.weather.place.is_empty() && c.weather.latitude.is_none());
        assert_eq!(c.weather.cache_minutes, 10);
        assert_eq!(c.timeout(), Duration::from_secs(5));
        assert_eq!(c.wikipedia.languages, ["fr", "en"]);
        assert_eq!(c.wikipedia.max_chars, 1200);
        let search = &c.search;
        assert!(search.enabled() && search.provider == "duckduckgo" && search.region == "fr-fr");
        assert_eq!((search.max_results, search.cache_minutes), (5, 10));
        assert_eq!(search.timeout(), Duration::from_secs(5));
        assert_eq!(search.min_interval(), Duration::from_secs(1));
        assert_eq!(search.backoff(), Duration::from_secs(30 * 60));
        assert!(
            search.user_agent.starts_with("Mozilla/5.0 "),
            "{}",
            search.user_agent
        );
        assert!(c.user_agent().starts_with("Emory/"), "{}", c.user_agent());
        assert!(WebConfig::default().check().is_ok());
    }

    #[test]
    fn the_keys_are_read() {
        let c = parse(
            "enabled = true\nuser_agent = \"Emory/9 (me@example.org)\"\ntimeout_ms = 2000\n\
             [weather]\nplace = \"Lyon\"\nlatitude = 45.75\nlongitude = 4.85\ncache_minutes = 0\n\
             [wikipedia]\nlanguages = [\"en\", \"pt-br\"]\nmax_chars = 500\n",
        )
        .unwrap();
        assert_eq!(c.weather.place, "Lyon");
        assert_eq!(
            (c.weather.latitude, c.weather.longitude),
            (Some(45.75), Some(4.85))
        );
        assert_eq!(c.weather.cache_minutes, 0);
        assert_eq!(c.user_agent(), "Emory/9 (me@example.org)");
        assert_eq!(c.timeout(), Duration::from_secs(2));
        assert_eq!(c.wikipedia.languages, ["en", "pt-br"]);
        assert_eq!(c.wikipedia.max_chars, 500);
    }

    #[test]
    fn the_search_keys_are_read() {
        let c = parse(
            "[search]\nprovider = \"duckduckgo\"\nregion = \"us-en\"\nmax_results = 3\n\
             timeout_ms = 2000\nmin_interval_ms = 0\ncache_minutes = 0\nbackoff_minutes = 5\n\
             user_agent = \"Some Browser/1.0\"\n",
        )
        .unwrap();
        let s = &c.search;
        assert_eq!((s.region.as_str(), s.max_results), ("us-en", 3));
        assert_eq!(s.timeout(), Duration::from_secs(2));
        assert_eq!(s.min_interval(), Duration::ZERO);
        assert_eq!(s.cache(), Duration::ZERO);
        assert_eq!(s.backoff(), Duration::from_secs(300));
        assert_eq!(s.user_agent, "Some Browser/1.0");
        // The API calls keep their own User-Agent.
        assert!(c.user_agent().starts_with("Emory/"));
    }

    #[test]
    fn provider_none_takes_the_search_away() {
        let c = parse("[search]\nprovider = \"none\"").unwrap();
        assert!(!c.search.enabled() && c.any_tool());
        // Search alone keeps the module; nothing left removes it.
        let only = "[weather]\nenabled = false\n[wikipedia]\nenabled = false\n";
        assert!(parse(only).unwrap().any_tool());
        assert!(
            !parse(&format!("{only}[search]\nprovider = \"none\""))
                .unwrap()
                .any_tool()
        );
    }

    #[test]
    fn the_module_can_be_disabled_whole_or_by_tool() {
        assert!(!parse("enabled = false").unwrap().any_tool());
        // One tool left is enough; none is no module.
        assert!(parse("[wikipedia]\nenabled = false").unwrap().any_tool());
        assert!(parse("[weather]\nenabled = false").unwrap().any_tool());
        assert!(
            !parse("[weather]\nenabled = false\n[wikipedia]\nenabled = false\n[search]\nprovider = \"none\"")
                .unwrap()
                .any_tool()
        );
    }

    #[test]
    fn a_wrong_value_is_refused_with_the_name_of_its_key() {
        for (bad, key) in [
            ("timeout_ms = 0", "web.timeout_ms"),
            ("timeout_ms = 100000", "web.timeout_ms"),
            ("user_agent = \" \"", "web.user_agent"),
            ("user_agent = \"a\\nb\"", "web.user_agent"),
            ("[weather]\nlatitude = 45.0", "web.weather"),
            ("[weather]\nlongitude = 4.0", "web.weather"),
            (
                "[weather]\nlatitude = 95.0\nlongitude = 4.0",
                "web.weather.latitude",
            ),
            (
                "[weather]\nlatitude = 45.0\nlongitude = 200.0",
                "web.weather.longitude",
            ),
            (
                "[weather]\ncache_minutes = 5000",
                "web.weather.cache_minutes",
            ),
            ("[weather]\nplace = \"a\\nb\"", "web.weather.place"),
            ("[wikipedia]\nlanguages = []", "web.wikipedia.languages"),
            (
                "[wikipedia]\nlanguages = [\"fr\", \"fr\"]",
                "web.wikipedia.languages",
            ),
            (
                "[wikipedia]\nlanguages = [\"fr\", \"en\", \"de\", \"es\", \"it\", \"pt\"]",
                "web.wikipedia.languages",
            ),
            (
                "[wikipedia]\nlanguages = [\"evil.example/x#\"]",
                "web.wikipedia.languages",
            ),
            (
                "[wikipedia]\nlanguages = [\"FR\"]",
                "web.wikipedia.languages",
            ),
            ("[wikipedia]\nlanguages = [\"\"]", "web.wikipedia.languages"),
            ("[search]\nprovider = \"bing\"", "web.search.provider"),
            ("[search]\nprovider = \"\"", "web.search.provider"),
            ("[search]\nregion = \"FR-fr\"", "web.search.region"),
            ("[search]\nregion = \"fr\"", "web.search.region"),
            ("[search]\nregion = \"fr-fr&x=1\"", "web.search.region"),
            ("[search]\nmax_results = 0", "web.search.max_results"),
            ("[search]\nmax_results = 11", "web.search.max_results"),
            ("[search]\ntimeout_ms = 10", "web.search.timeout_ms"),
            ("[search]\ntimeout_ms = 100000", "web.search.timeout_ms"),
            (
                "[search]\nmin_interval_ms = 70000",
                "web.search.min_interval_ms",
            ),
            ("[search]\ncache_minutes = 2000", "web.search.cache_minutes"),
            (
                "[search]\nbackoff_minutes = 0",
                "web.search.backoff_minutes",
            ),
            (
                "[search]\nbackoff_minutes = 5000",
                "web.search.backoff_minutes",
            ),
            ("[search]\nuser_agent = \" \"", "web.search.user_agent"),
            ("[search]\nuser_agent = \"a\\nb\"", "web.search.user_agent"),
            ("[wikipedia]\nmax_chars = 10", "web.wikipedia.max_chars"),
            ("[wikipedia]\nmax_chars = 90000", "web.wikipedia.max_chars"),
        ] {
            let error = format!("{:#}", parse(bad).unwrap_err());
            assert!(error.contains(key), "{bad}: {error}");
        }
        // An unknown key, a misspelt section, a value of the wrong type.
        for bad in [
            "volume = 3",
            "[weather]\ncity = \"Lyon\"",
            "[weather]\nplace = 3",
            "[wikipedia]\nlang = [\"fr\"]",
            "[wikipedi]\nenabled = true",
            "[search]\nengine = \"duckduckgo\"",
            "[search]\nmax_results = \"five\"",
            "timeout_ms = \"fast\"",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn language_codes_are_what_a_host_name_can_start_with() {
        for good in ["fr", "en", "simple", "pt-br", "zh-min-nan", "be-tarask"] {
            assert!(is_language_code(good), "{good}");
        }
        for bad in [
            "", "-", "f.r", "fr/", "fr-", "-fr", "1fr", "Fr", "fr:80", "fr@x",
        ] {
            assert!(!is_language_code(bad), "{bad}");
        }
    }
}
