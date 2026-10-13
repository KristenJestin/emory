//! The time: a module of Emory's MCP server (`time_<tool>`) for the timers and reminders the
//! owner asks for (`specs/time.md`). In this version they are kept and listed, and they survive a
//! restart; nothing rings yet (the ringing is the next unit, once the mind can speak by itself).
//!
//! Every answer says the durations and the due times in words, because the LLM repeats them to a
//! voice. The logs say the id, the kind, the due time and the device, never the text or the label.

pub mod config;
pub mod store;
pub mod words;
pub mod zone;

use std::{
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use futures_util::future::BoxFuture;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool};
use serde_json::{Value, json};
use tracing::{info, warn};

pub use config::{DataConfig, TimeConfig};
use store::{Item, Kind, NewItem, Store};
use zone::Zone;

use crate::{conversations::ConversationKey, conversations::Conversations, tools::Module};

/// The longest timer: a day.
const MAX_TIMER_SECONDS: u64 = 86_400;
/// The farthest a reminder may be: a year.
const MAX_REMINDER_SECONDS: u64 = 365 * 86_400;
/// Characters: what a spoken label and a spoken reminder can reasonably be.
const MAX_LABEL_CHARS: usize = 40;
const MAX_TEXT_CHARS: usize = 200;
/// The form of `due` in the list, and of `at` in a reminder.
const LOCAL_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";

/// The current time. The tests set their own.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> DateTime<Utc>;
}

/// The clock of the machine.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock that is moved by hand: for the tests.
pub struct ManualClock(Mutex<DateTime<Utc>>);

impl ManualClock {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self(Mutex::new(now))
    }

    pub fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

/// The `time` module: the four tools.
pub struct Time {
    store: Mutex<Store>,
    zone: Zone,
    clock: Arc<dyn Clock>,
    conversations: Conversations,
    max_active: usize,
    default_device: Option<String>,
}

impl Time {
    /// The module of this configuration, or `None` when it is off. Reads the file of the timers:
    /// a file that cannot be read is put aside (see [`Store::open`]); only a folder that cannot be
    /// made, or an unknown zone, stops `mind`.
    pub fn new(
        config: &TimeConfig,
        data: &DataConfig,
        conversations: Conversations,
    ) -> Result<Option<Self>> {
        if !config.enabled {
            return Ok(None);
        }
        let path = config.store_path(data)?;
        Self::with_clock(config, &path, Arc::new(SystemClock), conversations).map(Some)
    }

    /// The same, keeping the timers in `path` and reading the time from `clock`.
    pub fn with_clock(
        config: &TimeConfig,
        path: &Path,
        clock: Arc<dyn Clock>,
        conversations: Conversations,
    ) -> Result<Self> {
        let zone = Zone::new(&config.timezone).context("time.timezone")?;
        let now = clock.now();
        let opened = Store::open(path, now)
            .with_context(|| format!("opening the timers file {}", path.display()))?;
        let mut store = opened.store;
        let limit = Duration::hours(config.late_limit_hours as i64);
        let late = store
            .discard_late(now, limit)
            .with_context(|| format!("writing the timers file {}", path.display()))?;
        for item in &late {
            warn!(
                id = item.id,
                kind = ?item.kind,
                due = %item.due,
                device = ?item.device,
                "timer or reminder missed during a long stop: dropped"
            );
        }
        info!(
            items = store.items().len(),
            dropped = late.len(),
            zone = %zone_name(config),
            "timers read"
        );
        Ok(Self {
            store: Mutex::new(store),
            zone,
            clock,
            conversations,
            max_active: config.max_active,
            default_device: config.default_device.clone(),
        })
    }

    fn store(&self) -> MutexGuard<'_, Store> {
        self.store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The device that a timer set in this conversation belongs to.
    fn device_of(&self, conversation: Option<&ConversationKey>) -> Option<String> {
        conversation
            .and_then(|key| self.conversations.device(key))
            .map(|device| device.as_str().to_owned())
            .or_else(|| self.default_device.clone())
    }

    /// How many items are still waiting at `now`.
    fn active(store: &Store, now: DateTime<Utc>) -> usize {
        store
            .items()
            .iter()
            .filter(|i| i.due.with_timezone(&Utc) > now)
            .count()
    }

