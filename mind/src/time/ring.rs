//! The ringing (`specs/time.md`, « À l'échéance »): a task waits for the nearest due time, then
//! says the fixed phrase of the item on its device, on Emory's own ([`crate::speech`]), with no
//! LLM. If nobody presses on the device in the `repeat_after_s` after it was said, the phrase is
//! said again, `repeat` times at most; a press, whatever for, stops it. The item leaves the file
//! once said (or once it could not be).
//!
//! What came due while `mind` was stopped is said at start-up, « Avec du retard : » first when it
//! is more than a minute late; beyond `late_limit_hours`, dropped and logged.
//!
//! The state goes on the bus for a display: `mind.timer.changed` (the whole list) at start-up
//! and at each change, `mind.timer.fired` at each ring. The logs say the id, the kind, the device
//! and how late, never the text or the label.

use std::{
    collections::HashSet,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::Utc;
use contract::{Device, TimerFired, TimerItem, TimerKind, Timers};
use tokio::sync::Notify;
use tracing::{info, warn};

use super::{
    Clock, TimeConfig, lock,
    store::{Item, Kind, Store},
    words,
    zone::Zone,
};
use crate::{
    conversations::Conversations,
    speech::{Follow, Heard, Origin, Speaker, Speech},
};

/// The wall clock is looked at again at least this often: a sleep of the machine (suspend) or a
/// clock set again does not leave a due time waiting on a stale sleep.
pub const RECHECK: Duration = Duration::from_secs(30);
/// Later than this, the phrase says it is late.
pub const LATE_AFTER: chrono::Duration = chrono::Duration::seconds(60);
/// What a late phrase starts with.
pub const LATE: &str = "Avec du retard : ";

/// Where the state of the timers goes: the bus, or a recorder in the tests.
pub trait Board: Send + Sync + 'static {
    /// `mind.timer.changed`.
    fn changed(&self, timers: Timers) -> impl Future<Output = ()> + Send;
    /// `mind.timer.fired`.
    fn fired(&self, fired: TimerFired) -> impl Future<Output = ()> + Send;
}

/// Rings the timers and reminders of the [`Time`](super::Time) module it comes from
/// ([`Time::ringer`](super::Time::ringer)).
pub struct Ringer<B> {
    pub(super) store: Arc<Mutex<Store>>,
    pub(super) changed: Arc<Notify>,
    pub(super) zone: Zone,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) conversations: Conversations,
    pub(super) config: TimeConfig,
    pub(super) speaker: Speaker,
    pub(super) board: Arc<B>,
}

impl<B: Board> Ringer<B> {
    /// Rings until the mind stops.
    pub async fn run(self) {
        let this = Arc::new(self);
        // The items whose ring has started: not rung twice while they are said.
        let mut ringing: HashSet<u64> = HashSet::new();
        this.publish_changed().await;
        loop {
            let now = this.clock.now();
            let (due, next) = {
                let store = lock(&this.store);
                let mut due: Vec<Item> = Vec::new();
                let mut next: Option<chrono::DateTime<Utc>> = None;
                for item in store.items().iter().filter(|i| !ringing.contains(&i.id)) {
                    let at = item.due.with_timezone(&Utc);
                    if at <= now {
                        due.push(item.clone());
                    } else if next.is_none_or(|n| at < n) {
                        next = Some(at);
                    }
                }
                due.sort_by_key(|i| (i.due, i.id));
                (due, next)
            };
            for item in due {
                ringing.insert(item.id);
                if !this.clone().ring(item, now) {
                    info!("the mind has stopped: no more ringing");
                    return;
                }
            }
            let wait = next
                .and_then(|n| (n - now).to_std().ok())
                .map_or(RECHECK, |w| w.min(RECHECK));
            tokio::select! {
                () = this.changed.notified() => {
                    {
                        let store = lock(&this.store);
                        ringing.retain(|id| store.items().iter().any(|i| i.id == *id));
                    }
                    this.publish_changed().await;
                }
                () = tokio::time::sleep(wait) => {}
            }
        }
    }

