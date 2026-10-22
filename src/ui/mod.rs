//! Rendering. Reads `App` state only; selection widgets keep their scroll state in `App`.
//!
//! Layout: a top bar, a workspace of up to three panes (tables, items, the selected item) and
//! a footer of keys. Panes stand apart by their background rather than by borders, and the
//! focused pane's title takes the accent color. Popups and toasts float above the workspace.

mod cells;
mod details;
mod panes;
mod popups;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Screen, Status};
use crate::theme;

pub fn draw(f: &mut Frame, app: &mut App) {
    let t = theme::current();
    app.hit = Default::default();
    f.render_widget(
        Block::new().style(Style::new().bg(t.bg).fg(t.text)),
        f.area(),
    );
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(f.area());

    draw_header(f, app, header);
    if app.screen == Screen::Connections {
        panes::draw_launcher(f, app, body);
    } else {
        panes::draw_workspace(f, app, body);
    }
    match app.screen {
        Screen::Form => popups::draw_form(f, app, body),
        Screen::Update => popups::draw_update(f, app, body),
        Screen::PartiQL => popups::draw_partiql(f, app, body),
        Screen::Regions => popups::draw_regions(f, app, body),
        Screen::Saved => popups::draw_saved(f, app, body),
        _ => {}
    }
    draw_toast(f, app, body);
    draw_footer(f, app, footer);
    if app.confirm.is_some() {
        popups::draw_confirm(f, app, body);
    }
    if app.prompt.is_some() {
        popups::draw_prompt(f, app, body);
    }
    if app.palette.is_some() {
        popups::draw_palette(f, app, body);
    }
    if app.export_job.is_some() {
        popups::draw_export(f, app, body);
    }
    if app.show_help {
        popups::draw_help(f, app, body);
    }
}

/// Whether something floats over the workspace, which then shows no focus.
fn popup_open(app: &App) -> bool {
    app.confirm.is_some()
        || app.prompt.is_some()
        || app.show_help
        || app.palette.is_some()
        || app.screen != app.base_screen()
}

// ---------------------------------------------------------------- top bar, footer, toast

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let t = theme::current();
    let mut left = vec![
        Span::raw(" "),
        Span::styled(" ddb-tui ", theme::pill(t.on_accent, t.accent)),
    ];
    let connected = app.screen != Screen::Connections && !app.conn_label.is_empty();
    if connected && app.on_production() {
        left.push(Span::raw(" "));
        left.push(theme::production(" ● PROD "));
    }
    if connected && app.read_only() {
        left.push(Span::raw(" "));
        left.push(Span::styled(
            " read-only ",
            Style::new().fg(t.subtext).bg(t.raised),
        ));
    } else if connected && app.on_production() {
        left.push(Span::raw(" "));
        left.push(theme::production(" writes on "));
    }
    let right = if app.busy > 0 {
        vec![Span::styled(
            format!("{} working ", spinner()),
            theme::accent(),
        )]
    } else {
        Vec::new()
    };
    if connected {
        let mut crumbs = Vec::new();
        if let Some(req) = &app.request {
            crumbs.push(Span::styled("  ›  ", theme::muted()));
            crumbs.push(Span::styled(
                req.table().unwrap_or("PartiQL").to_string(),
                theme::bold(t.text),
            ));
            if let Some(index) = req.index() {
                crumbs.push(Span::styled("  ›  ", theme::muted()));
                crumbs.push(Span::styled(index.to_string(), theme::fg(t.accent)));
            }
        }
        // When the bar is short the region goes first, then the connection name shrinks; the
        // table name always stays.
        let room = (area.width as usize).saturating_sub(width_of(&right));
        let used = width_of(&left) + width_of(&crumbs);
        let conn = format!("  {}", app.conn_label);
        let region = format!("  {}", app.region);
        if used + conn.width() + region.width() <= room {
            left.push(Span::styled(conn, theme::fg(t.text)));
            left.push(Span::styled(region, theme::muted()));
        } else {
            let name = fit(&app.conn_label, false, room.saturating_sub(used + 2));
            left.push(Span::styled(format!("  {name}"), theme::fg(t.text)));
        }
        left.extend(crumbs);
    }
    let [l, r] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(width_of(&right) as u16),
    ])
    .areas(area);
    f.render_widget(Line::from(left), l);
    f.render_widget(Line::from(right), r);
}

