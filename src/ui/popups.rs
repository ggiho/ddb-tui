//! Everything that floats over the workspace: the read form, the update form, PartiQL, the
//! region and saved-operation pickers, confirmations, prompts, the key list and the command
//! palette.

use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, List, ListItem, ListState, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::{centered, clear_popup, fit};
use crate::app::{
    App, Click, Confirm, ItemPreview, JobKind, PromptAction, UpdateField, UpdateScope, plural,
};
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

/// Clickable spots worked out while a popup's lines are built: line, column, width and what a
/// click does. A width of `u16::MAX` spans the line.
type Spots = Vec<(usize, u16, u16, Click)>;

/// Makes `spots`, placed inside `inner`, the clickable parts of the topmost popup `popup`.
fn register(app: &mut App, popup: Rect, inner: Rect, spots: Spots) {
    app.hit.popup = popup;
    app.hit.buttons = spots
        .into_iter()
        .filter_map(|(line, x, width, click)| {
            let y = inner.y + u16::try_from(line).ok()?;
            let x = inner.x.checked_add(x)?;
            (y < inner.bottom() && x < inner.right()).then(|| {
                let width = width.min(inner.right() - x);
                (
                    Rect {
                        x,
                        y,
                        width,
                        height: 1,
                    },
                    click,
                )
            })
        })
        .collect();
}

/// The spots of a `choice` row whose first option starts at column `x`.
fn choice_spots(line: usize, x: u16, options: &[&str], click: impl Fn(usize) -> Click) -> Spots {
    let mut x = x;
    options
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let width = o.width() as u16 + 2;
            let spot = (line, x, width, click(i));
            x += width + 1;
            spot
        })
        .collect()
}

