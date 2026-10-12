//! The corrections table, applied after the transcription (`specs/hearing.md`).
//!
//! A TSV file, one rule per line: `pattern<TAB>replacement`. The pattern is a regular expression
//! (`a|b` for several forms), searched case-insensitively and on whole words. Empty lines and
//! those starting with `#` (possibly after spaces) are ignored, and so is a UTF-8 byte order mark
//! at the start of the file. The rules apply in the order of the file: « chris net » must come
//! before « chris ».
//!
//! **Escape mode** (for entries that are generated, from the titles of Hippocampe for instance): a
//! pattern that starts with `=` is a literal string, not a regex (`=C++` is the text « C++ »).
//!
//! **Word boundaries** are only asked on the sides where the pattern starts or ends with a letter,
//! a digit or `_`: « C++ » has none on its right, where `\b` could never match. For a regex, that
//! is read from the pattern as it is written: begin or end it with something else (a group, a
//! class) and write the `\b` yourself if you want it.
//!
//! The table and the text are both normalised to NFC, so that an accent written as a letter and a
//! combining mark still matches.
//!
//! It must only correct forms that are not French words: « trafic » or « chris » would be
//! rewritten otherwise.

use std::path::Path;

use anyhow::{Context, Result, bail};
use regex::{NoExpand, Regex};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug)]
struct Rule {
    pattern: Regex,
    replacement: String,
}

#[derive(Debug, Default)]
pub struct Corrections {
    rules: Vec<Rule>,
}

impl Corrections {
    pub fn read(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_tsv(&text).with_context(|| format!("table {}", path.display()))
    }