/// As many of the current keys as fit, with the palette and the key list on the right.
fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let t = theme::current();
    let key = |k: &str| Span::styled(k.to_string(), theme::bold(t.accent));
    let what = |l: String| Span::styled(l, theme::muted());
    let right = if app.palette_allowed() && app.palette.is_none() && !app.show_help {
        vec![
            key("⌃P"),
            what(" commands   ".into()),
            key("?"),
            what(" keys ".into()),
        ]
    } else {
        Vec::new()
    };
    let right_width = width_of(&right);
    let mut left = vec![Span::raw(" ")];
    let mut used = 1;
    for b in app.bindings() {
        let w = b.keys.width() + 1 + b.label.width() + 3;
        if used + w + right_width > area.width as usize {
            break;
        }
        used += w;
        left.push(key(b.keys));
        left.push(what(format!(" {}   ", b.label)));
    }
    let [l, r] = Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width as u16)])
        .areas(area);
    f.render_widget(Line::from(left), l);
    f.render_widget(Line::from(right), r);
}

/// The last status message as a toast in the bottom-right corner. Messages fade after a few
/// seconds, errors after longer; progress messages stay while requests run.
fn draw_toast(f: &mut Frame, app: &App, area: Rect) {
    let t = theme::current();
    let (icon, color, text, shown_for) = match &app.status {
        Status::Progress(s) if app.busy > 0 => (spinner(), t.accent, s, Duration::MAX),
        Status::Info(s) | Status::Progress(s) => ("•", t.accent, s, Duration::from_secs(4)),
        Status::Success(s) => ("✔", t.ok, s, Duration::from_secs(4)),
        Status::Error(s) => ("✖", t.err, s, Duration::from_secs(15)),
    };
    let max = area.width.saturating_sub(8).min(72);
    if text.is_empty() || app.status_age() > shown_for || max < 12 || area.height < 4 {
        return;
    }
    let paragraph = Paragraph::new(Line::from(vec![
        Span::styled(format!("{icon} "), theme::bold(color)),
        Span::styled(text.clone(), theme::fg(t.text)),
    ]))
    .wrap(Wrap { trim: false });
    let inner_width = (text.width() as u16 + 2).min(max);
    let height = (paragraph.line_count(inner_width) as u16).clamp(1, 4) + 2;
    let width = inner_width + 4;
    let rect = Rect {
        x: area.right().saturating_sub(width + 1),
        y: area.bottom().saturating_sub(height + 1),
        width,
        height,
    };
    clear_popup(f, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::fg(color))
        .padding(Padding::horizontal(1))
        .style(Style::new().bg(t.raised));
    f.render_widget(paragraph.block(block), rect);
}

// ---------------------------------------------------------------- shared pieces

/// Paints a pane and its title row. Returns the content area below the title, inset by one
/// column on each side.
fn pane(
    f: &mut Frame,
    area: Rect,
    title: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    focused: bool,
) -> Rect {
    let t = theme::current();
    f.render_widget(Block::new().style(Style::new().bg(t.surface)), area);
    if area.height == 0 {
        return area;
    }
    let mut spans = vec![Span::styled(
        if focused { "▍" } else { " " },
        theme::fg(t.accent),
    )];
    spans.extend(title);
    let [l, r] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(width_of(&right) as u16 + 1),
    ])
    .areas(Rect { height: 1, ..area });
    f.render_widget(Line::from(spans), l);
    f.render_widget(Line::from(right), r);
    Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height - 1,
    }
}

/// A pane's name in its title row: accent while the pane has focus.
fn label(text: &str, focused: bool) -> Span<'static> {
    let t = theme::current();
    Span::styled(
        text.to_string(),
        theme::bold(if focused { t.accent } else { t.muted }),
    )
}

