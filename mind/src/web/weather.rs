//! `web_weather`: the weather now and in the next days, from Open-Meteo (`specs/web.md`).
//!
//! A place name is found by the geocoding API (once: the places are kept), the forecast of its
//! coordinates is asked next. What the LLM gets is shaped to be said aloud: whole degrees, the
//! sky in words (the table of WMO codes is `prompts/weather.fr.toml`), days named in French. No
//! key; the forecast of a place is kept for `cache_minutes`.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use chrono::{Datelike, NaiveDate};
use reqwest::Url;
use rmcp::model::JsonObject;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tracing::{info, warn};

use super::{Fetch, config::WeatherConfig};

const GEOCODING_ORIGIN: &str = "https://geocoding-api.open-meteo.com";
const FORECAST_ORIGIN: &str = "https://api.open-meteo.com";
/// The language of the names Open-Meteo gives back (the country): the one of the words below.
const LANGUAGE: &str = "fr";
/// The longest place name, in characters.
const MAX_PLACE: usize = 100;
/// How many days are asked (and kept); the LLM sees as many as it asks for.
const FORECAST_DAYS: usize = 7;
const DEFAULT_DAYS: usize = 2;
/// How many places, and how many forecasts, are kept.
const KEPT: usize = 100;

const UNREACHABLE: &str = "La météo ne répond pas pour le moment.";
const UNREADABLE: &str = "La météo a répondu quelque chose d'incompréhensible.";
const NO_PLACE: &str = "Aucun lieu par défaut : demande à l'utilisateur pour quel lieu.";

/// The French words: the sky of each WMO code, the names of the days.
#[derive(Deserialize)]
struct Words {
    today: String,
    tomorrow: String,
    weekdays: Vec<String>,
    unknown: String,
    sky: HashMap<String, String>,
}

impl Words {
    fn shipped() -> Self {
        let words: Self = toml::from_str(include_str!("../../prompts/weather.fr.toml"))
            .expect("prompts/weather.fr.toml is valid");
        assert_eq!(words.weekdays.len(), 7, "seven days in a week");
        words
    }

    fn sky(&self, code: Option<i64>) -> &str {
        code.and_then(|c| self.sky.get(&c.to_string()))
            .unwrap_or(&self.unknown)
    }

    /// How `date` is said when `today` is today: « demain », then the name of the day.
    fn day(&self, today: NaiveDate, date: NaiveDate) -> String {
        match (date - today).num_days() {
            0 => self.today.clone(),
            1 => self.tomorrow.clone(),
            _ => self.weekdays[date.weekday().num_days_from_monday() as usize].clone(),
        }
    }
}

/// A place found: where it is, and how it is called.
#[derive(Clone, Debug)]
struct Place {
    name: Option<String>,
    country: Option<String>,
    latitude: f64,
    longitude: f64,
}

#[derive(Deserialize)]
struct GeocodingReply {
    #[serde(default)]
    results: Vec<Found>,
}

#[derive(Deserialize)]
struct Found {
    name: String,
    latitude: f64,
    longitude: f64,
    country: Option<String>,
}

#[derive(Deserialize)]
struct ForecastReply {
    current: Option<RawNow>,
    daily: Option<RawDaily>,
}

#[derive(Deserialize)]
struct RawNow {
    time: Option<String>,
    temperature_2m: Option<f64>,
    apparent_temperature: Option<f64>,
    precipitation: Option<f64>,
    weather_code: Option<i64>,
    wind_speed_10m: Option<f64>,
}

#[derive(Deserialize)]
struct RawDaily {
    time: Vec<String>,
    #[serde(default)]
    weather_code: Vec<Option<i64>>,
    #[serde(default)]
    temperature_2m_max: Vec<Option<f64>>,
    #[serde(default)]
    temperature_2m_min: Vec<Option<f64>>,
    #[serde(default)]
    precipitation_probability_max: Vec<Option<f64>>,
    #[serde(default)]
    precipitation_sum: Vec<Option<f64>>,
}

/// A forecast as kept: the days are named when it is told, from `today` (the date at the place).
struct Forecast {
    today: NaiveDate,
    now: Option<RawNow>,
    days: Vec<Day>,
}

struct Day {
    date: NaiveDate,
    code: Option<i64>,
    min: Option<f64>,
    max: Option<f64>,
    rain_chance: Option<f64>,
    rain_mm: Option<f64>,
}