    fn timer(
        &self,
        arguments: &JsonObject,
        conversation: Option<&ConversationKey>,
    ) -> Result<String, String> {
        let seconds = integer(arguments, "seconds")?;
        if !(1..=MAX_TIMER_SECONDS).contains(&seconds) {
            return Err(format!(
                "La durée d'un minuteur va d'une seconde à vingt-quatre heures, pas {seconds} secondes."
            ));
        }
        let label = optional_text(arguments, "label", MAX_LABEL_CHARS)?;
        let now = self.clock.now();
        let due = now + Duration::seconds(seconds as i64);
        let (created, due) = (self.zone.at(now), self.zone.at(due));
        let item = self.keep(
            now,
            NewItem {
                kind: Kind::Timer,
                label: label.clone(),
                text: None,
                seconds: Some(seconds),
                created,
                due,
                device: self.device_of(conversation),
            },
        )?;
        Ok(format!(
            "Minuteur {}{} : {}, échéance {}.",
            item.id,
            label.map(|l| format!(" ({l})")).unwrap_or_default(),
            words::duration(seconds),
            words::moment(created, due)
        ))
    }

    fn reminder(
        &self,
        arguments: &JsonObject,
        conversation: Option<&ConversationKey>,
    ) -> Result<String, String> {
        let text = optional_text(arguments, "text", MAX_TEXT_CHARS)?
            .ok_or_else(|| "« text » manque.".to_owned())?;
        let now = self.clock.now();
        let here = self.zone.at(now);
        let (due, skipped) = match (arguments.get("at"), arguments.get("in_seconds")) {
            (Some(_), Some(_)) | (None, None) => {
                return Err("Donne soit « at », soit « in_seconds », pas les deux.".to_owned());
            }
            (Some(at), None) => {
                let at = at.as_str().unwrap_or_default().trim();
                let resolved = self.parse_local(at).ok_or_else(|| {
                    "« at » doit être une heure locale au format AAAA-MM-JJTHH:MM.".to_owned()
                })?;
                (resolved.at, resolved.skipped)
            }
            (None, Some(_)) => {
                let seconds = integer(arguments, "in_seconds")?;
                if !(1..=MAX_REMINDER_SECONDS).contains(&seconds) {
                    return Err(
                        "« in_seconds » va d'une seconde à un an (31 536 000 secondes).".to_owned(),
                    );
                }
                (self.zone.at(now + Duration::seconds(seconds as i64)), false)
            }
        };
        if due <= here {
            return Err(format!(
                "Cette heure est passée : il est {}.",
                here.format(LOCAL_FORMAT)
            ));
        }
        if due - here > Duration::seconds(MAX_REMINDER_SECONDS as i64) {
            return Err("Un rappel ne peut pas être à plus d'un an.".to_owned());
        }
        let item = self.keep(
            now,
            NewItem {
                kind: Kind::Reminder,
                label: None,
                text: Some(text),
                seconds: None,
                created: here,
                due,
                device: self.device_of(conversation),
            },
        )?;
        let note = if skipped {
            " Cette heure n'existe pas ce jour-là (changement d'heure) : le rappel est décalé d'autant."
        } else {
            ""
        };
        Ok(format!(
            "Rappel {} : échéance {}.{note}",
            item.id,
            words::moment(here, due)
        ))
    }

