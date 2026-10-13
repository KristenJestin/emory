//! The instructions of the LLM: the prompt file is a template, filled from `[owner]` of the
//! configuration, so that the shipped text names nobody.
//!
//! Two placeholders, replaced wherever they stand in the file:
//! - `{owner}`: the owner's name, or « la personne qui te parle » when `[owner] name` is absent;
//! - `{address}`: `[owner] address` (how to speak to the owner), or « tu tutoies cette personne »
//!   when it is absent.
//!
//! Any other text between braces is left as it is.

use crate::config::OwnerConfig;

/// What `{owner}` stands for when the configuration gives no name.
pub const DEFAULT_OWNER: &str = "la personne qui te parle";
/// What `{address}` stands for when the configuration gives no address.
pub const DEFAULT_ADDRESS: &str = "tu tutoies cette personne";

/// Fills the placeholders of `template` from the owner's section. One pass: a value that holds a
/// placeholder is not filled again.
pub fn render(template: &str, owner: &OwnerConfig) -> String {
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
                &owner(Some("Alex"), Some("une femme ; tu la tutoies"))
            ),
            "Tu es Emory, l'assistant de Alex. Pour t'adresser à Alex : une femme ; tu la tutoies."
        );
    }

    #[test]
    fn without_an_owner_the_text_is_generic_and_names_nobody() {
        assert_eq!(
            render(TEMPLATE, &OwnerConfig::default()),
            "Tu es Emory, l'assistant de la personne qui te parle. \
             Pour t'adresser à la personne qui te parle : tu tutoies cette personne."
        );
    }

    #[test]
    fn a_name_alone_keeps_the_default_address() {
        let text = render(TEMPLATE, &owner(Some("Alex"), None));
        assert!(text.contains("à Alex : tu tutoies cette personne."));
    }

    #[test]
    fn other_braces_and_values_with_braces_are_left_alone() {
        let text = render("{x} {owner} {", &owner(Some("{address}"), Some("a")));
        assert_eq!(text, "{x} {address} {");
    }

    #[test]
    fn the_shipped_prompt_names_nobody_and_has_no_unfilled_placeholder() {
        let shipped = include_str!("../prompts/system.fr.md");
        let text = render(shipped, &OwnerConfig::default());
        assert!(text.contains(DEFAULT_OWNER));
    }
}
