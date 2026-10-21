//! How attribute values read in the grid and the preview: numbers in their own color and
//! right-aligned, nested values folded into `{3}` and `[2]`, status-like words with a dot.

use std::collections::HashMap;

use aws_sdk_dynamodb::types::AttributeValue;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::fit;
use crate::highlight;
use crate::item::Item;
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Number,
    /// `true` or `false`.
    Literal,
    Null,
    /// A map or a list of nested values, shown as its size.
    Folded,
    /// A list of plain values or a set, shown inline.
    Inline,
    Binary,
    Missing,
}

pub struct Shown {
    pub text: String,
    pub kind: Kind,
}

/// Lists of plain values up to this width show inline: `[a, 2]`.
const INLINE_WIDTH: usize = 32;

pub fn show(v: Option<&AttributeValue>) -> Shown {
    let Some(v) = v else {
        return Shown {
            text: String::new(),
            kind: Kind::Missing,
        };
    };
    let (text, kind) = match v {
        AttributeValue::S(s) => (s.replace(['\n', '\r', '\t'], " "), Kind::Text),
        AttributeValue::N(n) => (n.clone(), Kind::Number),
        AttributeValue::Bool(b) => (b.to_string(), Kind::Literal),
        AttributeValue::Null(_) => ("null".into(), Kind::Null),
        AttributeValue::B(b) => (format!("{} bytes", b.as_ref().len()), Kind::Binary),
        AttributeValue::M(m) => (format!("{{{}}}", m.len()), Kind::Folded),
        AttributeValue::L(l) => {
            let plain: Option<Vec<String>> = l.iter().map(plain_text).collect();
            match plain.map(|p| format!("[{}]", p.join(", "))) {
                Some(text) if text.width() <= INLINE_WIDTH => (text, Kind::Inline),
                _ => (format!("[{}]", l.len()), Kind::Folded),
            }
        }
        AttributeValue::Ss(v) | AttributeValue::Ns(v) => (v.join(", "), Kind::Inline),
        AttributeValue::Bs(v) => (format!("{} binary values", v.len()), Kind::Binary),
        _ => ("?".into(), Kind::Missing),
    };
    Shown { text, kind }
}

fn plain_text(v: &AttributeValue) -> Option<String> {
    match v {
        AttributeValue::S(s) => Some(s.replace(['\n', '\r', '\t'], " ")),
        AttributeValue::N(n) => Some(n.clone()),
        AttributeValue::Bool(b) => Some(b.to_string()),
        AttributeValue::Null(_) => Some("null".into()),
        _ => None,
    }
}

pub fn style(kind: Kind) -> Style {
    let t = theme::current();
    match kind {
        Kind::Text => theme::fg(t.text),
        Kind::Number => theme::fg(t.number),
        Kind::Literal => theme::fg(t.literal),
        Kind::Inline => theme::fg(t.subtext),
        Kind::Folded | Kind::Missing => theme::muted(),
        Kind::Null | Kind::Binary => theme::muted().add_modifier(Modifier::ITALIC),
    }
}

const GOOD: [&str; 16] = [
    "active",
    "enabled",
    "success",
    "succeeded",
    "ok",
    "done",
    "complete",
    "completed",
    "paid",
    "approved",
    "available",
    "healthy",
    "running",
    "live",
    "delivered",
    "valid",
];
const BAD: [&str; 14] = [
    "failed",
    "failure",
    "error",
    "inactive",
    "disabled",
    "deleted",
    "cancelled",
    "canceled",
    "rejected",
    "expired",
    "blocked",
    "stopped",
    "invalid",
    "suspended",
];
const WAITING: [&str; 12] = [
    "pending",
    "waiting",
    "queued",
    "processing",
    "in_progress",
    "in-progress",
    "draft",
    "review",
    "paused",
    "scheduled",
    "retrying",
    "new",
];

