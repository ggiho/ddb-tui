//! The table details view: summary tiles, the key schema, protection and settings with
//! on/off dots, the indexes as a table and the tags as chips. Built as lines so it scrolls.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::fit;
use crate::details::{IndexDetails, KeyPart, Setting, State, TableDetails, bytes};
use crate::{app, theme};

const LABEL_W: usize = 24;

pub(super) fn lines(d: &TableDetails, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    tiles(&mut out, d, width);

    out.push(Line::raw(""));
    out.push(section("KEY SCHEMA", None));
    let name_w = d.keys.iter().map(|k| k.name.width()).max().unwrap_or(0);
    for k in &d.keys {
        out.push(key_line(k, name_w));
    }

    out.push(Line::raw(""));
    settings(&mut out, d, width);

    out.push(Line::raw(""));
    indexes(
        &mut out,
        "GLOBAL SECONDARY INDEXES",
        &d.global_indexes,
        width,
    );
    out.push(Line::raw(""));
    indexes(&mut out, "LOCAL SECONDARY INDEXES", &d.local_indexes, width);

    out.push(Line::raw(""));
    tags(&mut out, d, width);

    out.push(Line::raw(""));
    out.push(Line::from(vec![
        Span::styled(" ARN  ", theme::bold(theme::current().muted)),
        Span::styled(
            fit(&d.arn, false, width.saturating_sub(7)),
            theme::fg(theme::current().subtext),
        ),
    ]));
    out
}

/// The status pill for the title row: green when active.
pub(super) fn status(d: &TableDetails) -> Span<'static> {
    let t = theme::current();
    let color = if d.status == "ACTIVE" { t.ok } else { t.warn };
    Span::styled(format!("● {}", d.status), theme::bold(color))
}

fn section(title: &str, count: Option<usize>) -> Line<'static> {
    let t = theme::current();
    let mut spans = vec![Span::styled(format!(" {title}"), theme::bold(t.muted))];
    if let Some(n) = count {
        spans.push(Span::styled(format!("  {n}"), theme::muted()));
    }
    Line::from(spans)
}

fn pad(s: &str, width: usize) -> String {
    let s = fit(s, false, width);
    let gap = width.saturating_sub(s.width());
    format!("{s}{}", " ".repeat(gap))
}

/// `12,345,678`.
fn grouped(n: i64) -> String {
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

// ---------------------------------------------------------------- tiles

struct Tile {
    label: &'static str,
    value: String,
    note: String,
    color: Color,
}

/// Four summary tiles in a row, or two rows of two when narrow.
fn tiles(out: &mut Vec<Line<'static>>, d: &TableDetails, width: usize) {
    let t = theme::current();
    let index_names: Vec<&str> = d
        .global_indexes
        .iter()
        .chain(&d.local_indexes)
        .map(|i| i.name.as_str())
        .collect();
    let tiles = [
        Tile {
            label: "ITEMS",
            value: format!("~{}", grouped(d.item_count)),
            note: "estimate, refreshed ~6 h".into(),
            color: t.accent,
        },
        Tile {
            label: "SIZE",
            value: bytes(d.size_bytes),
            note: match d.size_bytes.checked_div(d.item_count) {
                Some(avg) if d.item_count > 0 => format!("~{} per item", bytes(avg)),
                _ => "no items".into(),
            },
            color: t.number,
        },
        Tile {
            label: "INDEXES",
            value: format!(
                "{} GSI · {} LSI",
                d.global_indexes.len(),
                d.local_indexes.len()
            ),
            note: if index_names.is_empty() {
                "none".into()
            } else {
                index_names.join(", ")
            },
            color: t.literal,
        },
        match d.capacity {
            None => Tile {
                label: "CAPACITY",
                value: "On-demand".into(),
                note: "pay per request".into(),
                color: t.ok,
            },
            Some((r, w)) => Tile {
                label: "CAPACITY",
                value: format!("{r} / {w}"),
                note: "read / write units".into(),
                color: t.ok,
            },
        },
    ];
    const GAP: usize = 2;
    let per_row = if width > 4 * 20 + 3 * GAP { 4 } else { 2 };
    let tile_w = (width.saturating_sub(1 + GAP * (per_row - 1)) / per_row).max(8);
    let bg = Style::new().bg(t.raised);
    for (row_index, row) in tiles.chunks(per_row).enumerate() {
        if row_index > 0 {
            out.push(Line::raw(""));
        }
        for line in 0..5 {
            let mut spans = vec![Span::raw(" ")];
            for (i, tile) in row.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw(" ".repeat(GAP)));
                }
                spans.push(Span::styled("▍", bg.fg(tile.color)));
                let room = tile_w - 1;
                let (text, style) = match line {
                    1 => (tile.label.to_string(), bg.fg(t.muted)),
                    2 => (tile.value.clone(), theme::bold(tile.color).bg(t.raised)),
                    3 => (tile.note.clone(), bg.fg(t.subtext)),
                    _ => (String::new(), bg),
                };
                spans.push(Span::styled(pad(&format!(" {text}"), room), style));
            }
            out.push(Line::from(spans));
        }
    }
}

