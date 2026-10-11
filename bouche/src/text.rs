//! Le texte avant la synthèse.

/// Prépare une phrase pour le moteur. En version 1, seulement les espaces.
///
/// C'est ici que viendront la lecture des nombres, des sigles et des noms propres (le lexique de
/// prononciation de `specs/bouche.md`, « Ce qui est lu ») : question ouverte de la spec, rien
/// n'est fait pour l'instant.
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
