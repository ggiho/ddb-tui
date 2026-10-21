//! The panes: the connection launcher, the table list, the item grid, the preview of the
//! selected item and the full-size detail view.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Cell, List, ListItem, Paragraph, Row, Table};
use unicode_width::UnicodeWidthStr;

use super::{
    cells, details, draw_centered, draw_input, fit, key_hints, label, pane, popup_open, spinner,
    truncate, width_of,
};
use crate::app::{self, App, DetailKind, Screen};
use crate::ddb::{Connection, ReadRequest};
use crate::{highlight, theme};

const MAX_COL_WIDTH: usize = 36;
const COL_GAP: u16 = 2;

/// Tables on the left, the grid in the middle and the selected item on the right. Narrow
/// terminals drop the preview first, then the table list unless it has focus.
pub(super) fn draw_workspace(f: &mut Frame, app: &mut App, area: Rect) {
    let base = app.base_screen();
    let popup = popup_open(app);
    let has_items = !app.pager.is_empty();
    let mut rest = area;
    if base == Screen::Tables || area.width >= 90 {
        let w = if area.width >= 150 { 30 } else { 24 }.min(area.width);
        let focused = !popup && base == Screen::Tables;
        draw_sidebar(f, app, Rect { width: w, ..area }, focused);
        rest.x += w + 1;
        rest.width = rest.width.saturating_sub(w + 1);
    }
    if base != Screen::Detail && has_items && area.width >= 120 {
        let w = (rest.width * 36 / 100).clamp(34, 64);
        rest.width = rest.width.saturating_sub(w + 1);
        draw_preview(
            f,
            app,
            Rect {
                x: rest.right() + 1,
                width: w,
                ..rest
            },
        );
    }
    match base {
        Screen::Detail => draw_detail(f, app, rest, !popup),
        _ if !has_items && app.request.is_none() => draw_welcome(f, app, rest),
        _ => draw_grid(f, app, rest, !popup && base == Screen::Items),
    }
}

// ---------------------------------------------------------------- launcher

