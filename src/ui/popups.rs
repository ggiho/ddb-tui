//! Everything that floats over the workspace: the read form, the update form, PartiQL, the
//! region and saved-operation pickers, confirmations, prompts, the key list and the command
//! palette.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, List, ListItem, ListState, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::{centered, clear_popup, fit};
use crate::app::{self, App, Confirm};
use crate::ddb::{self, SkOp};
use crate::form::Field;
use crate::{highlight, item, theme};

/// A floating card with a rounded border in `color`; returns its inside.
fn card(f: &mut Frame, area: Rect, title: &str, color: Color) -> Rect {
    let t = theme::current();
    clear_popup(f, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::fg(color))
        .title(Span::styled(format!(" {title} "), theme::bold(color)))
        .style(Style::new().bg(t.raised).fg(t.text));
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

// ---------------------------------------------------------------- form pieces

const LABEL_W: usize = 16;

/// A form row: marker and label, then the value spans.
fn form_row(
    label: &str,
    required: bool,
    focused: bool,
    value: Vec<Span<'static>>,
) -> Line<'static> {
    let t = theme::current();
    let marker = if focused {
        Span::styled(" › ", theme::bold(t.accent))
    } else {
        Span::raw("   ")
    };
    let label_style = if focused {
        theme::bold(t.accent)
    } else {
        theme::fg(t.text)
    };
    let mut spans = vec![marker, Span::styled(label.to_string(), label_style)];
    let mut used = label.width();
    if required {
        spans.push(Span::styled(" *", theme::fg(t.err)));
        used += 2;
    }
    spans.push(Span::raw(" ".repeat(LABEL_W.saturating_sub(used))));
    spans.extend(value);
    Line::from(spans)
}

/// A text input `width` columns wide: the value with a cursor while focused, a dim example
/// while empty.
fn text_input(text: &str, placeholder: &str, focused: bool, width: usize) -> Vec<Span<'static>> {
    let t = theme::current();
    let bg = Style::new().bg(if focused { t.select } else { t.surface });
    let mut spans = Vec::new();
    let mut used;
    if focused {
        let typed = fit(text, true, width.saturating_sub(2));
        let body = typed.strip_suffix('▏').unwrap_or(&typed);
        spans.push(Span::styled(format!(" {body}"), bg.fg(t.text)));
        spans.push(Span::styled("▏", bg.fg(t.accent)));
        used = 1 + typed.width();
    } else {
        let shown = fit(text, false, width.saturating_sub(2));
        used = 1 + shown.width();
        spans.push(Span::styled(format!(" {shown}"), bg.fg(t.text)));
    }
    if text.is_empty() {
        let hint = fit(placeholder, false, width.saturating_sub(used + 1));
        used += hint.width();
        spans.push(Span::styled(
            hint,
            bg.fg(t.muted).add_modifier(Modifier::ITALIC),
        ));
    }
    spans.push(Span::styled(" ".repeat(width.saturating_sub(used)), bg));
    spans
}

/// Options side by side with the chosen one highlighted.
fn choice(options: &[&str], chosen: usize, focused: bool) -> Vec<Span<'static>> {
    let t = theme::current();
    let mut spans = Vec::new();
    for (i, o) in options.iter().enumerate() {
        let style = if i == chosen && focused {
            theme::pill(t.on_accent, t.accent)
        } else if i == chosen {
            theme::pill(t.text, t.select)
        } else {
            theme::muted()
        };
        spans.push(Span::styled(format!(" {o} "), style));
        spans.push(Span::raw(" "));
    }
    spans
}

/// A value picked with ←/→ from a longer list.
fn cycle(value: &str, focused: bool) -> Vec<Span<'static>> {
    let t = theme::current();
    let arrows = if focused {
        theme::accent()
    } else {
        theme::muted()
    };
    let value_style = if focused {
        theme::bold(t.text)
    } else {
        theme::fg(t.text)
    };
    vec![
        Span::styled("◀ ", arrows),
        Span::styled(value.to_string(), value_style),
        Span::styled(" ▶", arrows),
    ]
}