/// Dot colors for a status-like column: a few short words, all lowercase or all uppercase,
/// each repeated across the page. Common status words get the success, warning or error
/// color; the rest take the theme's tag colors in order.
pub fn categories<'a>(
    values: impl Iterator<Item = Option<&'a AttributeValue>>,
) -> Option<HashMap<String, Color>> {
    let mut words: Vec<String> = Vec::new();
    let mut present = 0;
    for v in values.flatten() {
        let AttributeValue::S(s) = v else {
            return None;
        };
        if !is_word(s) {
            return None;
        }
        present += 1;
        if !words.contains(s) {
            words.push(s.clone());
            if words.len() > 6 {
                return None;
            }
        }
    }
    if present < 4 || words.len() * 2 > present {
        return None;
    }
    words.sort();
    let t = theme::current();
    let mut next = 0;
    Some(
        words
            .into_iter()
            .map(|w| {
                let lower = w.to_lowercase();
                let color = if GOOD.contains(&lower.as_str()) {
                    t.ok
                } else if BAD.contains(&lower.as_str()) {
                    t.err
                } else if WAITING.contains(&lower.as_str()) {
                    t.warn
                } else {
                    next += 1;
                    t.tags[(next - 1) % t.tags.len()]
                };
                (w, color)
            })
            .collect(),
    )
}

fn is_word(s: &str) -> bool {
    let one_case =
        !s.chars().any(|c| c.is_ascii_uppercase()) || !s.chars().any(|c| c.is_ascii_lowercase());
    s.len() <= 16
        && one_case
        && s.starts_with(|c: char| c.is_ascii_alphabetic())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Columns, the name width and the search text of a preview.
struct Preview<'a> {
    name_width: usize,
    width: usize,
    find: &'a str,
}

/// An item as aligned name and value lines: key attributes first, nested maps and lists
/// unfolded below their parent, long text wrapped.
pub fn preview_lines(item: &Item, keys: &[String], width: usize, find: &str) -> Vec<Line<'static>> {
    let mut names: Vec<&String> = item.keys().collect();
    names.sort_by_key(|n| {
        (
            keys.iter().position(|k| k == *n).unwrap_or(usize::MAX),
            n.to_string(),
        )
    });
    let longest = names.iter().map(|n| n.width()).max().unwrap_or(0);
    let p = Preview {
        name_width: longest.clamp(4, (width / 2).max(4)),
        width,
        find,
    };
    let mut lines = Vec::new();
    for name in names {
        p.push(&mut lines, 0, name, &item[name], keys.contains(name));
    }
    lines
}

impl Preview<'_> {
    fn push(
        &self,
        lines: &mut Vec<Line<'static>>,
        depth: usize,
        name: &str,
        v: &AttributeValue,
        key: bool,
    ) {
        let t = theme::current();
        let label = fit(
            &format!("{}{name}", "  ".repeat(depth)),
            false,
            self.name_width,
        );
        let name_style = if key {
            theme::bold(t.key)
        } else if depth > 0 {
            theme::muted()
        } else {
            theme::fg(t.subtext)
        };
        let room = self.width.saturating_sub(self.name_width + 2);
        let shown = show(Some(v));
        let value_style = if key {
            theme::fg(t.key)
        } else {
            style(shown.kind)
        };
        let chunks = if shown.kind == Kind::Text {
            wrap(&shown.text, room, 4)
        } else {
            vec![fit(&shown.text, false, room)]
        };
        for (i, chunk) in chunks.into_iter().enumerate() {
            let mut spans = if i == 0 {
                let pad = self.name_width.saturating_sub(label.width()) + 2;
                vec![
                    Span::styled(label.clone(), name_style),
                    Span::raw(" ".repeat(pad)),
                ]
            } else {
                vec![Span::raw(" ".repeat(self.name_width + 2))]
            };
            spans.extend(highlight::find_spans(&chunk, self.find, value_style));
            lines.push(Line::from(spans));
        }
        if depth >= 3 {
            return;
        }
        match v {
            AttributeValue::M(m) => {
                let mut children: Vec<_> = m.iter().collect();
                children.sort_by(|a, b| a.0.cmp(b.0));
                for (k, child) in children {
                    self.push(lines, depth + 1, k, child, false);
                }
            }
            AttributeValue::L(l) if shown.kind == Kind::Folded => {
                for (i, child) in l.iter().enumerate() {
                    self.push(lines, depth + 1, &i.to_string(), child, false);
                }
            }
            _ => {}
        }
    }
}

