//! The vocabulary: the names that Parakeet favours when it hesitates (`specs/hearing.md`).
//!
//! A text file, one word or phrase per line; empty lines and lines starting with `#` are ignored.
//! It lives with the user's data, outside the repository, and grows while the hearing runs: the
//! mind adds a word the user has confirmed ([`Vocabulary::add`]), and the next sentence is
//! transcribed with it. The words go to sherpa-onnx with each sentence (a stream with its own
//! hotwords), so nothing is reloaded.
//!
//! Hotwords only work with the beam search (`modified_beam_search`), which costs more than the
//! greedy search: with no word in the file, the greedy search stays.

use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use anyhow::{Context, Result, bail};

use crate::config::DecodingMethod;

/// The longest word or phrase that may be added, in characters.
pub const MAX_WORD: usize = 40;

/// The words of a hotwords file.
pub fn parse(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// The word of a line, without the boost score it may carry (`Syncthing :2.5`).
fn phrase(line: &str) -> &str {
    match line.rsplit_once(" :") {
        Some((phrase, score)) if score.parse::<f32>().is_ok() => phrase.trim(),
        _ => line,
    }
}

/// A word that may be added: one line, at most [`MAX_WORD`] characters, letters (any alphabet),
/// digits, spaces, hyphens, apostrophes and dots, with at least one letter. Not `/`, which
/// separates the words given to sherpa-onnx, nor `:`, which starts a score, nor `#` first.
pub fn validate(word: &str) -> Result<String, String> {
    // A newline would make two words of one, and a tab or any other control has no business in
    // a name; only the spaces around and between the words are tidied.
    if word.chars().any(char::is_control) {
        return Err("le mot contient un saut de ligne ou un caractère de contrôle".to_owned());
    }
    let word = word.split_whitespace().collect::<Vec<_>>().join(" ");
    if word.is_empty() {
        return Err("le mot est vide".to_owned());
    }
    if word.chars().count() > MAX_WORD {
        return Err(format!("le mot dépasse {MAX_WORD} caractères"));
    }
    if !word.chars().any(char::is_alphabetic) {
        return Err("le mot n'a aucune lettre".to_owned());
    }
    let allowed = |c: char| c.is_alphanumeric() || matches!(c, ' ' | '-' | '\'' | '’' | '.');
    if let Some(c) = word.chars().find(|&c| !allowed(c)) {
        return Err(format!("caractère refusé dans le mot : {c:?}"));
    }
    Ok(word)
}

/// What [`Vocabulary::add`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Added {
    /// Written to the file.
    New,
    /// It was there already (whatever its case): nothing written.
    Known,
}

/// The vocabulary of the running hearing: its file, and the words as they are now.
pub struct Vocabulary {
    path: PathBuf,
    words: RwLock<Arc<Words>>,
    /// One writer at a time on the file.
    writing: Mutex<()>,
}

/// The words, and the form sherpa-onnx takes them in for a stream (separated by `/`).
#[derive(Debug, Default, PartialEq)]
pub struct Words {
    pub list: Vec<String>,
    pub joined: String,
}

impl Words {
    fn new(list: Vec<String>) -> Self {
        // A NUL would end the string sherpa-onnx is given (and panics on the way there).
        let joined = list
            .iter()
            .filter(|w| !w.contains('\0'))
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("/");
        Self { list, joined }
    }
}