/// The connection list: profiles with their region, account and credential kind, searchable
/// with `/`. Production profiles carry a badge, and selecting one says so at the bottom.
pub(super) fn draw_launcher(f: &mut Frame, app: &mut App, area: Rect) {
    let t = theme::current();
    let width = area.width.saturating_sub(4).min(112);
    let card = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + 1.min(area.height),
        width,
        height: area.height.saturating_sub(2),
    };
    f.render_widget(Block::new().style(Style::new().bg(t.surface)), card);
    let inner = Rect {
        x: card.x + 2,
        y: card.y + 1,
        width: card.width.saturating_sub(4),
        height: card.height.saturating_sub(2),
    };
    let [title, _, search, _, header, list_area, _, note] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    f.render_widget(
        Line::from(vec![
            Span::styled("ddb-tui", theme::bold(t.accent)),
            Span::styled("   choose a connection", theme::muted()),
        ]),
        title,
    );

    let visible = app.visible_connections();
    let count = if app.conn_filter.is_empty() {
        format!("{} connections", app.connections.len())
    } else {
        format!("{} of {}", visible.len(), app.connections.len())
    };
    draw_input(
        f,
        search,
        &app.conn_filter,
        app.conn_filtering,
        "search profiles, regions and accounts",
        vec![Span::styled(count, theme::fg(t.subtext))],
    );

    // Columns: the name (with room for the badge), then region, account and kind while they
    // fit; narrow terminals drop the kind first, then the account.
    const REGION_W: usize = 16;
    const ACCOUNT_W: usize = 14;
    const KIND_W: usize = 12;
    let row_width = list_area.width.saturating_sub(2) as usize;
    // Production badges line up in a slot at the end of the name column.
    let any_prod = app
        .connections
        .iter()
        .any(|c| matches!(c, Connection::Profile(n) if app::is_production(n)));
    let badge_slot = if any_prod {
        " ● prod ".width() + 2
    } else {
        0
    };
    let longest = app
        .connections
        .iter()
        .map(|c| match c {
            Connection::Profile(name) => name.width(),
            Connection::Local(_) => 5,
        })
        .max()
        .unwrap_or(0);
    let name_w = (longest + 1 + badge_slot).clamp(12, 48).min(row_width);
    let rest = row_width.saturating_sub(name_w + 2);
    let show_kind = rest >= REGION_W + ACCOUNT_W + KIND_W;
    let show_account = rest >= REGION_W + ACCOUNT_W;
    let mut head = vec![
        Span::raw("  "),
        Span::styled(pad_to("PROFILE", name_w + 2), theme::bold(t.muted)),
    ];
    head.push(Span::styled(
        pad_to("REGION", REGION_W),
        theme::bold(t.muted),
    ));
    if show_account {
        head.push(Span::styled(
            pad_to("ACCOUNT", ACCOUNT_W),
            theme::bold(t.muted),
        ));
    }
    if show_kind {
        head.push(Span::styled("TYPE", theme::bold(t.muted)));
    }
    f.render_widget(Line::from(head), header);

    let find = app.conn_filter.clone();
    let items: Vec<ListItem> = visible
        .iter()
        .map(|&i| {
            let mut spans = Vec::new();
            match &app.connections[i] {
                Connection::Profile(name) => {
                    let info = app.profile_info.get(name);
                    let room = name_w.saturating_sub(badge_slot);
                    let shown = pad_to(name, room);
                    spans.extend(highlight::find_spans(&shown, &find, theme::fg(t.text)));
                    if app::is_production(name) {
                        spans.push(Span::raw("  "));
                        spans.push(theme::production(" ● prod "));
                    } else {
                        spans.push(Span::raw(" ".repeat(badge_slot)));
                    }
                    spans.push(Span::raw(
                        " ".repeat(name_w.saturating_sub(room + badge_slot) + 2),
                    ));
                    // A missing value shows as a muted dash rather than in the column's color.
                    let mut column = |value: Option<&str>, width: usize, style: Style| {
                        let text = value.filter(|v| !v.is_empty());
                        let style = if text.is_some() {
                            style
                        } else {
                            theme::muted()
                        };
                        spans.extend(highlight::find_spans(
                            &pad_to(text.unwrap_or("—"), width),
                            &find,
                            style,
                        ));
                    };
                    column(
                        info.and_then(|i| i.region.as_deref()),
                        REGION_W,
                        theme::fg(t.subtext),
                    );
                    if show_account {
                        column(
                            info.and_then(|i| i.account.as_deref()),
                            ACCOUNT_W,
                            theme::fg(t.number),
                        );
                    }
                    if show_kind {
                        column(info.map(|i| i.kind), KIND_W, theme::muted());
                    }
                }
                Connection::Local(endpoint) => {
                    spans.extend(highlight::find_spans("local", &find, theme::bold(t.text)));
                    spans.push(Span::raw(" ".repeat(name_w.saturating_sub(5) + 2)));
                    spans.push(Span::styled(
                        fit(
                            &format!("DynamoDB Local · {endpoint}"),
                            false,
                            row_width.saturating_sub(name_w + 2),
                        ),
                        theme::muted(),
                    ));
                }
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let empty = items.is_empty();
    let list = List::new(items)
        .highlight_style(theme::selected(app.palette.is_none() && !app.show_help))
        .highlight_symbol(Span::styled("▌ ", theme::fg(t.accent)));
    f.render_stateful_widget(list, list_area, &mut app.conn_state);
    if empty {
        draw_centered(
            f,
            list_area,
            vec![
                Line::styled("No connection matches", theme::fg(t.subtext)),
                key_hints(&[("Esc", "clear")]),
            ],
        );
    }

    let selected_prod = matches!(
        app.selected_connection(),
        Some(Connection::Profile(name)) if app::is_production(name)
    );
    if selected_prod {
        f.render_widget(
            Line::from(vec![
                Span::styled("● ", theme::fg(t.err)),
                Span::styled("Production account", theme::bold(t.err)),
                Span::styled(
                    "  ·  every write asks for confirmation first",
                    theme::fg(t.subtext),
                ),
            ]),
            note,
        );
    }
}

fn pad_to(text: &str, width: usize) -> String {
    let shown = fit(text, false, width.saturating_sub(1));
    let gap = width.saturating_sub(shown.width());
    format!("{shown}{}", " ".repeat(gap))
}

// ---------------------------------------------------------------- table list

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    let t = theme::current();
    let names: Vec<String> = app
        .visible_tables()
        .into_iter()
        .map(str::to_string)
        .collect();
    let count = if app.filter.is_empty() {
        app.tables.len().to_string()
    } else {
        format!("{} of {}", names.len(), app.tables.len())
    };
    let inner = pane(
        f,
        area,
        vec![label("TABLES", focused)],
        vec![Span::styled(count, theme::muted())],
        focused,
    );
    let [search, _, list_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);
    draw_input(f, search, &app.filter, app.filtering, "search", Vec::new());

    let open = app
        .request
        .as_ref()
        .and_then(|r| r.table())
        .map(str::to_string);
    let row_width = list_area.width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = names
        .iter()
        .map(|name| {
            let count = app
                .cached_table(name)
                .and_then(|i| i.item_count)
                .map(compact_count)
                .unwrap_or_default();
            let base = if open.as_deref() == Some(name.as_str()) {
                theme::bold(t.accent)
            } else {
                theme::fg(t.text)
            };
            let shown = fit(name, false, row_width.saturating_sub(count.width() + 1));
            let mut spans = highlight::find_spans(&shown, &app.filter, base);
            spans.push(Span::raw(
                " ".repeat(row_width.saturating_sub(shown.width() + count.width())),
            ));
            spans.push(Span::styled(count, theme::muted()));
            ListItem::new(Line::from(spans))
        })
        .collect();
    let empty = items.is_empty();
    let symbol = if focused { t.accent } else { t.muted };
    let list = List::new(items)
        .highlight_style(theme::selected(focused))
        .highlight_symbol(Span::styled("▌ ", theme::fg(symbol)));
    f.render_stateful_widget(list, list_area, &mut app.table_state);
    if empty {
        let lines = if app.filter.is_empty() {
            vec![
                Line::styled("No tables here", theme::fg(t.subtext)),
                key_hints(&[("R", "region")]),
            ]
        } else {
            vec![
                Line::styled("No match", theme::fg(t.subtext)),
                key_hints(&[("Esc", "clear")]),
            ]
        };
        draw_centered(f, list_area, lines);
    }
}

/// `~70`, `~1.2k`, `~3.4M`: DescribeTable counts are estimates, refreshed every few hours.
fn compact_count(n: i64) -> String {
    match n {
        n if n >= 1_000_000 => format!("~{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("~{:.1}k", n as f64 / 1e3),
        n => format!("~{n}"),
    }
}

/// The middle pane before any table is open: what Enter would open.
fn draw_welcome(f: &mut Frame, app: &App, area: Rect) {
    let t = theme::current();
    let inner = pane(f, area, vec![label("ITEMS", false)], Vec::new(), false);
    let selected = app
        .table_state
        .selected()
        .and_then(|i| app.visible_tables().get(i).map(|s| s.to_string()));
    let mut lines = Vec::new();
    match selected {
        Some(name) => {
            lines.push(Line::styled(name.clone(), theme::bold(t.text)));
            if let Some(info) = app.cached_table(&name) {
                let keys: Vec<String> = info
                    .base()
                    .keys()
                    .map(|k| format!("{} ({})", k.name, k.ty))
                    .collect();
                lines.push(Line::styled(keys.join("  ·  "), theme::muted()));
            }
            lines.push(Line::raw(""));
            lines.push(key_hints(&[
                ("Enter", "scan"),
                ("t", "details"),
                ("x", "PartiQL"),
            ]));
        }
        None => lines.push(Line::styled("No table selected", theme::muted())),
    }
    draw_centered(f, inner, lines);
}

// ---------------------------------------------------------------- item grid

fn grid_title(app: &App, focused: bool) -> Vec<Span<'static>> {
    let t = theme::current();
    let (op, table, index, opts) = match &app.request {
        Some(ReadRequest::Scan { table, index, opts }) => ("SCAN", table, index, opts),
        Some(ReadRequest::Query {
            table, index, opts, ..
        }) => ("QUERY", table, index, opts),
        Some(ReadRequest::PartiQL { statement }) => {
            return vec![
                label("PARTIQL", focused),
                Span::styled(format!("  {}", truncate(statement, 60)), theme::muted()),
            ];
        }
        None => return vec![label("ITEMS", focused)],
    };
    let mut spans = vec![
        label(op, focused),
        Span::styled(format!("  {table}"), theme::bold(t.text)),
    ];
    if let Some(i) = index {
        spans.push(Span::styled(format!("  {i}"), theme::fg(t.accent)));
    }
    if !opts.summary.is_empty() {
        spans.push(Span::styled(
            format!("  {}", truncate(&opts.summary, 60)),
            theme::muted(),
        ));
    }
    spans
}

/// `50 items · 0.5 RCU · 2 marked` for the grid's title row.
fn grid_facts(app: &App) -> Vec<Span<'static>> {
    let t = theme::current();
    let Some(page) = app.pager.page() else {
        return Vec::new();
    };
    let mut spans = vec![Span::styled(
        format!("{} items", page.items.len()),
        theme::fg(t.subtext),
    )];
    if let Some(c) = page.consumed {
        spans.push(Span::styled(format!("  ·  {c} RCU"), theme::muted()));
    }
    if !app.marked.is_empty() {
        spans.push(Span::styled("  ·  ", theme::muted()));
        spans.push(Span::styled(
            format!("{} marked", app.marked.len()),
            theme::bold(t.mark),
        ));
    }
    spans
}