/// Splits text into lines of `width` columns; past `max_lines` the last one ends in `…`.
fn wrap(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut lines = vec![String::new()];
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            lines.push(String::new());
            used = 0;
        }
        used += w;
        lines.last_mut().expect("one line").push(c);
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        let last = lines.last_mut().expect("max_lines > 0");
        *last = fit(&format!("{last} "), false, width);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_dynamodb::primitives::Blob;

    fn s(v: &str) -> AttributeValue {
        AttributeValue::S(v.into())
    }

    #[test]
    fn values_show_by_type() {
        let cases = [
            (s("a\nb"), "a b", Kind::Text),
            (AttributeValue::N("1.50".into()), "1.50", Kind::Number),
            (AttributeValue::Bool(true), "true", Kind::Literal),
            (AttributeValue::Null(true), "null", Kind::Null),
            (
                AttributeValue::B(Blob::new(vec![1, 2, 3])),
                "3 bytes",
                Kind::Binary,
            ),
            (
                AttributeValue::M(Item::from([("x".to_string(), s("1"))])),
                "{1}",
                Kind::Folded,
            ),
            (
                AttributeValue::L(vec![s("seen"), AttributeValue::N("2".into())]),
                "[seen, 2]",
                Kind::Inline,
            ),
            (
                AttributeValue::L(vec![AttributeValue::M(Item::new())]),
                "[1]",
                Kind::Folded,
            ),
            (
                AttributeValue::Ss(vec!["a".into(), "t3".into()]),
                "a, t3",
                Kind::Inline,
            ),
        ];
        for (v, text, kind) in cases {
            let shown = show(Some(&v));
            assert_eq!((shown.text.as_str(), shown.kind), (text, kind), "{v:?}");
        }
        assert_eq!(show(None).kind, Kind::Missing);
    }

    #[test]
    fn status_like_columns_get_dots() {
        let t = theme::current();
        let status: Vec<AttributeValue> = [
            "active", "failed", "active", "pending", "active", "x1", "active", "failed",
        ]
        .map(s)
        .into();
        let colors = categories(status.iter().map(Some)).expect("status-like");
        assert_eq!(colors["active"], t.ok);
        assert_eq!(colors["failed"], t.err);
        assert_eq!(colors["pending"], t.warn);
        assert_eq!(colors["x1"], t.tags[0]);

        let names: Vec<AttributeValue> = ["Alice", "Bob", "Alice", "Bob"].map(s).into();
        assert!(
            categories(names.iter().map(Some)).is_none(),
            "mixed case: names"
        );
        let ids: Vec<AttributeValue> = ["a", "b", "c", "d"].map(s).into();
        assert!(
            categories(ids.iter().map(Some)).is_none(),
            "nothing repeats"
        );
    }

    #[test]
    fn preview_lists_keys_first_and_unfolds_maps() {
        let item = Item::from([
            ("zeta".to_string(), s("z")),
            ("pk".to_string(), s("u1")),
            (
                "profile".to_string(),
                AttributeValue::M(Item::from([("city".to_string(), s("서울"))])),
            ),
        ]);
        let lines: Vec<String> = preview_lines(&item, &["pk".into()], 40, "")
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(
            lines,
            ["pk       u1", "profile  {1}", "  city   서울", "zeta     z"]
        );
    }

    #[test]
    fn long_text_wraps_and_ends_in_an_ellipsis() {
        assert_eq!(wrap("abcdefgh", 3, 5), ["abc", "def", "gh"]);
        assert_eq!(wrap("abcdefghijk", 3, 2), ["abc", "de…"]);
        assert_eq!(wrap("서울시", 4, 3), ["서울", "시"]);
    }
}
