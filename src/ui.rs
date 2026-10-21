//! Rendering. Reads `App` state only; selection widgets keep their scroll state in `App`.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, List, Paragraph, Row, Table, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Confirm, Screen, Status};
use crate::ddb::{self, ReadOptions, ReadRequest, SkOp};
use crate::form::Field;
use crate::item;

const MAX_COL_WIDTH: usize = 40;

pub fn draw(f: &mut Frame, app: &mut App) {
    let [header, body, status, help] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(status_height(app, f.area().width)),
        Constraint::Length(1),
    ])
    .areas(f.area());

    draw_header(f, app, header);
    match app.screen {
        Screen::Connections => draw_connections(f, app, body),
        Screen::Tables => draw_tables(f, app, body),
        Screen::Regions => draw_regions(f, app, body),
        Screen::Items => draw_items(f, app, body),
        Screen::Form => {
            draw_items(f, app, body);
            draw_form(f, app, body);
        }
        Screen::Detail => draw_detail(f, app, body),
        Screen::PartiQL => draw_partiql(f, app, body),
        Screen::Update => {
            if app.update_return == Screen::Detail {
                draw_detail(f, app, body);
            } else {
                draw_items(f, app, body);
            }
            draw_update(f, app, body);
        }
        Screen::Saved => draw_saved(f, app, body),
    }
    draw_status(f, app, status);
    f.render_widget(
        Paragraph::new(footer(app)).style(Style::new().fg(Color::DarkGray)),
        help,
    );
    if app.confirm.is_some() {
        draw_confirm(f, app, body);
    }
    if app.prompt.is_some() {
        draw_prompt(f, app, body);
    }
    if app.show_help {
        draw_help(f, app, body);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![Span::styled(" ddb-tui ", Style::new().bold().reversed())];
    if !app.conn_label.is_empty() {
        spans.push(Span::raw(format!("  {} ({})", app.conn_label, app.region)));
    }
    if matches!(app.screen, Screen::Items | Screen::Detail | Screen::Form)
        && let Some(t) = app.request.as_ref().and_then(|r| r.table())
    {
        spans.push(Span::styled(format!("  ▸ {t}"), Style::new().bold()));
        if let Some(count) = app
            .table
            .as_ref()
            .filter(|i| i.name == t)
            .and_then(|i| i.item_count)
        {
            spans.push(Span::styled(
                format!("  ~{count} items"),
                Style::new().fg(Color::DarkGray),
            ));
        }
    }
    f.render_widget(Line::from(spans), area);
}

fn list_highlight() -> Style {
    Style::new().reversed()
}

fn draw_connections(f: &mut Frame, app: &mut App, area: Rect) {
    let labels: Vec<String> = app.connections.iter().map(|c| c.label()).collect();
    let list = List::new(labels)
        .block(Block::bordered().title(" Connections "))
        .highlight_style(list_highlight())
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut app.conn_state);
}

fn draw_tables(f: &mut Frame, app: &mut App, area: Rect) {
    let names: Vec<String> = app
        .visible_tables()
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut title = format!(" Tables ({}) ", names.len());
    if app.filtering || !app.filter.is_empty() {
        let cursor = if app.filtering { "▏" } else { "" };
        title = format!(" Tables ({}) /{}{cursor} ", names.len(), app.filter);
    }
    let list = List::new(names)
        .block(Block::bordered().title(title))
        .highlight_style(list_highlight())
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut app.table_state);
}

fn draw_regions(f: &mut Frame, app: &mut App, area: Rect) {
    let rows: Vec<String> = ddb::REGIONS
        .iter()
        .map(|r| {
            if *r == app.region {
                format!("{r}  (current)")
            } else {
                r.to_string()
            }
        })
        .collect();
    let list = List::new(rows)
        .block(Block::bordered().title(" Region "))
        .highlight_style(list_highlight())
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut app.region_state);
}

fn items_title(app: &App) -> String {
    let what = match &app.request {
        Some(ReadRequest::Scan { table, index, opts }) => {
            describe_read("Scan", table, index.as_deref(), opts)
        }
        Some(ReadRequest::Query {
            table, index, opts, ..
        }) => describe_read("Query", table, index.as_deref(), opts),
        Some(ReadRequest::PartiQL { statement }) => format!("PartiQL {}", truncate(statement, 50)),
        None => String::new(),
    };
    let Some(page) = app.pager.page() else {
        return format!(" {what} ");
    };
    let more = if app.pager.has_next() {
        "more →"
    } else {
        "end"
    };
    let marked = if app.marked.is_empty() {
        String::new()
    } else {
        format!(" · {} marked", app.marked.len())
    };
    let rcu = page
        .consumed
        .map(|c| format!(" · {c} RCU"))
        .unwrap_or_default();
    format!(
        " {what} · page {} · {} items{rcu}{marked} · {more} ",
        app.pager.number(),
        page.items.len()
    )
}

