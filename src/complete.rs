//! Tab completion of attribute names in expression inputs.

use std::collections::BTreeSet;

use crate::expr;

pub struct Completion {
    /// The input with the word completed as far as it is unambiguous.
    pub text: String,
    /// Every name that matched, shown as a hint when there is more than one.
    pub matches: Vec<String>,
}

/// Completes the attribute name being typed at the end of `text`. `None` means there is
/// nothing to complete (no word, no match, already complete, inside a string or a nested
/// path), so Tab can move to the next field instead.
pub fn complete(text: &str, names: &BTreeSet<String>) -> Option<Completion> {
    if inside_string(text) {
        return None;
    }
    let word_len = text
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .count();
    let split = text.char_indices().rev().nth(word_len.checked_sub(1)?)?.0;
    let (head, word) = text.split_at(split);
    if !word.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        || head.ends_with(['.', '`'])
    {
        return None;
    }
    let matches: Vec<String> = names
        .iter()
        .filter(|n| n.starts_with(word))
        .cloned()
        .collect();
    match matches.as_slice() {
        [] => None,
        [only] if only == word => None,
        [only] => Some(Completion {
            text: format!("{head}{}", expr::name_text(only)),
            matches,
        }),
        _ => {
            let common = common_prefix(&matches);
            Some(Completion {
                text: format!(
                    "{head}{}",
                    if common.len() > word.len() {
                        &common
                    } else {
                        word
                    }
                ),
                matches,
            })
        }
    }
}

fn common_prefix(names: &[String]) -> String {
    let first = &names[0];
    let len = names[1..].iter().fold(first.len(), |len, n| {
        first[..len]
            .char_indices()
            .zip(n.chars())
            .take_while(|((_, a), b)| a == b)
            .last()
            .map_or(0, |((i, a), _)| i + a.len_utf8())
    });
    first[..len].to_string()
}

/// Whether the end of `text` sits inside an unterminated '…' or "…" string.
fn inside_string(text: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    for c in text.chars() {
        match quote {
            Some(_) if escaped => escaped = false,
            Some(_) if c == '\\' => escaped = true,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None => {}
        }
    }
    quote.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> BTreeSet<String> {
        ["amount", "status", "statusCode", "stats", "my key", "note"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    fn done(text: &str) -> Option<String> {
        complete(text, &names()).map(|c| c.text)
    }

    #[test]
    fn completes_a_unique_prefix() {
        assert_eq!(done("am").as_deref(), Some("amount"));
        assert_eq!(done("x = 1 AND no").as_deref(), Some("x = 1 AND note"));
        assert_eq!(done("size(am").as_deref(), Some("size(amount"));
    }

    #[test]
    fn extends_to_the_common_prefix_and_lists_matches() {
        let c = complete("st", &names()).unwrap();
        assert_eq!(c.text, "stat");
        assert_eq!(c.matches, ["stats", "status", "statusCode"]);
        let c = complete("status", &names()).unwrap();
        assert_eq!(
            c.text, "status",
            "already a full name, but others continue it"
        );
        assert_eq!(c.matches.len(), 2);
    }

    #[test]
    fn quotes_names_that_need_it() {
        assert_eq!(done("my").as_deref(), Some("`my key`"));
    }

    #[test]
    fn nothing_to_do_lets_tab_move_on() {
        for text in [
            "",
            "amount = ",
            "note",
            "zz",
            "a = \"st",
            "profile.st",
            "x = 12",
        ] {
            assert!(done(text).is_none(), "{text:?}");
        }
    }
}
