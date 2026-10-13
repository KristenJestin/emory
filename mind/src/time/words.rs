//! Durations and moments in French words, for an answer that is spoken: « huit minutes »,
//! « demain à neuf heures ». A voice reads digits badly ("19:12"), and the LLM repeats what the
//! tools give it.

use chrono::{Datelike, Timelike};
use contract::Timestamp;

const UNITS: [&str; 17] = [
    "zéro", "un", "deux", "trois", "quatre", "cinq", "six", "sept", "huit", "neuf", "dix", "onze",
    "douze", "treize", "quatorze", "quinze", "seize",
];
const TENS: [&str; 6] = [
    "vingt",
    "trente",
    "quarante",
    "cinquante",
    "soixante",
    "soixante",
];
const DAYS: [&str; 7] = [
    "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche",
];
const MONTHS: [&str; 12] = [
    "janvier",
    "février",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
];

/// Until here (in seconds), what remains is a duration (« huit minutes »); beyond, the moment
/// (« demain à neuf heures »).
const REMAINING_AS_DURATION_UNTIL: i64 = 6 * 3600;

/// A number up to 9999 in words. `feminine`: « une » where « un » ends the number (« vingt et
/// une minutes »).
pub fn number(n: u64, feminine: bool) -> String {
    let one = if feminine { "une" } else { "un" };
    match n {
        0..=16 => {
            if n == 1 {
                one.to_owned()
            } else {
                UNITS[n as usize].to_owned()
            }
        }
        17..=19 => format!("dix-{}", UNITS[(n - 10) as usize]),
        20..=69 => {
            let (tens, unit) = (TENS[(n / 10 - 2) as usize], n % 10);
            match unit {
                0 => tens.to_owned(),
                1 => format!("{tens} et {one}"),
                _ => format!("{tens}-{}", UNITS[unit as usize]),
            }
        }
        70..=79 => match n {
            71 => "soixante et onze".to_owned(),
            _ => format!("soixante-{}", number(n - 60, feminine)),
        },
        80 => "quatre-vingts".to_owned(),
        81..=99 => format!("quatre-vingt-{}", number(n - 80, feminine)),
        100..=999 => {
            let (hundreds, rest) = (n / 100, n % 100);
            let head = match (hundreds, rest) {
                (1, _) => "cent".to_owned(),
                (h, 0) => format!("{} cents", number(h, false)),
                (h, _) => format!("{} cent", number(h, false)),
            };
            match rest {
                0 => head,
                _ => format!("{head} {}", number(rest, feminine)),
            }
        }
        1000..=9999 => {
            let (thousands, rest) = (n / 1000, n % 1000);
            let head = match thousands {
                1 => "mille".to_owned(),
                t => format!("{} mille", number(t, false)),
            };
            match rest {
                0 => head,
                _ => format!("{head} {}", number(rest, feminine)),
            }
        }
        _ => n.to_string(),
    }
}

fn counted(n: u64, singular: &str, plural: &str, feminine: bool) -> String {
    format!(
        "{} {}",
        number(n, feminine),
        if n > 1 { plural } else { singular }
    )
}

/// A duration, exactly: « dix minutes », « une heure et demie », « une minute et trente
/// secondes ».
pub fn duration(seconds: u64) -> String {
    let (days, hours, minutes, seconds) = (
        seconds / 86_400,
        seconds % 86_400 / 3600,
        seconds % 3600 / 60,
        seconds % 60,
    );
    // « une heure et demie », « deux heures et quart »: only after whole hours.
    if days == 0 && hours > 0 && seconds == 0 && (minutes == 30 || minutes == 15) {
        let hours = counted(hours, "heure", "heures", true);
        return format!(
            "{hours} et {}",
            if minutes == 30 { "demie" } else { "quart" }
        );
    }
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(counted(days, "jour", "jours", false));
    }
    if hours > 0 {
        parts.push(counted(hours, "heure", "heures", true));
    }
    if minutes > 0 {
        parts.push(counted(minutes, "minute", "minutes", true));
    }
    if seconds > 0 || parts.is_empty() {
        parts.push(counted(seconds, "seconde", "secondes", true));
    }
    let last = parts.pop().unwrap_or_default();
    match parts.len() {
        0 => last,
        _ => format!("{} et {last}", parts.join(", ")),
    }
}

/// What remains until a due time, for the ear: as long as the person can count it, to the
/// second; then to the minute from ten minutes, to the hour from a day.
fn coarse(seconds: u64) -> u64 {
    let round = |step: u64| (seconds + step / 2) / step * step;
    match seconds {
        0..600 => seconds,
        600..86_400 => round(60),
        _ => round(3600),
    }
}

/// The time left, in words: a duration up to six hours (« huit minutes »), the moment after
/// (« demain à neuf heures »). `now` and `due` carry the offset of the zone.
pub fn remaining(now: Timestamp, due: Timestamp) -> String {
    let left = (due - now).num_seconds().max(1);
    if left < REMAINING_AS_DURATION_UNTIL {
        duration(coarse(left as u64))
    } else {
        moment(now, due)
    }
}

/// A time of day: « neuf heures », « midi et demi », « dix-neuf heures douze ».
fn clock(hour: u32, minute: u32) -> String {
    let (name, midday) = match hour {
        0 => ("minuit".to_owned(), true),
        12 => ("midi".to_owned(), true),
        h => (counted(h as u64, "heure", "heures", true), false),
    };
    match minute {
        0 => name,
        15 => format!("{name} et quart"),
        30 if midday => format!("{name} et demi"),
        30 => format!("{name} et demie"),
        m => format!("{name} {}", number(m as u64, true)),
    }
}