    /// Starts the ring of an item come due; `false` if the mind has stopped.
    fn ring(self: Arc<Self>, item: Item, now: chrono::DateTime<Utc>) -> bool {
        let late = now - item.due.with_timezone(&Utc);
        let late_s = late.num_seconds().max(0) as u64;
        let limit = chrono::Duration::hours(self.config.late_limit_hours as i64);
        if late > limit {
            warn!(id = item.id, kind = ?item.kind, due = %item.due, late_s, "timer or reminder too late to be said: dropped");
            self.remove(item.id);
            return true;
        }
        let Some(device) = self.device(&item) else {
            warn!(id = item.id, kind = ?item.kind, due = %item.due, "timer or reminder with no device to ring on: dropped");
            self.remove(item.id);
            return true;
        };
        let text = self.phrase(&item, late > LATE_AFTER);
        info!(id = item.id, kind = ?item.kind, %device, late_s, "timer or reminder rings");
        // The first speech is asked now, before the next item: two due at once ring in order.
        let (speech, follow) = self.speech(&device, &text);
        if !self.speaker.speak(speech) {
            return false;
        }
        let fired = TimerFired {
            item: self.public(&item),
            late_s,
        };
        tokio::spawn(async move {
            self.board.fired(fired).await;
            self.repeat(item, device, text, follow).await;
        });
        true
    }

    /// Follows a ring: the item leaves the file once said; said again while nobody presses.
    async fn repeat(&self, item: Item, device: Device, text: String, mut follow: Follow) {
        let after = Duration::from_secs(self.config.repeat_after_s);
        let mut round = 0;
        loop {
            let said = follow.recv().await == Some(Heard::Said);
            if round == 0 {
                self.remove(item.id);
            }
            if !said {
                info!(id = item.id, %device, round, "timer or reminder not said: the device was never free");
                return;
            }
            if round >= self.config.repeat {
                return;
            }
            let pressed = tokio::time::timeout(after, async {
                while let Some(heard) = follow.recv().await {
                    if heard == Heard::Pressed {
                        return true;
                    }
                }
                false
            })
            .await;
            match pressed {
                Ok(true) => {
                    info!(id = item.id, %device, round, "timer or reminder heard: a press");
                    return;
                }
                // The mind has stopped.
                Ok(false) => return,
                Err(_) => {}
            }
            round += 1;
            info!(id = item.id, %device, round, "nobody pressed: said again");
            let (speech, next) = self.speech(&device, &text);
            if !self.speaker.speak(speech) {
                return;
            }
            follow = next;
        }
    }

    fn speech(&self, device: &Device, text: &str) -> (Speech, Follow) {
        Speech::new(device.clone(), text, Origin::Timer)
            .chime(self.config.chime)
            .followed()
    }

    /// The device of the item; else `default_device`; else the device of the last turn heard.
    fn device(&self, item: &Item) -> Option<Device> {
        item.device
            .as_deref()
            .and_then(|d| Device::new(d).ok())
            .or_else(|| {
                self.config
                    .default_device
                    .as_deref()
                    .and_then(|d| Device::new(d).ok())
            })
            .or_else(|| self.conversations.last_device())
    }

    /// The fixed phrase of the item, from the templates of `[time]`.
    fn phrase(&self, item: &Item, late: bool) -> String {
        let said = match (item.kind, &item.label) {
            (Kind::Timer, Some(label)) => self.config.timer_done.replace("{label}", label),
            (Kind::Timer, None) => self.config.timer_done_unlabeled.replace(
                "{duration}",
                &words::duration(item.seconds.unwrap_or_default()),
            ),
            (Kind::Reminder, _) => {
                let text = item.text.as_deref().unwrap_or_default();
                // « Rappel : {text}. » with a text that ends with its own full stop.
                let text = text.trim_end().trim_end_matches('.').trim_end();
                self.config.reminder.replace("{text}", text)
            }
        };
        if !late {
            return said;
        }
        let mut chars = said.chars();
        let first = chars.next().map(|c| c.to_lowercase().collect::<String>());
        format!("{LATE}{}{}", first.unwrap_or_default(), chars.as_str())
    }

