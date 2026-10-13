//! The text before the synthesis: the whitespace, then the pronunciation lexicon.
//!
//! A French engine reads « GitHub » as « jithub » and « SSH » as a word. The stage that fixes it
//! (`specs/voice.md`, « Le lexique de prononciation ») is made of two rules, applied to every
//! sentence, whatever the engine:
//!
//! 1. **The lexicon**: words (or any text) replaced by how they must be written to be read right,
//!    `GitHub` → « guite-heub ». Whole words only, longest entry first, and the punctuation around
//!    a word stays. An entry wins over rule 2.
//! 2. **The spelled acronyms**: a word of 2 to 6 capitals (digits allowed: « MP3 », « 4G »), which
//!    no entry covers and which is not in the list of acronyms read as words, is spelled with the
//!    French names of the letters and digits: « SSH » → « èsse-èsse-ache ».
//!
//! Neither rule rescans what the other wrote, and a word inside another (« GitHubber », « SSHD ») is
//! never touched. The file format is described in `data/lexicon.fr.toml`, the built-in lexicon.

use std::{collections::HashMap, collections::HashSet, path::Path};

use anyhow::{Context, bail};
use serde::Deserialize;

/// The lexicon shipped with the program: used when the configuration gives no file.
pub const BUILTIN: &str = include_str!("../data/lexicon.fr.toml");

/// The most letters (and digits) of a spelled acronym; a longer word is a word.
const MAX_SPELLED: usize = 6;

/// The names of the letters A to Z, as written for the engine to read them best (see
/// `specs/voice.md`). The lexicon file can change any of them with `[letters]`.
const LETTERS: [&str; 26] = [
    "a",
    "bé",
    "cé",
    "dé",
    "eu",
    "èf",
    "jé",
    "ache",
    "i",
    "ji",
    "ka",
    "èl",
    "èm",
    "èn",
    "o",
    "pé",
    "ku",
    "èr",
    "èsse",
    "té",
    "u",
    "vé",
    "double vé",
    "ix",
    "i-grec",
    "zède",
];

/// What joins the names of the letters of a spelled acronym: « èsse-èsse-ache ». Measured with
/// Pocket (see `specs/voice.md`): joined by hyphens the letters are read as an acronym, at the pace
/// of a person spelling; with spaces two equal letters merge and the names are heard as words
/// (« a pé i » as « à payer »); with commas each letter gets a pause and the acronym is slow.
const SEPARATOR: &str = "-";

/// The names of the digits 0 to 9.
const DIGITS: [&str; 10] = [
    "zéro", "un", "deux", "trois", "quatre", "cinq", "six", "sept", "huit", "neuf",
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    key: String,
    spoken: String,
    /// Matches whatever the case of the letters.
    any_case: bool,
    /// The key is an acronym (2 to 6 capitals or digits): `key` + `s` matches too, and the `s`
    /// is dropped (the plural of an acronym is silent).
    plural: bool,
}

/// What the file says: see `data/lexicon.fr.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct File {
    not_spelled: Vec<String>,
    letters: HashMap<String, String>,
    replace: HashMap<String, String>,
    replace_any_case: HashMap<String, String>,
}

/// The pronunciation lexicon and the acronym rule, ready to apply.
#[derive(Clone, Debug, Default)]
pub struct Lexicon {
    /// Off (the default value) leaves the words alone.
    enabled: bool,
    /// Longest key first, case-sensitive entries before the others of the same length.
    entries: Vec<Entry>,
    /// The acronyms read as words, never spelled.
    not_spelled: HashSet<String>,
    letters: HashMap<char, String>,
}

impl Lexicon {
    /// The stage switched off: only the whitespace is cleaned.
    pub fn disabled() -> Self {
        Self::default()
    }

    /// The lexicon shipped with the program.
    pub fn builtin() -> anyhow::Result<Self> {
        Self::parse(BUILTIN).context("the built-in lexicon")
    }