// ---------------------------------------------------------------- keys and settings

fn key_line(k: &KeyPart, name_w: usize) -> Line<'static> {
    let t = theme::current();
    let (role, color) = if k.sort {
        (" SORT      ", t.accent)
    } else {
        (" PARTITION ", t.key)
    };
    let ty = match k.ty.as_str() {
        "S" => "String",
        "N" => "Number",
        "B" => "Binary",
        _ => "?",
    };
    Line::from(vec![
        Span::raw("   "),
        Span::styled(role, theme::pill(t.on_accent, color)),
        Span::raw("  "),
        Span::styled(pad(&k.name, name_w), theme::bold(t.text)),
        Span::styled(format!("   {} · {ty}", k.ty), theme::muted()),
    ])
}

fn setting_line(s: &Setting, width: usize) -> Vec<Span<'static>> {
    let t = theme::current();
    let (dot, value) = match s.state {
        State::On => (t.ok, theme::fg(t.text)),
        State::Off => (t.muted, theme::fg(t.subtext)),
        State::Plain => (t.border, theme::fg(t.text)),
        State::Unavailable => (t.warn, theme::fg(t.warn)),
    };
    vec![
        Span::styled("   ● ", theme::fg(dot)),
        Span::styled(pad(s.label, LABEL_W), theme::fg(t.subtext)),
        Span::styled(
            fit(&s.value, false, width.saturating_sub(LABEL_W + 5)),
            value,
        ),
    ]
}

/// Protection and settings side by side when there is room, one after the other otherwise.
fn settings(out: &mut Vec<Line<'static>>, d: &TableDetails, width: usize) {
    if width >= 100 {
        let col = width / 2;
        let mut header = section("PROTECTION", None).spans;
        let used: usize = header.iter().map(|s| s.content.width()).sum();
        header.push(Span::raw(" ".repeat(col.saturating_sub(used))));
        header.extend(section("SETTINGS", None).spans);
        out.push(Line::from(header));
        for i in 0..d.protection.len().max(d.settings.len()) {
            let mut spans = match d.protection.get(i) {
                Some(s) => setting_line(s, col),
                None => Vec::new(),
            };
            let used: usize = spans.iter().map(|s| s.content.width()).sum();
            spans.push(Span::raw(" ".repeat(col.saturating_sub(used))));
            if let Some(s) = d.settings.get(i) {
                spans.extend(setting_line(s, width - col));
            }
            out.push(Line::from(spans));
        }
    } else {
        out.push(section("PROTECTION", None));
        out.extend(
            d.protection
                .iter()
                .map(|s| Line::from(setting_line(s, width))),
        );
        out.push(Line::raw(""));
        out.push(section("SETTINGS", None));
        out.extend(
            d.settings
                .iter()
                .map(|s| Line::from(setting_line(s, width))),
        );
    }
}

// ---------------------------------------------------------------- indexes and tags