#[derive(Default)]
struct Cache {
    /// Normalised name and place, the oldest first. A place does not move: no expiry.
    places: Vec<(String, Place)>,
    /// By coordinates (in ten-thousandths of a degree).
    forecasts: HashMap<(i64, i64), (Instant, Arc<Forecast>)>,
}

pub struct Weather {
    fetch: Arc<dyn Fetch>,
    /// The place of the owner: the name to find, or the place already found (coordinates given).
    default_name: String,
    default_place: Option<Place>,
    ttl: Duration,
    words: Words,
    geocoding_origin: String,
    forecast_origin: String,
    cache: Mutex<Cache>,
}

impl Weather {
    pub fn new(config: &WeatherConfig, fetch: Arc<dyn Fetch>) -> Self {
        let name = config.place.trim().to_owned();
        let default_place = config
            .latitude
            .zip(config.longitude)
            .map(|(latitude, longitude)| Place {
                name: (!name.is_empty()).then(|| name.clone()),
                country: None,
                latitude,
                longitude,
            });
        Self {
            fetch,
            default_name: name,
            default_place,
            ttl: Duration::from_secs(config.cache_minutes * 60),
            words: Words::shipped(),
            geocoding_origin: GEOCODING_ORIGIN.to_owned(),
            forecast_origin: FORECAST_ORIGIN.to_owned(),
            cache: Mutex::default(),
        }
    }

    /// Asks at other addresses (the geocoding, then the forecast), for the tests.
    pub fn at(mut self, geocoding: &str, forecast: &str) -> Self {
        self.geocoding_origin = geocoding.to_owned();
        self.forecast_origin = forecast.to_owned();
        self
    }

    /// Whether the owner's place is known: the description of the tool says so.
    pub fn has_default(&self) -> bool {
        self.default_place.is_some() || !self.default_name.is_empty()
    }

