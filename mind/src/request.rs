//! The text of a request to the LLM: the day and the time, what the memory flags for the moment
//! (first request of a conversation only: the session keeps it), what Emory said on her own since
//! the last request (a timer, an event), the entries found while the owner was speaking, then what
//! was said. The markers name nobody: the owner's name would cost tokens in
//! every request and add nothing (the instructions say who it is).

use std::fmt::Write;

use chrono::{Datelike, Timelike};
use contract::Timestamp;

use crate::memory::Found;

/// The most entries put into a request, all the sentences of the turn together.
pub const MAX_FOUND: usize = 10;

pub fn build(
    now: Timestamp,
    heads_up: Option<&str>,
    own: &[String],
    found: &[Found],
    said: &str,
) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "[Maintenant] {}\n", moment(now));
    if let Some(heads_up) = heads_up {
        let _ = writeln!(
            text,
            "[Ce que la mémoire signale pour aujourd'hui]\n{heads_up}\n"
        );
    }
    if !own.is_empty() {
        text.push_str("[Tu as dit de toi-même]\n");
        for said in own {
            let _ = writeln!(text, "- {said}");
        }
        text.push('\n');
    }
    if !found.is_empty() {
        text.push_str("[Fiches trouvées pendant qu'on te parlait]\n");
        for f in found.iter().take(MAX_FOUND) {
            let _ = write!(text, "- {} ({}, {}", f.title, f.kind, f.slug);
            if let Some(date) = &f.date {
                let _ = write!(text, ", le {date}");
            }
            text.push(')');
            if !f.summary.is_empty() {
                let _ = write!(text, " : {}", f.summary);
            }
            if !f.excerpt.is_empty() {
                let _ = write!(text, " Extrait : {}", f.excerpt);
            }
            text.push('\n');
        }
        text.push('\n');
    }
    let _ = write!(text, "[On te dit]\n{said}");
    text
}

/// `dimanche 12 octobre 2026, 14:05`: the LLM says it in words.
fn moment(now: Timestamp) -> String {
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
    format!(
        "{} {} {} {}, {:02}:{:02}",
        DAYS[now.weekday().num_days_from_monday() as usize],
        now.day(),
        MONTHS[now.month0() as usize],
        now.year(),
        now.hour(),
        now.minute()
    )
}

/// Puts together the entries found by the sentences of a turn: each once, in the order found.
pub fn merge(lists: impl IntoIterator<Item = Vec<Found>>) -> Vec<Found> {
    let mut merged: Vec<Found> = Vec::new();
    for found in lists.into_iter().flatten() {
        if !merged.iter().any(|f| f.slug == found.slug) {
            merged.push(found);
        }
    }
    merged.truncate(MAX_FOUND);
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(slug: &str) -> Found {
        Found {
            slug: slug.to_owned(),
            kind: "decision".to_owned(),
            title: format!("Titre {slug}"),
            summary: "Résumé.".to_owned(),
            excerpt: String::new(),
            date: None,
        }
    }

    fn at() -> Timestamp {
        "2026-10-12T14:05:00+02:00".parse().unwrap()
    }

    #[test]
    fn a_request_without_memory_is_the_moment_and_what_was_said() {
        assert_eq!(
            build(at(), None, &[], &[], "bonjour"),
            "[Maintenant] lundi 12 octobre 2026, 14:05\n\n[On te dit]\nbonjour"
        );
    }

    #[test]
    fn the_heads_up_and_the_entries_come_first() {
        let mut f = found("musique");
        f.date = Some("2026-09-02".to_owned());
        f.excerpt = "le salon".to_owned();
        let text = build(
            at(),
            Some("{\"upcoming\":[]}"),
            &[],
            &[f],
            "et la musique ?",
        );
        assert_eq!(
            text,
            "[Maintenant] lundi 12 octobre 2026, 14:05\n\n\
             [Ce que la mémoire signale pour aujourd'hui]\n{\"upcoming\":[]}\n\n\
             [Fiches trouvées pendant qu'on te parlait]\n\
             - Titre musique (decision, musique, le 2026-09-02) : Résumé. Extrait : le salon\n\n\
             [On te dit]\net la musique ?"
        );
    }

    #[test]
    fn what_emory_said_on_her_own_comes_before_the_entries() {
        let own = [
            "Le minuteur des pâtes est fini.".to_owned(),
            "Deux.".to_owned(),
        ];
        assert_eq!(
            build(at(), None, &own, &[found("a")], "relance-le"),
            "[Maintenant] lundi 12 octobre 2026, 14:05\n\n\
             [Tu as dit de toi-même]\n- Le minuteur des pâtes est fini.\n- Deux.\n\n\
             [Fiches trouvées pendant qu'on te parlait]\n\
             - Titre a (decision, a) : Résumé.\n\n\
             [On te dit]\nrelance-le"
        );
    }

    #[test]
    fn entries_are_merged_once_and_capped() {
        let merged = merge([vec![found("a"), found("b")], vec![found("b"), found("c")]]);
        let slugs: Vec<_> = merged.iter().map(|f| f.slug.as_str()).collect();
        assert_eq!(slugs, ["a", "b", "c"]);
        let many = merge((0..30).map(|i| vec![found(&i.to_string())]));
        assert_eq!(many.len(), MAX_FOUND);
    }
}
