//! The time zone of the module: the system's, or one named in the configuration. It gives the
//! offset an instant has there, and turns a local date and time into an instant across the
//! daylight-saving changes.

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset, NaiveDateTime, Utc};
use contract::Timestamp;
use jiff::{
    civil,
    tz::{AmbiguousOffset, TimeZone},
};

/// A zone, cheap to use and to share.
#[derive(Clone, Debug)]
pub struct Zone {
    zone: TimeZone,
}

/// A local date and time, as an instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub at: Timestamp,
    /// The local time does not exist (the clocks jumped over it): the instant is the one the same
    /// wait after the jump would give, so the local time of `at` is later than asked.
    pub skipped: bool,
}

impl Zone {
    /// `""`: the system's zone (the `TZ` variable, else `/etc/localtime`); else an IANA name
    /// (`Europe/Paris`).
    pub fn new(name: &str) -> Result<Self> {
        let name = name.trim();
        let zone = if name.is_empty() {
            TimeZone::system()
        } else {
            TimeZone::get(name).with_context(|| format!("unknown time zone \"{name}\""))?
        };
        Ok(Self { zone })
    }

    /// The offset of the zone at an instant.
    fn offset(&self, instant: DateTime<Utc>) -> FixedOffset {
        let seconds = jiff::Timestamp::from_second(instant.timestamp())
            .map(|ts| self.zone.to_offset(ts).seconds())
            .unwrap_or_default();
        FixedOffset::east_opt(seconds).unwrap_or_else(|| FixedOffset::east_opt(0).unwrap())
    }

    /// The instant, with the offset the zone has then.
    pub fn at(&self, instant: DateTime<Utc>) -> Timestamp {
        instant.with_timezone(&self.offset(instant))
    }

    /// The same instant, with the offset the zone has then, whatever offset it came with.
    pub fn local(&self, instant: Timestamp) -> Timestamp {
        self.at(instant.with_timezone(&Utc))
    }

    /// A local date and time. A time that happens twice (the clocks went back) is the first one;
    /// a time that does not happen (the clocks jumped) is moved on by the length of the jump, as
    /// a calendar does. `None` when the date is out of range.
    pub fn resolve(&self, local: NaiveDateTime) -> Option<Resolved> {
        use chrono::{Datelike, Timelike};
        let civil = civil::DateTime::new(
            i16::try_from(local.year()).ok()?,
            local.month() as i8,
            local.day() as i8,
            local.hour() as i8,
            local.minute() as i8,
            local.second() as i8,
            0,
        )
        .ok()?;
        let ambiguous = self.zone.to_ambiguous_timestamp(civil);
        let skipped = matches!(ambiguous.offset(), AmbiguousOffset::Gap { .. });
        let instant = ambiguous.compatible().ok()?;
        let instant = DateTime::from_timestamp(instant.as_second(), 0)?;
        Some(Resolved {
            at: self.at(instant),
            skipped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").unwrap()
    }

    #[test]
    fn a_zone_is_named_or_the_systems() {
        assert!(Zone::new("").is_ok());
        assert!(Zone::new("Europe/Paris").is_ok());
        let error = format!("{:#}", Zone::new("Mars/Olympus").unwrap_err());
        assert!(error.contains("Mars/Olympus"), "{error}");
    }

    #[test]
    fn the_offset_follows_the_daylight_saving_time() {
        let paris = Zone::new("Europe/Paris").unwrap();
        let at = |s: &str| paris.at(s.parse().unwrap());
        assert_eq!(at("2026-07-01T10:00:00Z").offset().local_minus_utc(), 7200);
        assert_eq!(at("2026-12-01T10:00:00Z").offset().local_minus_utc(), 3600);
    }

    #[test]
    fn a_local_time_in_summer_and_in_winter() {
        let paris = Zone::new("Europe/Paris").unwrap();
        let summer = paris.resolve(local("2026-07-01T09:00")).unwrap();
        assert_eq!(summer.at.to_rfc3339(), "2026-07-01T09:00:00+02:00");
        let winter = paris.resolve(local("2026-12-01T09:00")).unwrap();
        assert_eq!(winter.at.to_rfc3339(), "2026-12-01T09:00:00+01:00");
        assert!(!summer.skipped && !winter.skipped);
    }

    #[test]
    fn a_time_the_clocks_jump_over_is_moved_on() {
        // In Paris the clocks went from 02:00 to 03:00 on 2026-03-29.
        let paris = Zone::new("Europe/Paris").unwrap();
        let skipped = paris.resolve(local("2026-03-29T02:30")).unwrap();
        assert!(skipped.skipped);
        assert_eq!(skipped.at.to_rfc3339(), "2026-03-29T03:30:00+02:00");
    }

    #[test]
    fn a_time_that_happens_twice_is_the_first_one() {
        // In Paris the clocks went from 03:00 back to 02:00 on 2026-10-25.
        let paris = Zone::new("Europe/Paris").unwrap();
        let twice = paris.resolve(local("2026-10-25T02:30")).unwrap();
        assert!(!twice.skipped);
        assert_eq!(twice.at.to_rfc3339(), "2026-10-25T02:30:00+02:00");
    }

    #[test]
    fn an_instant_is_given_the_offset_it_has_then() {
        let paris = Zone::new("Europe/Paris").unwrap();
        // Set in winter with +01:00, due after the change: seen with +02:00.
        let due: Timestamp = "2026-03-29T01:30:00+01:00".parse().unwrap();
        assert_eq!(
            paris.local(due + chrono::Duration::hours(2)).to_rfc3339(),
            "2026-03-29T04:30:00+02:00"
        );
    }
}