fn draw_grid(f: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    let t = theme::current();
    let inner = pane(f, area, grid_title(app, focused), grid_facts(app), focused);
    let [search, _, body, pager] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(inner);
    let rows = app.visible_rows();
    let total = app.items().len();
    let search_right = if app.find.is_empty() {
        vec![
            Span::styled("f", theme::bold(t.accent)),
            Span::styled(" filter on the server", theme::muted()),
        ]
    } else {
        vec![Span::styled(
            format!("{} of {total}", rows.len()),
            theme::fg(t.subtext),
        )]
    };
    draw_input(
        f,
        search,
        &app.find,
        app.finding,
        "search this page",
        search_right,
    );

    if app.pager.page().is_none() {
        let line = if app.busy > 0 {
            Line::styled(format!("{}  Loading…", spinner()), theme::accent())
        } else {
            Line::styled("Nothing loaded", theme::muted())
        };
        draw_centered(f, body, vec![line]);
        return;
    }
    if rows.is_empty() {
        let lines = if total > 0 {
            vec![
                Line::styled(
                    format!("Nothing on this page matches \"{}\"", app.find),
                    theme::fg(t.subtext),
                ),
                Line::raw(""),
                key_hints(&[
                    ("Esc", "clear"),
                    ("n", "next page"),
                    ("f", "filter on the server"),
                ]),
            ]
        } else if matches!(app.request, Some(ReadRequest::PartiQL { .. })) {
            vec![Line::styled(
                "The statement ran and returned no items",
                theme::fg(t.subtext),
            )]
        } else {
            vec![
                Line::styled("No items", theme::fg(t.subtext)),
                Line::raw(""),
                key_hints(&[
                    ("f", "change the filter"),
                    ("s", "plain scan"),
                    ("a", "add"),
                ]),
            ]
        };
        draw_pager(f, app, pager, None);
        draw_centered(f, body, lines);
        return;
    }

    let (cols, keys) = app.columns();
    let items = app.items();
    let shown: Vec<Vec<cells::Shown>> = rows
        .iter()
        .map(|&i| cols.iter().map(|c| cells::show(items[i].get(c))).collect())
        .collect();
    let categories: Vec<_> = (0..cols.len())
        .map(|c| {
            (c >= keys)
                .then(|| cells::categories(rows.iter().map(|&i| items[i].get(&cols[c]))))
                .flatten()
        })
        .collect();
    let numeric: Vec<bool> = (0..cols.len())
        .map(|c| {
            let mut kinds = shown
                .iter()
                .map(|r| r[c].kind)
                .filter(|k| *k != cells::Kind::Missing);
            let first = kinds.next();
            first == Some(cells::Kind::Number) && kinds.all(|k| k == cells::Kind::Number)
        })
        .collect();
    let widths: Vec<usize> = (0..cols.len())
        .map(|c| {
            let dot = if categories[c].is_some() { 2 } else { 0 };
            let content = shown.iter().map(|r| r[c].text.width()).max().unwrap_or(0) + dot;
            content.max(cols[c].width()).clamp(3, MAX_COL_WIDTH)
        })
        .collect();

    // Key columns stay pinned; the rest scroll horizontally from the offset.
    let marking = !app.marked.is_empty();
    let avail = (body.width as usize).saturating_sub(1 + if marking { 3 } else { 0 });
    let start = keys + app.col_offset.min(cols.len().saturating_sub(keys + 1));
    let mut visible: Vec<usize> = (0..keys).collect();
    let mut used: usize = visible.iter().map(|&i| widths[i] + COL_GAP as usize).sum();
    for (i, w) in widths.iter().enumerate().skip(start) {
        if visible.len() > keys && used + w > avail {
            break;
        }
        used += w + COL_GAP as usize;
        visible.push(i);
    }

    let mut header_cells: Vec<Cell> = Vec::new();
    if marking {
        header_cells.push(Cell::from(""));
    }
    header_cells.extend(visible.iter().map(|&c| {
        let style = if c < keys {
            theme::bold(t.key)
        } else {
            theme::bold(t.subtext)
        };
        let line = Line::styled(cols[c].clone(), style);
        Cell::from(if numeric[c] {
            line.right_aligned()
        } else {
            line
        })
    }));
    let header = Row::new(header_cells).style(Style::new().bg(t.raised));

    let find = app.find.clone();
    let table_rows: Vec<Row> = shown
        .iter()
        .zip(&rows)
        .map(|(row, &item_index)| {
            let mut row_cells: Vec<Cell> = Vec::new();
            if marking {
                row_cells.push(Cell::from(Span::styled(
                    if app.marked.contains(&item_index) {
                        "●"
                    } else {
                        ""
                    },
                    theme::bold(t.mark),
                )));
            }
            row_cells.extend(visible.iter().map(|&c| {
                let s = &row[c];
                let style = if c < keys {
                    theme::fg(t.key)
                } else {
                    cells::style(s.kind)
                };
                let mut spans = Vec::new();
                let mut room = widths[c];
                if let Some(color) = categories[c].as_ref().and_then(|m| m.get(&s.text)) {
                    spans.push(Span::styled("● ", theme::fg(*color)));
                    room -= 2;
                }
                spans.extend(highlight::find_spans(
                    &fit(&s.text, false, room),
                    &find,
                    style,
                ));
                let line = Line::from(spans);
                Cell::from(if numeric[c] {
                    line.right_aligned()
                } else {
                    line
                })
            }));
            Row::new(row_cells)
        })
        .collect();

    let mut constraints: Vec<Constraint> = Vec::new();
    if marking {
        constraints.push(Constraint::Length(1));
    }
    constraints.extend(
        visible
            .iter()
            .map(|&c| Constraint::Length(widths[c] as u16)),
    );
    let last = *visible.last().expect("at least one column");
    let scrolled =
        (start > keys || last + 1 < cols.len()).then_some((start + 1, last + 1, cols.len()));
    draw_pager(f, app, pager, scrolled);

    let symbol = if focused { t.accent } else { t.muted };
    let table = Table::new(table_rows, constraints)
        .header(header)
        .column_spacing(COL_GAP)
        .row_highlight_style(theme::selected(focused))
        .highlight_symbol(Text::from(Span::styled("▌", theme::fg(symbol))));
    f.render_stateful_widget(table, body, &mut app.item_state);
}