impl Vocabulary {
    /// Reads the file. A file that does not exist yet is an empty vocabulary: the first word
    /// added creates it, with its folder.
    pub fn load(path: &Path) -> Result<Self> {
        let words = match std::fs::read_to_string(path) {
            Ok(text) => parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                return Err(e).with_context(|| format!("reading the hotwords {}", path.display()));
            }
        };
        Ok(Self {
            path: path.to_owned(),
            words: RwLock::new(Arc::new(Words::new(words))),
            writing: Mutex::new(()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The words now.
    pub fn words(&self) -> Arc<Words> {
        self.words
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Adds a word at the end of the file, and to the words in use. `heard_as`, what the
    /// transcription gave instead, goes on a comment line above it. A word already there, in any
    /// case, is not written again.
    pub fn add(&self, word: &str, heard_as: Option<&str>) -> Result<Added, String> {
        let word = validate(word)?;
        let _writing = self
            .writing
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = self.words();
        let same = |line: &String| phrase(line).to_lowercase() == word.to_lowercase();
        if current.list.iter().any(same) {
            return Ok(Added::Known);
        }
        self.append(&word, heard_as)
            .map_err(|e| format!("écriture du vocabulaire impossible : {e:#}"))?;
        let mut list = current.list.clone();
        list.push(word);
        *self
            .words
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(Words::new(list));
        Ok(Added::New)
    }

    fn append(&self, word: &str, heard_as: Option<&str>) -> Result<()> {
        if let Some(folder) = self.path.parent() {
            std::fs::create_dir_all(folder)
                .with_context(|| format!("creating {}", folder.display()))?;
        }
        // A file whose last line has no newline would glue the word to it.
        let ends_open = std::fs::read(&self.path)
            .map(|bytes| bytes.last().is_some_and(|&b| b != b'\n'))
            .unwrap_or(false);
        let mut text = String::new();
        if ends_open {
            text.push('\n');
        }
        let heard_as = heard_as
            .map(|h| h.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|h| !h.is_empty());
        if let Some(heard_as) = heard_as {
            let heard_as: String = heard_as.chars().take(80).collect();
            text.push_str(&format!(
                "# {} : entendu « {heard_as} »\n",
                chrono::Local::now().format("%Y-%m-%d")
            ));
        }
        text.push_str(word);
        text.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening {}", self.path.display()))?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_data())
            .with_context(|| format!("writing {}", self.path.display()))
    }
}

/// How Parakeet decodes.
#[derive(Debug, PartialEq, Eq)]
pub struct Decoding {
    pub method: DecodingMethod,
}

impl Decoding {
    /// Settles the method from the one asked for and the words there are at start.
    pub fn settle(requested: Option<DecodingMethod>, words: &[String]) -> Result<Self> {
        let method = match (requested, words.is_empty()) {
            (Some(DecodingMethod::GreedySearch), false) => bail!(
                "the hotwords need decoding_method = \"modified_beam_search\" \
                 (or no decoding_method, or no hotwords)"
            ),
            (Some(method), _) => method,
            (None, false) => DecodingMethod::ModifiedBeamSearch,
            (None, true) => DecodingMethod::GreedySearch,
        };
        Ok(Self { method })
    }

    /// Whether a word added now is used by the transcription before the next start.
    pub fn takes_words(&self) -> bool {
        self.method == DecodingMethod::ModifiedBeamSearch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let folder =
            std::env::temp_dir().join(format!("hearing-vocabulary-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        folder
    }

    #[test]
    fn comments_and_empty_lines_are_not_words() {
        let words = parse("# names\nEmory\n\n  Nextcloud  \n   # aside\nSyncthing :2.5\n");
        assert_eq!(words, ["Emory", "Nextcloud", "Syncthing :2.5"]);
        assert!(parse("# nothing\n\n  \n").is_empty());
        assert!(parse("").is_empty());
    }

    #[test]
    fn words_bring_the_beam_search_and_no_word_keeps_the_greedy_one() {
        let words = vec!["Emory".to_owned()];
        assert_eq!(
            Decoding::settle(None, &words).unwrap().method,
            DecodingMethod::ModifiedBeamSearch
        );
        assert_eq!(
            Decoding::settle(None, &[]).unwrap().method,
            DecodingMethod::GreedySearch
        );
        // Asked for explicitly, the beam search stays even without words.
        assert_eq!(
            Decoding::settle(Some(DecodingMethod::ModifiedBeamSearch), &[])
                .unwrap()
                .method,
            DecodingMethod::ModifiedBeamSearch
        );
        assert_eq!(
            Decoding::settle(Some(DecodingMethod::GreedySearch), &[])
                .unwrap()
                .method,
            DecodingMethod::GreedySearch
        );
    }

    #[test]
    fn hotwords_with_the_greedy_search_are_a_mistake_not_a_silent_no_op() {
        let result = Decoding::settle(Some(DecodingMethod::GreedySearch), &["Emory".into()]);
        assert!(result.is_err());
    }

    #[test]
    fn a_file_not_there_yet_is_an_empty_vocabulary() {
        let folder = folder("absent");
        let vocabulary = Vocabulary::load(&folder.join("hotwords.txt")).unwrap();
        assert!(vocabulary.words().list.is_empty());
        assert_eq!(vocabulary.words().joined, "");
    }

    #[test]
    fn a_word_is_appended_and_used_at_once() {
        let folder = folder("append");
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("hotwords.txt");
        // The last line has no newline: the word must not be glued to it.
        std::fs::write(&path, "# my names\nEmory").unwrap();
        let vocabulary = Vocabulary::load(&path).unwrap();
        assert_eq!(vocabulary.add("Grafana", None), Ok(Added::New));
        assert_eq!(
            vocabulary.add("  Gare   du Nord ", Some("gare  du nor")),
            Ok(Added::New)
        );
        assert_eq!(
            vocabulary.words().list,
            ["Emory", "Grafana", "Gare du Nord"]
        );
        assert_eq!(vocabulary.words().joined, "Emory/Grafana/Gare du Nord");
        let text = std::fs::read_to_string(&path).unwrap();
        let date = chrono::Local::now().format("%Y-%m-%d");
        assert_eq!(
            text,
            format!(
                "# my names\nEmory\nGrafana\n# {date} : entendu « gare du nor »\nGare du Nord\n"
            )
        );
        // What the file says is what a new start reads.
        assert_eq!(
            Vocabulary::load(&path).unwrap().words().list,
            vocabulary.words().list
        );
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_word_already_there_is_not_written_twice() {
        let folder = folder("known");
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("hotwords.txt");
        std::fs::write(&path, "Emory\nSyncthing :2.5\n").unwrap();
        let vocabulary = Vocabulary::load(&path).unwrap();
        assert_eq!(vocabulary.add("Emory", None), Ok(Added::Known));
        assert_eq!(vocabulary.add("emory", Some("et mori")), Ok(Added::Known));
        assert_eq!(vocabulary.add("syncthing", None), Ok(Added::Known));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "Emory\nSyncthing :2.5\n"
        );
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn the_first_word_creates_the_file_and_its_folder() {
        let folder = folder("create");
        let path = folder.join("deep/hotwords.txt");
        let vocabulary = Vocabulary::load(&path).unwrap();
        assert_eq!(vocabulary.add("Grafana", None), Ok(Added::New));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Grafana\n");
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_word_that_would_break_the_file_or_the_model_is_refused() {
        for word in [
            "",
            "   ",
            "#Grafana",
            "Grafana\nEmory",
            "Gra/fana",
            "Grafana :3",
            "1234",
            "Grafana\u{7}",
            &"a".repeat(MAX_WORD + 1),
        ] {
            assert!(validate(word).is_err(), "{word:?}");
        }
        for word in [
            "Grafana",
            "Gare du Nord",
            "Saint-Ouen",
            "l'Hôtel",
            "C3PO",
            "Ελένη",
            "U.S.",
        ] {
            assert_eq!(validate(word).as_deref(), Ok(word));
        }
        let folder = folder("refused");
        let path = folder.join("hotwords.txt");
        let vocabulary = Vocabulary::load(&path).unwrap();
        assert!(vocabulary.add("Gra/fana", None).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn the_score_of_a_line_is_not_part_of_its_word() {
        assert_eq!(phrase("Syncthing :2.5"), "Syncthing");
        assert_eq!(phrase("Emory"), "Emory");
        assert_eq!(phrase("a :b"), "a :b");
    }
}