    /// `AAAA-MM-JJTHH:MM` (seconds allowed) in the zone of the module; an instant with its own
    /// offset is taken as it is.
    fn parse_local(&self, at: &str) -> Option<zone::Resolved> {
        if let Ok(instant) = DateTime::parse_from_rfc3339(at) {
            return Some(zone::Resolved {
                at: self.zone.local(instant),
                skipped: false,
            });
        }
        ["%Y-%m-%dT%H:%M", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M"]
            .iter()
            .find_map(|format| NaiveDateTime::parse_from_str(at, format).ok())
            .and_then(|local| self.zone.resolve(local))
    }

    /// Adds the item if there is room, and says why not otherwise.
    fn keep(&self, now: DateTime<Utc>, new: NewItem) -> Result<Item, String> {
        let mut store = self.store();
        if Self::active(&store, now) >= self.max_active {
            return Err(format!(
                "Il y a déjà {} minuteurs et rappels en cours : c'est le maximum.",
                self.max_active
            ));
        }
        let item = store.add(new).map_err(|e| {
            warn!(error = ?e.kind(), "timers: the file could not be written");
            "Je n'ai pas pu garder ça : le fichier des minuteurs ne s'écrit pas.".to_owned()
        })?;
        info!(
            id = item.id,
            kind = ?item.kind,
            due = %item.due,
            device = ?item.device,
            "timer or reminder set"
        );
        Ok(item)
    }

    fn list(&self) -> String {
        let now = self.clock.now();
        let here = self.zone.at(now);
        let mut items: Vec<Item> = self
            .store()
            .items()
            .iter()
            .filter(|i| i.due.with_timezone(&Utc) > now)
            .cloned()
            .collect();
        if items.is_empty() {
            return "Aucun minuteur ni rappel en cours.".to_owned();
        }
        items.sort_by_key(|i| (i.due, i.id));
        let items: Vec<Value> = items
            .iter()
            .map(|item| {
                let due = self.zone.local(item.due);
                let mut entry = json!({
                    "id": item.id,
                    "kind": item.kind,
                    "due": due.format(LOCAL_FORMAT).to_string(),
                    "remaining": words::remaining(here, due),
                });
                let map = entry.as_object_mut().expect("an object");
                if let Some(label) = &item.label {
                    map.insert("label".into(), json!(label));
                }
                if let Some(text) = &item.text {
                    map.insert("text".into(), json!(text));
                }
                if let Some(device) = &item.device {
                    map.insert("device".into(), json!(device));
                }
                entry
            })
            .collect();
        json!({ "items": items }).to_string()
    }

    fn cancel(&self, arguments: &JsonObject) -> Result<String, String> {
        let id = integer(arguments, "id")?;
        let removed = self.store().remove(id).map_err(|e| {
            warn!(error = ?e.kind(), "timers: the file could not be written");
            "Je n'ai pas pu annuler : le fichier des minuteurs ne s'écrit pas.".to_owned()
        })?;
        let Some(item) = removed else {
            return Err(format!(
                "Il n'y a ni minuteur ni rappel numéro {id} : time_list donne les numéros."
            ));
        };
        info!(id = item.id, kind = ?item.kind, "timer or reminder cancelled");
        Ok(match item.kind {
            Kind::Timer => format!(
                "Minuteur {}{} annulé.",
                item.id,
                item.label.map(|l| format!(" ({l})")).unwrap_or_default()
            ),
            Kind::Reminder => format!("Rappel {} annulé.", item.id),
        })
    }
}

fn zone_name(config: &TimeConfig) -> &str {
    if config.timezone.trim().is_empty() {
        "system"
    } else {
        config.timezone.trim()
    }
}

/// A whole number argument; a float with no fraction is one too (some models write `600.0`).
fn integer(arguments: &JsonObject, name: &str) -> Result<u64, String> {
    match arguments.get(name) {
        None | Some(Value::Null) => Err(format!("« {name} » manque.")),
        Some(Value::Number(n)) => n
            .as_u64()
            .or_else(|| {
                n.as_f64()
                    .filter(|f| f.fract() == 0.0 && *f >= 0.0 && *f < 1e15)
                    .map(|f| f as u64)
            })
            .ok_or_else(|| format!("« {name} » doit être un nombre entier positif.")),
        Some(_) => Err(format!("« {name} » doit être un nombre entier positif.")),
    }
}

/// A text argument on one line, at most `max` characters; `None` if blank or absent.
fn optional_text(arguments: &JsonObject, name: &str, max: usize) -> Result<Option<String>, String> {
    let Some(value) = arguments.get(name).filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let text = value
        .as_str()
        .map(|v| v.split_whitespace().collect::<Vec<_>>().join(" "))
        .ok_or_else(|| format!("« {name} » doit être un texte."))?;
    if text.is_empty() {
        return Ok(None);
    }
    if text.chars().count() > max {
        return Err(format!(
            "« {name} » est trop long : {max} caractères au plus."
        ));
    }
    Ok(Some(text))
}

fn schema(value: Value) -> Arc<JsonObject> {
    match value {
        Value::Object(map) => Arc::new(map),
        _ => unreachable!(),
    }
}

impl Module for Time {
    fn name(&self) -> &'static str {
        "time"
    }