    /// Reads a lexicon file.
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("lexicon {}", path.display()))
    }

    /// Parses the text of a lexicon file.
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let file: File = toml::from_str(text)?;
        let mut letters: HashMap<char, String> = ('A'..='Z')
            .zip(LETTERS)
            .chain(('0'..='9').zip(DIGITS))
            .map(|(c, name)| (c, name.to_owned()))
            .collect();
        for (key, spoken) in file.letters {
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                bail!("[letters] {key:?}: a key is one capital letter or one digit");
            };
            if !c.is_ascii_uppercase() && !c.is_ascii_digit() {
                bail!("[letters] {key:?}: a key is one capital letter or one digit");
            }
            check_spoken("letters", &key, &spoken)?;
            letters.insert(c, spoken.trim().to_owned());
        }

        let mut not_spelled = HashSet::new();
        for word in file.not_spelled {
            if word.is_empty() || !word.chars().all(is_word_char) {
                bail!("not_spelled: {word:?} is not a single word");
            }
            not_spelled.insert(word);
        }

        let mut entries = Vec::new();
        for (table, any_case, map) in [
            ("replace", false, file.replace),
            ("replace_any_case", true, file.replace_any_case),
        ] {
            for (key, spoken) in map {
                check_spoken(table, &key, &spoken)?;
                // Whitespace inside a key is a single space: the text is cleaned the same way.
                let key = key.split_whitespace().collect::<Vec<_>>().join(" ");
                if key.is_empty() {
                    bail!("[{table}] an entry has an empty word");
                }
                entries.push(Entry {
                    plural: is_acronym(&key),
                    key,
                    spoken: spoken.trim().to_owned(),
                    any_case,
                });
            }
        }
        // The longest first, so « Wi-Fi » wins over « Wi »; then the case-sensitive ones; then the
        // alphabetical order, so that a file is read the same on every run (a map has no order).
        entries.sort_by_key(|e| {
            (
                std::cmp::Reverse(e.key.chars().count()),
                e.any_case,
                e.key.clone(),
            )
        });
        Ok(Self {
            enabled: true,
            entries,
            not_spelled,
            letters,
        })
    }

    /// `true` when the stage does something.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// The number of entries of the lexicon (without the acronyms read as words).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The text as the engine must read it: the entries, then the spelled acronyms. The text is
    /// returned as it is when the stage is off.
    pub fn apply(&self, text: &str) -> String {
        if !self.enabled {
            return text.to_owned();
        }
        let mut out = String::with_capacity(text.len() + 16);
        let mut i = 0;
        let mut prev: Option<char> = None;
        while i < text.len() {
            let rest = &text[i..];
            let at_word_start = !prev.is_some_and(is_word_char);
            if let Some((len, spoken)) = self.entry_at(rest, prev) {
                out.push_str(spoken);
                i += len;
                prev = rest[..len].chars().next_back();
                continue;
            }
            let c = rest.chars().next().expect("not at the end");
            if at_word_start && is_word_char(c) {
                let len = rest.find(|c| !is_word_char(c)).unwrap_or(rest.len());
                if let Some(spelled) = self.spell(&rest[..len]) {
                    out.push_str(&spelled);
                    i += len;
                    prev = rest[..len].chars().next_back();
                    continue;
                }
                // Not an acronym: the word goes through as it is, in one piece, so that nothing
                // starts in the middle of it.
                out.push_str(&rest[..len]);
                i += len;
                prev = rest[..len].chars().next_back();
                continue;
            }
            out.push(c);
            i += c.len_utf8();
            prev = Some(c);
        }
        out
    }

    /// The entry that matches at the start of `rest` (preceded by `prev`), and how many bytes of
    /// `rest` it covers.
    fn entry_at(&self, rest: &str, prev: Option<char>) -> Option<(usize, &str)> {
        let first = rest.chars().next()?;
        self.entries.iter().find_map(|entry| {
            let key_first = entry.key.chars().next()?;
            let key_last = entry.key.chars().next_back()?;
            if is_word_char(key_first) && prev.is_some_and(is_word_char) {
                return None;
            }
            // A cheap test before the comparison.
            if !entry.any_case && first != key_first {
                return None;
            }
            let len = matched_len(&entry.key, rest, entry.any_case)?;
            let next = rest[len..].chars().next();
            if !is_word_char(key_last) || !next.is_some_and(is_word_char) {
                return Some((len, entry.spoken.as_str()));
            }
            // « PCs »: the plural of an acronym.
            let after = &rest[len..];
            if entry.plural
                && after.starts_with('s')
                && !after[1..].chars().next().is_some_and(is_word_char)
            {
                return Some((len + 1, entry.spoken.as_str()));
            }
            None
        })
    }

    /// An acronym (« SSH », « PCs ») in letters, or `None` when `word` is not one or is read as a
    /// word. `word` is a whole word (a run of letters, digits and underscores).
    fn spell(&self, word: &str) -> Option<String> {
        let base = match word.strip_suffix('s') {
            Some(base) if is_acronym(base) => base,
            _ => word,
        };
        if !is_acronym(base) || self.not_spelled.contains(base) {
            return None;
        }
        let names: Vec<&str> = base.chars().map(|c| self.letters[&c].as_str()).collect();
        Some(names.join(SEPARATOR))
    }
}