    /// The answer for the LLM (compact JSON), or what it must be told when there is none.
    /// `place`: the one asked, or the default; `days`: how many days (1 to 7, 2 when not said).
    pub async fn report(&self, arguments: &JsonObject) -> Result<String, String> {
        let asked = arguments
            .get("place")
            .and_then(Value::as_str)
            .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|p| !p.is_empty());
        let days = days(arguments.get("days"));
        let place = match &asked {
            Some(name) => self.geocode(name).await?,
            None => self.default_place().await?,
        };
        let forecast = self.forecast(&place).await?;
        Ok(self
            .tell(&place, asked.is_some(), &forecast, days)
            .to_string())
    }

    async fn default_place(&self) -> Result<Place, String> {
        match &self.default_place {
            Some(place) => Ok(place.clone()),
            None if self.default_name.is_empty() => Err(NO_PLACE.to_owned()),
            None => self.geocode(&self.default_name).await,
        }
    }

    async fn get(&self, url: Url) -> Result<Vec<u8>, String> {
        let fetched = self
            .fetch
            .get(url)
            .await
            .map_err(|_| UNREACHABLE.to_owned())?;
        if fetched.status != 200 {
            warn!(status = fetched.status, "web: weather, request refused");
            return Err(UNREACHABLE.to_owned());
        }
        Ok(fetched.body)
    }

    fn url(origin: &str, path: &str, query: &[(&str, &str)]) -> Result<Url, String> {
        let mut url = Url::parse(origin).map_err(|e| {
            warn!("web: weather, the address of the service is wrong: {e}");
            UNREACHABLE.to_owned()
        })?;
        url.set_path(path);
        url.query_pairs_mut().extend_pairs(query);
        Ok(url)
    }

    /// The place a name stands for: the best one for Open-Meteo, or an error when it has none.
    async fn geocode(&self, name: &str) -> Result<Place, String> {
        if name.chars().count() > MAX_PLACE {
            return Err("Le lieu est trop long pour une recherche.".to_owned());
        }
        let key = name.to_lowercase();
        if let Some((_, place)) = self
            .cache
            .lock()
            .unwrap()
            .places
            .iter()
            .find(|(k, _)| *k == key)
        {
            return Ok(place.clone());
        }
        let url = Self::url(
            &self.geocoding_origin,
            "/v1/search",
            &[
                ("name", name),
                ("count", "1"),
                ("language", LANGUAGE),
                ("format", "json"),
            ],
        )?;
        let body = self.get(url).await?;
        let reply: GeocodingReply = serde_json::from_slice(&body).map_err(|e| {
            warn!("web: weather, unreadable geocoding answer: {e}");
            UNREADABLE.to_owned()
        })?;
        let Some(found) = reply.results.into_iter().next() else {
            info!("web: weather, unknown place");
            return Err(format!("Lieu inconnu : « {name} »."));
        };
        let place = Place {
            name: Some(found.name),
            country: found.country,
            latitude: found.latitude,
            longitude: found.longitude,
        };
        let mut cache = self.cache.lock().unwrap();
        if cache.places.len() >= KEPT {
            cache.places.remove(0);
        }
        cache.places.push((key, place.clone()));
        Ok(place)
    }

    async fn forecast(&self, place: &Place) -> Result<Arc<Forecast>, String> {
        let key = (
            (place.latitude * 1e4).round() as i64,
            (place.longitude * 1e4).round() as i64,
        );
        if let Some((at, forecast)) = self.cache.lock().unwrap().forecasts.get(&key)
            && at.elapsed() < self.ttl
        {
            info!("web: weather, from the cache");
            return Ok(forecast.clone());
        }
        let url = Self::url(
            &self.forecast_origin,
            "/v1/forecast",
            &[
                ("latitude", &place.latitude.to_string()),
                ("longitude", &place.longitude.to_string()),
                (
                    "current",
                    "temperature_2m,apparent_temperature,precipitation,weather_code,wind_speed_10m",
                ),
                (
                    "daily",
                    "weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,precipitation_sum",
                ),
                ("timezone", "auto"),
                ("forecast_days", &FORECAST_DAYS.to_string()),
            ],
        )?;
        let body = self.get(url).await?;
        let reply: ForecastReply = serde_json::from_slice(&body).map_err(|e| {
            warn!("web: weather, unreadable forecast: {e}");
            UNREADABLE.to_owned()
        })?;
        let forecast = Arc::new(Forecast::from(reply).ok_or_else(|| {
            warn!("web: weather, the forecast has no day");
            UNREADABLE.to_owned()
        })?);
        let mut cache = self.cache.lock().unwrap();
        if cache.forecasts.len() >= KEPT {
            let ttl = self.ttl;
            cache.forecasts.retain(|_, (at, _)| at.elapsed() < ttl);
            if cache.forecasts.len() >= KEPT {
                cache.forecasts.clear();
            }
        }
        if !self.ttl.is_zero() {
            cache
                .forecasts
                .insert(key, (Instant::now(), forecast.clone()));
        }
        Ok(forecast)
    }

    /// The forecast as the LLM gets it: only what is known, in numbers that can be said.
    fn tell(&self, place: &Place, asked: bool, forecast: &Forecast, days: usize) -> Value {
        let mut answer = Map::new();
        if let Some(name) = &place.name {
            answer.insert("place".to_owned(), json!(name));
        }
        // The owner knows the country of their own place; a place they named may be another one
        // than the one meant (« Brest » is in Belarus too), and the LLM can say it.
        if let Some(country) = place.country.as_ref().filter(|_| asked) {
            answer.insert("country".to_owned(), json!(country));
        }
        if let Some(now) = &forecast.now {
            let mut o = Map::new();
            put(&mut o, "temperature", now.temperature_2m.map(whole));
            put(&mut o, "feels_like", now.apparent_temperature.map(whole));
            o.insert("sky".to_owned(), json!(self.words.sky(now.weather_code)));
            put(&mut o, "wind_kmh", now.wind_speed_10m.map(whole));
            put(
                &mut o,
                "rain_mm",
                now.precipitation.filter(|mm| *mm > 0.0).map(tenths),
            );
            answer.insert("now".to_owned(), Value::Object(o));
        }
        let days: Vec<Value> = forecast
            .days
            .iter()
            .take(days)
            .map(|d| {
                let mut o = Map::new();
                o.insert(
                    "day".to_owned(),
                    json!(self.words.day(forecast.today, d.date)),
                );
                o.insert("sky".to_owned(), json!(self.words.sky(d.code)));
                put(&mut o, "min", d.min.map(whole));
                put(&mut o, "max", d.max.map(whole));
                put(&mut o, "rain_chance", d.rain_chance.map(whole));
                // Under a millimetre, « pluie faible » says it all.
                put(
                    &mut o,
                    "rain_mm",
                    d.rain_mm.filter(|mm| *mm >= 1.0).map(tenths),
                );
                Value::Object(o)
            })
            .collect();
        answer.insert("days".to_owned(), Value::Array(days));
        Value::Object(answer)
    }
}