    // Short: the descriptions go with every request.
    fn tools(&self) -> BoxFuture<'_, Vec<Tool>> {
        Box::pin(async {
            vec![
                Tool::new(
                    "timer",
                    "Lance un minuteur.",
                    schema(json!({
                        "type": "object",
                        "properties": {
                            "seconds": {
                                "type": "integer",
                                "minimum": 1,
                                "maximum": MAX_TIMER_SECONDS,
                                "description": "la durée en secondes (dix minutes : 600)"
                            },
                            "label": {
                                "type": "string",
                                "maxLength": MAX_LABEL_CHARS,
                                "description": "à quoi il sert, si la personne l'a dit (« pâtes »)"
                            }
                        },
                        "required": ["seconds"]
                    })),
                ),
                Tool::new(
                    "reminder",
                    "Un rappel à une heure, ou dans un délai. Le texte, dit à la personne, à la deuxième personne.",
                    schema(json!({
                        "type": "object",
                        "properties": {
                            "text": {
                                "type": "string",
                                "maxLength": MAX_TEXT_CHARS,
                                "description": "ce qu'il faut rappeler (« appeler le garage »)"
                            },
                            "at": {
                                "type": "string",
                                "description": "l'heure locale, AAAA-MM-JJTHH:MM, calculée depuis [Maintenant]"
                            },
                            "in_seconds": {
                                "type": "integer",
                                "minimum": 1,
                                "maximum": MAX_REMINDER_SECONDS,
                                "description": "ou le délai en secondes (dans une heure : 3600), sans at"
                            }
                        },
                        "required": ["text"]
                    })),
                ),
                Tool::new(
                    "list",
                    "Les minuteurs et rappels en cours, avec le temps qui reste.",
                    schema(json!({"type": "object", "properties": {}})),
                ),
                Tool::new(
                    "cancel",
                    "Annule un minuteur ou un rappel de time_list.",
                    schema(json!({
                        "type": "object",
                        "properties": {
                            "id": {"type": "integer", "description": "son numéro dans time_list"}
                        },
                        "required": ["id"]
                    })),
                ),
            ]
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
                "timer" => self.timer(&arguments, conversation.as_ref()),
                "reminder" => self.reminder(&arguments, conversation.as_ref()),
                "list" => Ok(self.list()),
                "cancel" => self.cancel(&arguments),
                _ => Err(format!("Outil inconnu : time_{tool}")),
            };
            match outcome {
                Ok(answer) => CallToolResult::success(vec![ContentBlock::text(answer)]),
                Err(e) => CallToolResult::error(vec![ContentBlock::text(e)]),
            }
        })
    }
}