/// A one-line search input: `/` and the text, or a placeholder; `right` sits at the end.
fn draw_input(
    f: &mut Frame,
    area: Rect,
    text: &str,
    active: bool,
    placeholder: &str,
    right: Vec<Span<'static>>,
) {
    let t = theme::current();
    let bg = if active { t.select_dim } else { t.raised };
    f.render_widget(Block::new().style(Style::new().bg(bg)), area);
    let [l, r] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(width_of(&right) as u16 + 1),
    ])
    .areas(area);
    let room = (l.width as usize).saturating_sub(4);
    let slash = if active {
        theme::bold(t.accent)
    } else {
        theme::muted()
    };
    let mut spans = vec![Span::styled(" / ", slash)];
    if text.is_empty() && !active {
        spans.push(Span::styled(
            fit(placeholder, false, room),
            theme::muted().add_modifier(Modifier::ITALIC),
        ));
    } else if active {
        let shown = fit(text, true, room);
        spans.push(Span::raw(
            shown.strip_suffix('▏').unwrap_or(&shown).to_string(),
        ));
        spans.push(Span::styled("▏", theme::accent()));
    } else {
        spans.push(Span::raw(fit(text, false, room)));
    }
    f.render_widget(Line::from(spans), l);
    f.render_widget(Line::from(right), r);
}

/// `Enter scan   t details` as one line of key hints.
fn key_hints(pairs: &[(&str, &str)]) -> Line<'static> {
    let t = theme::current();
    let mut spans = Vec::new();
    for (i, (k, what)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("    "));
        }
        spans.push(Span::styled(k.to_string(), theme::bold(t.accent)));
        spans.push(Span::styled(format!(" {what}"), theme::muted()));
    }
    Line::from(spans)
}

/// Lines centered in an area, for loading and empty states.
fn draw_centered(f: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    let [mid] = Layout::vertical([Constraint::Length(lines.len() as u16)])
        .flex(Flex::Center)
        .areas(area);
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), mid);
}

