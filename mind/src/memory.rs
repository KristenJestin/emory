//! What the mind reads in the long-term memory before the LLM answers: the entries a sentence
//! finds, and what the memory flags for the moment. Hippocampe is in [`crate::hippocampe`]; the
//! tests use a fake.

use std::future::Future;

use serde_json::Value;

/// An entry found by a search: only what goes into the request to the LLM (a raw answer is about
/// 1 100 tokens for 5 entries, mostly identifiers and dates).
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub slug: String,
    pub kind: String,
    pub title: String,
    pub summary: String,
    pub excerpt: String,
    /// The day it happened, for a dated entry.
    pub date: Option<String>,
}

pub trait Memory: Send + Sync + 'static {
    /// The entries a sentence finds, the most relevant first.
    fn search(
        &self,
        query: &str,
        limit: u32,
    ) -> impl Future<Output = anyhow::Result<Vec<Found>>> + Send;

    /// What the memory flags for today (coming dates, overdue deadlines…), as compact text, or
    /// `None` when there is nothing.
    fn heads_up(&self) -> impl Future<Output = anyhow::Result<Option<String>>> + Send;
}

/// Reads the results of Hippocampe's `search` (its `structuredContent`).
pub fn found_in(structured: &Value) -> Vec<Found> {
    let text = |v: &Value, key: &str| v[key].as_str().unwrap_or_default().to_owned();
    structured["results"]
        .as_array()
        .map(|results| {
            results
                .iter()
                .filter_map(|r| {
                    let slug = text(r, "slug");
                    (!slug.is_empty()).then(|| Found {
                        slug,
                        kind: text(r, "type"),
                        title: text(r, "title"),
                        summary: text(r, "summary"),
                        excerpt: unmark(&text(r, "excerpt")),
                        date: r["date"].as_str().map(str::to_owned),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Hippocampe marks the words found with `<mark>`: they mean nothing to the LLM.
fn unmark(excerpt: &str) -> String {
    excerpt.replace("<mark>", "").replace("</mark>", "")
}

/// The longest heads-up kept, in characters.
const HEADS_UP_MAX: usize = 3000;

/// Makes the answer of Hippocampe's `briefing` fit a request: without what waits for the owner's
/// confirmation (`waiting`, a matter for the command line), without identifiers and empty parts,
/// in compact JSON, cut if too long. `None` when nothing is left.
pub fn heads_up_of(briefing: &Value) -> Option<String> {
    let mut briefing = briefing.clone();
    if let Some(map) = briefing.as_object_mut() {
        map.remove("waiting");
    }
    let pruned = prune(briefing)?;
    let mut text = pruned.to_string();
    if text.chars().count() > HEADS_UP_MAX {
        text = text.chars().take(HEADS_UP_MAX).collect::<String>() + "…";
    }
    Some(text)
}

/// Removes the identifiers, the nulls and what is empty, recursively.
fn prune(value: Value) -> Option<Value> {
    match value {
        Value::Null => None,
        Value::String(s) if s.is_empty() => None,
        Value::Array(items) => {
            let items: Vec<Value> = items.into_iter().filter_map(prune).collect();
            (!items.is_empty()).then_some(Value::Array(items))
        }
        Value::Object(map) => {
            let map: serde_json::Map<String, Value> = map
                .into_iter()
                .filter(|(k, _)| k != "id")
                .filter_map(|(k, v)| prune(v).map(|v| (k, v)))
                .collect();
            (!map.is_empty()).then_some(Value::Object(map))
        }
        other => Some(other),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn search_results_keep_what_the_llm_needs() {
        let structured = json!({"results": [
            {"id": "01a", "slug": "contrat-internet", "type": "contract", "title": "Contrat internet",
             "summary": "Free, 29 euros.", "excerpt": "le <mark>contrat</mark> finit", "score": 0.8,
             "date": "2027-03-14"},
            {"id": "01b", "slug": "", "title": "sans slug"},
            {"slug": "musique", "title": "Musique"}
        ], "total": 3});
        let found = found_in(&structured);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].excerpt, "le contrat finit");
        assert_eq!(found[0].kind, "contract");
        assert_eq!(found[0].date.as_deref(), Some("2027-03-14"));
        assert_eq!(found[1].summary, "");
        assert!(found_in(&json!({"error": "x"})).is_empty());
    }

    #[test]
    fn the_heads_up_drops_what_waits_identifiers_and_empty_parts() {
        let briefing = json!({
            "upcoming": [{"id": "1", "slug": "dentiste", "title": "Dentiste", "days_left": 2}],
            "overdue": [],
            "a_year_ago": {"created": [], "dates": null},
            "waiting": {"supposed": [{"slug": "x"}]}
        });
        let text = heads_up_of(&briefing).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!({"upcoming": [{"days_left": 2, "slug": "dentiste", "title": "Dentiste"}]})
        );
        assert!(heads_up_of(&json!({"overdue": [], "waiting": {"a": 1}})).is_none());
    }

    #[test]
    fn a_long_heads_up_is_cut() {
        let briefing = json!({"upcoming": [{"title": "x".repeat(5000)}]});
        assert_eq!(
            heads_up_of(&briefing).unwrap().chars().count(),
            HEADS_UP_MAX + 1
        );
    }
}