/// The text of a result, for the tests.
#[cfg(test)]
fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARIS: &str = "Europe/Paris";

    struct Bench {
        time: Time,
        clock: Arc<ManualClock>,
        dir: tempfile::TempDir,
        config: TimeConfig,
        conversations: Conversations,
    }

    impl Bench {
        /// Starts at `now` (an instant, as the clock reads it), in Paris.
        fn at(now: &str) -> Self {
            Self::with(now, TimeConfig::default())
        }

        fn with(now: &str, mut config: TimeConfig) -> Self {
            config.timezone = PARIS.to_owned();
            let dir = tempfile::tempdir().unwrap();
            let clock = Arc::new(ManualClock::new(now.parse().unwrap()));
            let conversations = Conversations::default();
            let time = Time::with_clock(
                &config,
                &dir.path().join("timers.json"),
                clock.clone(),
                conversations.clone(),
            )
            .unwrap();
            Self {
                time,
                clock,
                dir,
                config,
                conversations,
            }
        }

        /// `mind` stops and starts again, at the instant the clock now reads.
        fn restart(&mut self) {
            self.time = Time::with_clock(
                &self.config,
                &self.dir.path().join("timers.json"),
                self.clock.clone(),
                self.conversations.clone(),
            )
            .unwrap();
        }

        async fn call(&self, tool: &str, arguments: Value) -> (bool, String) {
            let result = self
                .time
                .call_in(tool, arguments.as_object().cloned(), None)
                .await;
            (result.is_error != Some(true), text_of(&result))
        }

        async fn ok(&self, tool: &str, arguments: Value) -> String {
            let (ok, text) = self.call(tool, arguments).await;
            assert!(ok, "{tool}: {text}");
            text
        }

        async fn refused(&self, tool: &str, arguments: Value) -> String {
            let (ok, text) = self.call(tool, arguments).await;
            assert!(!ok, "{tool} should have been refused: {text}");
            text
        }

        async fn list(&self) -> Vec<Value> {
            let text = self.ok("list", json!({})).await;
            match serde_json::from_str::<Value>(&text) {
                Ok(json) => json["items"].as_array().unwrap().clone(),
                Err(_) => Vec::new(),
            }
        }
    }

    // 2026-10-13 is a Tuesday; Paris is at +02:00, so 17:00Z is 19:00.
    const EVENING: &str = "2026-10-13T17:00:00Z";

    #[tokio::test]
    async fn four_tools_under_the_name_time() {
        let bench = Bench::at(EVENING);
        let tools = tools_of(&bench.time).await;
        assert_eq!(bench.time.name(), "time");
        assert_eq!(tools, ["timer", "reminder", "list", "cancel"]);
        // As the LLM sees them, prefixed by the server.
        let all = crate::tools::Tools::new(vec![Arc::new(bench.time)]);
        let names: Vec<String> = all
            .list()
            .await
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        assert_eq!(
            names,
            ["time_timer", "time_reminder", "time_list", "time_cancel"]
        );
    }

    async fn tools_of(time: &Time) -> Vec<String> {
        time.tools()
            .await
            .iter()
            .map(|t| t.name.to_string())
            .collect()
    }

    #[tokio::test]
    async fn a_timer_is_set_and_said_in_words() {
        let bench = Bench::at(EVENING);
        let said = bench
            .ok("timer", json!({"seconds": 600, "label": "pâtes"}))
            .await;
        assert_eq!(
            said,
            "Minuteur 1 (pâtes) : dix minutes, échéance à dix-neuf heures dix."
        );
        let said = bench.ok("timer", json!({"seconds": 90})).await;
        assert_eq!(
            said,
            "Minuteur 2 : une minute et trente secondes, échéance à dix-neuf heures une."
        );
        // A float with no fraction, as some models write it.
        bench.ok("timer", json!({"seconds": 60.0})).await;
    }

    #[tokio::test]
    async fn the_list_gives_what_is_left_in_words_soonest_first() {
        let bench = Bench::at(EVENING);
        bench
            .ok("timer", json!({"seconds": 3600, "label": "linge"}))
            .await;
        bench
            .ok("timer", json!({"seconds": 600, "label": "pâtes"}))
            .await;
        bench
            .ok(
                "reminder",
                json!({"text": "appeler le garage", "at": "2026-10-14T09:00"}),
            )
            .await;
        bench.clock.advance(Duration::seconds(108));
        assert_eq!(
            bench.list().await,
            [
                json!({"id": 2, "kind": "timer", "label": "pâtes", "due": "2026-10-13T19:10:00",
                       "remaining": "huit minutes et douze secondes"}),
                json!({"id": 1, "kind": "timer", "label": "linge", "due": "2026-10-13T20:00:00",
                       "remaining": "cinquante-huit minutes"}),
                json!({"id": 3, "kind": "reminder", "text": "appeler le garage",
                       "due": "2026-10-14T09:00:00", "remaining": "demain à neuf heures"}),
            ]
        );
        let empty = Bench::at(EVENING);
        assert_eq!(
            empty.ok("list", json!({})).await,
            "Aucun minuteur ni rappel en cours."
        );
    }

    #[tokio::test]
    async fn what_came_due_is_no_longer_listed() {
        let bench = Bench::at(EVENING);
        bench.ok("timer", json!({"seconds": 60})).await;
        bench.clock.advance(Duration::seconds(61));
        assert!(bench.list().await.is_empty());
    }

    #[tokio::test]
    async fn timers_and_reminders_survive_a_restart() {
        let mut bench = Bench::at(EVENING);
        bench
            .ok("timer", json!({"seconds": 3600, "label": "linge"}))
            .await;
        bench
            .ok(
                "reminder",
                json!({"text": "sortir le linge", "in_seconds": 7200}),
            )
            .await;
        let before = bench.list().await;
        // Two minutes without `mind`.
        bench.clock.advance(Duration::minutes(2));
        bench.restart();
        let after = bench.list().await;
        assert_eq!(after.len(), 2);
        assert_eq!(after[0]["due"], before[0]["due"]);
        assert_eq!(after[0]["remaining"], "cinquante-huit minutes");
        assert_eq!(after[1]["text"], "sortir le linge");
        // The next one does not take an id already given.
        let said = bench.ok("timer", json!({"seconds": 60})).await;
        assert!(said.starts_with("Minuteur 3 "), "{said}");
    }

    #[tokio::test]
    async fn a_reminder_is_set_for_a_time_or_a_delay() {
        let bench = Bench::at(EVENING);
        assert_eq!(
            bench
                .ok(
                    "reminder",
                    json!({"text": "appeler le garage", "at": "2026-10-14T09:00"})
                )
                .await,
            "Rappel 1 : échéance demain à neuf heures."
        );
        assert_eq!(
            bench
                .ok(
                    "reminder",
                    json!({"text": "sortir le linge", "in_seconds": 3600})
                )
                .await,
            "Rappel 2 : échéance à vingt heures."
        );
        // Seconds are accepted; so is an instant with its own offset.
        bench
            .ok(
                "reminder",
                json!({"text": "a", "at": "2026-10-13T21:30:00"}),
            )
            .await;
        let said = bench
            .ok(
                "reminder",
                json!({"text": "b", "at": "2026-10-13T22:00:00+02:00"}),
            )
            .await;
        assert_eq!(said, "Rappel 4 : échéance à vingt-deux heures.");
        let said = bench
            .ok(
                "reminder",
                json!({"text": "c", "at": "2026-10-13T21:45:00Z"}),
            )
            .await;
        assert_eq!(
            said,
            "Rappel 5 : échéance à vingt-trois heures quarante-cinq."
        );
    }

    #[tokio::test]
    async fn an_at_reminder_across_the_change_of_hour_keeps_the_wall_clock() {
        // The night the clocks go back (2026-10-25, 03:00 to 02:00 in Paris): set the evening
        // before for nine in the morning, which is an hour later than 24 hours on.
        let bench = Bench::at("2026-10-24T17:00:00Z"); // 19:00 at +02:00
        let said = bench
            .ok("reminder", json!({"text": "x", "at": "2026-10-25T09:00"}))
            .await;
        assert_eq!(said, "Rappel 1 : échéance demain à neuf heures.");
        let items = bench.list().await;
        assert_eq!(items[0]["due"], "2026-10-25T09:00:00");
        // The stored instant is 09:00 at +01:00, that is 08:00Z: fourteen hours and a bit more
        // than thirteen from 17:00Z... exactly fifteen.
        let file = std::fs::read_to_string(bench.dir.path().join("timers.json")).unwrap();
        assert!(
            file.contains("\"due\": \"2026-10-25T09:00:00+01:00\""),
            "{file}"
        );
        // The remaining time is the real one: 15 hours, not 14.
        bench.clock.advance(Duration::hours(14));
        let items = bench.list().await;
        assert_eq!(items[0]["remaining"], "une heure");
        // And it survives a restart on the other side of the change.
        let mut bench = bench;
        bench.restart();
        assert_eq!(bench.list().await[0]["due"], "2026-10-25T09:00:00");
    }

    #[tokio::test]
    async fn a_reminder_in_the_hour_the_clocks_skip_is_moved_on_and_says_so() {
        // 2026-03-29: 02:00 became 03:00 in Paris.
        let bench = Bench::at("2026-03-28T17:00:00Z");
        let said = bench
            .ok("reminder", json!({"text": "x", "at": "2026-03-29T02:30"}))
            .await;
        assert!(
            said.starts_with("Rappel 1 : échéance demain à trois heures et demie."),
            "{said}"
        );
        assert!(said.contains("changement d'heure"), "{said}");
        assert_eq!(bench.list().await[0]["due"], "2026-03-29T03:30:00");
    }

    #[tokio::test]
    async fn a_timer_over_the_change_of_hour_lasts_what_was_asked() {
        // 01:30 at +01:00; an hour later is 03:30 at +02:00, as the clocks jumped.
        let bench = Bench::at("2026-03-29T00:30:00Z");
        bench.ok("timer", json!({"seconds": 3600})).await;
        let items = bench.list().await;
        assert_eq!(items[0]["due"], "2026-03-29T03:30:00");
        assert_eq!(items[0]["remaining"], "une heure");
    }

    #[tokio::test]
    async fn a_reminder_that_cannot_be_kept_is_refused_with_the_reason() {
        let bench = Bench::at(EVENING);
        let past = bench
            .refused("reminder", json!({"text": "x", "at": "2026-10-13T08:00"}))
            .await;
        assert_eq!(past, "Cette heure est passée : il est 2026-10-13T19:00:00.");
        for (arguments, expected) in [
            (
                json!({"text": "x", "at": "2028-01-01T10:00"}),
                "plus d'un an",
            ),
            (json!({"text": "x", "in_seconds": 40_000_000}), "un an"),
            (json!({"text": "x", "in_seconds": 0}), "un an"),
            (json!({"text": "x"}), "soit « at », soit « in_seconds »"),
            (
                json!({"text": "x", "at": "2026-10-14T09:00", "in_seconds": 60}),
                "pas les deux",
            ),
            (
                json!({"text": "x", "at": "demain neuf heures"}),
                "AAAA-MM-JJTHH:MM",
            ),
            (
                json!({"text": "x", "at": "2026-02-30T09:00"}),
                "AAAA-MM-JJTHH:MM",
            ),
            (json!({"at": "2026-10-14T09:00"}), "« text » manque"),
            (json!({"text": "  ", "in_seconds": 60}), "« text » manque"),
            (
                json!({"text": "x".repeat(201), "in_seconds": 60}),
                "200 caractères",
            ),
            (
                json!({"text": 5, "in_seconds": 60}),
                "« text » doit être un texte",
            ),
            (json!({"text": "x", "in_seconds": "soon"}), "nombre entier"),
            (json!({"text": "x", "in_seconds": -5}), "nombre entier"),
        ] {
            let said = bench.refused("reminder", arguments.clone()).await;
            assert!(said.contains(expected), "{arguments}: {said}");
        }
        assert!(bench.list().await.is_empty());
    }

    #[tokio::test]
    async fn a_timer_that_cannot_be_kept_is_refused_with_the_reason() {
        let bench = Bench::at(EVENING);
        for (arguments, expected) in [
            (json!({}), "« seconds » manque"),
            (json!({"seconds": 0}), "d'une seconde à vingt-quatre heures"),
            (json!({"seconds": 86_401}), "vingt-quatre heures"),
            (json!({"seconds": 1.5}), "nombre entier"),
            (json!({"seconds": "dix"}), "nombre entier"),
            (
                json!({"seconds": 60, "label": "x".repeat(41)}),
                "40 caractères",
            ),
            (
                json!({"seconds": 60, "label": 3}),
                "« label » doit être un texte",
            ),
        ] {
            let said = bench.refused("timer", arguments.clone()).await;
            assert!(said.contains(expected), "{arguments}: {said}");
        }
        // The longest is a day, the shortest a second; a blank label is no label.
        bench.ok("timer", json!({"seconds": 86_400})).await;
        bench
            .ok("timer", json!({"seconds": 1, "label": "  "}))
            .await;
        assert!(bench.list().await[0].get("label").is_none());
    }

    #[tokio::test]
    async fn the_number_of_items_waiting_is_capped() {
        let config = TimeConfig {
            max_active: 2,
            ..TimeConfig::default()
        };
        let bench = Bench::with(EVENING, config);
        bench.ok("timer", json!({"seconds": 60})).await;
        bench.ok("timer", json!({"seconds": 120})).await;
        let said = bench.refused("timer", json!({"seconds": 60})).await;
        assert!(
            said.contains("2 minuteurs et rappels") && said.contains("maximum"),
            "{said}"
        );
        // One comes due: there is room again. Cancelling gives room too.
        bench.clock.advance(Duration::seconds(61));
        bench.ok("timer", json!({"seconds": 60})).await;
        bench.ok("cancel", json!({"id": 3})).await;
        bench
            .ok("reminder", json!({"text": "x", "in_seconds": 60}))
            .await;
    }

    #[tokio::test]
    async fn an_item_is_cancelled_by_its_number() {
        let mut bench = Bench::at(EVENING);
        bench
            .ok("timer", json!({"seconds": 600, "label": "pâtes"}))
            .await;
        bench
            .ok("reminder", json!({"text": "x", "in_seconds": 600}))
            .await;
        assert_eq!(
            bench.ok("cancel", json!({"id": 1})).await,
            "Minuteur 1 (pâtes) annulé."
        );
        let again = bench.refused("cancel", json!({"id": 1})).await;
        assert!(
            again.contains("numéro 1") && again.contains("time_list"),
            "{again}"
        );
        bench.refused("cancel", json!({})).await;
        bench.refused("cancel", json!({"id": "pâtes"})).await;
        // Cancelled for good: not back after a restart.
        bench.restart();
        let items = bench.list().await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], 2);
        assert_eq!(
            bench.ok("cancel", json!({"id": 2})).await,
            "Rappel 2 annulé."
        );
        assert!(bench.list().await.is_empty());
    }

    #[tokio::test]
    async fn what_came_due_during_a_long_stop_is_dropped_at_start() {
        let mut bench = Bench::at(EVENING);
        bench.ok("timer", json!({"seconds": 600})).await; // due 17:10Z
        bench
            .ok("reminder", json!({"text": "x", "in_seconds": 20 * 3600}))
            .await;
        // Down for 13 hours: the timer is older than the limit, the reminder is not yet due.
        bench.clock.advance(Duration::hours(13));
        bench.restart();
        let file = std::fs::read_to_string(bench.dir.path().join("timers.json")).unwrap();
        assert!(!file.contains("\"timer\""), "{file}");
        assert!(file.contains("\"reminder\""), "{file}");
        // Down for 3 hours: it would be said late (the next unit), so it is kept.
        let mut bench = Bench::at(EVENING);
        bench.ok("timer", json!({"seconds": 600})).await;
        bench.clock.advance(Duration::hours(3));
        bench.restart();
        let file = std::fs::read_to_string(bench.dir.path().join("timers.json")).unwrap();
        assert!(file.contains("\"timer\""), "{file}");
    }

    #[tokio::test]
    async fn a_broken_file_does_not_stop_the_module() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("timers.json"), "{ not json").unwrap();
        let config = TimeConfig {
            timezone: PARIS.to_owned(),
            ..TimeConfig::default()
        };
        let clock = Arc::new(ManualClock::new(EVENING.parse().unwrap()));
        let time = Time::with_clock(
            &config,
            &dir.path().join("timers.json"),
            clock,
            Conversations::default(),
        )
        .unwrap();
        let list = time.call("list", None).await;
        assert_eq!(text_of(&list), "Aucun minuteur ni rappel en cours.");
        let said = time
            .call("timer", json!({"seconds": 60}).as_object().cloned())
            .await;
        assert!(said.is_error != Some(true), "{}", text_of(&said));
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.contains(&"timers.json.broken-20261013T170000Z".to_owned()),
            "{names:?}"
        );
        assert!(names.contains(&"timers.json".to_owned()), "{names:?}");
    }

    #[tokio::test]
    async fn a_timer_belongs_to_the_device_of_its_conversation() {
        let config = TimeConfig {
            default_device: Some("kitchen".to_owned()),
            ..TimeConfig::default()
        };
        let bench = Bench::with(EVENING, config);
        let office = bench
            .conversations
            .open_on(contract::Device::new("office").unwrap());
        let anonymous = bench.conversations.open();
        let set = |conversation| {
            bench.time.call_in(
                "timer",
                json!({"seconds": 600}).as_object().cloned(),
                conversation,
            )
        };
        set(Some(office)).await;
        set(Some(anonymous)).await;
        set(None).await;
        let devices: Vec<_> = bench
            .list()
            .await
            .iter()
            .map(|i| i["device"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(devices, ["office", "kitchen", "kitchen"]);
        // Without a default, a timer without a device has none.
        let bench = Bench::at(EVENING);
        bench.ok("timer", json!({"seconds": 60})).await;
        assert!(bench.list().await[0].get("device").is_none());
    }

    #[tokio::test]
    async fn an_unknown_tool_is_an_error_for_the_llm_to_read() {
        let bench = Bench::at(EVENING);
        let said = bench.refused("snooze", json!({})).await;
        assert!(said.contains("time_snooze"), "{said}");
    }

    #[test]
    fn the_module_is_off_when_the_configuration_says_so() {
        let config = TimeConfig {
            enabled: false,
            ..TimeConfig::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let data = DataConfig {
            dir: Some(crate::config::ConfigPath(dir.path().join("data"))),
        };
        assert!(
            Time::new(&config, &data, Conversations::default())
                .unwrap()
                .is_none()
        );
        // Off, it touches nothing; on, it makes its folder.
        assert!(!dir.path().join("data").exists());
        let on = TimeConfig::default();
        assert!(
            Time::new(&on, &data, Conversations::default())
                .unwrap()
                .is_some()
        );
        assert!(dir.path().join("data").is_dir());
    }
}
