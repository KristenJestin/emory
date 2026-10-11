//! The text before the synthesis.

/// Prepares a sentence for the engine. In version 1, only the whitespace.
///
/// This is where the reading of numbers, acronyms and proper names will come (the pronunciation
/// lexicon of `specs/voice.md`, « Ce qui est lu »): an open question of the spec, nothing is done
/// for now.
pub fn clean(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::clean;

    #[test]
    fn collapses_whitespace() {
        assert_eq!(clean("  Bonjour \n  Kris\t! "), "Bonjour Kris !");
    }

    #[test]
    fn blank_text_is_empty() {
        assert_eq!(clean(" \n\t "), "");
    }

    #[test]
    fn leaves_the_words_alone() {
        assert_eq!(clean("MCP et Neo4j : 12 %"), "MCP et Neo4j : 12 %");
    }
}
