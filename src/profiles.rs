//! Profile names from the shared AWS config files. Only section headers are read; keys and
//! secrets are left to the SDK's own profile provider.

use std::path::PathBuf;

pub fn profile_names() -> Vec<String> {
    let mut names = Vec::new();
    if let Some(text) = read(config_path()) {
        names.extend(config_profiles(&text));
    }
    if let Some(text) = read(credentials_path()) {
        names.extend(credentials_profiles(&text));
    }
    names.sort();
    names.dedup();
    names
}

fn read(path: Option<PathBuf>) -> Option<String> {
    std::fs::read_to_string(path?).ok()
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn config_path() -> Option<PathBuf> {
    std::env::var_os("AWS_CONFIG_FILE")
        .map(PathBuf::from)
        .or_else(|| Some(home()?.join(".aws/config")))
}

fn credentials_path() -> Option<PathBuf> {
    std::env::var_os("AWS_SHARED_CREDENTIALS_FILE")
        .map(PathBuf::from)
        .or_else(|| Some(home()?.join(".aws/credentials")))
}

fn headers(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix('[')?.strip_suffix(']').map(str::trim))
}

/// `[default]` and `[profile name]`; other sections such as `[sso-session x]` are skipped.
pub fn config_profiles(text: &str) -> Vec<String> {
    headers(text)
        .filter_map(|h| {
            if h == "default" {
                Some(h.to_string())
            } else {
                h.strip_prefix("profile ").map(|n| n.trim().to_string())
            }
        })
        .collect()
}

pub fn credentials_profiles(text: &str) -> Vec<String> {
    headers(text).map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_keeps_only_profile_sections() {
        let text = "[default]\nregion = us-east-1\n\n[profile dev]\nsso_session = corp\n\
                    [sso-session corp]\nsso_region = us-east-1\n# [profile commented]\n[profile  spaced ]\n";
        assert_eq!(config_profiles(text), ["default", "dev", "spaced"]);
    }

    #[test]
    fn credentials_sections_are_profile_names() {
        let text = "[default]\naws_access_key_id = x\n[prod]\naws_secret_access_key = y\n";
        assert_eq!(credentials_profiles(text), ["default", "prod"]);
    }
}
