//! User settings from `~/.config/ddb-tui/config`, written like the AWS config files:
//! `key = value` lines and `#` comments. For example:
//!
//! ```text
//! # Treated as production whatever the profile is called.
//! production_accounts = 111122223333, 444455556666
//! production_profiles = legacy-main
//! # production (default): production connections start read-only; all; none
//! read_only = production
//! ```

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReadOnly {
    /// Production connections start read-only; `W` allows writes for the session.
    #[default]
    Production,
    /// Every connection starts read-only.
    All,
    /// No connection starts read-only.
    None,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub production_accounts: Vec<String>,
    pub production_profiles: Vec<String>,
    pub read_only: ReadOnly,
}

/// `$XDG_CONFIG_HOME/ddb-tui`, or `~/.config/ddb-tui`.
pub fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("ddb-tui"))
}

/// The settings file, or the defaults when there is none.
pub fn load() -> Settings {
    config_dir()
        .and_then(|d| std::fs::read_to_string(d.join("config")).ok())
        .map(|text| parse(&text))
        .unwrap_or_default()
}

/// Unknown keys and values are ignored, so a newer file still loads.
pub fn parse(text: &str) -> Settings {
    let mut settings = Settings::default();
    for line in text.lines().map(str::trim) {
        if line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let list = || {
            value
                .split([',', ' ', '\t'])
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        match key.trim() {
            "production_accounts" => settings.production_accounts = list(),
            "production_profiles" => settings.production_profiles = list(),
            "read_only" => {
                settings.read_only = match value.trim() {
                    "all" => ReadOnly::All,
                    "none" => ReadOnly::None,
                    _ => ReadOnly::Production,
                }
            }
            _ => {}
        }
    }
    settings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_lists_and_the_read_only_mode() {
        let s = parse(
            "# comment\nproduction_accounts = 111122223333, 444455556666\n\
             production_profiles=legacy main\nread_only = all\nunknown = x\n",
        );
        assert_eq!(s.production_accounts, ["111122223333", "444455556666"]);
        assert_eq!(s.production_profiles, ["legacy", "main"]);
        assert_eq!(s.read_only, ReadOnly::All);
        assert_eq!(parse("").read_only, ReadOnly::Production, "safe default");
    }
}
