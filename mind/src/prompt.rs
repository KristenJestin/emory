//! The instructions of the LLM: the prompt file is a template, filled from `[owner]` of the
//! configuration, so that the shipped text names nobody.
//!
//! Two placeholders, replaced wherever they stand in the file:
//! - `{owner}`: the owner's name, or « la personne qui te parle » when `[owner] name` is absent;
//! - `{address}`: `[owner] address` (how to speak to the owner), or « tu tutoies cette personne »
//!   when it is absent.
//!
//! A line that starts with `{if <tools>}` is kept, without that prefix, only when its tools are
//! offered to the LLM, so that the prompt (paid at every turn) never speaks of a tool it has not
//! got. `<tools>` are names as the LLM sees them (`web_search`) separated by spaces: all of them
//! must be there, and `a|b` stands for « a or b » (`{if web_wikipedia|web_search web_read}`).
//!
//! Any other text between braces is left as it is.

use crate::config::OwnerConfig;

/// What `{owner}` stands for when the configuration gives no name.
pub const DEFAULT_OWNER: &str = "la personne qui te parle";
/// What `{address}` stands for when the configuration gives no address.
pub const DEFAULT_ADDRESS: &str = "tu tutoies cette personne";

/// Fills the placeholders of `template` from the owner's section, and keeps the lines whose
/// `{if ...}` tools are all in `tools`. One pass: a value that holds a placeholder is not filled
/// again.
pub fn render(template: &str, owner: &OwnerConfig, tools: &[&str]) -> String {
    let template = keep_lines_for(template, tools);
    let template = template.as_str();
    let values = [
        ("{owner}", owner.name.as_deref().unwrap_or(DEFAULT_OWNER)),
        (
            "{address}",
            owner.address.as_deref().unwrap_or(DEFAULT_ADDRESS),
        ),
    ];
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        match values.iter().find(|(key, _)| rest.starts_with(key)) {
            Some((key, value)) => {
                out.push_str(value.trim());
                rest = &rest[key.len()..];
            }
            None => {
                out.push('{');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `template` without the lines of tools that are not offered, and without the `{if ...}` of
/// the others. A `{if` that is not closed on its line is left as text.
fn keep_lines_for(template: &str, tools: &[&str]) -> String {
    let offered = |term: &str| term.split('|').any(|name| tools.contains(&name));
    template
        .split_inclusive('\n')
        .filter_map(|line| {
            let Some(rest) = line.strip_prefix("{if ") else {
                return Some(line);
            };
            let Some((condition, text)) = rest.split_once('}') else {
                return Some(line);
            };
            condition
                .split_whitespace()
                .all(offered)
                .then(|| text.trim_start_matches(' '))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: &str =
        "Tu es Emory, l'assistant de {owner}. Pour t'adresser à {owner} : {address}.";

    fn owner(name: Option<&str>, address: Option<&str>) -> OwnerConfig {
        OwnerConfig {
            name: name.map(str::to_owned),
            address: address.map(str::to_owned),
        }
    }

    #[test]
    fn the_placeholders_are_filled_from_the_owner_section() {
        assert_eq!(
            render(
                TEMPLATE,
                &owner(Some("Alex"), Some("une femme ; tu la tutoies")),
                &[]
            ),
            "Tu es Emory, l'assistant de Alex. Pour t'adresser à Alex : une femme ; tu la tutoies."
        );
    }

    #[test]
    fn without_an_owner_the_text_is_generic_and_names_nobody() {
        assert_eq!(
            render(TEMPLATE, &OwnerConfig::default(), &[]),
            "Tu es Emory, l'assistant de la personne qui te parle. \
             Pour t'adresser à la personne qui te parle : tu tutoies cette personne."
        );
    }

    #[test]
    fn a_name_alone_keeps_the_default_address() {
        let text = render(TEMPLATE, &owner(Some("Alex"), None), &[]);
        assert!(text.contains("à Alex : tu tutoies cette personne."));
    }

    #[test]
    fn other_braces_and_values_with_braces_are_left_alone() {
        let text = render("{x} {owner} {", &owner(Some("{address}"), Some("a")), &[]);
        assert_eq!(text, "{x} {address} {");
    }

    #[test]
    fn the_shipped_prompt_names_nobody_and_has_no_unfilled_placeholder() {
        let shipped = include_str!("../prompts/system.fr.md");
        let text = render(shipped, &OwnerConfig::default(), &[]);
        assert!(text.contains(DEFAULT_OWNER));
    }

    #[test]
    fn the_shipped_prompt_gives_the_rules_of_writing_to_the_memory() {
        let text = render(
            include_str!("../prompts/system.fr.md"),
            &OwnerConfig::default(),
            &[],
        );
        // Only on request, search first, where the words come from, and how to find and undo.
        for rule in [
            "te demande de noter",
            "Cherche d'abord",
            "`extracted`",
            "`said_by`",
            "`inferred`",
            "`by` valant `emory`",
            "`since`",
            "archive la fiche",
        ] {
            assert!(text.contains(rule), "the prompt no longer says: {rule}");
        }
    }

    #[test]
    fn a_line_with_tools_is_kept_only_when_they_are_offered() {
        let template = "a\n{if x}b\n{if x|y z}c\n{if y}d\n{if x\ne\n{if}f\n";
        let lines = |tools: &[&str]| render(template, &OwnerConfig::default(), tools);
        assert_eq!(lines(&[]), "a\n{if x\ne\n{if}f\n");
        assert_eq!(lines(&["x"]), "a\nb\n{if x\ne\n{if}f\n");
        assert_eq!(lines(&["y"]), "a\nd\n{if x\ne\n{if}f\n");
        assert_eq!(lines(&["y", "z"]), "a\nc\nd\n{if x\ne\n{if}f\n");
        assert_eq!(lines(&["x", "z"]), "a\nb\nc\n{if x\ne\n{if}f\n");
        // The last line may have no line break.
        assert_eq!(
            render("a\n{if x}b", &OwnerConfig::default(), &["x"]),
            "a\nb"
        );
        assert_eq!(render("a\n{if x}b", &OwnerConfig::default(), &[]), "a\n");
    }

    /// The lines about the web in the shipped prompt that need several tools (the source line
    /// needs either of two): the words that show a line is there, and the tools that must all be
    /// offered for it to be.
    const WEB_LINES: [(&str, &[&str]); 2] = [
        (
            "lis l'article avec web_read",
            &["web_wikipedia", "web_read"],
        ),
        ("essaie web_wikipedia", &["web_wikipedia", "web_search"]),
    ];

    fn shipped(tools: &[&str]) -> String {
        render(
            include_str!("../prompts/system.fr.md"),
            &OwnerConfig::default(),
            tools,
        )
    }

    #[test]
    fn the_shipped_prompt_has_each_web_line_only_with_its_tools() {
        let all = ["web_weather", "web_wikipedia", "web_search", "web_read"];
        let none = shipped(&[]);
        for tool in ["web_", "Wikipédia (", "d'où ça vient", "{if", "{"] {
            assert!(
                !none.contains(tool),
                "without tools, the prompt says {tool}"
            );
        }
        for (words, needs) in WEB_LINES {
            assert!(shipped(&all).contains(words), "{words}");
            // Every offered set that misses one of the tools leaves the line out.
            for left_out in needs {
                let others: Vec<&str> = all.iter().copied().filter(|t| t != left_out).collect();
                assert!(
                    !shipped(&others).contains(words),
                    "{words} without {left_out}"
                );
            }
        }
        // The weather alone has no line: its tool says enough, and the prompt is paid every turn.
        assert_eq!(shipped(&["web_weather"]), none);
        // The source line serves either tool that finds something.
        assert!(shipped(&["web_search"]).contains("d'où ça vient"));
        assert!(shipped(&["web_wikipedia"]).contains("d'où ça vient"));
        assert!(!shipped(&["web_read"]).contains("d'où ça vient"));
        // A line left out leaves no empty line behind.
        assert!(!shipped(&all).contains("\n\n\n") && !none.contains("\n\n\n"));
    }

    #[test]
    fn the_web_lines_are_short_and_read_no_address() {
        let (none, all) = (
            shipped(&[]),
            shipped(&["web_weather", "web_wikipedia", "web_search", "web_read"]),
        );
        // Paid at every turn: the whole of them stays under 300 characters.
        let added = all.chars().count() - none.chars().count();
        assert!(added < 300, "{added} characters added");
        assert!(all.contains("sans jamais lire une adresse"));
    }
}