fn error_line(message: &str) -> Line<'static> {
    Line::styled(
        format!("{:w$}✖ {message}", "", w = LABEL_W + 3),
        theme::fg(theme::current().err),
    )
}

fn section(title: &str) -> Line<'static> {
    Line::styled(format!(" {title}"), theme::bold(theme::current().muted))
}

fn hint(text: &str) -> Line<'static> {
    Line::styled(format!(" {text}"), theme::muted())
}

/// Matching attribute names after an ambiguous Tab.
fn hint_line(app: &App) -> Option<Line<'static>> {
    if app.hint.is_empty() {
        return None;
    }
    let t = theme::current();
    let mut spans = vec![Span::styled(
        format!("{:w$}Tab ", "", w = LABEL_W + 3),
        theme::muted(),
    )];
    for name in app.hint.iter().take(8) {
        spans.push(Span::styled(
            format!(" {name} "),
            Style::new().bg(t.surface).fg(t.key),
        ));
        spans.push(Span::raw(" "));
    }
    if app.hint.len() > 8 {
        spans.push(Span::styled(
            format!("+{}", app.hint.len() - 8),
            theme::muted(),
        ));
    }
    Some(Line::from(spans))
}

// ---------------------------------------------------------------- read and update forms

pub(super) fn draw_form(f: &mut Frame, app: &App, area: Rect) {
    let Some(t) = &app.table else { return };
    let form = &app.form;
    let idx = form.selected_index(t);
    let active = form.active(t);
    let width = (area.width as usize).saturating_sub(4).min(92);
    let value_w = width.saturating_sub(LABEL_W + 6);
    let error_for = |field: Field| {
        app.form_error
            .as_ref()
            .filter(|(f, _)| *f == field)
            .map(|(_, m)| error_line(m))
    };
    let mut lines: Vec<Line> = Vec::new();
    let mut push = |line: Line<'static>, field: Option<Field>| {
        lines.push(line);
        if let Some(e) = field.and_then(error_for) {
            lines.push(e);
        }
    };

    // Query / Scan tabs
    let mode_focused = active == Field::Mode;
    let mut tabs = vec![if mode_focused {
        Span::styled(" › ", theme::bold(theme::current().accent))
    } else {
        Span::raw("   ")
    }];
    tabs.extend(choice(
        &["Query", "Scan"],
        usize::from(!form.query),
        mode_focused,
    ));
    tabs.push(Span::styled(
        if form.query {
            "  read one partition by key"
        } else {
            "  read the whole table or index"
        },
        theme::muted(),
    ));
    push(Line::raw(""), None);
    push(Line::from(tabs), None);
    push(Line::raw(""), None);

    push(section("SOURCE"), None);
    push(
        form_row(
            "Index",
            false,
            active == Field::Index,
            cycle(
                idx.name.as_deref().unwrap_or("table"),
                active == Field::Index,
            ),
        ),
        Some(Field::Index),
    );
    if form.query {
        push(
            form_row(
                &format!("{} ({})", idx.pk.name, idx.pk.ty),
                true,
                active == Field::Pk,
                text_input(
                    &form.pk,
                    "partition key value",
                    active == Field::Pk,
                    value_w,
                ),
            ),
            Some(Field::Pk),
        );
        if let Some(sk) = &idx.sk {
            let op = form.sk_op().map_or("any", |o| o.label());
            push(
                form_row(
                    &format!("{} ({})", sk.name, sk.ty),
                    false,
                    active == Field::Op,
                    cycle(op, active == Field::Op),
                ),
                Some(Field::Op),
            );
            if form.op > 0 {
                let between = form.sk_op() == Some(SkOp::Between);
                push(
                    form_row(
                        if between { "  from" } else { "  value" },
                        true,
                        active == Field::Sk,
                        text_input(&form.sk, "sort key value", active == Field::Sk, value_w),
                    ),
                    Some(Field::Sk),
                );
                if between {
                    push(
                        form_row(
                            "  to",
                            true,
                            active == Field::Sk2,
                            text_input(&form.sk2, "upper bound", active == Field::Sk2, value_w),
                        ),
                        Some(Field::Sk2),
                    );
                }
            }
        }
    }
    push(Line::raw(""), None);
    push(section("FILTER & PROJECTION"), None);
    push(
        form_row(
            "Filter",
            false,
            active == Field::Filter,
            text_input(
                &form.filter,
                "status = \"open\" AND size(tags) > 2",
                active == Field::Filter,
                value_w,
            ),
        ),
        Some(Field::Filter),
    );
    push(
        form_row(
            "Projection",
            false,
            active == Field::Projection,
            text_input(
                &form.projection,
                "pk, name, profile.city   (empty: all attributes)",
                active == Field::Projection,
                value_w,
            ),
        ),
        Some(Field::Projection),
    );
    if let Some(h) = hint_line(app) {
        push(h, None);
    }
    push(Line::raw(""), None);
    push(section("OPTIONS"), None);
    push(
        form_row(
            "Consistent",
            false,
            active == Field::Consistent,
            choice(
                &["off", "on"],
                usize::from(form.consistent),
                active == Field::Consistent,
            ),
        ),
        None,
    );
    if form.query {
        push(
            form_row(
                "Order",
                false,
                active == Field::Order,
                choice(
                    &["ascending", "descending"],
                    usize::from(form.descending),
                    active == Field::Order,
                ),
            ),
            None,
        );
    }
    push(Line::raw(""), None);

    let popup = centered(area, width as u16, lines.len() as u16 + 2);
    let inner = card(
        f,
        popup,
        &format!("Read · {}", t.name),
        theme::current().accent,
    );
    f.render_widget(Paragraph::new(lines), inner);
}