fn describe_read(op: &str, table: &str, index: Option<&str>, opts: &ReadOptions) -> String {
    let mut s = format!("{op} {table}");
    if let Some(i) = index {
        s.push_str(&format!(" · {i}"));
    }
    if !opts.summary.is_empty() {
        s.push_str(&format!(" · {}", truncate(&opts.summary, 60)));
    }
    s
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

fn draw_items(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::bordered().title(items_title(app));
    if app.pager.is_empty() {
        let text = if app.busy > 0 { "Loading…" } else { "" };
        f.render_widget(Paragraph::new(text).block(block), area);
        return;
    }
    if app.items().is_empty() {
        let text = if matches!(app.request, Some(ReadRequest::PartiQL { .. })) {
            "Statement ran; no items returned"
        } else {
            "No items"
        };
        f.render_widget(Paragraph::new(text).block(block), area);
        return;
    }

    let (cols, keys) = app.columns();
    let cells: Vec<Vec<String>> = app
        .items()
        .iter()
        .map(|it| {
            cols.iter()
                .map(|c| it.get(c).map(item::cell).unwrap_or_default())
                .collect()
        })
        .collect();
    let widths: Vec<usize> = (0..cols.len())
        .map(|i| {
            let content = cells.iter().map(|r| r[i].width()).max().unwrap_or(0);
            content.max(cols[i].width()).clamp(3, MAX_COL_WIDTH)
        })
        .collect();

    // Key columns stay pinned; the rest scroll horizontally from the offset.
    let avail = area.width.saturating_sub(2) as usize;
    let start = keys + app.col_offset.min(cols.len().saturating_sub(keys + 1));
    let mut visible: Vec<usize> = (0..keys).collect();
    let mut used: usize = visible.iter().map(|&i| widths[i] + 1).sum();
    for (i, w) in widths.iter().enumerate().skip(start) {
        if visible.len() > keys && used + w + 1 > avail {
            break;
        }
        used += w + 1;
        visible.push(i);
    }

    let header = Row::new(visible.iter().map(|&i| {
        let style = if i < keys {
            Style::new().bold().fg(Color::Yellow)
        } else {
            Style::new().bold()
        };
        Cell::from(cols[i].clone()).style(style)
    }));
    let marked = Style::new().fg(Color::Cyan).bold();
    let rows = cells.iter().enumerate().map(|(n, r)| {
        let row = Row::new(visible.iter().map(|&i| r[i].clone()));
        if app.marked.contains(&n) {
            row.style(marked)
        } else {
            row
        }
    });
    let mut block = block;
    let last = *visible.last().expect("at least one column");
    if start > keys || last + 1 < cols.len() {
        block = block.title_bottom(format!(
            " keys pinned · columns {}-{} of {} (←/→) ",
            start + 1,
            last + 1,
            cols.len()
        ));
    }
    let table = Table::new(
        rows,
        visible
            .iter()
            .map(|&i| Constraint::Length(widths[i] as u16)),
    )
    .header(header)
    .block(block)
    .column_spacing(1)
    .row_highlight_style(list_highlight());
    f.render_stateful_widget(table, area, &mut app.item_state);
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect) {
    let p = Paragraph::new(app.detail_text.as_str())
        .block(Block::bordered().title(app.detail_title.as_str()))
        .scroll((app.detail_scroll, 0));
    f.render_widget(p, area);
}

const FORM_VALUE_WIDTH: usize = 54;

fn draw_form(f: &mut Frame, app: &App, area: Rect) {
    let Some(t) = &app.table else { return };
    let form = &app.form;
    let idx = form.selected_index(t);
    let active = form.active(t);
    let row = |label: String, value: String, field: Field| -> Line<'static> {
        let style = if field == active {
            Style::new().reversed()
        } else {
            Style::new()
        };
        Line::from(vec![
            Span::raw(format!(" {label:<20} ")),
            Span::styled(pad(&value, FORM_VALUE_WIDTH), style),
        ])
    };
    let text = |s: &str, field: Field| fit(s, field == active, FORM_VALUE_WIDTH);
    let choice = |s: &str| format!("◀ {s} ▶");

    let mut lines = vec![
        row(
            "Operation".into(),
            choice(if form.query { "Query" } else { "Scan" }),
            Field::Mode,
        ),
        row(
            "Index".into(),
            choice(idx.name.as_deref().unwrap_or("(table)")),
            Field::Index,
        ),
    ];
    if form.query {
        lines.push(row(
            format!("{} ({}) =", idx.pk.name, idx.pk.ty),
            text(&form.pk, Field::Pk),
            Field::Pk,
        ));
        if let Some(sk) = &idx.sk {
            let op = form.sk_op().map_or("(none)", |o| o.label());
            lines.push(row(
                format!("{} ({})", sk.name, sk.ty),
                choice(op),
                Field::Op,
            ));
            if form.op > 0 {
                lines.push(row("  value".into(), text(&form.sk, Field::Sk), Field::Sk));
            }
            if form.sk_op() == Some(SkOp::Between) {
                lines.push(row("  and".into(), text(&form.sk2, Field::Sk2), Field::Sk2));
            }
        }
    }
    lines.push(row(
        "Filter".into(),
        text(&form.filter, Field::Filter),
        Field::Filter,
    ));
    lines.push(row(
        "Projection".into(),
        text(&form.projection, Field::Projection),
        Field::Projection,
    ));
    lines.push(row(
        "Consistent read".into(),
        choice(if form.consistent { "on" } else { "off" }),
        Field::Consistent,
    ));
    if form.query {
        lines.push(row(
            "Order".into(),
            choice(if form.descending {
                "descending"
            } else {
                "ascending"
            }),
            Field::Order,
        ));
    }
    let dim = Style::new().fg(Color::DarkGray);
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        " Filter: status = \"open\" AND size(tags) > 2 · values: \"text\" 12 true null",
        dim,
    ));
    lines.push(Line::styled(
        " Projection: pk, profile.city, tags[0] · `odd name` for unusual names",
        dim,
    ));
    if let Some(hint) = hint_line(app) {
        lines.push(hint);
    }
    let popup = centered(area, 80, lines.len() as u16 + 2);
    clear_popup(f, popup);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(format!(" Read {} ", t.name))),
        popup,
    );
}

