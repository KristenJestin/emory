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
        self.enabled && (self.weather.enabled || self.wikipedia.enabled)
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
        self.wikipedia.check()
    }
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
    fn the_module_can_be_disabled_whole_or_by_tool() {
        assert!(!parse("enabled = false").unwrap().any_tool());
        // One tool left is enough; none is no module.
        assert!(parse("[wikipedia]\nenabled = false").unwrap().any_tool());
        assert!(parse("[weather]\nenabled = false").unwrap().any_tool());
        assert!(
            !parse("[weather]\nenabled = false\n[wikipedia]\nenabled = false")
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