/// A row of pill buttons and their spots on `line`.
fn buttons(line: usize, items: &[(&str, Style, Click)]) -> (Line<'static>, Spots) {
    let mut spans = vec![Span::raw(" ")];
    let mut spots = Vec::new();
    let mut x = 1;
    for (label, style, click) in items {
        let text = format!(" {label} ");
        let width = text.width() as u16;
        spots.push((line, x, width, *click));
        spans.push(Span::styled(text, *style));
        spans.push(Span::raw("   "));
        x += width + 3;
    }
    (Line::from(spans), spots)
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

/// The column where a form row's value starts: after the marker and the label, which a long
/// key name can push past `LABEL_W`.
fn value_col(label: &str, required: bool) -> u16 {
    (3 + LABEL_W.max(label.width() + if required { 2 } else { 0 })) as u16
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

pub(super) fn draw_form(f: &mut Frame, app: &mut App, area: Rect) {
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
    let sk_label = idx.sk.as_ref().map(|sk| format!("{} ({})", sk.name, sk.ty));
    let mut lines: Vec<Line> = Vec::new();
    let mut rows: Vec<(usize, Field)> = Vec::new();
    let mut push = |line: Line<'static>, field: Option<Field>| {
        if let Some(field) = field {
            rows.push((lines.len(), field));
        }
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
    push(Line::from(tabs), Some(Field::Mode));
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
        if let Some(label) = &sk_label {
            let op = form.sk_op().map_or("any", |o| o.label());
            push(
                form_row(
                    label,
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
        Some(Field::Consistent),
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
            Some(Field::Order),
        );
    }
    push(Line::raw(""), None);

    // A click on a row focuses it; on an option or an arrow it also picks that value.
    let index_name = idx.name.clone().unwrap_or_else(|| "table".into());
    let op_name = form.sk_op().map_or("any", |o| o.label()).to_string();
    let mut spots: Spots = Vec::new();
    for &(line, field) in &rows {
        spots.push((line, 0, u16::MAX, Click::FormField(field)));
        let options: &[&str] = match field {
            Field::Mode => &["Query", "Scan"],
            Field::Consistent => &["off", "on"],
            Field::Order => &["ascending", "descending"],
            _ => &[],
        };
        let value_x = match field {
            Field::Op => value_col(sk_label.as_deref().unwrap_or_default(), false),
            _ => value_col("", false),
        };
        let x = if field == Field::Mode { 3 } else { value_x };
        spots.extend(choice_spots(line, x, options, |i| {
            Click::FormChoice(field, i)
        }));
        let value = match field {
            Field::Index => &index_name,
            Field::Op => &op_name,
            _ => continue,
        };
        spots.push((line, value_x, 2, Click::FormStep(field, -1)));
        spots.push((
            line,
            value_x + 2 + value.width() as u16,
            2,
            Click::FormStep(field, 1),
        ));
    }

    let title = format!("Read · {}", t.name);
    let popup = centered(area, width as u16, lines.len() as u16 + 2);
    let inner = card(f, popup, &title, theme::current().accent);
    f.render_widget(Paragraph::new(lines), inner);
    register(app, popup, inner, spots);
}

pub(super) fn draw_update(f: &mut Frame, app: &mut App, area: Rect) {
    let form = &app.update_form;
    let width = (area.width as usize).saturating_sub(4).min(92);
    let value_w = width.saturating_sub(LABEL_W + 6);
    let on = |field: UpdateField| form.field == field;
    let mut lines = vec![
        Line::raw(""),
        form_row(
            "Update",
            true,
            on(UpdateField::Update),
            text_input(
                &form.update,
                "SET views = views + 1 REMOVE draft",
                on(UpdateField::Update),
                value_w,
            ),
        ),
    ];
    let mut spots: Spots = vec![(1, 0, u16::MAX, Click::UpdateField(UpdateField::Update))];
    if let Some((false, m)) = &app.update_error {
        lines.push(error_line(m));
    }
    spots.push((
        lines.len(),
        0,
        u16::MAX,
        Click::UpdateField(UpdateField::Condition),
    ));
    lines.push(form_row(
        "Condition",
        false,
        on(UpdateField::Condition),
        text_input(
            &form.condition,
            "optional, e.g. attribute_exists(pk)",
            on(UpdateField::Condition),
            value_w,
        ),
    ));
    if let Some((true, m)) = &app.update_error {
        lines.push(error_line(m));
    }
    let scopes = app.update_scopes();
    if scopes.len() > 1 {
        let labels: Vec<&str> = scopes.iter().map(|(_, l)| l.as_str()).collect();
        let chosen = scopes
            .iter()
            .position(|(s, _)| *s == form.scope)
            .unwrap_or(0);
        let line = lines.len();
        spots.push((line, 0, u16::MAX, Click::UpdateField(UpdateField::Scope)));
        spots.extend(choice_spots(line, (3 + LABEL_W) as u16, &labels, |i| {
            Click::UpdateScope(scopes[i].0)
        }));
        lines.push(form_row(
            "Apply to",
            false,
            on(UpdateField::Scope),
            choice(&labels, chosen, on(UpdateField::Scope)),
        ));
        if form.scope == UpdateScope::All {
            lines.push(Line::styled(
                format!(
                    "{:w$}Every item the read matches, on all pages; keys are read first.",
                    "",
                    w = LABEL_W + 3
                ),
                theme::fg(theme::current().warn),
            ));
        }
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
    let title = match app.update_count() {
        n if n > 1 => format!("UpdateItem · {n} marked items"),
        _ => "UpdateItem".to_string(),
    };
    let inner = card(f, popup, &title, theme::current().accent);
    f.render_widget(Paragraph::new(lines), inner);
    register(app, popup, inner, spots);
}

/// "what it would change" lines for the update confirmations: the first few items, read
/// fresh, with each touched attribute's current and new value.
fn preview_lines(app: &App, req: &ddb::UpdateRequest) -> Vec<Line<'static>> {
    let t = theme::current();
    let mut lines = vec![
        Line::raw(""),
        section("PREVIEW  · read just now; DynamoDB checks the condition"),
    ];
    let Some(rows) = app.update_preview(req) else {
        lines.push(hint("  reading the items…"));
        return lines;
    };
    let value = |v: &Option<aws_sdk_dynamodb::types::AttributeValue>| {
        v.as_ref()
            .map_or("—".to_string(), |v| fit(&item::text(v), false, 28))
    };
    for (key, preview) in rows {
        lines.push(Line::styled(
            format!("   {}", item::item_to_plain_json(&key)),
            theme::fg(t.subtext),
        ));
        let changes = match preview {
            ItemPreview::Changes(changes) => changes,
            ItemPreview::Gone => {
                lines.push(Line::styled(
                    "     no longer exists; it will not be updated",
                    theme::fg(t.warn),
                ));
                continue;
            }
            ItemPreview::Unread(e) => {
                lines.push(hint(&format!("    could not be read: {e}")));
                continue;
            }
        };
        if changes.is_empty() {
            lines.push(hint("    not worked out here"));
        }
        for c in changes {
            let (after, style) = match &c.after {
                Some(Some(v)) => (fit(&item::text(v), false, 28), theme::fg(t.accent)),
                Some(None) => ("(removed)".to_string(), theme::fg(t.warn)),
                None => ("?".to_string(), theme::muted()),
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("     {:<18}", fit(&c.path, false, 18)),
                    theme::fg(t.text),
                ),
                Span::styled(value(&c.before), theme::muted()),
                Span::styled("  →  ", theme::muted()),
                Span::styled(after, style),
            ]));
        }
    }
    lines
}