/// A moment, from `now`, in words: « à neuf heures » today, « demain à neuf heures », « lundi à
/// midi » within the week, « le trois novembre à neuf heures » beyond (with the year if it is not
/// this one). Both carry the offset of the zone, so that their dates are the local ones.
pub fn moment(now: Timestamp, due: Timestamp) -> String {
    let time = clock(due.hour(), due.minute());
    let days = (due.date_naive() - now.date_naive()).num_days();
    let day = match days {
        ..=0 => String::new(),
        1 => "demain ".to_owned(),
        2 => "après-demain ".to_owned(),
        3..=6 => format!("{} ", DAYS[due.weekday().num_days_from_monday() as usize]),
        _ => {
            let day = match due.day() {
                1 => "premier".to_owned(),
                d => number(d as u64, false),
            };
            let year = if due.year() == now.year() {
                String::new()
            } else {
                format!(" {}", number(due.year() as u64, false))
            };
            format!("le {day} {}{year} ", MONTHS[due.month0() as usize])
        }
    };
    format!("{day}à {time}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn numbers_are_spelled() {
        for (n, words) in [
            (0, "zéro"),
            (1, "un"),
            (16, "seize"),
            (17, "dix-sept"),
            (21, "vingt et un"),
            (22, "vingt-deux"),
            (60, "soixante"),
            (71, "soixante et onze"),
            (77, "soixante-dix-sept"),
            (80, "quatre-vingts"),
            (81, "quatre-vingt-un"),
            (91, "quatre-vingt-onze"),
            (99, "quatre-vingt-dix-neuf"),
            (100, "cent"),
            (101, "cent un"),
            (200, "deux cents"),
            (280, "deux cent quatre-vingts"),
            (365, "trois cent soixante-cinq"),
            (1000, "mille"),
            (2026, "deux mille vingt-six"),
        ] {
            assert_eq!(number(n, false), words, "{n}");
        }
        assert_eq!(number(21, true), "vingt et une");
        assert_eq!(number(81, true), "quatre-vingt-une");
        assert_eq!(number(71, true), "soixante et onze");
    }

    #[test]
    fn durations_are_said_the_way_they_are_spoken() {
        for (seconds, words) in [
            (1, "une seconde"),
            (45, "quarante-cinq secondes"),
            (60, "une minute"),
            (90, "une minute et trente secondes"),
            (600, "dix minutes"),
            (3600, "une heure"),
            (5400, "une heure et demie"),
            (8100, "deux heures et quart"),
            (3660, "une heure et une minute"),
            (4500, "une heure et quart"),
            (5100, "une heure et vingt-cinq minutes"),
            (3725, "une heure, deux minutes et cinq secondes"),
            (86_400, "un jour"),
            (90_000, "un jour et une heure"),
            (2 * 86_400 + 3 * 3600, "deux jours et trois heures"),
        ] {
            assert_eq!(duration(seconds), words, "{seconds}");
        }
    }

    #[test]
    fn what_remains_is_rounded_to_what_the_ear_can_use() {
        let now = at("2026-10-13T19:00:00+02:00");
        let left = |seconds| remaining(now, now + chrono::Duration::seconds(seconds));
        assert_eq!(left(1), "une seconde");
        assert_eq!(left(492), "huit minutes et douze secondes");
        // From ten minutes, to the minute.
        assert_eq!(left(1000), "dix-sept minutes");
        assert_eq!(left(3 * 3600 + 29), "trois heures");
        // Past six hours, the moment.
        assert_eq!(left(14 * 3600), "demain à neuf heures");
    }

    #[test]
    fn moments_name_the_day_the_way_a_person_would() {
        let now = at("2026-10-13T19:00:00+02:00"); // a Tuesday
        for (due, words) in [
            ("2026-10-13T19:12:00+02:00", "à dix-neuf heures douze"),
            ("2026-10-13T23:30:00+02:00", "à vingt-trois heures et demie"),
            ("2026-10-14T00:00:00+02:00", "demain à minuit"),
            ("2026-10-14T09:00:00+02:00", "demain à neuf heures"),
            ("2026-10-14T12:30:00+02:00", "demain à midi et demi"),
            ("2026-10-14T13:00:00+02:00", "demain à treize heures"),
            ("2026-10-14T01:15:00+02:00", "demain à une heure et quart"),
            (
                "2026-10-15T08:05:00+02:00",
                "après-demain à huit heures cinq",
            ),
            ("2026-10-17T10:00:00+02:00", "samedi à dix heures"),
            ("2026-10-20T10:00:00+02:00", "le vingt octobre à dix heures"),
            (
                "2026-11-01T10:00:00+01:00",
                "le premier novembre à dix heures",
            ),
            (
                "2027-01-02T21:00:00+01:00",
                "le deux janvier deux mille vingt-sept à vingt et une heures",
            ),
        ] {
            assert_eq!(moment(now, at(due)), words, "{due}");
        }
    }

    #[test]
    fn the_day_is_the_local_one() {
        // 23:30 in Paris is already tomorrow in a zone two hours ahead, but the dates compared
        // are the ones the instants carry.
        let now = at("2026-10-13T23:30:00+02:00");
        assert_eq!(
            moment(now, at("2026-10-14T00:10:00+02:00")),
            "demain à minuit dix"
        );
    }
}
