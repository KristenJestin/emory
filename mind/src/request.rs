//! The text of a request to the LLM: what the memory flags for the moment (first request of a
//! conversation only: the session keeps it), the entries found while Kris was speaking, then what
//! Kris said.

use std::fmt::Write;

use crate::memory::Found;

/// The most entries put into a request, all the sentences of the turn together.
pub const MAX_FOUND: usize = 10;

pub fn build(heads_up: Option<&str>, found: &[Found], said: &str) -> String {
    let mut text = String::new();
    if let Some(heads_up) = heads_up {
        let _ = writeln!(
            text,
            "[Ce que la mémoire signale pour aujourd'hui]\n{heads_up}\n"
        );
    }
    if !found.is_empty() {
        text.push_str("[Fiches trouvées dans la mémoire pendant que Kris parlait]\n");
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
    let _ = write!(text, "[Kris dit]\n{said}");
    text
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

    #[test]
    fn a_request_without_memory_is_what_kris_said() {
        assert_eq!(build(None, &[], "bonjour"), "[Kris dit]\nbonjour");
    }

    #[test]
    fn the_heads_up_and_the_entries_come_first() {
        let mut f = found("musique");
        f.date = Some("2026-09-02".to_owned());
        f.excerpt = "le salon".to_owned();
        let text = build(Some("{\"upcoming\":[]}"), &[f], "et la musique ?");
        assert_eq!(
            text,
            "[Ce que la mémoire signale pour aujourd'hui]\n{\"upcoming\":[]}\n\n\
             [Fiches trouvées dans la mémoire pendant que Kris parlait]\n\
             - Titre musique (decision, musique, le 2026-09-02) : Résumé. Extrait : le salon\n\n\
             [Kris dit]\net la musique ?"
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