/// Fits text into `width` columns: while typing the end stays visible, otherwise the start.
fn fit(s: &str, typing: bool, width: usize) -> String {
    let s = if typing {
        format!("{s}▏")
    } else {
        s.to_string()
    };
    if s.width() <= width {
        return s;
    }
    let mut kept: Vec<char> = Vec::new();
    let mut used = 1; // the ellipsis
    let chars: Vec<char> = if typing {
        s.chars().rev().collect()
    } else {
        s.chars().collect()
    };
    for c in chars {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        used += w;
        kept.push(c);
    }
    if typing {
        format!("…{}", kept.iter().rev().collect::<String>())
    } else {
        format!("{}…", kept.iter().collect::<String>())
    }
}

fn pad(s: &str, width: usize) -> String {
    format!("{s}{}", " ".repeat(width.saturating_sub(s.width())))
}

fn draw_partiql(f: &mut Frame, app: &App, area: Rect) {
    let [input, hint] = Layout::vertical([Constraint::Length(5), Constraint::Min(0)]).areas(area);
    let p = Paragraph::new(format!("{}▏", app.partiql))
        .wrap(Wrap { trim: false })
        .block(Block::bordered().title(" PartiQL "));
    f.render_widget(p, input);
    let examples = " SELECT * FROM \"Table\" WHERE pk = 'a'\n UPDATE \"Table\" SET n = 1 WHERE pk = 'a'\n\n \
                    SELECT results are paged; other statements run once.";
    f.render_widget(
        Paragraph::new(examples).style(Style::new().fg(Color::DarkGray)),
        hint,
    );
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let (text, style) = match &app.status {
        Status::Info(s) => (s.as_str(), Style::new()),
        Status::Error(s) => (s.as_str(), Style::new().fg(Color::Red)),
    };
    let prefix = if app.busy > 0 { "⏳ " } else { "" };
    f.render_widget(
        Paragraph::new(format!("{prefix}{text}"))
            .style(style)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// Errors may wrap onto up to three lines; everything else stays on one.
fn status_height(app: &App, width: u16) -> u16 {
    match &app.status {
        Status::Error(s) if width > 0 => (s.width() as u16 + 2).div_ceil(width).clamp(1, 3),
        _ => 1,
    }
}

fn draw_confirm(f: &mut Frame, app: &App, area: Rect) {
    let warn = Style::new().fg(Color::Yellow);
    let mut lines = match &app.confirm {
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
                Style::new().bold(),
            )];
            if *key_changed {
                lines.push(Line::styled(
                    " Key attributes changed: this writes a NEW item and leaves the original.",
                    warn,
                ));
            }
            if *create {
                lines.push(Line::raw(
                    " Fails instead of overwriting if the key already exists.",
                ));
            }
            lines
        }
        Some(Confirm::Delete { table, keys }) => {
            let mut lines = vec![Line::styled(
                format!(
                    " Delete {} item(s) from {table}? This cannot be undone.",
                    keys.len()
                ),
                Style::new().bold().fg(Color::Red),
            )];
            for key in keys.iter().take(5) {
                lines.push(Line::raw(format!("   {}", item::item_to_plain_json(key))));
            }
            if keys.len() > 5 {
                lines.push(Line::raw(format!("   … and {} more", keys.len() - 5)));
            }
            lines
        }
        Some(Confirm::Update(req)) => vec![
            Line::styled(
                format!(" Update this item in {}?", req.table),
                Style::new().bold(),
            ),
            Line::raw(format!("   {}", item::item_to_plain_json(&req.key))),
            Line::raw(format!("   {}", req.summary)),
        ],
        None => return,
    };
    lines.push(Line::raw(""));
    lines.push(Line::raw(" y: yes    n: cancel"));
    let popup = centered(area, 80, lines.len() as u16 + 2);
    clear_popup(f, popup);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Confirm ")),
        popup,
    );
}