pub(super) fn draw_update(f: &mut Frame, app: &App, area: Rect) {
    let form = &app.update_form;
    let width = (area.width as usize).saturating_sub(4).min(92);
    let value_w = width.saturating_sub(LABEL_W + 6);
    let mut lines = vec![
        Line::raw(""),
        form_row(
            "Update",
            true,
            !form.on_condition,
            text_input(
                &form.update,
                "SET views = views + 1 REMOVE draft",
                !form.on_condition,
                value_w,
            ),
        ),
    ];
    if let Some((false, m)) = &app.update_error {
        lines.push(error_line(m));
    }
    lines.push(form_row(
        "Condition",
        false,
        form.on_condition,
        text_input(
            &form.condition,
            "optional, e.g. attribute_exists(pk)",
            form.on_condition,
            value_w,
        ),
    ));
    if let Some((true, m)) = &app.update_error {
        lines.push(error_line(m));
    }
    if let Some(h) = hint_line(app) {
        lines.push(h);
    }
    lines.push(Line::raw(""));
    lines.push(section("EXAMPLES"));
    for example in [
        "SET a = a + 1, b = list_append(b, [\"x\"]), c = if_not_exists(c, 0)",
        "REMOVE old       ADD tags <<\"new\">>       DELETE tags <<\"gone\">>",
    ] {
        lines.push(Line::styled(format!("   {example}"), theme::muted()));
    }
    lines.push(Line::raw(""));
    let popup = centered(area, width as u16, lines.len() as u16 + 2);
    let inner = card(f, popup, "UpdateItem", theme::current().accent);
    f.render_widget(Paragraph::new(lines), inner);
}