/// `‹ p   page 2   n ›` and, when columns scroll, which ones are showing.
fn draw_pager(f: &mut Frame, app: &App, area: Rect, columns: Option<(usize, usize, usize)>) {
    let t = theme::current();
    let arrow = |on: bool| {
        if on {
            theme::bold(t.accent)
        } else {
            theme::muted()
        }
    };
    let mut left = vec![
        Span::styled("‹ p", arrow(app.pager.number() > 1)),
        Span::styled(
            format!("   page {}   ", app.pager.number()),
            theme::fg(t.subtext),
        ),
        Span::styled("n ›", arrow(app.pager.has_next())),
    ];
    if !app.pager.has_next() {
        left.push(Span::styled("   last page", theme::muted()));
    }
    let right = match columns {
        Some((first, last, total)) => vec![
            Span::styled(
                format!("columns {first}–{last} of {total}  "),
                theme::muted(),
            ),
            Span::styled("← →", theme::bold(t.accent)),
        ],
        None => Vec::new(),
    };
    let [l, r] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(width_of(&right) as u16),
    ])
    .areas(area);
    f.render_widget(Line::from(left), l);
    f.render_widget(Line::from(right), r);
}

// ---------------------------------------------------------------- preview and detail

fn draw_preview(f: &mut Frame, app: &App, area: Rect) {
    let t = theme::current();
    let inner = pane(
        f,
        area,
        vec![label("ITEM", false)],
        vec![
            Span::styled("Enter", theme::bold(t.accent)),
            Span::styled(" open", theme::muted()),
        ],
        false,
    );
    let body = Rect {
        y: inner.y + 1,
        height: inner.height.saturating_sub(1),
        ..inner
    };
    let Some(item) = app.selected_item() else {
        draw_centered(
            f,
            body,
            vec![Line::styled("No item selected", theme::muted())],
        );
        return;
    };
    let (cols, keys) = app.columns();
    let mut lines = cells::preview_lines(item, &cols[..keys], body.width as usize, &app.find);
    let room = body.height as usize;
    if lines.len() > room && room > 0 {
        let hidden = lines.len() - room + 1;
        lines.truncate(room - 1);
        lines.push(Line::styled(
            format!("… {hidden} more lines · Enter opens the item"),
            theme::muted(),
        ));
    }
    f.render_widget(Paragraph::new(lines), body);
}

