//! Coloring for JSON and quick-search matches.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::theme;

/// DynamoDB JSON type tags, dimmed so the values stand out.
const TYPE_TAGS: [&str; 10] = ["S", "N", "B", "BOOL", "NULL", "M", "L", "SS", "NS", "BS"];

/// One line of pretty-printed JSON: keys, strings, numbers and literals in their own colors.
pub fn json_line(line: &str) -> Line<'static> {
    let t = theme::current();
    let chars: Vec<char> = line.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                i += if chars[i] == '\\' { 2 } else { 1 };
            }
            i = (i + 1).min(chars.len());
            let text: String = chars[start..i].iter().collect();
            let is_key = chars[i..].iter().find(|c| !c.is_whitespace()) == Some(&':');
            let style = if is_key && TYPE_TAGS.contains(&text.trim_matches('"')) {
                theme::muted()
            } else if is_key {
                theme::fg(t.accent)
            } else {
                theme::fg(t.string)
            };
            spans.push(Span::styled(text, style));
        } else if c == '-' || c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || "+-.".contains(chars[i]))
            {
                i += 1;
            }
            spans.push(Span::styled(
                chars[start..i].iter().collect::<String>(),
                theme::fg(t.number),
            ));
        } else if c.is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            spans.push(Span::styled(
                chars[start..i].iter().collect::<String>(),
                theme::fg(t.literal),
            ));
        } else {
            let start = i;
            while i < chars.len()
                && !(chars[i] == '"' || chars[i] == '-' || chars[i].is_ascii_alphanumeric())
            {
                i += 1;
            }
            spans.push(Span::styled(
                chars[start..i].iter().collect::<String>(),
                theme::muted(),
            ));
        }
    }
    Line::from(spans)
}

/// `text` split into spans with every case-insensitive occurrence of `needle` highlighted.
pub fn find_spans(text: &str, needle: &str, base: Style) -> Vec<Span<'static>> {
    if needle.is_empty() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let lower: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let chars: Vec<char> = text.chars().collect();
    let needle: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    // Lowercasing can change the length of a few scripts; fall back to no highlight then.
    if lower.len() != chars.len() || needle.is_empty() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    while i < chars.len() {
        if lower[i..].starts_with(&needle) {
            if !plain.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut plain), base));
            }
            spans.push(Span::styled(
                chars[i..i + needle.len()].iter().collect::<String>(),
                theme::matched(),
            ));
            i += needle.len();
        } else {
            plain.push(chars[i]);
            i += 1;
        }
    }
    if !plain.is_empty() {
        spans.push(Span::styled(plain, base));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(line: &Line) -> Vec<String> {
        line.spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn json_keys_values_and_type_tags_get_their_own_spans() {
        let line = json_line(r#"  "amount": {"N": "-1.5"}, "ok": true"#);
        assert_eq!(
            texts(&line),
            [
                "  ",
                "\"amount\"",
                ": {",
                "\"N\"",
                ": ",
                "\"-1.5\"",
                "}, ",
                "\"ok\"",
                ": ",
                "true"
            ]
        );
        assert_eq!(line.spans[3].style, theme::muted(), "type tag");
        assert_eq!(line.spans[1].style.fg, Some(theme::current().accent), "key");
        assert_eq!(
            line.spans[5].style.fg,
            Some(theme::current().string),
            "string value"
        );
    }

    #[test]
    fn escaped_quotes_stay_inside_strings() {
        assert_eq!(texts(&json_line(r#""a\"b": 1"#)), ["\"a\\\"b\"", ": ", "1"]);
    }

    #[test]
    fn find_highlights_every_match_case_insensitively() {
        let spans = find_spans("Bob and BOBBY", "bob", Style::new());
        let parts: Vec<(&str, bool)> = spans
            .iter()
            .map(|s| (s.content.as_ref(), s.style == theme::matched()))
            .collect();
        assert_eq!(
            parts,
            [
                ("Bob", true),
                (" and ", false),
                ("BOB", true),
                ("BY", false)
            ]
        );
        assert_eq!(
            find_spans("서울 Busan", "서울", Style::new())[0].style,
            theme::matched()
        );
    }
}