pub(super) fn draw_partiql(f: &mut Frame, app: &App, area: Rect) {
    let t = theme::current();
    let width = area.width.saturating_sub(4).min(100);
    let input_width = width.saturating_sub(6);
    let input = Paragraph::new(Line::from(vec![
        Span::styled(app.partiql.clone(), theme::fg(t.text)),
        Span::styled("▏", theme::accent()),
    ]))
    .wrap(Wrap { trim: false });
    let lines = input.line_count(input_width) as u16;
    let input_height = lines.clamp(3, 8);
    // Long statements scroll so the end, where typing happens, stays visible.
    let input = input.scroll((lines.saturating_sub(input_height), 0));
    let examples = vec![
        section("EXAMPLES"),
        hint("  SELECT * FROM \"Table\" WHERE pk = 'a'"),
        hint("  SELECT pk, name FROM \"Table\".\"index\" WHERE status = 'open'"),
        hint("  UPDATE \"Table\" SET n = 1 WHERE pk = 'a'"),
        Line::raw(""),
        hint("SELECT results are paged; other statements run once."),
    ];
    let height = input_height + examples.len() as u16 + 4;
    let popup = centered(area, width, height);
    let inner = card(f, popup, "PartiQL", t.accent);
    let [_, input_area, _, examples_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(input_height),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);
    let input_area = Rect {
        x: input_area.x + 1,
        width: input_area.width.saturating_sub(2),
        ..input_area
    };
    f.render_widget(Block::new().style(Style::new().bg(t.surface)), input_area);
    f.render_widget(
        input,
        Rect {
            x: input_area.x + 1,
            width: input_area.width.saturating_sub(2),
            ..input_area
        },
    );
    f.render_widget(Paragraph::new(examples), examples_area);
}

// ---------------------------------------------------------------- pickers