impl Forecast {
    fn from(reply: ForecastReply) -> Option<Self> {
        let raw = reply.daily?;
        let at = |i: usize, v: &[Option<f64>]| v.get(i).copied().flatten();
        let days: Vec<Day> = raw
            .time
            .iter()
            .enumerate()
            .filter_map(|(i, date)| {
                Some(Day {
                    date: NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?,
                    code: raw.weather_code.get(i).copied().flatten(),
                    min: at(i, &raw.temperature_2m_min),
                    max: at(i, &raw.temperature_2m_max),
                    rain_chance: at(i, &raw.precipitation_probability_max),
                    rain_mm: at(i, &raw.precipitation_sum),
                })
            })
            .collect();
        // « Today » is the date at the place, from the moment of the observation.
        let today = reply
            .current
            .as_ref()
            .and_then(|c| c.time.as_deref())
            .and_then(|t| t.get(..10))
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            .or_else(|| days.first().map(|d| d.date))?;
        Some(Self {
            today,
            now: reply.current,
            days,
        })
    }
}

/// How many days the LLM asked for (a number, or a number written as text), 1 to 7.
fn days(value: Option<&Value>) -> usize {
    let asked = match value {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    };
    asked.filter(|d| d.is_finite()).map_or(DEFAULT_DAYS, |d| {
        d.round().clamp(1.0, FORECAST_DAYS as f64) as usize
    })
}

fn put(object: &mut Map<String, Value>, key: &str, value: Option<impl Into<Value>>) {
    if let Some(value) = value {
        object.insert(key.to_owned(), value.into());
    }
}

/// To the nearest whole number: a degree, a kilometre an hour, a percent.
fn whole(value: f64) -> i64 {
    value.round() as i64
}

