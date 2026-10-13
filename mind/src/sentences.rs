//! Cuts the text of the LLM into sentences as it streams in, so that each one can be said at once.
//!
//! A sentence ends at `.`, `!`, `?` or `…` (closing quotes and brackets included) followed by a
//! space, or at a line break. It is released when the first letter of the next one arrives: then
//! it is certain that it was not the last, and the last one, released by [`Splitter::finish`], can
//! be marked `last`. A sentence that runs on without an end is cut at a comma once it is long, so
//! that the voice does not wait for it.

/// Beyond this many characters, a sentence without an end is cut at its last comma.
const LONG: usize = 220;

/// Abbreviations whose dot does not end a sentence.
const ABBREVIATIONS: [&str; 5] = ["M", "Mme", "Mlle", "Dr", "St"];

#[derive(Debug, Default)]
pub struct Splitter {
    pending: String,
}

impl Splitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a piece of text; returns the sentences it closed, cleaned, in order.
    pub fn push(&mut self, chunk: &str) -> Vec<String> {
        self.pending.push_str(chunk);
        let mut out = Vec::new();
        while let Some(end) = self.boundary() {
            let rest = self.pending.split_off(end);
            let sentence = std::mem::replace(&mut self.pending, rest);
            out.extend(clean(&sentence));
        }
        out
    }

    /// The stream is over: what is left is the last sentence, if it says anything.
    pub fn finish(&mut self) -> Option<String> {
        clean(&std::mem::take(&mut self.pending))
    }

    /// The byte where the first confirmed sentence of `pending` ends, if any: right after its
    /// punctuation, once the next sentence has started.
    fn boundary(&self) -> Option<usize> {
        let text = &self.pending;
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let mut i = 0;
        while i < chars.len() {
            let (at, c) = chars[i];
            let ends = match c {
                '.' => !after_abbreviation(&text[..at]),
                '!' | '?' | '…' => true,
                '\n' => true,
                _ => false,
            };
            if ends {
                // Closing marks stay with the sentence.
                let mut j = i + 1;
                loop {
                    match chars.get(j).map(|&(_, c)| c) {
                        Some('»' | '"' | '\'' | ')' | '’' | '.') => j += 1,
                        // French puts a space before the closing guillemet.
                        Some(' ' | '\u{a0}' | '\u{202f}')
                            if chars.get(j + 1).map(|&(_, c)| c) == Some('»') =>
                        {
                            j += 2
                        }
                        _ => break,
                    }
                }
                let end = chars.get(j).map_or(text.len(), |&(b, _)| b);
                // A space must follow, then the start of the next sentence.
                let mut k = j;
                let mut spaced = c == '\n';
                while k < chars.len() && chars[k].1.is_whitespace() {
                    spaced = true;
                    k += 1;
                }
                if spaced && k < chars.len() {
                    return Some(end);
                }
                if k >= chars.len() {
                    // Not confirmed yet: wait for more text.
                    return None;
                }
                i = j;
                continue;
            }
            i += 1;
        }
        // A long sentence without an end: cut at its last comma, once text follows it.
        if text.chars().count() > LONG
            && let Some(comma) = text.rfind(", ")
            && text[comma + 2..].trim_start().chars().next().is_some()
        {
            return Some(comma + 1);
        }
        None
    }
}

fn after_abbreviation(before: &str) -> bool {
    let word = before
        .rsplit(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or("");
    ABBREVIATIONS.contains(&word)
}

/// What the voice must not read: formatting marks, list bullets, surrounding spaces. `None` when
/// nothing is left to say.
fn clean(sentence: &str) -> Option<String> {
    let mut text: String = sentence
        .chars()
        .filter(|c| !matches!(c, '*' | '`' | '#' | '_'))
        .collect();
    let trimmed = text.trim();
    let trimmed = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("• "))
        .unwrap_or(trimmed)
        .trim();
    text = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    text.chars().any(char::is_alphanumeric).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds the text in pieces of `size` characters; returns the sentences released while
    /// streaming, and the last one.
    fn stream(text: &str, size: usize) -> (Vec<String>, Option<String>) {
        let mut s = Splitter::new();
        let chars: Vec<char> = text.chars().collect();
        let mut out = Vec::new();
        for piece in chars.chunks(size) {
            out.extend(s.push(&piece.iter().collect::<String>()));
        }
        (out, s.finish())
    }

    #[test]
    fn sentences_are_released_when_the_next_one_starts() {
        let mut s = Splitter::new();
        assert!(s.push("Bonjour Léa.").is_empty());
        assert!(s.push(" ").is_empty());
        assert_eq!(s.push("Tu"), ["Bonjour Léa."]);
        assert!(s.push(" vas bien ?").is_empty());
        assert_eq!(s.finish().as_deref(), Some("Tu vas bien ?"));
    }

    #[test]
    fn the_cut_does_not_depend_on_the_size_of_the_pieces() {
        let text =
            "Oui. Le contrat finit le quatorze mars ! Tu veux un rappel ? D'accord… On verra.";
        for size in [1, 2, 3, 7, 1000] {
            let (out, last) = stream(text, size);
            assert_eq!(
                out,
                [
                    "Oui.",
                    "Le contrat finit le quatorze mars !",
                    "Tu veux un rappel ?",
                    "D'accord…"
                ],
                "size {size}"
            );
            assert_eq!(last.as_deref(), Some("On verra."));
        }
    }

    #[test]
    fn the_last_sentence_is_held_even_with_trailing_spaces() {
        let (out, last) = stream("Première. Dernière.\n\n", 4);
        assert_eq!(out, ["Première."]);
        assert_eq!(last.as_deref(), Some("Dernière."));
    }

    #[test]
    fn numbers_abbreviations_and_quotes_do_not_cut() {
        let (out, last) = stream("Il fait 3.5 degrés. M. Dupont a dit « oui. » Voilà.", 3);
        assert_eq!(out, ["Il fait 3.5 degrés.", "M. Dupont a dit « oui. »"]);
        assert_eq!(last.as_deref(), Some("Voilà."));
    }

    #[test]
    fn line_breaks_and_formatting_are_cleaned() {
        let (out, last) = stream("**Bien sûr**\n- la `musique`\n\n# Fin", 5);
        assert_eq!(out, ["Bien sûr", "la musique"]);
        assert_eq!(last.as_deref(), Some("Fin"));
    }

    #[test]
    fn nothing_to_say_gives_nothing() {
        let (out, last) = stream("  \n * ", 2);
        assert!(out.is_empty() && last.is_none());
    }

    #[test]
    fn a_long_sentence_without_an_end_is_cut_at_a_comma() {
        let long = format!("{}, et puis {}", "a".repeat(200), "b".repeat(30));
        let (out, last) = stream(&long, 10);
        assert_eq!(out, [format!("{},", "a".repeat(200))]);
        assert_eq!(last, Some(format!("et puis {}", "b".repeat(30))));
    }
}