fn key_text(keys: &[KeyPart], sort: bool) -> String {
    keys.iter()
        .find(|k| k.sort == sort)
        .map_or("—".to_string(), |k| format!("{} · {}", k.name, k.ty))
}

fn indexes(out: &mut Vec<Line<'static>>, title: &str, list: &[IndexDetails], width: usize) {
    let t = theme::current();
    out.push(section(title, Some(list.len())));
    if list.is_empty() {
        out.push(Line::styled("   none", theme::muted()));
        return;
    }
    let with_status = list.iter().any(|i| !i.status.is_empty());
    let with_capacity = list.iter().any(|i| i.capacity.is_some());
    let mut headers = vec!["NAME", "PARTITION KEY", "SORT KEY", "PROJECTION"];
    if with_status {
        headers.push("STATUS");
    }
    headers.extend(["ITEMS", "SIZE"]);
    if with_capacity {
        headers.push("READ / WRITE");
    }
    let rows: Vec<Vec<String>> = list
        .iter()
        .map(|i| {
            let mut row = vec![
                i.name.clone(),
                key_text(&i.keys, false),
                key_text(&i.keys, true),
                i.projection.clone(),
            ];
            if with_status {
                row.push(format!("● {}", i.status));
            }
            row.push(format!("~{}", grouped(i.item_count)));
            row.push(bytes(i.size_bytes));
            if with_capacity {
                row.push(i.capacity.map_or("—".into(), |(r, w)| format!("{r} / {w}")));
            }
            row
        })
        .collect();
    let mut widths: Vec<usize> = headers
        .iter()
        .enumerate()
        .map(|(c, h)| {
            rows.iter()
                .map(|r| r[c].width())
                .max()
                .unwrap_or(0)
                .max(h.width())
        })
        .collect();
    let style_for = |header: &str, index: &IndexDetails| match header {
        "NAME" => theme::bold(t.text),
        "PARTITION KEY" | "SORT KEY" => theme::fg(t.key),
        "STATUS" if index.status == "ACTIVE" => theme::fg(t.ok),
        "STATUS" => theme::fg(t.warn),
        "ITEMS" | "SIZE" | "READ / WRITE" => theme::fg(t.number),
        _ => theme::fg(t.subtext),
    };
    // The projection gives way first, since it can list many attributes; when the table
    // still does not fit, each index gets its own block of lines.
    let total = |widths: &[usize]| widths.iter().sum::<usize>() + 3 * widths.len() + 3;
    let over = total(&widths).saturating_sub(width);
    widths[3] = widths[3].saturating_sub(over).max(widths[3].min(12));
    if total(&widths) > width {
        for (row, index) in rows.iter().zip(list) {
            let mut first = vec![
                Span::raw("   "),
                Span::styled(index.name.clone(), theme::bold(t.text)),
            ];
            if with_status {
                first.push(Span::styled(
                    format!("   ● {}", index.status),
                    style_for("STATUS", index),
                ));
            }
            out.push(Line::from(first));
            for (c, text) in row.iter().enumerate() {
                if matches!(headers[c], "NAME" | "STATUS") {
                    continue;
                }
                out.push(Line::from(vec![
                    Span::styled(
                        format!("     {}", pad(&headers[c].to_lowercase(), 15)),
                        theme::muted(),
                    ),
                    Span::styled(
                        fit(text, false, width.saturating_sub(20)),
                        style_for(headers[c], index),
                    ),
                ]));
            }
        }
        return;
    }
    let cell = |text: &str, c: usize| pad(&fit(text, false, widths[c]), widths[c] + 3);
    let mut header = vec![Span::raw("   ")];
    header.extend(
        headers
            .iter()
            .enumerate()
            .map(|(c, h)| Span::styled(cell(h, c), theme::bold(t.muted))),
    );
    out.push(Line::from(header));
    for (row, index) in rows.iter().zip(list) {
        let mut spans = vec![Span::raw("   ")];
        for (c, text) in row.iter().enumerate() {
            spans.push(Span::styled(cell(text, c), style_for(headers[c], index)));
        }
        out.push(Line::from(spans));
    }
}

