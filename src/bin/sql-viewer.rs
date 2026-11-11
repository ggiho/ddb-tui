//! Interactive pager for already formatted SQL results; never connects to a database.
use anyhow::Result;
use ddb_tui::{
    sql_table::{self, SqlTable},
    theme, ui,
};
use ratatui::crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseEventKind,
    },
    execute,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::Line,
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};
use std::io::{self, IsTerminal, Read, Write};
use std::process::{Command, Stdio};
use unicode_width::UnicodeWidthStr;

// Compare plain decimal display values without rounding large identifiers.
fn decimal(text: &str) -> Option<(bool, &str, &str)> {
    let negative = text.starts_with('-');
    let unsigned = text.strip_prefix(['-', '+']).unwrap_or(text);
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if (whole.is_empty() && fraction.is_empty())
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let whole = whole.trim_start_matches('0');
    let fraction = fraction.trim_end_matches('0');
    Some((
        negative && (!whole.is_empty() || !fraction.is_empty()),
        whole,
        fraction,
    ))
}
fn compare_decimal(a: &str, b: &str) -> std::cmp::Ordering {
    let (an, aw, af) = decimal(a).expect("validated decimal");
    let (bn, bw, bf) = decimal(b).expect("validated decimal");
    if an != bn {
        return bn.cmp(&an);
    }
    let order = aw.len().cmp(&bw.len()).then(aw.cmp(bw)).then(af.cmp(bf));
    if an { order.reverse() } else { order }
}

const VIEWER_THEMES: [&str; 4] = [
    "terminal",
    "catppuccin-mocha",
    "catppuccin-latte",
    "tokyo-night",
];

fn theme_path() -> Option<std::path::PathBuf> {
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    Some(root.join("sql-viewer/theme"))
}

fn load_theme(path: Option<&std::path::Path>) -> usize {
    path.and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|name| VIEWER_THEMES.iter().position(|n| *n == name.trim()))
        .unwrap_or(0)
}