    /// The item as the bus gives it.
    fn public(&self, item: &Item) -> TimerItem {
        TimerItem {
            id: item.id,
            kind: match item.kind {
                Kind::Timer => TimerKind::Timer,
                Kind::Reminder => TimerKind::Reminder,
            },
            label: item.label.clone(),
            text: item.text.clone(),
            due: self.zone.local(item.due),
            device: item.device.as_deref().and_then(|d| Device::new(d).ok()),
        }
    }

    /// Takes the item out of the file: the store tells the loop, which publishes the list.
    fn remove(&self, id: u64) {
        let removed = lock(&self.store).remove(id);
        match removed {
            Ok(Some(_)) => self.changed.notify_one(),
            // Cancelled meanwhile.
            Ok(None) => {}
            Err(e) => {
                warn!(id, error = ?e.kind(), "timers: the file could not be written, the item stays")
            }
        }
    }

    async fn publish_changed(&self) {
        let mut items: Vec<Item> = lock(&self.store).items().to_vec();
        items.sort_by_key(|i| (i.due, i.id));
        let items = items.iter().map(|i| self.public(i)).collect();
        self.board.changed(Timers { items }).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::{ManualClock, Time};

    struct Nowhere;

    impl Board for Nowhere {
        async fn changed(&self, _: Timers) {}
        async fn fired(&self, _: TimerFired) {}
    }

    fn ringer(config: TimeConfig) -> (Ringer<Nowhere>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::new("2026-10-13T17:00:00Z".parse().unwrap()));
        let time = Time::with_clock(
            &config,
            &dir.path().join("timers.json"),
            clock,
            Conversations::default(),
        )
        .unwrap();
        let (speaker, _) = crate::speech::channel();
        (time.ringer(&config, speaker, Arc::new(Nowhere)), dir)
    }

    fn item(kind: Kind, label: Option<&str>, text: Option<&str>, seconds: Option<u64>) -> Item {
        let at: contract::Timestamp = "2026-10-13T19:00:00+02:00".parse().unwrap();
        Item {
            id: 1,
            kind,
            label: label.map(str::to_owned),
            text: text.map(str::to_owned),
            seconds,
            created: at,
            due: at,
            device: None,
        }
    }

    #[test]
    fn the_phrases_come_from_the_templates() {
        let (r, _dir) = ringer(TimeConfig::default());
        let pasta = item(Kind::Timer, Some("pâtes"), None, Some(600));
        assert_eq!(r.phrase(&pasta, false), "Le minuteur pâtes est fini.");
        let plain = item(Kind::Timer, None, None, Some(600));
        assert_eq!(
            r.phrase(&plain, false),
            "Le minuteur de dix minutes est fini."
        );
        let call = item(Kind::Reminder, None, Some("appeler le garage."), None);
        assert_eq!(r.phrase(&call, false), "Rappel : appeler le garage.");
        assert_eq!(
            r.phrase(&call, true),
            "Avec du retard : rappel : appeler le garage."
        );
        assert_eq!(
            r.phrase(&pasta, true),
            "Avec du retard : le minuteur pâtes est fini."
        );
    }

    #[test]
    fn the_device_falls_back_to_the_default_then_the_last_heard() {
        let config = TimeConfig {
            default_device: Some("kitchen".to_owned()),
            ..TimeConfig::default()
        };
        let (r, _dir) = ringer(config);
        let mut set = item(Kind::Timer, None, None, Some(60));
        set.device = Some("office".to_owned());
        assert_eq!(r.device(&set), Some(Device::new("office").unwrap()));
        set.device = None;
        assert_eq!(r.device(&set), Some(Device::new("kitchen").unwrap()));
        let (r, _dir) = ringer(TimeConfig::default());
        assert_eq!(r.device(&set), None);
        let key = r.conversations.open_on(Device::new("salon").unwrap());
        r.conversations.spoke(&key, contract::TurnId::new());
        assert_eq!(r.device(&set), Some(Device::new("salon").unwrap()));
    }
}
