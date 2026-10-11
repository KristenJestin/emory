//! The corrections table, applied after the transcription (`specs/hearing.md`).
//!
//! A TSV file, one rule per line: `pattern<TAB>replacement`. The pattern is a regular expression
//! (`a|b` for several forms), searched on whole words, case-insensitively. Empty lines and those
//! starting with `#` are ignored. The rules apply in the order of the file: « chris net » must
//! come before « chris ».
//!
//! It must only correct forms that are not French words: « trafic » or « chris » would be
//! rewritten otherwise.

use std::path::Path;

use anyhow::{Context, Result, bail};
use regex::{NoExpand, Regex};

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
        let mut rules = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((pattern, replacement)) = line.split_once('\t') else {
                bail!("line {}: pattern and replacement separated by a tab", i + 1);
            };
            let pattern = Regex::new(&format!(r"(?i)\b(?:{})\b", pattern.trim()))
                .with_context(|| format!("line {}: invalid pattern", i + 1))?;
            rules.push(Rule {
                pattern,
                replacement: replacement.trim().to_owned(),
            });
        }
        Ok(Self { rules })
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn apply(&self, text: &str) -> String {
        let mut text = text.to_owned();
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