/// A character of a word: what an entry or an acronym cannot start or end in the middle of.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 2 to 6 capitals A to Z or digits, with at least one capital. A word that starts with several
/// digits (« 10H », « 20KG ») is a quantity, not an acronym; « 4G » and « MP3 » are.
fn is_acronym(word: &str) -> bool {
    let len = word.len();
    (2..=MAX_SPELLED).contains(&len)
        && word
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && word.bytes().any(|b| b.is_ascii_uppercase())
        && !word.as_bytes()[..2].iter().all(u8::is_ascii_digit)
}

/// How many bytes of `text` the `key` covers at the start of `text`, if it does.
fn matched_len(key: &str, text: &str, any_case: bool) -> Option<usize> {
    let mut chars = text.char_indices();
    for k in key.chars() {
        let (_, t) = chars.next()?;
        let same = if any_case {
            k.to_lowercase().eq(t.to_lowercase())
        } else {
            k == t
        };
        if !same {
            return None;
        }
    }
    Some(chars.next().map_or(text.len(), |(i, _)| i))
}

fn check_spoken(table: &str, key: &str, spoken: &str) -> anyhow::Result<()> {
    if spoken.trim().is_empty() {
        bail!("[{table}] {key:?}: the written form is empty");
    }
    if spoken.chars().any(char::is_control) {
        bail!("[{table}] {key:?}: the written form holds a control character");
    }
    Ok(())
}

/// Prepares a sentence for the engine: the whitespace, then the lexicon.
pub fn clean(text: &str, lexicon: &Lexicon) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    lexicon.apply(&text)
}

#[cfg(test)]
mod tests {
    use super::{BUILTIN, Lexicon, clean};

    fn lexicon(file: &str) -> Lexicon {
        Lexicon::parse(file).unwrap()
    }

    fn bare() -> Lexicon {
        lexicon("")
    }

    #[test]
    fn collapses_whitespace() {
        let off = Lexicon::disabled();
        assert_eq!(clean("  Bonjour \n  Léa\t! ", &off), "Bonjour Léa !");
    }

    #[test]
    fn blank_text_is_empty() {
        assert_eq!(clean(" \n\t ", &Lexicon::disabled()), "");
        assert_eq!(clean(" \n\t ", &bare()), "");
        assert_eq!(clean("", &bare()), "");
    }

    #[test]
    fn a_disabled_stage_leaves_the_words_alone() {
        assert_eq!(
            clean("MCP et Neo4j : 12 %", &Lexicon::disabled()),
            "MCP et Neo4j : 12 %"
        );
        assert!(!Lexicon::disabled().is_enabled());
    }

    #[test]
    fn acronyms_are_spelled() {
        let l = bare();
        assert_eq!(l.apply("SSH"), "èsse-èsse-ache");
        assert_eq!(l.apply("Je lance MCP."), "Je lance èm-cé-pé.");
        assert_eq!(l.apply("(API) et API,"), "(a-pé-i) et a-pé-i,");
        assert_eq!(l.apply("HTTPS"), "ache-té-té-pé-èsse");
        assert_eq!(l.apply("OK"), "o-ka");
    }

    #[test]
    fn acronyms_with_digits_say_the_digits() {
        let l = bare();
        assert_eq!(l.apply("MP3"), "èm-pé-trois");
        assert_eq!(l.apply("la 4G"), "la quatre-jé");
        assert_eq!(l.apply("H264"), "ache-deux-six-quatre");
        assert_eq!(l.apply("A4"), "a-quatre");
    }