fn draw_prompt(f: &mut Frame, app: &App, area: Rect) {
    let Some(p) = &app.prompt else { return };
    let lines = vec![
        Line::raw(format!(" {}", fit(&p.text, true, 74))),
        Line::styled(
            " Enter: write (never overwrites) · Esc: cancel",
            Style::new().fg(Color::DarkGray),
        ),
    ];
    let popup = centered(area, 80, 4);
    clear_popup(f, popup);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(p.title.as_str())),
        popup,
    );
}

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let keys = screen_keys(app);
    let lines: Vec<Line> = keys
        .iter()
        .map(|(k, what)| {
            Line::from(vec![
                Span::styled(format!(" {k:>12}  "), Style::new().bold()),
                Span::raw(*what),
            ])
        })
        .collect();
    let popup = centered(area, 60, lines.len() as u16 + 2);
    clear_popup(f, popup);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Keys · any key closes ")),
        popup,
    );
}

/// Keys for the current screen, most used first; the footer shows as many as fit.
fn screen_keys(app: &App) -> &'static [(&'static str, &'static str)] {
    if app.prompt.is_some() {
        return &[("Enter", "write file"), ("Esc", "cancel")];
    }
    if app.confirm.is_some() {
        return &[("y", "yes"), ("n / Esc", "cancel")];
    }
    match app.screen {
        Screen::Connections => &[("↑↓ / jk", "move"), ("Enter", "connect"), ("q", "quit")],
        Screen::Tables if app.filtering => &[
            ("type", "filter tables"),
            ("Enter", "open"),
            ("Esc", "clear filter"),
        ],
        Screen::Tables => &[
            ("↑↓ / jk", "move"),
            ("Enter", "open (scan)"),
            ("/", "filter"),
            ("x", "PartiQL"),
            ("R", "switch region"),
            ("o", "saved operations"),
            ("t", "table details"),
            ("r", "refresh"),
            ("Esc", "connections"),
        ],
        Screen::Regions => &[("↑↓ / jk", "move"), ("Enter", "reconnect"), ("Esc", "back")],
        Screen::Items => &[
            ("↑↓ / jk", "rows"),
            ("n / p", "next / previous page"),
            ("Enter", "view item"),
            ("f", "scan/query with filter"),
            ("e", "edit"),
            ("u", "update expression"),
            ("a", "add item"),
            ("c", "copy as new item"),
            ("space", "mark row"),
            ("d", "delete marked/selected"),
            ("y / Y", "copy DynamoDB / plain JSON"),
            ("E", "export page or marked"),
            ("i", "inspect requests"),
            ("w", "save this read (Workbench)"),
            ("o", "saved operations"),
            ("t", "table details"),
            ("+ / -", "page size"),
            ("←→ / hl", "columns"),
            ("s", "plain scan"),
            ("x", "PartiQL"),
            ("r", "refresh page"),
            ("Esc", "back"),
        ],
        Screen::Detail => &[
            ("↑↓ / jk", "scroll"),
            ("e", "edit"),
            ("u", "update expression"),
            ("c", "copy as new item"),
            ("y / Y", "copy DynamoDB / plain JSON"),
            ("Esc", "back"),
        ],
        Screen::Form => &[
            ("Tab", "complete name / next field"),
            ("↑↓", "field"),
            ("←→", "change choice"),
            ("Enter", "run"),
            ("Esc", "cancel"),
        ],
        Screen::PartiQL => &[("Enter", "run"), ("Esc", "back")],
        Screen::Update => &[
            ("Tab", "complete name / switch field"),
            ("Enter", "review"),
            ("Esc", "cancel"),
        ],
        Screen::Saved => &[("↑↓ / jk", "move"), ("Enter", "run"), ("Esc", "back")],
    }
}

