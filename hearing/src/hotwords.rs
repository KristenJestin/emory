//! The vocabulary: the names that Parakeet favours when it hesitates (`specs/hearing.md`).
//!
//! A text file, one word or phrase per line; empty lines and lines starting with `#` are ignored.
//! sherpa-onnx reads it from a file and does not know comments, so what it is given is a clean
//! copy, written for the moment the recognizer loads.
//!
//! Hotwords only work with the beam search (`modified_beam_search`), which costs more than the
//! greedy search: with no word in the file, the greedy search stays.

use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::config::DecodingMethod;

/// The words of a hotwords file.
pub fn parse(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// How Parakeet decodes, and with which vocabulary.
#[derive(Debug, PartialEq, Eq)]
pub struct Decoding {
    pub method: DecodingMethod,
    /// Empty: no vocabulary.
    pub hotwords: Vec<String>,
}

impl Decoding {
    /// Reads the vocabulary file, if there is one, and settles the method.
    pub fn read(requested: Option<DecodingMethod>, hotwords: Option<&Path>) -> Result<Self> {
        let words = match hotwords {
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("reading the hotwords {}", path.display()))?;
                parse(&text)
            }
            None => Vec::new(),
        };
        Self::settle(requested, words)
    }

    fn settle(requested: Option<DecodingMethod>, hotwords: Vec<String>) -> Result<Self> {
        let method = match (requested, hotwords.is_empty()) {
            (Some(DecodingMethod::GreedySearch), false) => bail!(
                "the hotwords need decoding_method = \"modified_beam_search\" \
                 (or no decoding_method, or no hotwords)"
            ),
            (Some(method), _) => method,
            (None, false) => DecodingMethod::ModifiedBeamSearch,
            (None, true) => DecodingMethod::GreedySearch,
        };
        Ok(Self { method, hotwords })
    }
}

/// The clean copy of the hotwords that sherpa-onnx reads; removed when dropped.
pub struct HotwordsFile(PathBuf);

impl HotwordsFile {
    pub fn write(words: &[String]) -> Result<Self> {
        let path =
            std::env::temp_dir().join(format!("emory-hearing-{}-hotwords.txt", std::process::id()));
        // `create_new`: never write through a file or a link that was put there.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("writing {}", path.display()))?;
        let guard = Self(path);
        for word in words {
            writeln!(file, "{word}").with_context(|| format!("writing {}", guard.0.display()))?;
        }
        Ok(guard)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for HotwordsFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_and_empty_lines_are_not_words() {
        let words = parse("# names\nEmory\n\n  Hippocampe  \n   # aside\nKrisNet :2.5\n");
        assert_eq!(words, ["Emory", "Hippocampe", "KrisNet :2.5"]);
        assert!(parse("# nothing\n\n  \n").is_empty());
        assert!(parse("").is_empty());
    }

    #[test]
    fn words_bring_the_beam_search_and_no_word_keeps_the_greedy_one() {
        let words = vec!["Emory".to_owned()];
        assert_eq!(
            Decoding::settle(None, words.clone()).unwrap().method,
            DecodingMethod::ModifiedBeamSearch
        );
        assert_eq!(
            Decoding::settle(None, Vec::new()).unwrap().method,
            DecodingMethod::GreedySearch
        );
        // Asked for explicitly, the beam search stays even without words.
        assert_eq!(
            Decoding::settle(Some(DecodingMethod::ModifiedBeamSearch), Vec::new())
                .unwrap()
                .method,
            DecodingMethod::ModifiedBeamSearch
        );
        assert_eq!(
            Decoding::settle(Some(DecodingMethod::GreedySearch), Vec::new())
                .unwrap()
                .method,
            DecodingMethod::GreedySearch
        );
    }

    #[test]
    fn hotwords_with_the_greedy_search_are_a_mistake_not_a_silent_no_op() {
        let result = Decoding::settle(Some(DecodingMethod::GreedySearch), vec!["Emory".into()]);
        assert!(result.is_err());
    }

    #[test]
    fn an_absent_file_is_no_vocabulary_but_a_missing_one_is_an_error() {
        let decoding = Decoding::read(None, None).unwrap();
        assert_eq!(decoding.method, DecodingMethod::GreedySearch);
        assert!(decoding.hotwords.is_empty());
        assert!(Decoding::read(None, Some(Path::new("/nonexistent/hotwords.txt"))).is_err());
    }

    #[test]
    fn the_clean_copy_is_removed_when_dropped() {
        let file = HotwordsFile::write(&["Emory".to_owned(), "Omarchy".to_owned()]).unwrap();
        let path = file.path().to_owned();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Emory\nOmarchy\n");
        drop(file);
        assert!(!path.exists());
    }
}