    #[test]
    fn what_is_not_an_acronym_is_left_alone() {
        let l = bare();
        // One letter, seven letters, numbers, mixed case, digits only.
        for text in [
            "A", "I", "WYSIWYG", "2024", "42", "12 %", "iPhone", "Neo4j", "Ssh", "sSH", "MAX_SIZE",
            "_SSH", "SSH_", "10H", "20KG", "ÉTÉ", "ÀB",
        ] {
            assert_eq!(l.apply(text), text, "{text}");
        }
    }

    #[test]
    fn a_plural_acronym_loses_its_s() {
        let l = bare();
        assert_eq!(l.apply("des PCs."), "des pé-cé.");
        assert_eq!(l.apply("les APIs"), "les a-pé-i");
        // Not a plural: an upper case S is a letter of the acronym, a longer word is a word.
        assert_eq!(l.apply("PCS"), "pé-cé-èsse");
        assert_eq!(l.apply("PCsx"), "PCsx");
    }

    #[test]
    fn acronyms_read_as_words_are_not_spelled() {
        let l = lexicon(r#"not_spelled = ["NASA", "RAM"]"#);
        assert_eq!(
            l.apply("La NASA et la RAM, pas le CPU."),
            "La NASA et la RAM, pas le cé-pé-u."
        );
        assert_eq!(l.apply("des RAMs"), "des RAMs");
    }

    #[test]
    fn the_letters_can_be_changed() {
        let l = lexicon("[letters]\nS = \"ès\"\n4 = \"quatre !\"\n");
        assert_eq!(l.apply("SSH 4G"), "ès-ès-ache quatre !-jé");
    }

    #[test]
    fn an_entry_is_replaced_with_its_punctuation() {
        let l = lexicon("[replace]\nGitHub = \"guite-heub\"\n");
        assert_eq!(l.apply("GitHub."), "guite-heub.");
        assert_eq!(
            l.apply("(GitHub), « GitHub » et l'GitHub"),
            "(guite-heub), « guite-heub » et l'guite-heub"
        );
        assert_eq!(l.apply("GitHub"), "guite-heub");
    }

    #[test]
    fn an_entry_wins_over_the_acronym_rule() {
        let l = lexicon("[replace]\nJSON = \"djéïzone\"\nOK = \"okay\"\n");
        assert_eq!(
            l.apply("Du JSON, OK, et du XML."),
            "Du djéïzone, okay, et du ix-èm-èl."
        );
    }

    #[test]
    fn the_longest_entry_wins() {
        let l = lexicon(
            "[replace]\nWi = \"ouïe\"\n\"Wi-Fi\" = \"waïe-faïe\"\n\"Wi-Fi 6\" = \"waïe-faïe six\"\n",
        );
        assert_eq!(
            l.apply("Wi-Fi 6 et Wi-Fi et Wi"),
            "waïe-faïe six et waïe-faïe et ouïe"
        );
    }

    #[test]
    fn entries_are_case_sensitive_unless_asked() {
        let l = lexicon(
            "[replace]\nLinux = \"linuxe\"\n[replace_any_case]\nyoutube = \"you-tioube\"\n",
        );
        assert_eq!(l.apply("Linux, linux"), "linuxe, linux");
        assert_eq!(
            l.apply("YouTube, youtube, YOUTUBE"),
            "you-tioube, you-tioube, you-tioube"
        );
    }

    #[test]
    fn a_word_inside_another_is_untouched() {
        let l = lexicon("[replace]\nGit = \"guite\"\nSSH = \"ès\"\n");
        for text in [
            "GitHub", "Gitan", "agit", "xSSH", "SSHd", "Git_x", "_Git", "SSH_",
        ] {
            assert_eq!(l.apply(text), text, "{text}");
        }
        // The acronym rule still reads « SSH2 » as an acronym, and the lexicon is not asked.
        assert_eq!(l.apply("SSH2"), "èsse-èsse-ache-deux");
        assert_eq!(l.apply("Git-hub"), "guite-hub");
    }

    #[test]
    fn an_entry_that_is_not_a_word_matches_where_it_stands() {
        let l = lexicon("[replace]\n\"C++\" = \"cé plus plus\"\n\".NET\" = \"dot nète\"\n");
        assert_eq!(l.apply("Du C++, du .NET."), "Du cé plus plus, du dot nète.");
        // Inside a word, an entry does not start: « ABC » is spelled, then the plusses stay.
        assert_eq!(l.apply("ABC++"), "a-bé-cé++");
    }

    #[test]
    fn the_plural_of_an_acronym_entry_loses_its_s() {
        let l = lexicon("[replace]\nPC = \"péssé\"\n[replace_any_case]\nAPI = \"apî\"\n");
        assert_eq!(l.apply("des PCs, un PC"), "des péssé, un péssé");
        assert_eq!(l.apply("des APIs, des apis"), "des apî, des apî");
        // An entry that is not an acronym has no plural.
        let l = lexicon("[replace]\nGit = \"guite\"\n");
        assert_eq!(l.apply("des Gits"), "des Gits");
    }

    #[test]
    fn an_entry_is_not_rescanned() {
        let l = lexicon("[replace]\nSSH = \"SSH\"\nA = \"B\"\nB = \"A\"\n");
        assert_eq!(l.apply("SSH A B"), "SSH B A");
    }

    #[test]
    fn a_key_with_a_space_matches_a_cleaned_text() {
        let l = lexicon("[replace]\n\"Google  Home\" = \"gougueule homme\"\n");
        assert_eq!(clean("Mon  Google \n Home.", &l), "Mon gougueule homme.");
    }

    #[test]
    fn accents_and_case_folding_do_not_break_the_comparison() {
        let l = lexicon("[replace_any_case]\n\"École\" = \"écol\"\n");
        assert_eq!(l.apply("école, ÉCOLE, Écoles"), "écol, écol, Écoles");
    }

    #[test]
    fn numbers_are_untouched() {
        let l = lexicon("[replace]\nOK = \"okay\"\n");
        assert_eq!(
            l.apply("Il est 12 h 30, soit 3,5 % de 2024."),
            "Il est 12 h 30, soit 3,5 % de 2024."
        );
    }

    #[test]
    fn malformed_files_say_what_is_wrong() {
        for (bad, hint) in [
            ("replace = 3", "invalid type"),
            ("[replace]\nA = 3\n", "expected a string"),
            ("[replace]\nGit = \"\"\n", "empty"),
            ("[replace]\n\"\" = \"x\"\n", "empty"),
            ("[replace]\n\"  \" = \"x\"\n", "empty"),
            ("[replace]\nGit = \"a\\nb\"\n", "control"),
            ("[letters]\nAB = \"x\"\n", "one capital"),
            ("[letters]\na = \"x\"\n", "one capital"),
            ("[letters]\nA = \" \"\n", "empty"),
            ("not_spelled = [\"two words\"]", "not a single word"),
            ("not_spelled = [\"\"]", "not a single word"),
            ("not_spelled = \"NASA\"", "invalid type"),
            ("unknown = 1", "unknown field"),
            ("[replace\nA = \"x\"", "invalid"),
        ] {
            let error = format!("{:#}", Lexicon::parse(bad).unwrap_err());
            assert!(error.contains(hint), "{bad:?}: {error}");
        }
    }

    #[test]
    fn the_built_in_lexicon_parses_and_does_its_job() {
        let l = Lexicon::builtin().unwrap();
        assert!(l.is_enabled() && !l.is_empty());
        assert!(
            BUILTIN.starts_with('#'),
            "a French header explains the file"
        );
        assert_eq!(l.apply("SSH"), "èsse-èsse-ache");
        assert_ne!(l.apply("GitHub"), "GitHub");
        assert_eq!(l.apply("la NASA"), "la NASA");
        assert_eq!(l.apply("Bonjour, 12 % de 2024."), "Bonjour, 12 % de 2024.");
        // A line of the built-in lexicon never produces a word that the stage would change again.
        for entry in &l.entries {
            assert_eq!(l.apply(&entry.spoken), entry.spoken, "{}", entry.key);
        }
    }

    #[test]
    fn an_unreadable_file_is_an_error_with_its_path() {
        let error = format!(
            "{:#}",
            Lexicon::read(std::path::Path::new("/nonexistent/lexicon.toml")).unwrap_err()
        );
        assert!(error.contains("/nonexistent/lexicon.toml"), "{error}");
    }
}
