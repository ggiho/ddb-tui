//! Color themes. Screens ask for colors by role (accent, muted, surface), so switching the
//! theme recolors everything. Themes are 24-bit; terminals that do not announce truecolor
//! through `COLORTERM` get the nearest 256-color values instead.

use std::io;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use ratatui::style::{Color, Modifier, Style};

pub struct Theme {
    pub name: &'static str,
    /// Behind the panes: the top bar, the footer and the gaps between panes.
    pub bg: Color,
    /// Pane background.
    pub surface: Color,
    /// Inputs, table headers, popups and toasts.
    pub raised: Color,
    /// The selected row of the focused pane, and of the other panes.
    pub select: Color,
    pub select_dim: Color,
    pub border: Color,
    pub text: Color,
    pub subtext: Color,
    pub muted: Color,
    pub accent: Color,
    /// Text on an accent or status background.
    pub on_accent: Color,
    pub key: Color,
    pub string: Color,
    pub number: Color,
    /// `true`, `false` and `null`.
    pub literal: Color,
    pub ok: Color,
    pub warn: Color,
    pub err: Color,
    /// A faint error tint for badges that warn without shouting, such as production.
    pub err_soft: Color,
    pub mark: Color,
    /// Dots for the values of status-like columns.
    pub tags: [Color; 6],
}

pub const NAMES: [&str; 3] = ["tokyo-night", "catppuccin-mocha", "catppuccin-latte"];

static THEMES: OnceLock<[Theme; 3]> = OnceLock::new();
static CURRENT: AtomicUsize = AtomicUsize::new(0);

fn themes() -> &'static [Theme; 3] {
    THEMES.get_or_init(|| {
        let truecolor = std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit");
        let c = move |rgb: u32| {
            if truecolor {
                Color::from_u32(rgb)
            } else {
                Color::Indexed(nearest_256(rgb))
            }
        };
        [tokyo_night(&c), catppuccin_mocha(&c), catppuccin_latte(&c)]
    })
}

pub fn current() -> &'static Theme {
    &themes()[CURRENT.load(Ordering::Relaxed)]
}

/// Switches to the named theme; `false` if there is no such theme.
pub fn set(name: &str) -> bool {
    match NAMES.iter().position(|n| *n == name) {
        Some(i) => {
            CURRENT.store(i, Ordering::Relaxed);
            true
        }
        None => false,
    }
}

/// Switches to the next theme and returns its name.
pub fn cycle() -> &'static str {
    let next = (CURRENT.load(Ordering::Relaxed) + 1) % NAMES.len();
    CURRENT.store(next, Ordering::Relaxed);
    NAMES[next]
}

/// `theme` in the settings directory.
fn saved_path() -> Option<PathBuf> {
    Some(crate::settings::config_dir()?.join("theme"))
}

/// Selects the theme chosen last time, if any.
pub fn load_saved() {
    if let Some(name) = saved_path().and_then(|p| std::fs::read_to_string(p).ok()) {
        set(name.trim());
    }
}