fn width_of(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

fn spinner() -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    FRAMES[(ms / 80) as usize % FRAMES.len()]
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
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

/// Clears the popup area plus one column on each side, painted in the theme background. A
/// double-width character (Korean, CJK) cut by the popup edge would otherwise swallow the
/// border cell next to it.
fn clear_popup(f: &mut Frame, popup: Rect) {
    let screen = f.area();
    let x = popup.x.saturating_sub(1).max(screen.x);
    let right = (popup.x + popup.width + 1).min(screen.x + screen.width);
    let area = Rect {
        x,
        width: right.saturating_sub(x),
        ..popup
    };
    f.render_widget(Clear, area);
    f.render_widget(
        Block::new().style(Style::new().bg(theme::current().bg)),
        area,
    );
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [r] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [r] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(r);
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Confirm, DetailKind, Palette};
    use crate::ddb::{Connection, Cursor, Index, Key, Page, ReadRequest, TableInfo};
    use crate::details::{IndexDetails, KeyPart, Setting, State, TableDetails};
    use crate::item::Item;
    use aws_sdk_dynamodb::types::AttributeValue;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn sample_app(rt: &tokio::runtime::Runtime) -> App {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            vec![
                Connection::Profile("acme-prod".into()),
                Connection::Local("http://localhost:8000".into()),
            ],
            rt.handle().clone(),
            tx,
        );
        app.conn_label = "local".into();
        app.region = "us-east-1".into();
        app.tables = vec!["Demo".into(), "Orders".into()];
        app.table_state.select(Some(0));
        app.table = Some(TableInfo {
            name: "Demo".into(),
            indexes: vec![Index {
                name: None,
                pk: Key {
                    name: "pk".into(),
                    ty: "S".into(),
                },
                sk: Some(Key {
                    name: "sk".into(),
                    ty: "N".into(),
                }),
                global: false,
            }],
            item_count: Some(3),
            size_bytes: Some(4096),
        });
        app.request = Some(ReadRequest::scan("Demo"));
        let rows: Vec<Item> = (0..3)
            .map(|i| {
                Item::from([
                    ("pk".to_string(), AttributeValue::S(format!("user#{i}"))),
                    ("sk".to_string(), AttributeValue::N(i.to_string())),
                    ("status".to_string(), AttributeValue::S("active".into())),
                    ("city".to_string(), AttributeValue::S("서울".into())),
                    (
                        "profile".to_string(),
                        AttributeValue::M(Item::from([(
                            "age".to_string(),
                            AttributeValue::N("30".into()),
                        )])),
                    ),
                ])
            })
            .collect();
        app.pager.push(Page {
            items: rows,
            next: Some(Cursor::Token("next".into())),
            consumed: Some(0.5),
            calls: Vec::new(),
        });
        app.item_state.select(Some(0));
        app.detail_kind = DetailKind::Item;
        app.detail_text = "{\n  \"pk\": {\"S\": \"user#0\"}\n}".into();
        let key = |name: &str, sort| KeyPart {
            name: name.into(),
            ty: "S".into(),
            sort,
        };
        app.details = Some(TableDetails {
            name: "Demo".into(),
            status: "ACTIVE".into(),
            arn: "arn:aws:dynamodb:us-east-1:000000000000:table/Demo".into(),
            item_count: 1_234_567,
            size_bytes: 98_765_432,
            keys: vec![key("pk", false), key("sk", true)],
            capacity: Some((5, 5)),
            protection: vec![
                Setting {
                    label: "Deletion protection",
                    value: "on".into(),
                    state: State::On,
                },
                Setting {
                    label: "Point-in-time recovery",
                    value: "unavailable: AccessDenied".into(),
                    state: State::Unavailable,
                },
            ],
            settings: vec![Setting {
                label: "Table class",
                value: "STANDARD".into(),
                state: State::Plain,
            }],
            global_indexes: vec![IndexDetails {
                name: "by_status".into(),
                keys: vec![key("status", false)],
                projection: "INCLUDE (a, b, c, d, e, f, g, h)".into(),
                status: "ACTIVE".into(),
                item_count: 70,
                size_bytes: 7_200,
                capacity: Some((1, 1)),
            }],
            local_indexes: Vec::new(),
            tags: Ok(vec![
                ("env".into(), "prod".into()),
                ("team".into(), "data".into()),
            ]),
        });
        app
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer();
        (0..height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn every_screen_draws_at_any_size() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = sample_app(&rt);
        for (w, h) in [(1, 1), (20, 6), (60, 16), (80, 24), (120, 34), (220, 60)] {
            for screen in [
                Screen::Connections,
                Screen::Tables,
                Screen::Items,
                Screen::Detail,
                Screen::Form,
                Screen::Update,
                Screen::PartiQL,
                Screen::Regions,
                Screen::Saved,
            ] {
                app.screen = screen;
                render(&mut app, w, h);
            }
            app.screen = Screen::Detail;
            app.detail_kind = DetailKind::Table;
            app.detail_scroll = 500;
            render(&mut app, w, h);
            app.detail_kind = DetailKind::Item;
            app.detail_scroll = 0;
            app.screen = Screen::Items;
            app.palette = Some(Palette::default());
            render(&mut app, w, h);
            app.palette = None;
            app.show_help = true;
            render(&mut app, w, h);
            app.show_help = false;
            app.confirm = Some(Confirm::Delete {
                table: "Demo".into(),
                keys: app.items().to_vec(),
                hidden: 1,
            });
            render(&mut app, w, h);
            app.confirm = None;
        }
    }

    #[test]
    fn production_connections_are_flagged_in_the_top_bar() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = sample_app(&rt);
        app.screen = Screen::Items;
        assert!(
            !render(&mut app, 120, 20)
                .lines()
                .next()
                .unwrap()
                .contains("PROD")
        );
        app.conn_label = "acme-apac-prod".into();
        app.connection = Some(Connection::Profile("acme-apac-prod".into()));
        assert!(
            render(&mut app, 120, 20)
                .lines()
                .next()
                .unwrap()
                .contains("● PROD")
        );
    }

    #[test]
    fn wide_terminals_show_all_three_panes() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = sample_app(&rt);
        app.screen = Screen::Items;
        let wide = render(&mut app, 150, 30);
        assert!(wide.contains("TABLES") && wide.contains("SCAN") && wide.contains("ITEM"));
        let narrow = render(&mut app, 80, 24);
        assert!(
            !narrow.contains("TABLES") && narrow.contains("SCAN"),
            "the table list and the preview fold away"
        );
    }
}