/// Matching attribute names after an ambiguous Tab.
fn hint_line(app: &App) -> Option<Line<'static>> {
    if app.hint.is_empty() {
        return None;
    }
    let shown: Vec<&str> = app.hint.iter().take(8).map(String::as_str).collect();
    let more = if app.hint.len() > 8 {
        format!(" … +{}", app.hint.len() - 8)
    } else {
        String::new()
    };
    Some(Line::styled(
        format!(" Tab: {}{more}", shown.join(" · ")),
        Style::new().fg(Color::Yellow),
    ))
}

fn draw_update(f: &mut Frame, app: &App, area: Rect) {
    let form = &app.update_form;
    let field = |label: &str, text: &str, active: bool| -> Line<'static> {
        let style = if active {
            Style::new().reversed()
        } else {
            Style::new()
        };
        Line::from(vec![
            Span::raw(format!(" {label:<11} ")),
            Span::styled(pad(&fit(text, active, 64), 64), style),
        ])
    };
    let dim = Style::new().fg(Color::DarkGray);
    let mut lines = vec![
        field("Update", &form.update, !form.on_condition),
        field("Condition", &form.condition, form.on_condition),
        Line::raw(""),
        Line::styled(
            " SET a = a + 1, b = list_append(b, [\"x\"]), c = if_not_exists(c, 0)",
            dim,
        ),
        Line::styled(
            " REMOVE old · ADD tags <<\"new\">> · DELETE tags <<\"gone\">>",
            dim,
        ),
        Line::styled(
            " Condition (optional): attribute_exists(pk) AND version = 3",
            dim,
        ),
    ];
    if let Some(hint) = hint_line(app) {
        lines.push(hint);
    }
    let popup = centered(area, 80, lines.len() as u16 + 2);
    clear_popup(f, popup);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" UpdateItem ")),
        popup,
    );
}

fn draw_saved(f: &mut Frame, app: &mut App, area: Rect) {
    let rows: Vec<String> = app
        .saved
        .iter()
        .map(|e| format!("{:<40} {}", e.name, e.operation))
        .collect();
    let list = List::new(rows)
        .block(Block::bordered().title(format!(
            " Saved operations · {} · {} (shared with NoSQL Workbench) ",
            app.conn_label, app.region
        )))
        .highlight_style(list_highlight())
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut app.saved_state);
}

fn footer(app: &App) -> String {
    let keys: Vec<String> = screen_keys(app)
        .iter()
        .map(|(k, what)| format!("{k} {what}"))
        .collect();
    let help = if matches!(app.screen, Screen::Form | Screen::PartiQL) || app.filtering {
        ""
    } else {
        "? all keys · "
    };
    format!(" {help}{}", keys.join(" · "))
}

/// Clears the popup area plus one column on each side. A double-width character (Korean,
/// CJK) cut by the popup edge would otherwise swallow the border cell next to it.
fn clear_popup(f: &mut Frame, popup: Rect) {
    let screen = f.area();
    let x = popup.x.saturating_sub(1).max(screen.x);
    let right = (popup.x + popup.width + 1).min(screen.x + screen.width);
    f.render_widget(
        Clear,
        Rect {
            x,
            width: right - x,
            ..popup
        },
    );
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [r] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [r] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(r);
    r
}