    pub fn from_tsv(text: &str) -> Result<Self> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut rules = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let Some((pattern, replacement)) = line.split_once('\t') else {
                bail!("line {}: pattern and replacement separated by a tab", i + 1);
            };
            let pattern: String = pattern.trim().nfc().collect();
            let pattern = Self::regex(&pattern)
                .with_context(|| format!("line {}: invalid pattern", i + 1))?;
            rules.push(Rule {
                pattern,
                replacement: replacement.trim().nfc().collect(),
            });
        }
        Ok(Self { rules })
    }

    /// The regex of a pattern line: `=literal`, or a regular expression, with word boundaries on
    /// the sides that start or end with a word character.
    fn regex(pattern: &str) -> Result<Regex> {
        let (source, literal) = match pattern.strip_prefix('=') {
            Some(literal) => (regex::escape(literal), literal),
            None => (pattern.to_owned(), pattern),
        };
        if literal.is_empty() {
            bail!("empty pattern");
        }
        let is_word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        let left = if is_word(literal.chars().next()) {
            r"\b"
        } else {
            ""
        };
        let right = if is_word(literal.chars().next_back()) {
            r"\b"
        } else {
            ""
        };
        Ok(Regex::new(&format!("(?i){left}(?:{source}){right}"))?)
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn apply(&self, text: &str) -> String {
        let mut text: String = text.nfc().collect();
        for rule in &self.rules {
            text = rule
                .pattern
                .replace_all(&text, NoExpand(&rule.replacement))
                .into_owned();
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Corrections {
        Corrections::from_tsv(
            "# pattern\treplacement\n\
             hippocamp|hypocamp\tHippocampe\n\
             \n\
             chris net\tkrisnet\n\
             chris\tKris\n\
             émery\tEmory\n",
        )
        .unwrap()
    }

    #[test]
    fn corrects_regardless_of_case() {
        assert_eq!(
            table().apply("Hippocamp et HYPOCAMP"),
            "Hippocampe et Hippocampe"
        );
    }

    #[test]
    fn only_corrects_whole_words() {
        let t = table();
        assert_eq!(t.apply("christophe"), "christophe");
        assert_eq!(t.apply("anti-chris, chris."), "anti-Kris, Kris.");
        assert_eq!(t.apply("hippocampes"), "hippocampes");
    }

    #[test]
    fn the_rules_apply_in_the_order_of_the_file() {
        assert_eq!(
            table().apply("sur chris net et chris"),
            "sur krisnet et Kris"
        );
    }

    #[test]
    fn accented_letters_are_letters() {
        let t = table();
        assert_eq!(t.apply("l'émery"), "l'Emory");
        assert_eq!(t.apply("rémery"), "rémery");
    }

    #[test]
    fn the_replacement_is_taken_as_it_is() {
        let t = Corrections::from_tsv("prix\t$1 et \\0\n").unwrap();
        assert_eq!(t.apply("le prix"), "le $1 et \\0");
    }

    #[test]
    fn comment_lines_may_be_indented() {
        let t =
            Corrections::from_tsv("  # pattern\treplacement\n\t# another\nchris\tKris\n").unwrap();
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn a_byte_order_mark_at_the_start_is_ignored() {
        // Without it, the BOM would stick to the first pattern and it would never match.
        let t = Corrections::from_tsv("\u{feff}chris\tKris\n").unwrap();
        assert_eq!(t.apply("chris"), "Kris");
        let t = Corrections::from_tsv("\u{feff}# comment\nchris\tKris\n").unwrap();
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn the_table_and_the_text_are_compared_in_nfc() {
        let decomposed = "e\u{301}mery";
        let composed = "\u{e9}mery";
        // A decomposed text meets a composed table, and the other way round.
        assert_eq!(table().apply(&format!("l'{decomposed}")), "l'Emory");
        let t = Corrections::from_tsv(&format!("{decomposed}\tEmory\n")).unwrap();
        assert_eq!(t.apply(&format!("l'{composed}")), "l'Emory");
        // The text that no rule touches comes out composed.
        assert_eq!(t.apply("cafe\u{301}"), "caf\u{e9}");
    }

    #[test]
    fn an_entry_starting_with_equals_is_a_literal_string() {
        let t = Corrections::from_tsv("=a.b\tX\n=C++\tCpp\n=(neo4j)\tNeo4j\n").unwrap();
        assert_eq!(t.apply("a.b et axb"), "X et axb");
        assert_eq!(t.apply("du C++ moderne"), "du Cpp moderne");
        assert_eq!(t.apply("le (neo4j) ici"), "le Neo4j ici");
        // Not a regex: `|` is a character, and a pattern that would not compile is fine.
        let t = Corrections::from_tsv("=a|b\tX\n=(ouvert\tY\n").unwrap();
        assert_eq!(t.apply("a|b a b (ouvert"), "X a b Y");
    }

    #[test]
    fn a_regex_stays_a_regex_even_when_it_contains_an_equals_sign() {
        let t = Corrections::from_tsv("a=b|c\tX\n").unwrap();
        assert_eq!(t.apply("a=b c"), "X X");
    }

    #[test]
    fn word_boundaries_are_only_where_the_pattern_has_a_word_character() {
        let t = Corrections::from_tsv("=C++\tCpp\n=.net\tdotnet\n").unwrap();
        // « C++ » ends with `+`: it is replaced before a letter, but not inside a word on its left.
        assert_eq!(
            t.apply("C++, C++ ou C++17 mais pas ABC++"),
            "Cpp, Cpp ou Cpp17 mais pas ABC++"
        );
        // « .net » starts with `.`: nothing is asked on its left, but it ends on a word.
        assert_eq!(
            t.apply("asp.net et .net, mais .network"),
            "aspdotnet et dotnet, mais .network"
        );
    }

    #[test]
    fn an_empty_pattern_is_refused() {
        assert!(Corrections::from_tsv("=\tx\n").is_err());
        assert!(Corrections::from_tsv("\tx\n").is_err());
    }

    #[test]
    fn the_example_table_brings_emory_back_and_only_there() {
        let t = Corrections::from_tsv(include_str!("../examples/corrections.tsv")).unwrap();
        assert_eq!(
            t.apply("Parle-moi des mains des Maury."),
            "Parle-moi des mains d'Emory."
        );
        assert_eq!(t.apply("les mains d'Emory"), "les mains d'Emory");
        assert_eq!(t.apply("et maury, raconte"), "Emory, raconte");
        // Maury alone, or in another word, stays.
        assert_eq!(
            t.apply("le vin de Maury et les desmaury"),
            "le vin de Maury et les desmaury"
        );
    }

    #[test]
    fn an_empty_table_changes_nothing() {
        let t = Corrections::from_tsv("").unwrap();
        assert!(t.is_empty());
        assert_eq!(t.apply("rien à faire"), "rien à faire");
    }

    #[test]
    fn a_line_without_a_tab_or_an_invalid_pattern_is_refused() {
        assert!(Corrections::from_tsv("sans tabulation\n").is_err());
        assert!(Corrections::from_tsv("(ouvert\tx\n").is_err());
    }
}
