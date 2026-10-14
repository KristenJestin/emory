//! Cuts the text of the LLM into sentences as it streams in, so that each one can be said at once.
//!
//! A sentence ends at `.`, `!`, `?` or `…` (closing quotes and brackets included) followed by a
//! space, or at a line break. It is released as soon as that space arrives: nothing waits for the
//! next sentence, so the voice can start while the LLM calls a tool or thinks. The end of the
//! answer is not carried by the last sentence: what is pending when the stream pauses (a tool
//! call) or ends is taken by [`Splitter::flush`], and the end is signalled on its own.
//!
//! A dot that may not end a sentence waits for the next letter: after a number (« le 3. » or
//! « 3. Puis »), or after « etc. ». A lower-case letter next means the sentence goes on. A number
//! with its dot at the start of a line is a list item, never an end. A dot with no space after it
//! (« 3.5 », « 1.000 », « v0.7 », an address) never cuts. A sentence that runs on without an end is
//! cut at a comma once it is long, so that the voice does not wait for it.

/// Beyond this many characters, a sentence without an end is cut at its last comma.
const LONG: usize = 220;

/// Abbreviations whose dot does not end a sentence.
const ABBREVIATIONS: [&str; 5] = ["M", "Mme", "Mlle", "Dr", "St"];

/// Words whose dot ends a sentence only if what follows does not go on in lower case.
const AMBIGUOUS: [&str; 1] = ["etc"];

#[derive(Debug, Default)]
pub struct Splitter {
    pending: String,
}