fn draw_detail(f: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    let t = theme::current();
    let width = area.width.saturating_sub(2) as usize;
    let lines: Vec<Line> = match app.detail_kind {
        DetailKind::Item | DetailKind::Json => {
            app.detail_text.lines().map(highlight::json_line).collect()
        }
        DetailKind::Table => app
            .details
            .as_ref()
            .map(|d| details::lines(d, width))
            .unwrap_or_default(),
    };
    let height = area.height.saturating_sub(2) as usize;
    let total = lines.len();
    // Table details are laid out to the width, so the scroll limit is only known here.
    app.detail_scroll = app.detail_scroll.min(total.saturating_sub(height) as u16);
    let mut title = vec![label(
        match app.detail_kind {
            DetailKind::Item => "ITEM",
            DetailKind::Json => "REQUESTS",
            DetailKind::Table => "DETAILS",
        },
        focused,
    )];
    let mut right = Vec::new();
    match app.detail_kind {
        DetailKind::Item => {
            let (now, other) = if app.detail_plain {
                ("plain JSON", "DynamoDB JSON")
            } else {
                ("DynamoDB JSON", "plain JSON")
            };
            title.push(Span::styled(format!("  {now}"), theme::muted()));
            right.push(Span::styled("v", theme::bold(t.accent)));
            right.push(Span::styled(format!(" {other}"), theme::muted()));
        }
        DetailKind::Json => title.push(Span::styled(
            format!("  {}", app.detail_title.trim()),
            theme::muted(),
        )),
        DetailKind::Table => {
            if let Some(d) = &app.details {
                title.push(Span::styled(format!("  {}", d.name), theme::bold(t.text)));
                right.push(details::status(d));
            }
        }
    }
    if total > height {
        let first = app.detail_scroll as usize + 1;
        let last = (app.detail_scroll as usize + height).min(total);
        right.push(Span::styled(
            format!("    {first}–{last} of {total}"),
            theme::muted(),
        ));
    }
    let inner = pane(f, area, title, right, focused);
    let body = Rect {
        y: inner.y + 1,
        height: inner.height.saturating_sub(1),
        ..inner
    };
    f.render_widget(Paragraph::new(lines).scroll((app.detail_scroll, 0)), body);
}