/// To a tenth: a millimetre of rain.
fn tenths(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::{FetchError, Fetched};
    use futures_util::future::BoxFuture;

    const GEOCODE_BREST: &str = include_str!("../../tests/fixtures/web/weather-geocode-brest.json");
    const GEOCODE_UNKNOWN: &str =
        include_str!("../../tests/fixtures/web/weather-geocode-unknown.json");
    const FORECAST_BREST: &str =
        include_str!("../../tests/fixtures/web/weather-forecast-brest.json");

    /// Recorded answers, by host, path and the first parameter (`name` or `latitude`); whatever
    /// else is asked is unreachable. What was asked is kept.
    #[derive(Default)]
    struct Recorded {
        answers: Mutex<HashMap<String, (u16, String)>>,
        asked: Mutex<Vec<Url>>,
    }

    impl Recorded {
        fn key(url: &Url) -> String {
            let first = url.query_pairs().next().map(|(_, v)| v.into_owned());
            format!(
                "{}{}|{}",
                url.host_str().unwrap_or_default(),
                url.path(),
                first.unwrap_or_default()
            )
        }

        fn on(self: &Arc<Self>, key: &str, status: u16, body: &str) -> &Arc<Self> {
            self.answers
                .lock()
                .unwrap()
                .insert(key.to_owned(), (status, body.to_owned()));
            self
        }

        fn asked(&self) -> Vec<Url> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl Fetch for Recorded {
        fn get(&self, url: Url) -> BoxFuture<'_, Result<Fetched, FetchError>> {
            let answer = self.answers.lock().unwrap().get(&Self::key(&url)).cloned();
            self.asked.lock().unwrap().push(url);
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

    const GEOCODING: &str = "geocoding-api.open-meteo.com/v1/search";
    const FORECAST: &str = "api.open-meteo.com/v1/forecast";

    /// The recorded answers for Brest (France), asked as « Brest, France » or by its coordinates.
    fn brest() -> Arc<Recorded> {
        let recorded = Arc::new(Recorded::default());
        recorded
            .on(&format!("{GEOCODING}|Brest, France"), 200, GEOCODE_BREST)
            .on(&format!("{GEOCODING}|zzqxvkj"), 200, GEOCODE_UNKNOWN)
            .on(&format!("{FORECAST}|48.39029"), 200, FORECAST_BREST);
        recorded
    }

    fn weather(recorded: &Arc<Recorded>, toml: &str) -> Weather {
        let config: WeatherConfig = toml::from_str(toml).unwrap();
        Weather::new(&config, recorded.clone())
    }

    fn arguments(value: Value) -> JsonObject {
        value.as_object().cloned().unwrap()
    }

    fn told(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn the_shipped_words_cover_every_code_of_the_wmo_table() {
        let words = Words::shipped();
        for code in [
            0, 1, 2, 3, 45, 48, 51, 53, 55, 56, 57, 61, 63, 65, 66, 67, 71, 73, 75, 77, 80, 81, 82,
            85, 86, 95, 96, 99,
        ] {
            assert_ne!(words.sky(Some(code)), words.unknown, "code {code}");
        }
        assert_eq!(words.sky(Some(0)), "ciel dégagé");
        assert_eq!(words.sky(Some(3)), "couvert");
        assert_eq!(words.sky(Some(61)), "pluie faible");
        assert_eq!(words.sky(Some(95)), "orage");
        // A code the table does not know, or none at all.
        assert_eq!(words.sky(Some(42)), "temps indéterminé");
        assert_eq!(words.sky(None), "temps indéterminé");
        // The words are for speaking: no digit, no code.
        assert!(words.sky.values().all(|w| !w.contains(char::is_numeric)));
    }

    #[test]
    fn days_are_named_in_french() {
        let words = Words::shipped();
        // 2026-10-13 is a Tuesday.
        let today = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
        let names: Vec<String> = (0..7)
            .map(|i| words.day(today, today + chrono::Days::new(i)))
            .collect();
        assert_eq!(
            names,
            [
                "aujourd'hui",
                "demain",
                "jeudi",
                "vendredi",
                "samedi",
                "dimanche",
                "lundi"
            ]
        );
        // The week wraps over a month and a year.
        let today = NaiveDate::from_ymd_opt(2026, 12, 30).unwrap();
        assert_eq!(words.day(today, today + chrono::Days::new(2)), "vendredi");
        assert_eq!(words.day(today, today + chrono::Days::new(6)), "mardi");
    }

    #[test]
    fn the_number_of_days_is_read_loosely_and_kept_between_one_and_seven() {
        for (value, expected) in [
            (None, 2),
            (Some(json!(3)), 3),
            (Some(json!("4")), 4),
            (Some(json!(2.6)), 3),
            (Some(json!(0)), 1),
            (Some(json!(-5)), 1),
            (Some(json!(15)), 7),
            (Some(json!("beaucoup")), 2),
            (Some(json!(null)), 2),
            (Some(json!(true)), 2),
        ] {
            assert_eq!(days(value.as_ref()), expected, "{value:?}");
        }
    }

    #[tokio::test]
    async fn the_forecast_of_the_default_place_is_told_for_the_voice() {
        let recorded = brest();
        let w = weather(&recorded, "place = \"Brest, France\"");
        let text = w.report(&arguments(json!({}))).await.unwrap();
        let a = told(&text);
        assert_eq!(a["place"], "Brest");
        // The owner's own place: no country to say.
        assert!(a.get("country").is_none());
        assert_eq!(
            a["now"],
            json!({"temperature": 16, "feels_like": 16, "sky": "ciel dégagé", "wind_kmh": 4})
        );
        // Two days when not said: today, tomorrow. Whole degrees; rain under 1 mm is not given.
        assert_eq!(
            a["days"],
            json!([
                {"day": "aujourd'hui", "sky": "bruine faible", "min": 14, "max": 20, "rain_chance": 80},
                {"day": "demain", "sky": "couvert", "min": 12, "max": 19, "rain_chance": 3},
            ])
        );
        // Compact, with no unit nor identifier to read out.
        assert!(!text.contains('\n') && text.len() < 400, "{text}");

        // The requests: the name asked as written, then the coordinates found.
        let asked = recorded.asked();
        assert_eq!(asked.len(), 2);
        let query = |u: &Url| -> Vec<(String, String)> {
            u.query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        };
        let geocoding = query(&asked[0]);
        assert!(geocoding.contains(&("name".into(), "Brest, France".into())));
        assert!(geocoding.contains(&("count".into(), "1".into())));
        assert!(geocoding.contains(&("language".into(), "fr".into())));
        let forecast = query(&asked[1]);
        assert!(forecast.contains(&("latitude".into(), "48.39029".into())));
        assert!(forecast.contains(&("longitude".into(), "-4.48628".into())));
        assert!(forecast.contains(&("timezone".into(), "auto".into())));
        let current = &forecast.iter().find(|(k, _)| k == "current").unwrap().1;
        assert!(current.contains("apparent_temperature") && current.contains("weather_code"));
    }

    #[tokio::test]
    async fn the_days_after_tomorrow_have_their_name_and_a_heavy_rain_its_millimetres() {
        let recorded = brest();
        let w = weather(&recorded, "place = \"Brest, France\"");
        let a = told(&w.report(&arguments(json!({"days": 7}))).await.unwrap());
        let days = a["days"].as_array().unwrap();
        assert_eq!(days.len(), 7);
        let names: Vec<&str> = days.iter().map(|d| d["day"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "aujourd'hui",
                "demain",
                "jeudi",
                "vendredi",
                "samedi",
                "dimanche",
                "lundi"
            ]
        );
        // Friday 1.9 mm; Monday 9.7 mm (« bruine forte ») and 16 %.
        assert_eq!(days[3]["rain_mm"], 1.9);
        assert_eq!(days[3]["sky"], "bruine");
        assert_eq!(days[6]["rain_mm"], 9.7);
        assert_eq!(days[6]["sky"], "bruine forte");
        assert_eq!(days[6]["rain_chance"], 16);
        assert!(days[1].get("rain_mm").is_none());
        // One day: only today.
        let a = told(&w.report(&arguments(json!({"days": 1}))).await.unwrap());
        assert_eq!(a["days"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_place_asked_is_found_and_its_country_given() {
        let recorded = brest();
        let w = weather(&recorded, "");
        let a = told(
            &w.report(&arguments(json!({"place": "  Brest,   France "})))
                .await
                .unwrap(),
        );
        assert_eq!(a["place"], "Brest");
        assert_eq!(a["country"], "France");
        assert_eq!(
            recorded.asked()[0].query_pairs().next().unwrap().1,
            "Brest, France"
        );
    }

    #[tokio::test]
    async fn an_unknown_place_is_said_so() {
        let recorded = brest();
        let w = weather(&recorded, "place = \"Brest, France\"");
        let e = w
            .report(&arguments(json!({"place": "zzqxvkj"})))
            .await
            .unwrap_err();
        assert_eq!(e, "Lieu inconnu : « zzqxvkj ».");
        // The forecast was not asked for nowhere.
        assert_eq!(recorded.asked().len(), 1);
        let e = w
            .report(&arguments(json!({"place": "x".repeat(101)})))
            .await
            .unwrap_err();
        assert!(e.contains("trop long"), "{e}");
        assert_eq!(recorded.asked().len(), 1);
    }

    #[tokio::test]
    async fn without_a_default_place_the_llm_is_told_to_ask() {
        let recorded = brest();
        let w = weather(&recorded, "");
        assert!(!w.has_default());
        let e = w.report(&arguments(json!({}))).await.unwrap_err();
        assert!(e.contains("Aucun lieu par défaut"), "{e}");
        assert!(recorded.asked().is_empty());
    }

    #[tokio::test]
    async fn coordinates_in_the_configuration_save_the_geocoding() {
        let recorded = brest();
        let w = weather(
            &recorded,
            "place = \"Chez moi\"\nlatitude = 48.39029\nlongitude = -4.48628",
        );
        assert!(w.has_default());
        let a = told(&w.report(&arguments(json!({}))).await.unwrap());
        assert_eq!(a["place"], "Chez moi");
        let asked = recorded.asked();
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert_eq!(asked[0].host_str(), Some("api.open-meteo.com"));
        // No name: no place in the answer, still a forecast.
        let recorded = brest();
        let w = weather(&recorded, "latitude = 48.39029\nlongitude = -4.48628");
        let a = told(&w.report(&arguments(json!({}))).await.unwrap());
        assert!(a.get("place").is_none() && a["days"].as_array().unwrap().len() == 2);
    }

    #[tokio::test]
    async fn a_place_and_a_forecast_are_kept_and_the_forecast_expires() {
        let recorded = brest();
        let w = weather(&recorded, "place = \"Brest, France\"");
        for _ in 0..3 {
            w.report(&arguments(json!({}))).await.unwrap();
        }
        // The same name written otherwise (the case): the place is kept, so is its forecast.
        w.report(&arguments(json!({"place": "BREST,  france"})))
            .await
            .unwrap();
        let hosts: Vec<_> = recorded
            .asked()
            .iter()
            .map(|u| u.host_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            hosts,
            ["geocoding-api.open-meteo.com", "api.open-meteo.com"]
        );

        // Another number of days is the same forecast.
        let recorded = brest();
        let w = weather(&recorded, "place = \"Brest, France\"");
        w.report(&arguments(json!({"days": 2}))).await.unwrap();
        w.report(&arguments(json!({"days": 5}))).await.unwrap();
        assert_eq!(recorded.asked().len(), 2);

        // No cache: the forecast is asked every time, the place only once.
        let recorded = brest();
        let w = weather(&recorded, "place = \"Brest, France\"\ncache_minutes = 0");
        w.report(&arguments(json!({}))).await.unwrap();
        w.report(&arguments(json!({}))).await.unwrap();
        assert_eq!(recorded.asked().len(), 3);
    }

    #[tokio::test]
    async fn a_service_that_fails_is_said_simply() {
        // Nothing answers.
        let recorded = Arc::new(Recorded::default());
        let w = weather(&recorded, "place = \"Brest, France\"");
        let e = w.report(&arguments(json!({}))).await.unwrap_err();
        assert_eq!(e, "La météo ne répond pas pour le moment.");

        // A refusal (a code other than 200) at the geocoding, then at the forecast.
        let recorded = brest();
        recorded.on(&format!("{GEOCODING}|Brest, France"), 429, "{}");
        let w = weather(&recorded, "place = \"Brest, France\"");
        let e = w.report(&arguments(json!({}))).await.unwrap_err();
        assert_eq!(e, "La météo ne répond pas pour le moment.");
        let recorded = brest();
        recorded.on(
            &format!("{FORECAST}|48.39029"),
            400,
            r#"{"error":true,"reason":"Latitude must be in range of -90 to 90°."}"#,
        );
        let w = weather(&recorded, "place = \"Brest, France\"");
        let e = w.report(&arguments(json!({}))).await.unwrap_err();
        assert_eq!(e, "La météo ne répond pas pour le moment.");

        // An answer that is not what was expected.
        let recorded = brest();
        recorded.on(
            &format!("{FORECAST}|48.39029"),
            200,
            "<html>maintenance</html>",
        );
        let w = weather(&recorded, "place = \"Brest, France\"");
        let e = w.report(&arguments(json!({}))).await.unwrap_err();
        assert!(e.contains("incompréhensible"), "{e}");
        let recorded = brest();
        recorded.on(
            &format!("{FORECAST}|48.39029"),
            200,
            r#"{"daily":{"time":[]}}"#,
        );
        let w = weather(&recorded, "place = \"Brest, France\"");
        let e = w.report(&arguments(json!({}))).await.unwrap_err();
        assert!(e.contains("incompréhensible"), "{e}");
    }

    #[tokio::test]
    async fn missing_values_are_left_out_rather_than_made_up() {
        let recorded = brest();
        recorded.on(
            &format!("{FORECAST}|48.39029"),
            200,
            r#"{"current":{"time":"2026-10-13T20:45","temperature_2m":-0.4,"weather_code":71},
                "daily":{"time":["2026-10-13","2026-10-14"],"weather_code":[73,null],
                "temperature_2m_max":[1.2,null],"temperature_2m_min":[-3.5,-4.0],
                "precipitation_probability_max":[null,40],"precipitation_sum":[0.0,4.04]}}"#,
        );
        let w = weather(&recorded, "place = \"Brest, France\"");
        let a = told(&w.report(&arguments(json!({}))).await.unwrap());
        // -0.4 is zero degrees, not minus zero.
        assert_eq!(a["now"], json!({"temperature": 0, "sky": "neige faible"}));
        assert_eq!(
            a["days"],
            json!([
                {"day": "aujourd'hui", "sky": "neige", "min": -4, "max": 1},
                {"day": "demain", "sky": "temps indéterminé", "min": -4, "rain_chance": 40, "rain_mm": 4.0},
            ])
        );
    }

    #[tokio::test]
    async fn today_is_the_date_at_the_place_not_the_one_of_this_machine() {
        // The recorded forecast says it is the 13th at the place: the names follow it.
        let recorded = brest();
        let w = weather(&recorded, "place = \"Brest, France\"");
        let a = told(&w.report(&arguments(json!({"days": 3}))).await.unwrap());
        assert_eq!(a["days"][2]["day"], "jeudi");
    }
}