/// Remembers the current theme for the next start.
pub fn save() -> io::Result<()> {
    let path = saved_path().ok_or_else(|| io::Error::other("HOME is not set"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, format!("{}\n", current().name))
}

pub fn fg(color: Color) -> Style {
    Style::new().fg(color)
}

pub fn bold(color: Color) -> Style {
    fg(color).add_modifier(Modifier::BOLD)
}

pub fn muted() -> Style {
    fg(current().muted)
}

pub fn accent() -> Style {
    fg(current().accent)
}

/// A badge: bold text on a colored background.
pub fn pill(text: Color, bg: Color) -> Style {
    Style::new().fg(text).bg(bg).add_modifier(Modifier::BOLD)
}

/// The production badge: error-colored text on a faint tint of it.
pub fn production(text: &'static str) -> ratatui::text::Span<'static> {
    let t = current();
    ratatui::text::Span::styled(
        text,
        Style::new()
            .fg(t.err)
            .bg(t.err_soft)
            .add_modifier(Modifier::BOLD),
    )
}

/// The selected row; brighter in the focused pane.
pub fn selected(focused: bool) -> Style {
    let t = current();
    Style::new().bg(if focused { t.select } else { t.select_dim })
}

/// Text that matches the quick search. Foreground-only, so it stays visible on the
/// selected row, whose highlight replaces the background.
pub fn matched() -> Style {
    fg(current().warn).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
}

/// `fg` laid over `bg` at `amount` opacity.
fn mix(fg: u32, bg: u32, amount: f32) -> u32 {
    let channel = |shift: u32| {
        let (f, b) = ((fg >> shift) & 0xff, (bg >> shift) & 0xff);
        ((f as f32 * amount + b as f32 * (1.0 - amount)).round() as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

/// The xterm 256-color index closest to an RGB color: the 6×6×6 cube or the gray ramp.
fn nearest_256(rgb: u32) -> u8 {
    const LEVELS: [i32; 6] = [0, 95, 135, 175, 215, 255];
    let (r, g, b) = (
        (rgb >> 16) as i32 & 0xff,
        (rgb >> 8) as i32 & 0xff,
        rgb as i32 & 0xff,
    );
    let dist = |(x, y, z): (i32, i32, i32)| (x - r).pow(2) + (y - g).pow(2) + (z - b).pow(2);
    let level = |v: i32| {
        (0..6)
            .min_by_key(|&i| (LEVELS[i] - v).abs())
            .expect("six levels")
    };
    let (ri, gi, bi) = (level(r), level(g), level(b));
    let cube = (16 + 36 * ri + 6 * gi + bi) as u8;
    let cube_dist = dist((LEVELS[ri], LEVELS[gi], LEVELS[bi]));
    let gray = ((r + g + b) / 3 - 8).clamp(0, 230) / 10;
    let gray_value = 8 + 10 * gray;
    if dist((gray_value, gray_value, gray_value)) < cube_dist {
        232 + gray as u8
    } else {
        cube
    }
}

fn tokyo_night(c: &impl Fn(u32) -> Color) -> Theme {
    Theme {
        name: NAMES[0],
        bg: c(0x16161e),
        surface: c(0x1a1b26),
        raised: c(0x24283b),
        select: c(0x2e3c64),
        select_dim: c(0x292e42),
        border: c(0x3b4261),
        text: c(0xc0caf5),
        subtext: c(0xa9b1d6),
        muted: c(0x565f89),
        accent: c(0x7aa2f7),
        on_accent: c(0x16161e),
        key: c(0xe0af68),
        string: c(0x9ece6a),
        number: c(0xff9e64),
        literal: c(0xbb9af7),
        ok: c(0x9ece6a),
        warn: c(0xe0af68),
        err: c(0xf7768e),
        err_soft: c(mix(0xf7768e, 0x1a1b26, 0.18)),
        mark: c(0x7dcfff),
        tags: [
            c(0x7aa2f7),
            c(0xbb9af7),
            c(0x7dcfff),
            c(0xff9e64),
            c(0x73daca),
            c(0x9d7cd8),
        ],
    }
}

fn catppuccin_mocha(c: &impl Fn(u32) -> Color) -> Theme {
    Theme {
        name: NAMES[1],
        bg: c(0x11111b),
        surface: c(0x1e1e2e),
        raised: c(0x313244),
        select: c(0x45475a),
        select_dim: c(0x2a2b3c),
        border: c(0x45475a),
        text: c(0xcdd6f4),
        subtext: c(0xa6adc8),
        muted: c(0x6c7086),
        accent: c(0xcba6f7),
        on_accent: c(0x11111b),
        key: c(0xf9e2af),
        string: c(0xa6e3a1),
        number: c(0xfab387),
        literal: c(0x89dceb),
        ok: c(0xa6e3a1),
        warn: c(0xf9e2af),
        err: c(0xf38ba8),
        err_soft: c(mix(0xf38ba8, 0x1e1e2e, 0.18)),
        mark: c(0xf5c2e7),
        tags: [
            c(0x89b4fa),
            c(0xcba6f7),
            c(0x94e2d5),
            c(0xfab387),
            c(0x74c7ec),
            c(0xf5c2e7),
        ],
    }
}

fn catppuccin_latte(c: &impl Fn(u32) -> Color) -> Theme {
    Theme {
        name: NAMES[2],
        bg: c(0xdce0e8),
        surface: c(0xeff1f5),
        raised: c(0xffffff),
        select: c(0xccd0da),
        select_dim: c(0xe1e4ea),
        border: c(0xbcc0cc),
        text: c(0x4c4f69),
        subtext: c(0x5c5f77),
        muted: c(0x8c8fa1),
        accent: c(0x8839ef),
        on_accent: c(0xeff1f5),
        key: c(0xdf8e1d),
        string: c(0x40a02b),
        number: c(0xfe640b),
        literal: c(0x04a5e5),
        ok: c(0x40a02b),
        warn: c(0xdf8e1d),
        err: c(0xd20f39),
        err_soft: c(mix(0xd20f39, 0xeff1f5, 0.12)),
        mark: c(0xea76cb),
        tags: [
            c(0x1e66f5),
            c(0x8839ef),
            c(0x179299),
            c(0xfe640b),
            c(0x209fb5),
            c(0xea76cb),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_maps_to_the_nearest_256_color() {
        assert_eq!(nearest_256(0x000000), 16);
        assert_eq!(nearest_256(0xffffff), 231);
        assert_eq!(nearest_256(0xff0000), 196);
        assert_eq!(nearest_256(0x808080), 244, "gray ramp");
        assert_eq!(
            nearest_256(0x1a1b26),
            234,
            "dark backgrounds stay dark gray"
        );
    }

    #[test]
    fn mixing_blends_each_channel() {
        assert_eq!(mix(0xff0000, 0x000000, 0.5), 0x800000);
        assert_eq!(mix(0x123456, 0xabcdef, 1.0), 0x123456);
        assert_eq!(mix(0x123456, 0xabcdef, 0.0), 0xabcdef);
    }

    #[test]
    fn every_theme_is_reachable_by_name() {
        for (i, name) in NAMES.iter().enumerate() {
            assert_eq!(themes()[i].name, *name);
        }
        assert!(!set("no-such-theme"));
    }
}