pub(super) fn draw_regions(f: &mut Frame, app: &mut App, area: Rect) {
    let t = theme::current();
    let items: Vec<ListItem> = ddb::REGIONS
        .iter()
        .map(|r| {
            let mut spans = vec![Span::styled(r.to_string(), theme::fg(t.text))];
            if *r == app.region {
                spans.push(Span::styled("   current", theme::accent()));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let height = (items.len() as u16 + 2).min(area.height.saturating_sub(2));
    let popup = centered(area, 40, height);
    let inner = card(f, popup, "Region", t.accent);
    let list = List::new(items)
        .highlight_style(theme::selected(true))
        .highlight_symbol(Span::styled("▌ ", theme::fg(t.accent)));
    f.render_stateful_widget(list, inner, &mut app.region_state);
}

pub(super) fn draw_saved(f: &mut Frame, app: &mut App, area: Rect) {
    let t = theme::current();
    let width = area.width.saturating_sub(4).min(80);
    let row_width = width.saturating_sub(6) as usize;
    let items: Vec<ListItem> = app
        .saved
        .iter()
        .map(|e| {
            let name = fit(
                &e.name,
                false,
                row_width.saturating_sub(e.operation.width() + 2),
            );
            let pad = row_width.saturating_sub(name.width() + e.operation.width());
            ListItem::new(Line::from(vec![
                Span::styled(name, theme::fg(t.text)),
                Span::raw(" ".repeat(pad)),
                Span::styled(e.operation.clone(), theme::muted()),
            ]))
        })
        .collect();
    let height = (items.len() as u16 + 4).min(area.height.saturating_sub(2));
    let popup = centered(area, width, height);
    let inner = card(f, popup, "Saved operations", t.accent);
    let [list_area, _, note] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let list = List::new(items)
        .highlight_style(theme::selected(true))
        .highlight_symbol(Span::styled("▌ ", theme::fg(t.accent)));
    f.render_stateful_widget(list, list_area, &mut app.saved_state);
    f.render_widget(
        hint(&format!(
            "{} · {} · shared with NoSQL Workbench",
            app.conn_label, app.region
        )),
        note,
    );
}

// ---------------------------------------------------------------- confirmations and prompts

pub(super) fn draw_confirm(f: &mut Frame, app: &App, area: Rect) {
    let t = theme::current();
    let bold = theme::bold(t.text);
    let (title, danger, mut lines) = match &app.confirm {
        Some(Confirm::Save {
            table,
            create,
            key_changed,
            ..
        }) => {
            let mut lines = vec![Line::styled(
                if *create {
                    format!(" Create a new item in {table}?")
                } else {
                    format!(" Replace the item in {table}?")
                },
                bold,
            )];
            if *key_changed {
                lines.push(Line::styled(
                    " Key attributes changed: this writes a NEW item and leaves the original.",
                    theme::fg(t.warn),
                ));
            }
            if *create {
                lines.push(hint(
                    "Fails instead of overwriting if the key already exists.",
                ));
            }
            ("Save", false, lines)
        }
        Some(Confirm::Delete {
            table,
            keys,
            hidden,
        }) => {
            let mut lines = vec![Line::styled(
                format!(
                    " Delete {} item(s) from {table}? This cannot be undone.",
                    keys.len()
                ),
                theme::bold(t.err),
            )];
            for key in keys.iter().take(5) {
                lines.push(Line::styled(
                    format!("   {}", item::item_to_plain_json(key)),
                    theme::fg(t.subtext),
                ));
            }
            if keys.len() > 5 {
                lines.push(hint(&format!("  … and {} more", keys.len() - 5)));
            }
            if *hidden > 0 {
                lines.push(Line::styled(
                    format!(" {hidden} of the marked items are hidden by the current search."),
                    theme::fg(t.warn),
                ));
            }
            ("Delete", true, lines)
        }
        Some(Confirm::Statement(statement)) => {
            let width = (area.width as usize).saturating_sub(10).min(76);
            let mut lines = vec![
                Line::styled(" Run this PartiQL statement?", bold),
                hint("It changes data directly and cannot be undone."),
                Line::raw(""),
            ];
            // The whole statement, wrapped, up to eight lines.
            let chars: Vec<char> = statement.chars().collect();
            let chunks: Vec<String> = chars
                .chunks(width.max(1))
                .map(|c| c.iter().collect())
                .collect();
            for chunk in chunks.iter().take(8) {
                lines.push(Line::styled(format!("   {chunk}"), theme::fg(t.text)));
            }
            if chunks.len() > 8 {
                lines.push(hint("  …"));
            }
            ("Run statement", true, lines)
        }
        Some(Confirm::Update(req)) => (
            "Update",
            false,
            vec![
                Line::styled(format!(" Update this item in {}?", req.table), bold),
                hint(&format!("  {}", item::item_to_plain_json(&req.key))),
                Line::styled(format!("   {}", req.summary), theme::fg(t.text)),
            ],
        ),
        None => return,
    };
    lines.insert(0, Line::raw(""));
    if app::is_production(&app.conn_label) {
        lines.insert(
            0,
            Line::from(vec![
                Span::raw(" "),
                theme::production(" ● PRODUCTION connection "),
            ]),
        );
        lines.insert(0, Line::raw(""));
    }
    lines.push(Line::raw(""));
    let yes = if danger {
        theme::pill(t.on_accent, t.err)
    } else {
        theme::pill(t.on_accent, t.accent)
    };
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(" y  yes ", yes),
        Span::raw("   "),
        Span::styled(" n  cancel ", Style::new().bg(t.select).fg(t.text)),
    ]));
    lines.push(Line::raw(""));
    let width = (area.width.saturating_sub(4)).min(84);
    let popup = centered(area, width, lines.len() as u16 + 2);
    let color = if danger { t.err } else { t.accent };
    let inner = card(f, popup, title, color);
    f.render_widget(Paragraph::new(lines), inner);
}