pub(super) fn draw_partiql(f: &mut Frame, app: &mut App, area: Rect) {
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
    register(app, popup, inner, Vec::new());
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
    let offset = app.region_state.offset();
    let spots = (0..inner.height as usize)
        .filter(|row| offset + row < ddb::REGIONS.len())
        .map(|row| (row, 0, u16::MAX, Click::ListRow(offset + row)))
        .collect();
    register(app, popup, inner, spots);
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
    let offset = app.saved_state.offset();
    let spots = (0..list_area.height as usize)
        .filter(|row| offset + row < app.saved.len())
        .map(|row| (row, 0, u16::MAX, Click::ListRow(offset + row)))
        .collect();
    register(app, popup, inner, spots);
}

// ---------------------------------------------------------------- confirmations and prompts

pub(super) fn draw_confirm(f: &mut Frame, app: &mut App, area: Rect) {
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
            let what = match keys.len() {
                1 => "this item".to_string(),
                n => format!("{n} items"),
            };
            let mut lines = vec![Line::styled(
                format!(" Delete {what} from {table}? This cannot be undone."),
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
        Some(Confirm::ExportAll { path }) => (
            "Export all results",
            false,
            vec![
                Line::styled(format!(" Read the whole result into {path}?"), bold),
                Line::styled(format!("   {}", app.read_all_estimate()), theme::fg(t.text)),
                hint("Pages are written as they arrive; Esc stops and removes the file."),
            ],
        ),
        Some(Confirm::CollectAll(req)) => (
            "Update all results",
            false,
            vec![
                Line::styled(
                    format!(" Read the keys of every result in {} first?", req.table),
                    bold,
                ),
                Line::styled(format!("   {}", app.read_all_estimate()), theme::fg(t.text)),
                Line::styled(format!("   {}", req.summary), theme::fg(t.text)),
                hint("Only keys are read. Nothing changes until you type the count next."),
            ],
        ),
        Some(Confirm::AllowWrites) => (
            "Allow writes",
            true,
            vec![
                Line::styled(
                    format!(" Allow writes to {} for this session?", app.conn_label),
                    bold,
                ),
                hint("Edits, deletes, updates and PartiQL writes become possible."),
                hint("Each still asks first, and W turns writes off again."),
            ],
        ),
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
        Some(Confirm::UpdateMany { req, keys, hidden }) => {
            let mut lines = vec![Line::styled(
                format!(" Update {} items in {}?", keys.len(), req.table),
                bold,
            )];
            for key in keys.iter().take(5) {
                lines.push(hint(&format!("  {}", item::item_to_plain_json(key))));
            }
            if keys.len() > 5 {
                lines.push(hint(&format!("  … and {} more", keys.len() - 5)));
            }
            lines.push(Line::styled(
                format!("   {}", req.summary),
                theme::fg(t.text),
            ));
            lines.extend(preview_lines(app, req));
            if *hidden > 0 {
                lines.push(Line::styled(
                    format!(" {hidden} of the marked items are hidden by the current search."),
                    theme::fg(t.warn),
                ));
            }
            lines.push(Line::raw(""));
            lines.push(hint(&format!(
                "Sent {} at a time; a failure does not stop the rest.",
                ddb::UPDATE_CONCURRENCY
            )));
            ("Update", false, lines)
        }
        Some(Confirm::Update(req)) => (
            "Update",
            false,
            vec![
                Line::styled(format!(" Update this item in {}?", req.table), bold),
                hint(&format!("  {}", item::item_to_plain_json(&req.key))),
                Line::styled(format!("   {}", req.summary), theme::fg(t.text)),
            ]
            .into_iter()
            .chain(preview_lines(app, req))
            .collect(),
        ),
        None => return,
    };
    lines.insert(0, Line::raw(""));
    if app.on_production() {
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
    let (row, spots) = buttons(
        lines.len(),
        &[
            ("y  yes", yes, Click::Key(KeyCode::Char('y'))),
            (
                "n  cancel",
                Style::new().bg(t.select).fg(t.text),
                Click::Key(KeyCode::Char('n')),
            ),
        ],
    );
    lines.push(row);
    lines.push(Line::raw(""));
    let width = (area.width.saturating_sub(4)).min(84);
    let popup = centered(area, width, lines.len() as u16 + 2);
    let color = if danger { t.err } else { t.accent };
    let inner = card(f, popup, title, color);
    f.render_widget(Paragraph::new(lines), inner);
    register(app, popup, inner, spots);
}

pub(super) fn draw_prompt(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(p) = &app.prompt else { return };
    if p.action == PromptAction::ConfirmCount {
        draw_count_prompt(f, app, area);
        return;
    }
    let width = (area.width as usize).saturating_sub(4).min(84);
    let mut input = vec![Span::raw(" ")];
    input.extend(text_input(
        &p.text,
        "file name",
        true,
        width.saturating_sub(5),
    ));
    let mut lines = vec![Line::raw(""), Line::from(input), Line::raw("")];
    let mut spots: Spots = Vec::new();
    if p.action == PromptAction::Export {
        let options = [format!("this {}", p.scope), "all results".to_string()];
        let options: Vec<&str> = options.iter().map(String::as_str).collect();
        spots = choice_spots(lines.len(), 1, &options, |i| Click::ExportScope(i == 1));
        let mut scope = vec![Span::raw(" ")];
        scope.extend(choice(&options, usize::from(p.all), true));
        scope.push(Span::styled("  Tab switches", theme::muted()));
        lines.push(Line::from(scope));
        lines.push(Line::raw(""));
    }
    lines.push(hint("Enter writes (never overwrites)   Esc cancels"));
    let title = p.title.trim().to_string();
    let popup = centered(area, width as u16, lines.len() as u16 + 2);
    let inner = card(f, popup, &title, theme::current().accent);
    f.render_widget(Paragraph::new(lines), inner);
    register(app, popup, inner, spots);
}

/// The last step before an update of all results: the count to type, the update and a preview.
fn draw_count_prompt(f: &mut Frame, app: &mut App, area: Rect) {
    let (Some(p), Some((req, keys))) = (&app.prompt, app.pending_update()) else {
        return;
    };
    let t = theme::current();
    let width = (area.width as usize).saturating_sub(4).min(92);
    let count = keys.len();
    let mut input = vec![Span::raw(" ")];
    input.extend(text_input(&p.text, &count.to_string(), true, 16));
    let mut lines = vec![Line::raw("")];
    if app.on_production() {
        lines.push(Line::from(vec![
            Span::raw(" "),
            theme::production(" ● PRODUCTION connection "),
        ]));
        lines.push(Line::raw(""));
    }
    lines.push(Line::styled(
        format!(
            " Update all {count} items the read matches in {}?",
            req.table
        ),
        theme::bold(t.err),
    ));
    lines.push(Line::styled(
        format!("   {}", req.summary),
        theme::fg(t.text),
    ));
    lines.extend(preview_lines(app, req));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(format!(" Type {count} to confirm "), theme::bold(t.text)),
        Span::raw(" "),
    ]));
    lines.push(Line::from(input));
    lines.push(Line::raw(""));
    lines.push(hint(
        "Enter updates · Esc cancels · nothing has changed yet",
    ));
    let popup = centered(area, width as u16, lines.len() as u16 + 2);
    let inner = card(f, popup, "Update all results", t.err);
    f.render_widget(Paragraph::new(lines), inner);
    register(app, popup, inner, Vec::new());
}