fn save_theme(path: Option<&std::path::Path>, name: &str) -> io::Result<()> {
    let path = path.ok_or_else(|| io::Error::other("no configuration directory"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{name}\n"))
}

struct App {
    data: SqlTable,
    selected_theme: usize,
    row: usize,
    col: usize,
    top: usize,
    left: usize,
    search: String,
    editing: bool,
    notice: String,
    descending: bool,
    sorted: Option<usize>,
    help: bool,
    details_scroll: u16,
    details_max: u16,
    details_area: Rect,
    detail_hits: Vec<(Rect, usize)>,
    show_details: bool,
    hits: Vec<(Rect, usize)>,
    grid: Rect,
    page: usize,
    widths: Vec<u16>,
}
impl App {
    fn new(data: SqlTable) -> Self {
        let widths = data
            .headers
            .iter()
            .enumerate()
            .map(|(i, h)| {
                data.rows
                    .iter()
                    .map(|r| r[i].width())
                    .chain([h.width()])
                    .max()
                    .unwrap_or(4)
                    .clamp(6, 36) as u16
                    + 2
            })
            .collect();
        Self {
            widths,
            data,
            selected_theme: 0,
            row: 0,
            col: 0,
            top: 0,
            left: 0,
            search: String::new(),
            editing: false,
            notice: String::new(),
            descending: false,
            sorted: None,
            help: false,
            details_scroll: 0,
            details_max: 0,
            details_area: Rect::default(),
            detail_hits: vec![],
            show_details: true,
            hits: vec![],
            grid: Rect::default(),
            page: 1,
        }
    }
    fn view_state(&self) -> serde_json::Value {
        serde_json::json!({"row":self.row,"col":self.col,"top":self.top,"left":self.left,
            "search":self.search,"sorted":self.sorted,"descending":self.descending,
            "details_scroll":self.details_scroll,"show_details":self.show_details})
    }
    fn restore_view(&mut self, state: &serde_json::Value) {
        let index = |name: &str| state[name].as_u64().and_then(|n| usize::try_from(n).ok());
        if let Some(col) = index("sorted").filter(|c| *c < self.data.headers.len()) {
            self.col = col;
            self.sorted = Some(col);
            self.descending = !state["descending"].as_bool().unwrap_or(false);
            self.sort();
        }
        self.row = index("row")
            .unwrap_or(0)
            .min(self.data.rows.len().saturating_sub(1));
        self.col = index("col")
            .unwrap_or(0)
            .min(self.data.headers.len().saturating_sub(1));
        self.top = index("top").unwrap_or(0).min(self.row);
        self.left = index("left").unwrap_or(0).min(self.col);
        self.search = state["search"].as_str().unwrap_or("").to_owned();
        self.details_scroll = index("details_scroll").unwrap_or(0).min(u16::MAX as usize) as u16;
        self.show_details = state["show_details"].as_bool().unwrap_or(true);
        self.notice.clear();
    }
    fn apply_theme(&mut self, index: usize) {
        self.selected_theme = index;
        theme::set(if index == 0 {
            "catppuccin-mocha"
        } else {
            VIEWER_THEMES[index]
        });
    }
    fn cycle_theme(&mut self) {
        self.apply_theme((self.selected_theme + 1) % VIEWER_THEMES.len());
        let name = VIEWER_THEMES[self.selected_theme];
        self.notice = match save_theme(theme_path().as_deref(), name) {
            Ok(()) => format!("Theme: {name} (saved)"),
            Err(e) => format!("Theme: {name} (not saved: {e})"),
        };
    }
    fn colors(&self) -> theme::Theme {
        let mut colors = theme::current().clone();
        if self.selected_theme == 0 {
            // Reset uses the terminal's foreground/background, including its
            // default-background opacity. ANSI slots follow its live palette.
            colors.bg = Color::Reset;
            colors.text = Color::Reset;
            colors.raised = Color::Reset;
            colors.select = Color::Indexed(8);
            colors.border = Color::Indexed(8);
            colors.subtext = Color::Indexed(7);
            colors.accent = Color::Indexed(6);
            colors.on_accent = Color::Indexed(0);
            colors.warn = Color::Indexed(3);
        }
        colors
    }
    fn select_cell(&mut self, x: u16, y: u16) {
        if y >= self.grid.y
            && y < self.grid.bottom()
            && let Some((_, col)) = self.hits.iter().find(|(r, _)| x >= r.x && x < r.right())
        {
            let row = self.top + (y - self.grid.y) as usize;
            if row < self.data.rows.len() {
                self.col = *col;
                if self.row != row {
                    self.details_scroll = 0;
                }
                self.row = row;
            }
        }
    }
    fn on_mouse(&mut self, mouse: event::MouseEvent) {
        let in_details = self.details_area.contains((mouse.column, mouse.row).into());
        match mouse.kind {
            MouseEventKind::ScrollDown if in_details => {
                self.details_scroll = self.details_scroll.saturating_add(3).min(self.details_max)
            }
            MouseEventKind::ScrollUp if in_details => {
                self.details_scroll = self.details_scroll.saturating_sub(3)
            }
            MouseEventKind::ScrollDown => self.move_row(3),
            MouseEventKind::ScrollUp => self.move_row(-3),
            MouseEventKind::Down(event::MouseButton::Left) if in_details => {
                if let Some((_, col)) = self
                    .detail_hits
                    .iter()
                    .find(|(r, _)| r.contains((mouse.column, mouse.row).into()))
                {
                    self.col = *col;
                }
            }
            MouseEventKind::Down(event::MouseButton::Left) => {
                self.select_cell(mouse.column, mouse.row)
            }
            _ => {}
        }
    }
    fn move_row(&mut self, delta: isize) {
        self.row = self
            .row
            .saturating_add_signed(delta)
            .min(self.data.rows.len().saturating_sub(1));
        self.details_scroll = 0;
    }
    fn find(&mut self, backwards: bool) {
        let columns = self.data.headers.len();
        let count = self.data.rows.len() * columns;
        if count == 0 || self.search.is_empty() {
            return;
        }
        let start = self.row * columns + self.col;
        let query = self.search.to_lowercase();
        for step in 1..=count {
            let i = if backwards {
                (start + count - step) % count
            } else {
                (start + step) % count
            };
            if self.data.rows[i / columns][i % columns]
                .to_lowercase()
                .contains(&query)
            {
                self.row = i / columns;
                self.col = i % columns;
                self.details_scroll = 0;
                self.notice.clear();
                return;
            }
        }
        self.notice = "No match".into();
    }
    fn sort(&mut self) {
        self.descending = self.sorted == Some(self.col) && !self.descending;
        self.sorted = Some(self.col);
        let col = self.col;
        let numeric = self.data.rows.iter().all(|r| decimal(&r[col]).is_some());
        self.data.rows.sort_by(|a, b| {
            let order = if numeric {
                compare_decimal(&a[col], &b[col])
            } else {
                a[col].cmp(&b[col])
            };
            if self.descending {
                order.reverse()
            } else {
                order
            }
        });
        self.row = 0;
        self.top = 0;
        self.details_scroll = 0;
        self.notice = format!(
            "Sorted {} {} ({})",
            self.data.headers[col],
            if self.descending { "↓" } else { "↑" },
            if numeric { "decimal" } else { "text" }
        );
    }
    fn copy(&mut self, whole_row: bool) {
        let Some(row) = self.data.rows.get(self.row) else {
            return;
        };
        let text = if whole_row {
            row.join("\t")
        } else {
            row[self.col].clone()
        };
        let result = (|| -> io::Result<()> {
            let mut child = Command::new("pbcopy").stdin(Stdio::piped()).spawn()?;
            let written = child.stdin.take().unwrap().write_all(text.as_bytes());
            let status = child.wait()?;
            written?;
            if !status.success() {
                return Err(io::Error::other("pbcopy failed"));
            }
            Ok(())
        })();
        self.notice = match result {
            Ok(()) => if whole_row {
                "Row copied (TSV)"
            } else {
                "Cell copied"
            }
            .into(),
            Err(e) => format!("Copy failed: {e}"),
        };
    }
    fn key(&mut self, key: event::KeyEvent) -> bool {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control && key.code == KeyCode::Char('g') {
            return true;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return true;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('d' | 'u'))
        {
            if !self.editing && !self.help {
                let half = (self.page / 2).max(1);
                let down = key.code == KeyCode::Char('d');
                self.move_row(if down {
                    half as isize
                } else {
                    -(half as isize)
                });
                self.top = if down {
                    self.top
                        .saturating_add(half)
                        .min(self.data.rows.len().saturating_sub(self.page))
                } else {
                    self.top.saturating_sub(half)
                };
            }
            return false;
        }
        if self.editing {
            match key.code {
                KeyCode::Esc => self.editing = false,
                KeyCode::Enter => {
                    self.editing = false;
                    self.find(false);
                }
                KeyCode::Backspace => {
                    self.search.pop();
                }
                KeyCode::Char(c) => self.search.push(c),
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.help {
                    self.help = false;
                } else {
                    return true;
                }
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_row(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_row(-1),
            KeyCode::Char('h') | KeyCode::Left => {
                self.col = self.col.saturating_sub(1);
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.col = (self.col + 1).min(self.data.headers.len() - 1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.move_row(-(self.row as isize)),
            KeyCode::Char('G') | KeyCode::End => self.move_row(self.data.rows.len() as isize),
            KeyCode::PageDown => self.move_row(self.page as isize),
            KeyCode::PageUp => self.move_row(-(self.page as isize)),
            KeyCode::Char('/') => {
                self.editing = true;
                self.search.clear();
            }
            KeyCode::Char('n') => self.find(false),
            KeyCode::Char('N') => self.find(true),
            KeyCode::Char('s') => self.sort(),
            KeyCode::Char('v') => self.copy(false),
            KeyCode::Char('y') => self.copy(true),
            KeyCode::Char('d') => {
                self.show_details = !self.show_details;
                self.left = 0;
                self.notice = if self.show_details {
                    "Details enabled (visible at 80+ columns)"
                } else {
                    "Details hidden"
                }
                .into();
            }
            KeyCode::Char('T') => self.cycle_theme(),
            KeyCode::Char('?') => self.help = !self.help,
            KeyCode::Char(']') => {
                self.details_scroll = self.details_scroll.saturating_add(3).min(self.details_max)
            }
            KeyCode::Char('[') => self.details_scroll = self.details_scroll.saturating_sub(3),
            _ => {}
        }
        false
    }
}
// Reserve actual padding; clipping must never consume the column gap.
fn draw_cell(f: &mut Frame, text: &str, area: Rect, style: Style) {
    f.render_widget(Block::default().style(style), area);
    let width = area.width.saturating_sub(2);
    if width > 0 {
        let shown = ui::fit(text, false, width as usize);
        f.render_widget(
            Paragraph::new(shown).style(style),
            Rect::new(area.x + 1, area.y, width, area.height),
        );
    }
}

// Keep every field in query-column order. Selection changes emphasis only.
fn detail_lines(app: &App, t: &theme::Theme) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
    let Some(row) = app.data.rows.get(app.row) else {
        return (vec![Line::raw("No record")], vec![]);
    };
    let mut lines = Vec::new();
    let mut entries = Vec::new();
    for (col, (name, value)) in app.data.headers.iter().zip(row).enumerate() {
        entries.push((lines.len(), col));
        lines.push(Line::styled(
            format!("{} {name}:", if col == app.col { "›" } else { " " }),
            theme::bold(t.accent),
        ));
        lines.push(Line::styled(
            format!("    {}", if value.is_empty() { "(empty)" } else { value }),
            theme::fg(if value.is_empty() { t.subtext } else { t.text }),
        ));
        lines.push(Line::raw(""));
    }
    if !entries.is_empty() {
        lines.pop();
    }
    (lines, entries)
}

fn draw_details(f: &mut Frame, app: &mut App, area: Rect, t: &theme::Theme) {
    app.details_area = area;
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(format!(
            " Record {} / {} ",
            if app.data.rows.is_empty() {
                0
            } else {
                app.row + 1
            },
            app.data.rows.len()
        ))
        .border_style(theme::fg(t.border));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let padded = Rect::new(
        inner.x.saturating_add(1).min(inner.right()),
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    );
    let [content, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).areas(padded);
    if content.width == 0 || content.height == 0 {
        return;
    }
    let (lines, entries) = detail_lines(app, t);
    // Compute wrapped positions so preview clicks and scroll limits match rendering.
    let mut offsets = vec![0usize];
    for line in &lines {
        let height = Paragraph::new(line.clone())
            .wrap(Wrap { trim: false })
            .line_count(content.width)
            .max(1);
        offsets.push(offsets.last().copied().unwrap_or(0) + height);
    }
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let total = paragraph.line_count(content.width);
    app.details_max = total
        .saturating_sub(content.height as usize)
        .min(u16::MAX as usize) as u16;
    app.details_scroll = app.details_scroll.min(app.details_max);
    let scroll = app.details_scroll as usize;
    for (start, col) in entries {
        let top = offsets[start].max(scroll);
        let bottom = offsets[start + 2].min(scroll + content.height as usize);
        if top < bottom {
            app.detail_hits.push((
                Rect::new(
                    content.x,
                    content.y + (top - scroll) as u16,
                    content.width,
                    (bottom - top) as u16,
                ),
                col,
            ));
        }
    }
    f.render_widget(paragraph.scroll((app.details_scroll, 0)), content);
    let position = if total > content.height as usize {
        format!(
            "{}–{} / {} lines",
            scroll + 1,
            (scroll + content.height as usize).min(total),
            total
        )
    } else {
        "All fields visible".into()
    };
    f.render_widget(
        Paragraph::new(vec![
            Line::styled("[ ] scroll · v copy", theme::fg(t.subtext)),
            Line::styled(position, theme::fg(t.subtext)),
        ]),
        footer,
    );
}

fn draw(f: &mut Frame, app: &mut App) {
    let t = app.colors();
    let [title, body, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .areas(f.area());
    f.render_widget(
        Block::default().style(Style::new().bg(t.bg).fg(t.text)),
        f.area(),
    );
    f.render_widget(
        Paragraph::new(format!(
            " SQL results   {} rows · {} columns   {} · T theme · ? help",
            app.data.rows.len(),
            app.data.headers.len(),
            VIEWER_THEMES[app.selected_theme]
        ))
        .style(theme::bold(t.accent)),
        title,
    );
    if app.help {
        f.render_widget(Paragraph::new("hjkl / arrows  Move cell\ng / G  First / last row\nPgUp / PgDn  Page\nCtrl+D / Ctrl+U  Half page down / up\nv  Copy displayed cell\ny  Copy displayed row as TSV\n/  Search · Enter to find · n/N next/previous\ns  Sort selected column, toggle direction\n[ / ]  Scroll record details\nd  Show / hide record details\nMouse  Select cell/field; wheel scrolls the hovered pane\nT  Change theme (saved for next result)\nCtrl+G  Return to query input\nq / Esc  Close\n\nValues are formatted display text. Padding is removed.\nInput must use ascii_escaped; copied values retain visible escapes.").wrap(Wrap{trim:false}).block(Block::bordered().title(" Keys ")),body);
        return;
    }
    let (grid, details) = if app.show_details && body.width >= 80 {
        let [a, b] = Layout::horizontal([Constraint::Percentage(65), Constraint::Percentage(35)])
            .areas(body);
        (a, Some(b))
    } else {
        (body, None)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Results ")
        .border_style(theme::fg(t.border));
    let inside = block.inner(grid);
    f.render_widget(block, grid);
    app.grid = Rect::new(
        inside.x,
        inside.y.saturating_add(2).min(inside.bottom()),
        inside.width,
        inside.height.saturating_sub(2),
    );
    app.page = app.grid.height.max(1) as usize;
    if inside.height > 1 {
        f.render_widget(
            Paragraph::new("─".repeat(inside.width as usize)).style(theme::fg(t.border)),
            Rect::new(inside.x, inside.y + 1, inside.width, 1),
        );
    }
    if app.row < app.top {
        app.top = app.row;
    }
    if app.row >= app.top + app.page {
        app.top = app.row + 1 - app.page;
    }
    // Keep the selected row continuous across separators and unused space.
    if app.row >= app.top && app.row < app.top + app.grid.height as usize {
        f.render_widget(
            Block::default().style(Style::new().bg(t.select)),
            Rect::new(
                app.grid.x,
                app.grid.y + (app.row - app.top) as u16,
                app.grid.width,
                1,
            ),
        );
    }
    if app.col < app.left {
        app.left = app.col;
    }
    let widths = &app.widths;
    while app.left < app.col
        && widths[app.left..=app.col]
            .iter()
            .map(|w| *w as usize)
            .sum::<usize>()
            + app.col
            - app.left
            > inside.width as usize
    {
        app.left += 1;
    }
    app.hits.clear();
    let mut x = inside.x;
    for (c, width) in widths.iter().enumerate().skip(app.left) {
        if x >= inside.right() {
            break;
        }
        let width = (*width).min(inside.right() - x);
        let rect = Rect::new(x, inside.y, width, inside.height);
        app.hits.push((rect, c));
        if inside.height > 0 {
            let style = if c == app.col {
                theme::bold(t.accent).bg(t.raised)
            } else {
                theme::bold(t.subtext).bg(t.raised)
            };
            draw_cell(
                f,
                &app.data.headers[c],
                Rect::new(x, inside.y, width, 1),
                style,
            );
        }
        for (r, row) in app
            .data
            .rows
            .iter()
            .enumerate()
            .skip(app.top)
            .take(app.grid.height as usize)
        {
            let selected = r == app.row && c == app.col;
            let matched = !app.search.is_empty()
                && row[c].to_lowercase().contains(&app.search.to_lowercase());
            let style = if selected {
                theme::fg(t.on_accent).bg(t.accent)
            } else if r == app.row {
                theme::fg(t.text).bg(t.select)
            } else if matched {
                theme::bold(t.warn)
            } else {
                theme::fg(t.text)
            };
            draw_cell(
                f,
                &row[c],
                Rect::new(x, app.grid.y + (r - app.top) as u16, width, 1),
                style,
            );
        }
        x += width;
        if c + 1 < widths.len() && x < inside.right() {
            for y in inside.y..inside.bottom() {
                let symbol = if y == inside.y + 1 { "┼" } else { "│" };
                f.render_widget(
                    Paragraph::new(symbol).style(theme::fg(t.border)),
                    Rect::new(x, y, 1, 1),
                );
            }
            x += 1;
        }
    }
    app.details_area = Rect::default();
    app.detail_hits.clear();
    app.details_max = 0;
    if let Some(area) = details {
        draw_details(f, app, area, &t);
    }
    let message = if app.editing {
        format!("/{}▏", app.search)
    } else if !app.notice.is_empty() {
        app.notice.clone()
    } else {
        format!(
            "row {}/{} · column {}/{}",
            if app.data.rows.is_empty() {
                0
            } else {
                app.row + 1
            },
            app.data.rows.len(),
            app.col + 1,
            app.data.headers.len()
        )
    };
    f.render_widget(
        Paragraph::new(format!(
            " {message}\n hjkl move  v cell  y row  / search  s sort  d details  T theme  ^G query  q quit"
        ))
        .style(theme::fg(t.subtext)),
        status,
    );
}
struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        ratatui::restore();
    }
}
fn main() -> Result<()> {
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!(
            "sql-viewer: read cli_helpers ascii_escaped output on stdin (--escaped required).\nKeys: hjkl, Ctrl+D/U half page, v cell copy, y row copy, / search, s sort, d details, T theme (saved), Ctrl+G query, q quit.\nIntegration: --view-state PATH restores session state.\nExit 2: unsupported input; caller should use a text pager."
        );
        return Ok(());
    }
    let mut args = std::env::args().skip(1);
    let mut view_path = None;
    while let Some(arg) = args.next() {
        if arg == "--view-state" {
            view_path =
                Some(std::path::PathBuf::from(args.next().ok_or_else(|| {
                    anyhow::anyhow!("--view-state needs a path")
                })?));
        }
    }
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    if !io::stdout().is_terminal() || input.trim().is_empty() {
        print!("{input}");
        return Ok(());
    }
    if !std::env::args().any(|a| a == "--escaped") {
        std::process::exit(2);
    }
    let Some(data) = sql_table::parse_escaped(&input) else {
        std::process::exit(2);
    };
    if data.rows.is_empty() {
        print!("{input}");
        return Ok(());
    }
    // On macOS kqueue cannot watch a newly opened /dev/tty. Reuse the
    // already-open terminal descriptor after consuming the piped result.
    // This pager owns stdin and has no background readers at this point.
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn dup2(old_fd: i32, new_fd: i32) -> i32;
        }
        // SAFETY: 1 is the validated terminal stdout; dup2 atomically replaces 0.
        if unsafe { dup2(1, 0) } == -1 {
            return Err(io::Error::last_os_error().into());
        }
    }
    let mut app = App::new(data);
    app.apply_theme(load_theme(theme_path().as_deref()));
    if let Some(state) = view_path
        .as_ref()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    {
        app.restore_view(&state);
    }
    let mut terminal = ratatui::try_init()?;
    let _restore = Restore;
    // ratatui restores raw mode on panic; also release mouse capture.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        previous(info);
    }));
    execute!(io::stdout(), EnableMouseCapture)?;
    loop {
        terminal.draw(|f| draw(f, &mut app))?;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if app.key(key) {
                    break;
                }
            }
            Event::Mouse(m) => app.on_mouse(m),
            _ => {}
        }
    }
    drop(_restore);
    if let Some(path) = view_path {
        let staged = path.with_extension("json.new");
        let saved = (|| -> io::Result<()> {
            std::fs::write(&staged, app.view_state().to_string())?;
            std::fs::rename(&staged, &path)
        })();
        if let Err(error) = saved {
            eprintln!("Could not save result position: {error}");
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> App {
        App::new(SqlTable {
            headers: vec!["id".into(), "value".into()],
            rows: vec![
                vec!["10".into(), "서울".into()],
                vec!["2".into(), "a|b".into()],
            ],
        })
    }
    #[test]
    fn search_and_numeric_sort() {
        let mut a = app();
        a.sort();
        assert_eq!(a.data.rows[0][0], "2");
        a.sort();
        assert_eq!(a.data.rows[0][0], "10");
        a.search = "a|b".into();
        a.find(false);
        assert_eq!((a.row, a.col), (1, 1));
        a.move_row(100);
        assert_eq!(a.row, 1);
        a.move_row(-100);
        assert_eq!(a.row, 0);
    }
    #[test]
    fn vim_half_page_moves_grid_without_toggling_details() {
        let mut a = app();
        a.data.rows = (0..40)
            .map(|i| vec![i.to_string(), "value".into()])
            .collect();
        a.page = 9;
        a.top = 5;
        a.row = 8;
        a.col = 1;
        let ctrl = |c| event::KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert!(!a.key(ctrl('d')));
        assert_eq!((a.row, a.top, a.col), (12, 9, 1));
        assert!(a.show_details);
        a.key(ctrl('u'));
        assert_eq!((a.row, a.top), (8, 5));
        for _ in 0..20 {
            a.key(ctrl('d'));
        }
        assert_eq!((a.row, a.top), (39, 31));
        for _ in 0..20 {
            a.key(ctrl('u'));
        }
        assert_eq!((a.row, a.top), (0, 0));
        a.editing = true;
        a.search = "name".into();
        a.key(ctrl('d'));
        assert_eq!(a.search, "name");
        assert_eq!(a.row, 0);
        a.editing = false;
        a.page = 1;
        a.key(ctrl('d'));
        assert_eq!(a.row, 1);
        a.data.rows.clear();
        a.row = 0;
        a.top = 0;
        a.key(ctrl('d'));
        assert_eq!((a.row, a.top), (0, 0));
    }
    #[test]
    fn resume_restores_sorted_selection_search_and_panel_position() {
        let mut original = app();
        original.sort();
        original.row = 1;
        original.col = 1;
        original.search = "서울".into();
        original.show_details = false;
        original.details_scroll = 7;
        let state = original.view_state();
        let mut reopened = app();
        reopened.restore_view(&state);
        assert_eq!(reopened.data.rows, original.data.rows);
        assert_eq!((reopened.row, reopened.col), (1, 1));
        assert_eq!(reopened.search, "서울");
        assert!(!reopened.show_details);
        assert_eq!(reopened.details_scroll, 7);
        reopened.restore_view(&serde_json::json!({"row":999999,"col":999999,"sorted":999999}));
        assert_eq!((reopened.row, reopened.col), (1, 1));
        let ctrl = |c| event::KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert!(reopened.key(ctrl('g')));
    }
    #[test]
    fn decimal_sort_is_exact() {
        use std::cmp::Ordering::*;
        assert_eq!(
            compare_decimal("9007199254740993", "9007199254740992"),
            Greater
        );
        assert_eq!(compare_decimal("-12.001", "-12"), Less);
        assert_eq!(compare_decimal(".12", "0.2"), Less);
        assert_eq!(compare_decimal("-0.00", "+0"), Equal);
        assert!(decimal("1e2").is_none());
    }
    #[test]
    fn separators_padding_and_mouse_share_the_same_geometry() {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
        let mut a = app();
        terminal.draw(|f| draw(f, &mut a)).unwrap();
        let x = a.grid.x;
        let y = a.grid.y;
        let divider = x + a.widths[0];
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(divider, y)].symbol(), "│");
        assert_eq!(buffer[(divider, y - 1)].symbol(), "┼");
        assert_eq!(buffer[(x, y)].symbol(), " ");
        assert_eq!(buffer[(divider - 1, y)].symbol(), " ");
        assert_eq!(buffer[(x, y - 1)].symbol(), "─");
        assert_eq!(buffer[(x, y - 2)].bg, Color::Reset);
        a.select_cell(divider, y + 1);
        assert_eq!((a.row, a.col), (0, 0), "divider is not a cell");
        a.select_cell(divider + 2, y - 1);
        assert_eq!((a.row, a.col), (0, 0), "header rule is not a row");
        a.select_cell(divider + 2, y + 1);
        assert_eq!((a.row, a.col), (1, 1));
        let before = a.grid.width;
        a.key(event::KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        terminal.draw(|f| draw(f, &mut a)).unwrap();
        assert!(a.grid.width > before);
        assert_eq!((a.row, a.col), (1, 1));
        a.key(event::KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        terminal.draw(|f| draw(f, &mut a)).unwrap();
        assert_eq!(a.grid.width, before);
    }
    #[test]
    fn truncated_wide_values_keep_boundaries_and_original_data() {
        let value = "서울".repeat(30);
        let mut a = App::new(SqlTable {
            headers: vec!["long_header".repeat(5), "id".into()],
            rows: vec![vec![value.clone(), "42".into()]],
        });
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut a)).unwrap();
        let buffer = terminal.backend().buffer();
        for y in [a.grid.y - 2, a.grid.y] {
            assert!((a.grid.x..a.grid.x + a.widths[0]).any(|x| buffer[(x, y)].symbol() == "…"));
        }
        let divider = a.grid.x + a.widths[0];
        assert_eq!(buffer[(divider, a.grid.y)].symbol(), "│");
        assert_eq!(a.data.rows[0][0], value);
        a.col = 1;
        let mut narrow = ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 8)).unwrap();
        narrow.draw(|f| draw(f, &mut a)).unwrap();
        assert!(a.hits.iter().any(|(_, col)| *col == 1));
        assert_eq!(a.page, a.grid.height as usize);
    }
    #[test]
    fn only_the_selected_row_has_a_background() {
        let mut a = app();
        a.data.rows = (0..12)
            .map(|i| vec![i.to_string(), "value".into()])
            .collect();
        a.row = 4;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 12)).unwrap();
        terminal.draw(|f| draw(f, &mut a)).unwrap();
        let colors = a.colors();
        let y = a.grid.y;
        let x = a.grid.x;
        let separator = x + a.widths[0];
        let last = a.grid.right() - 1;
        let buffer = terminal.backend().buffer();
        for column in [x, x + 1, separator, separator + 1, last] {
            assert_eq!(buffer[(column, y + 1)].bg, colors.bg);
            assert_eq!(buffer[(column, y + 2)].bg, colors.bg);
        }
        assert_eq!(buffer[(x + 1, y + 4)].bg, colors.accent);
        assert_eq!(buffer[(separator, y + 4)].bg, colors.select);
        assert_eq!(buffer[(last, y + 4)].bg, colors.select);
        a.move_row(1);
        terminal.draw(|f| draw(f, &mut a)).unwrap();
        assert_eq!(a.top, 1);
        assert_eq!(terminal.backend().buffer()[(x, y)].bg, colors.bg);
    }
    #[test]
    fn record_fields_keep_order_and_full_values_when_selection_changes() {
        let raw = r#"{"id":9007199254740993,"city":"서울","items":[1,2]}"#;
        let mut a = App::new(SqlTable {
            headers: vec!["metadata".into(), "status".into(), "empty".into()],
            rows: vec![vec![raw.into(), "active".into(), String::new()]],
        });
        let colors = a.colors();
        let (before, positions) = detail_lines(&a, &colors);
        assert_eq!(before[0].to_string(), "› metadata:");
        assert_eq!(before[1].to_string(), format!("    {raw}"));
        assert_eq!(before[7].to_string(), "    (empty)");
        assert_eq!(before[0].style.fg, Some(colors.accent));
        assert_eq!(before[1].style.fg, Some(colors.text));
        a.col = 1;
        let (after, after_positions) = detail_lines(&a, &colors);
        assert_eq!(after_positions, positions);
        assert_eq!(after[0].to_string(), "  metadata:");
        assert_eq!(after[3].to_string(), "› status:");
        for i in [1, 4, 7] {
            assert_eq!(before[i], after[i]);
        }
        assert_eq!(a.data.rows[0][0], raw);
    }
    #[test]
    fn inspector_tracks_selection_and_clamps_scrolling_and_mouse_hits() {
        let mut a = App::new(SqlTable {
            headers: (0..8).map(|i| format!("field_{i}")).collect(),
            rows: vec![
                (0..8)
                    .map(|i| format!("value_{i} {}", "서울 ".repeat(25)))
                    .collect(),
            ],
        });
        a.col = 7;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
        terminal.draw(|f| draw(f, &mut a)).unwrap();
        let (lines, _) = detail_lines(&a, &a.colors());
        assert_eq!(lines[0].to_string(), "  field_0:");
        assert_eq!(lines[21].to_string(), "› field_7:");
        assert!(a.details_max > 0);
        let mouse = |kind, x, y| event::MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        a.on_mouse(mouse(
            MouseEventKind::ScrollDown,
            a.details_area.x + 2,
            a.details_area.y + 2,
        ));
        assert_eq!(a.row, 0);
        assert_eq!(a.details_scroll, 3);
        a.details_scroll = u16::MAX;
        terminal.draw(|f| draw(f, &mut a)).unwrap();
        assert_eq!(a.details_scroll, a.details_max);
        let (hit, column) = a.detail_hits[0];
        a.col = (column + 1) % a.data.headers.len();
        let scroll_before_click = a.details_scroll;
        a.on_mouse(mouse(
            MouseEventKind::Down(event::MouseButton::Left),
            hit.x,
            hit.y,
        ));
        assert_eq!(a.col, column);
        assert_eq!(a.details_scroll, scroll_before_click);
        a.details_scroll = 3;
        a.key(event::KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(a.details_scroll, 3);
        a.show_details = false;
        terminal.draw(|f| draw(f, &mut a)).unwrap();
        assert!(a.detail_hits.is_empty());
        assert_eq!(a.details_area, Rect::default());
    }
    #[test]
    fn render_small_and_large_terminals() {
        for (w, h) in [(1, 1), (20, 4), (80, 24), (140, 40)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            let mut a = app();
            terminal.draw(|f| draw(f, &mut a)).unwrap();
            a.data.rows.clear();
            terminal.draw(|f| draw(f, &mut a)).unwrap();
        }
    }
}