pub(super) fn draw_prompt(f: &mut Frame, app: &App, area: Rect) {
    let Some(p) = &app.prompt else { return };
    let width = (area.width as usize).saturating_sub(4).min(84);
    let mut input = vec![Span::raw(" ")];
    input.extend(text_input(
        &p.text,
        "file name",
        true,
        width.saturating_sub(5),
    ));
    let lines = vec![
        Line::raw(""),
        Line::from(input),
        Line::raw(""),
        hint("Enter writes (never overwrites)   Esc cancels"),
    ];
    let popup = centered(area, width as u16, lines.len() as u16 + 2);
    let inner = card(f, popup, p.title.trim(), theme::current().accent);
    f.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------- keys and commands

pub(super) fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let t = theme::current();
    let mut entries: Vec<(&str, &str)> = app.bindings().iter().map(|b| (b.keys, b.label)).collect();
    if app.palette_allowed() {
        entries.push(("⌃P", "command palette"));
    }
    let line = |(k, what): &(&str, &str)| {
        Line::from(vec![
            Span::styled(format!(" {k:>11}  "), theme::bold(t.accent)),
            Span::styled(what.to_string(), theme::fg(t.text)),
        ])
    };
    // Two columns when one would not fit.
    let room = area.height.saturating_sub(5) as usize;
    let columns = if entries.len() > room { 2 } else { 1 };
    let per_column = entries.len().div_ceil(columns);
    let col_width: u16 = 44;
    let width = (col_width * columns as u16 + 2).min(area.width);
    let height = per_column as u16 + 4;
    let popup = centered(area, width, height);
    let inner = card(f, popup, "Keys", t.accent);
    let [body, _, note] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    for (i, chunk) in entries.chunks(per_column.max(1)).enumerate() {
        let x = body.x + i as u16 * col_width;
        if x >= body.right() {
            break;
        }
        let column = Rect {
            x,
            width: col_width.min(body.right() - x),
            ..body
        };
        f.render_widget(
            Paragraph::new(chunk.iter().map(line).collect::<Vec<_>>()),
            column,
        );
    }
    f.render_widget(hint("any key closes"), note);
}

pub(super) fn draw_palette(f: &mut Frame, app: &App, area: Rect) {
    let t = theme::current();
    let Some(palette) = &app.palette else { return };
    let entries = app.palette_entries();
    let width = area.width.saturating_sub(4).min(72);
    let rows = (entries.len().max(1) as u16).min(12);
    let height = (rows + 4).min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + (area.height.saturating_sub(height) / 4),
        width,
        height,
    };
    let inner = card(f, popup, "Commands", t.accent);
    let [input, rule, list_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);
    let mut spans = vec![Span::styled(" › ", theme::bold(t.accent))];
    if palette.query.is_empty() {
        spans.push(Span::styled("▏", theme::accent()));
        spans.push(Span::styled(
            "type a command or a table name",
            theme::muted().add_modifier(Modifier::ITALIC),
        ));
    } else {
        let shown = fit(&palette.query, true, input.width.saturating_sub(4) as usize);
        spans.push(Span::styled(
            shown.strip_suffix('▏').unwrap_or(&shown).to_string(),
            theme::fg(t.text),
        ));
        spans.push(Span::styled("▏", theme::accent()));
    }
    f.render_widget(Line::from(spans), input);
    f.render_widget(
        Line::styled("─".repeat(rule.width as usize), theme::fg(t.border)),
        rule,
    );
    if entries.is_empty() {
        f.render_widget(hint("No matching command"), list_area);
        return;
    }
    let row_width = list_area.width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = entries
        .iter()
        .map(|e| {
            let label = capitalize(&e.label);
            let keys_width = if e.keys.is_empty() {
                0
            } else {
                e.keys.width() + 2
            };
            let shown = fit(&label, false, row_width.saturating_sub(keys_width + 1));
            let mut spans = highlight::find_spans(&shown, &palette.query, theme::fg(t.text));
            spans.push(Span::raw(
                " ".repeat(row_width.saturating_sub(shown.width() + keys_width)),
            ));
            if !e.keys.is_empty() {
                spans.push(Span::styled(
                    format!(" {} ", e.keys),
                    Style::new().bg(t.surface).fg(t.subtext),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(palette.selected));
    let list = List::new(items)
        .highlight_style(theme::selected(true))
        .highlight_symbol(Span::styled("▌ ", theme::fg(t.accent)));
    f.render_stateful_widget(list, list_area, &mut state);
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