/// What a punctuation mark says of the end of its sentence.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// Not an end.
    No,
    /// An end, once a space follows.
    End,
    /// An end once a space follows, unless the next letter is in lower case.
    Maybe,
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

    /// The stream pauses (a tool call) or is over: what is pending leaves as it is, even without
    /// its end, if it says anything.
    pub fn flush(&mut self) -> Option<String> {
        clean(&std::mem::take(&mut self.pending))
    }

    /// The byte where the first finished sentence of `pending` ends, if any: right after its
    /// punctuation, once a space follows it.
    fn boundary(&self) -> Option<usize> {
        let text = &self.pending;
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let mut i = 0;
        while i < chars.len() {
            let (at, c) = chars[i];
            let mark = match c {
                '.' => dot(&text[..at]),
                '!' | '?' | '…' => Mark::End,
                // A line break ends a sentence at once.
                '\n' => return Some(at + 1),
                _ => Mark::No,
            };
            if mark == Mark::No {
                i += 1;
                continue;
            }
            // Closing marks stay with the sentence.
            let mut j = i + 1;
            let mut quoted = open_quote(&text[..at]);
            loop {
                match chars.get(j).map(|&(_, c)| c) {
                    Some('»') => {
                        quoted = false;
                        j += 1;
                    }
                    Some('"' | '\'' | ')' | '’' | '.') => j += 1,
                    // French puts a space before the closing guillemet: inside a quotation, the
                    // space may be followed by it, so wait for the next character.
                    Some(' ' | '\u{a0}' | '\u{202f}') if quoted => {
                        match chars.get(j + 1).map(|&(_, c)| c) {
                            Some('»') => {
                                quoted = false;
                                j += 2;
                            }
                            Some(_) => break,
                            None => return None,
                        }
                    }
                    _ => break,
                }
            }
            let end = chars.get(j).map_or(text.len(), |&(b, _)| b);
            let mut k = j;
            while k < chars.len() && chars[k].1.is_whitespace() {
                k += 1;
            }
            if k == j {
                if k >= chars.len() {
                    // Nothing after it yet: wait for more text.
                    return None;
                }
                // No space after it (« 3.5 »): the sentence goes on.
                i = j;
                continue;
            }
            if mark == Mark::End {
                return Some(end);
            }
            match chars.get(k) {
                None => return None,
                Some(&(_, next)) if next.is_lowercase() => i = k,
                Some(_) => return Some(end),
            }
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

/// What a dot ends, from the text before it.
fn dot(before: &str) -> Mark {
    let word = last_word(before);
    if ABBREVIATIONS.contains(&word) {
        return Mark::No;
    }
    if !word.is_empty() && word.chars().all(|c| c.is_ascii_digit()) {
        // A number alone at the start of a line is a list item: « 1. Paris ».
        let line = before.rsplit('\n').next().unwrap_or(before);
        if line.trim() == word {
            return Mark::No;
        }
        return Mark::Maybe;
    }
    if AMBIGUOUS.contains(&word) {
        return Mark::Maybe;
    }
    Mark::End
}

/// Whether a « opened in this text is not closed yet.
fn open_quote(text: &str) -> bool {
    match (text.rfind('«'), text.rfind('»')) {
        (Some(open), Some(close)) => open > close,
        (open, _) => open.is_some(),
    }
}

/// The word right before the end of `before`.
fn last_word(before: &str) -> &str {
    before
        .rsplit(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or("")
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
    /// streaming, and what is left at the end.
    fn stream(text: &str, size: usize) -> (Vec<String>, Option<String>) {
        let mut s = Splitter::new();
        let chars: Vec<char> = text.chars().collect();
        let mut out = Vec::new();
        for piece in chars.chunks(size) {
            out.extend(s.push(&piece.iter().collect::<String>()));
        }
        (out, s.flush())
    }

    /// The sentences of a whole text, whatever the size of the pieces: all the same.
    fn sentences(text: &str) -> Vec<String> {
        let whole = {
            let (mut out, rest) = stream(text, 1000);
            out.extend(rest);
            out
        };
        for size in [1, 2, 3, 7] {
            let (mut out, rest) = stream(text, size);
            out.extend(rest);
            assert_eq!(out, whole, "size {size}");
        }
        whole
    }

    #[test]
    fn a_sentence_leaves_as_soon_as_a_space_follows_its_end() {
        let mut s = Splitter::new();
        assert!(s.push("Bonjour Léa.").is_empty());
        assert_eq!(s.push(" "), ["Bonjour Léa."]);
        assert!(s.push("Tu vas bien ?").is_empty());
        assert_eq!(s.push("\n"), ["Tu vas bien ?"]);
        assert!(s.flush().is_none());
    }

    #[test]
    fn a_transition_before_a_tool_call_leaves_before_the_tool() {
        let mut s = Splitter::new();
        assert_eq!(
            s.push("Je regarde sur Wikipédia. "),
            ["Je regarde sur Wikipédia."]
        );
        // Without its space, it is what a flush at the tool call takes.
        let mut s = Splitter::new();
        assert!(s.push("Je regarde sur Wikipédia.").is_empty());
        assert_eq!(s.flush().as_deref(), Some("Je regarde sur Wikipédia."));
        // Even without any end.
        assert!(s.push("Je regarde").is_empty());
        assert_eq!(s.flush().as_deref(), Some("Je regarde"));
        assert!(s.flush().is_none());
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
    fn trailing_spaces_release_the_last_sentence() {
        let (out, last) = stream("Première. Dernière.\n\n", 4);
        assert_eq!(out, ["Première.", "Dernière."]);
        assert!(last.is_none());
    }

    #[test]
    fn numbers_abbreviations_and_quotes_do_not_cut() {
        assert_eq!(
            sentences("Il fait 3.5 degrés. M. Dupont a dit « oui. » Voilà."),
            ["Il fait 3.5 degrés.", "M. Dupont a dit « oui. »", "Voilà."]
        );
        assert_eq!(
            sentences(
                "La ville a 1.000 habitants. La version v0.7 est sortie, voir exemple.org. Bien."
            ),
            [
                "La ville a 1.000 habitants.",
                "La version v0.7 est sortie, voir exemple.org.",
                "Bien."
            ]
        );
        assert_eq!(
            sentences("Mme. Martin et le Dr. Roy sont là. Fin."),
            ["Mme. Martin et le Dr. Roy sont là.", "Fin."]
        );
    }

    #[test]
    fn a_closing_guillemet_after_a_space_stays_with_its_sentence() {
        let mut s = Splitter::new();
        assert!(s.push("Il a dit « oui.").is_empty());
        assert!(s.push(" ").is_empty(), "the guillemet may come");
        assert!(s.push("»").is_empty());
        assert_eq!(s.push(" "), ["Il a dit « oui. »"]);
        // Outside a quotation, the space is enough.
        assert_eq!(s.push("« Non », dit-il. "), ["« Non », dit-il."]);
        // Inside one that does not close there, the next character decides.
        assert!(s.push("« Bon. ").is_empty());
        assert_eq!(s.push("Et"), ["« Bon."]);
    }

    #[test]
    fn a_dot_after_a_number_waits_for_the_next_letter() {
        let mut s = Splitter::new();
        assert!(s.push("On est le 3. ").is_empty(), "le 3. may go on");
        assert_eq!(s.push("Demain"), ["On est le 3."]);
        assert_eq!(
            sentences("Le 3. juin, il pleut. Il y en a 12. Voilà."),
            ["Le 3. juin, il pleut.", "Il y en a 12.", "Voilà."]
        );
        // At the end of the answer, the flush takes it.
        let (out, last) = stream("Il y en a 12. ", 3);
        assert!(out.is_empty());
        assert_eq!(last.as_deref(), Some("Il y en a 12."));
    }

    #[test]
    fn etc_ends_a_sentence_only_before_a_new_one() {
        assert_eq!(
            sentences("Des pommes, des poires, etc. et des noix. Des fruits, etc. Ensuite."),
            [
                "Des pommes, des poires, etc. et des noix.",
                "Des fruits, etc.",
                "Ensuite."
            ]
        );
    }

    #[test]
    fn a_numbered_list_is_cut_at_its_lines_not_at_its_numbers() {
        assert_eq!(
            sentences("Trois villes :\n1. Paris\n2. Lyon, au centre.\n3. Nice"),
            [
                "Trois villes :",
                "1. Paris",
                "2. Lyon, au centre.",
                "3. Nice"
            ]
        );
        // The number of an item leaves with it, even when the item waits.
        let mut s = Splitter::new();
        assert_eq!(s.push("Voici :\n1. "), ["Voici :"]);
        assert!(s.push("Paris").is_empty());
        assert_eq!(s.flush().as_deref(), Some("1. Paris"));
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