/// Tag keys whose production value is worth a warning color.
const ENV_KEYS: [&str; 5] = ["env", "environment", "stage", "tier", "account"];

fn tags(out: &mut Vec<Line<'static>>, d: &TableDetails, width: usize) {
    let t = theme::current();
    match &d.tags {
        Err(e) => {
            out.push(section("TAGS", None));
            out.push(Line::from(vec![
                Span::styled("   ● ", theme::fg(t.warn)),
                Span::styled(
                    fit(&format!("unavailable: {e}"), false, width.saturating_sub(5)),
                    theme::fg(t.warn),
                ),
            ]));
        }
        Ok(tags) if tags.is_empty() => {
            out.push(section("TAGS", Some(0)));
            out.push(Line::styled("   none", theme::muted()));
        }
        Ok(tags) => {
            out.push(section("TAGS", Some(tags.len())));
            out.extend(tag_rows(tags, width));
        }
    }
}

/// Tags as a two-column table, one tag per line: the key in the accent color, the value
/// beside it. Only a production value takes a warning color. AWS's own `aws:` tags follow in
/// a muted group, aligned on their own so their long keys do not push the others aside.
fn tag_rows(tags: &[(String, String)], width: usize) -> Vec<Line<'static>> {
    let t = theme::current();
    let (system, own): (Vec<_>, Vec<_>) = tags.iter().partition(|(k, _)| k.starts_with("aws:"));
    let key_width = |group: &[&(String, String)]| {
        group
            .iter()
            .map(|(k, _)| k.width())
            .max()
            .unwrap_or(0)
            .clamp(3, 32)
    };
    let first = if own.is_empty() { &system } else { &own };
    let mut rows = vec![Line::from(vec![
        Span::styled(
            format!("   {}", pad("KEY", key_width(first) + 3)),
            theme::bold(t.muted),
        ),
        Span::styled("VALUE", theme::bold(t.muted)),
    ])];
    for (group, muted) in [(&own, false), (&system, true)] {
        if group.is_empty() {
            continue;
        }
        if muted && !own.is_empty() {
            rows.push(Line::raw(""));
        }
        let key_w = key_width(group);
        for (k, v) in group.iter() {
            let prod =
                !muted && ENV_KEYS.contains(&k.to_lowercase().as_str()) && app::is_production(v);
            let (key_style, value_style) = if muted {
                (theme::muted(), theme::muted())
            } else if prod {
                (theme::fg(t.accent), theme::bold(t.err))
            } else {
                (theme::fg(t.accent), theme::fg(t.text))
            };
            let value = if v.is_empty() { "—" } else { v.as_str() };
            rows.push(Line::from(vec![
                Span::styled(format!("   {}", pad(k, key_w + 3)), key_style),
                Span::styled(
                    fit(value, false, width.saturating_sub(key_w + 6)),
                    value_style,
                ),
            ]));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(line: &Line) -> Vec<String> {
        line.spans.iter().map(|s| s.content.to_string()).collect()
    }

    fn tags(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn tags_are_a_key_value_table_with_aws_tags_last() {
        let rows = tag_rows(
            &tags(&[
                ("aws:cloudformation:stack-name", "demo"),
                ("env", "prod"),
                ("team", "data"),
            ]),
            80,
        );
        let text: Vec<String> = rows
            .iter()
            .map(|r| texts(r).concat().trim_end().to_string())
            .collect();
        assert_eq!(
            text,
            [
                "   KEY    VALUE",
                "   env    prod",
                "   team   data",
                "",
                "   aws:cloudformation:stack-name   demo",
            ]
        );
        assert_eq!(
            rows[1].spans[0].style.fg,
            Some(theme::current().accent),
            "keys stand out"
        );
        assert_eq!(
            rows[1].spans[1].style.fg,
            Some(theme::current().err),
            "production value"
        );
    }

    #[test]
    fn numbers_group_by_thousands() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(grouped(-1_000), "-1,000");
    }
}