/// The running job: an export, the key search for an update of all results, or an update of
/// many items, with its counts and a bar when the total is known.
pub(super) fn draw_job(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(job) = &app.job else { return };
    let t = theme::current();
    let secs = job.started.elapsed().as_secs();
    let clock = Span::styled(
        format!("   ·   {}:{:02}", secs / 60, secs % 60),
        theme::fg(t.subtext),
    );
    let sep = |text: String| Span::styled(format!("   ·   {text}"), theme::fg(t.subtext));
    let spin = Span::styled(format!(" {} ", super::spinner()), theme::accent());
    let (title, what, counts, bar, note) = match &job.kind {
        JobKind::Export { path, progress: p } => (
            "Exporting",
            vec![
                spin,
                Span::styled("Reading every page into ", theme::fg(t.text)),
                Span::styled(path.clone(), theme::bold(t.text)),
            ],
            vec![
                Span::styled(format!("   {} items", p.items), theme::bold(t.number)),
                sep(format!(
                    "{} {}",
                    p.requests,
                    if p.requests == 1 {
                        "request"
                    } else {
                        "requests"
                    }
                )),
                sep(format!("{} RCU", p.consumed)),
                clock,
            ],
            None,
            "Esc cancels and removes the partial file",
        ),
        JobKind::Collect { found, consumed } => (
            "Finding the items",
            vec![
                spin,
                Span::styled(
                    "Reading the keys of every item the read matches",
                    theme::fg(t.text),
                ),
            ],
            vec![
                Span::styled(format!("   {found} items"), theme::bold(t.number)),
                sep(format!("{consumed} RCU")),
                clock,
            ],
            None,
            "Nothing is updated before you confirm the count · Esc cancels",
        ),
        JobKind::Update { progress: p, .. } => (
            "Updating",
            vec![
                spin,
                Span::styled(format!("Updating {} items", p.total), theme::fg(t.text)),
            ],
            vec![
                Span::styled(
                    format!("   {} of {} done", p.done, p.total),
                    theme::bold(t.number),
                ),
                Span::styled(
                    format!("   ·   {} failed", p.failed),
                    theme::fg(if p.failed > 0 { t.err } else { t.subtext }),
                ),
                sep(format!("{} WCU", p.consumed)),
                clock,
            ],
            Some((p.done, p.total)),
            "Esc stops sending; updates already sent still finish",
        ),
    };
    let width = area.width.saturating_sub(4).min(76);
    let mut lines = vec![
        Line::raw(""),
        Line::from(what),
        Line::raw(""),
        Line::from(counts),
    ];
    if let Some((done, total)) = bar {
        let room = (width as usize).saturating_sub(8);
        let filled = (done * room).checked_div(total).unwrap_or(0).min(room);
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled("━".repeat(filled), theme::fg(t.accent)),
            Span::styled("━".repeat(room - filled), theme::fg(t.border)),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(hint(note));
    lines.push(Line::raw(""));
    let (row, spots) = buttons(
        lines.len(),
        &[(
            "Esc  cancel",
            Style::new().bg(t.select).fg(t.text),
            Click::Key(KeyCode::Esc),
        )],
    );
    lines.push(row);
    let popup = centered(area, width, lines.len() as u16 + 2);
    let inner = card(f, popup, title, t.accent);
    f.render_widget(Paragraph::new(lines), inner);
    register(app, popup, inner, spots);
}

/// The outcome of an update of many items: counts, then each item not updated and why.
pub(super) fn draw_batch_result(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(result) = &app.batch_result else {
        return;
    };
    let t = theme::current();
    let r = &result.report;
    let width = area.width.saturating_sub(4).min(100);
    let mut lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                format!(" ✔ {} updated", r.updated),
                theme::bold(if r.updated == 0 { t.muted } else { t.ok }),
            ),
            Span::styled(
                format!("    ✖ {} failed", r.failed.len()),
                theme::bold(if r.failed.is_empty() { t.muted } else { t.err }),
            ),
            Span::styled(
                format!("    ◌ {} not sent", r.skipped.len()),
                theme::bold(if r.skipped.is_empty() {
                    t.muted
                } else {
                    t.warn
                }),
            ),
            Span::styled(
                format!(
                    "    {} WCU · {}:{:02}",
                    r.consumed.unwrap_or(0.0),
                    result.secs / 60,
                    result.secs % 60
                ),
                theme::fg(t.subtext),
            ),
        ]),
        Line::styled(format!("   {}", result.req.summary), theme::muted()),
    ];
    // Each reason once, with the keys it applies to below it; the scroll in the app counts
    // these lines the same way.
    let key_line = |key: &item::Item| {
        Line::styled(
            format!(
                "       {}",
                fit(
                    &item::item_to_plain_json(key).to_string(),
                    false,
                    (width as usize).saturating_sub(10)
                )
            ),
            theme::fg(t.text),
        )
    };
    let heading = |icon: &'static str, color, why: &str, n: usize| {
        Line::from(vec![
            Span::styled(format!("   {icon} "), theme::fg(color)),
            Span::styled(
                fit(why, false, (width as usize).saturating_sub(20)),
                theme::fg(t.subtext),
            ),
            Span::styled(format!("  · {}", plural(n, "item")), theme::bold(color)),
        ])
    };
    let mut rows: Vec<Line> = Vec::new();
    for (why, keys) in r.failures_by_reason() {
        rows.push(heading("✖", t.err, why, keys.len()));
        rows.extend(keys.into_iter().map(key_line));
    }
    if !r.skipped.is_empty() {
        rows.push(heading("◌", t.warn, "not sent", r.skipped.len()));
        rows.extend(r.skipped.iter().map(key_line));
    }
    let room = (area.height as usize).saturating_sub(12).max(3);
    if !rows.is_empty() {
        lines.push(Line::raw(""));
        lines.push(section("NOT UPDATED"));
        let first = result.scroll.min(rows.len().saturating_sub(room));
        let shown = rows.len().min(room);
        lines.extend(rows.into_iter().skip(first).take(shown));
    }
    lines.push(Line::raw(""));
    let retry = r.failed.len() + r.skipped.len();
    let retry_label = format!("r  retry the {retry} not updated");
    let mut choices = Vec::new();
    if retry > 0 {
        choices.push((
            retry_label.as_str(),
            theme::pill(t.on_accent, t.accent),
            Click::Key(KeyCode::Char('r')),
        ));
    }
    choices.push((
        "Esc  close",
        Style::new().bg(t.select).fg(t.text),
        Click::Key(KeyCode::Esc),
    ));
    let (row, spots) = buttons(lines.len(), &choices);
    lines.push(row);
    lines.push(Line::raw(""));
    let popup = centered(area, width, lines.len() as u16 + 2);
    let color = if r.failed.is_empty() { t.accent } else { t.err };
    let title = if r.skipped.is_empty() {
        "Update finished"
    } else {
        "Update stopped"
    };
    let inner = card(f, popup, title, color);
    f.render_widget(Paragraph::new(lines), inner);
    register(app, popup, inner, spots);
}

// ---------------------------------------------------------------- keys and commands

pub(super) fn draw_help(f: &mut Frame, app: &mut App, area: Rect) {
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
    f.render_widget(hint("any key or click closes"), note);
    register(app, popup, inner, Vec::new());
}

pub(super) fn draw_palette(f: &mut Frame, app: &mut App, area: Rect) {
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
        register(app, popup, inner, Vec::new());
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
    let count = entries.len();
    let mut state = ListState::default().with_selected(Some(palette.selected));
    let list = List::new(items)
        .highlight_style(theme::selected(true))
        .highlight_symbol(Span::styled("▌ ", theme::fg(t.accent)));
    f.render_stateful_widget(list, list_area, &mut state);
    let first = usize::from(list_area.y - inner.y);
    let spots = (0..list_area.height as usize)
        .filter(|row| state.offset() + row < count)
        .map(|row| {
            (
                first + row,
                0,
                u16::MAX,
                Click::Palette(state.offset() + row),
            )
        })
        .collect();
    register(app, popup, inner, spots);
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
