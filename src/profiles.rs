//! Profiles from the shared AWS config files, for the connection list. The credentials file
//! contributes only its section names; from the config file only the region, the account and
//! how credentials are obtained are read. Secrets are left to the SDK's own profile provider.

use std::collections::HashMap;
use std::path::PathBuf;

/// What the connection list shows about a profile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileInfo {
    pub region: Option<String>,
    /// The AWS account ID, from `sso_account_id` or the account in `role_arn`.
    pub account: Option<String>,
    /// How credentials are obtained: `SSO`, `assume role`, `process` or `keys`.
    pub kind: &'static str,
}

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

/// Region, account and credential kind per profile name.
pub fn profile_info() -> HashMap<String, ProfileInfo> {
    let mut info = read(config_path())
        .map(|text| config_info(&text))
        .unwrap_or_default();
    if let Some(text) = read(credentials_path()) {
        for name in credentials_profiles(&text) {
            let entry = info.entry(name).or_default();
            if entry.kind.is_empty() {
                entry.kind = "keys";
            }
        }
    }
    info
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
    headers(text).filter_map(profile_name).collect()
}

fn profile_name(header: &str) -> Option<String> {
    if header == "default" {
        Some(header.to_string())
    } else {
        header
            .strip_prefix("profile ")
            .map(|n| n.trim().to_string())
    }
}

/// The region, account and credential kind of each config profile. Only the names of the
/// settings that decide the kind are looked at, never their values, except for the region and
/// the account.
pub fn config_info(text: &str) -> HashMap<String, ProfileInfo> {
    let mut info: HashMap<String, ProfileInfo> = HashMap::new();
    let mut current: Option<String> = None;
    for line in text.lines().map(str::trim) {
        if line.starts_with(['#', ';']) {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = profile_name(header.trim());
            if let Some(name) = &current {
                info.entry(name.clone()).or_default();
            }
            continue;
        }
        let (Some(entry), Some((key, value))) = (
            current.as_ref().and_then(|n| info.get_mut(n)),
            line.split_once('='),
        ) else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "region" if !value.is_empty() => entry.region = Some(value.to_string()),
            "sso_account_id" => {
                entry.account = Some(value.to_string());
                entry.kind = "SSO";
            }
            "sso_session" | "sso_start_url" if entry.kind.is_empty() => entry.kind = "SSO",
            "role_arn" => {
                entry.account = value
                    .split(':')
                    .nth(4)
                    .filter(|a| !a.is_empty())
                    .map(str::to_string);
                entry.kind = "assume role";
            }
            "credential_process" => entry.kind = "process",
            "aws_access_key_id" if entry.kind.is_empty() => entry.kind = "keys",
            _ => {}
        }
    }
    info
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
    fn config_gives_region_account_and_kind() {
        let text = "[profile sso]\nsso_session = corp\nsso_account_id = 111122223333\nregion = ap-northeast-2\n\
                    [profile role]\nrole_arn = arn:aws:iam::444455556666:role/admin\nsource_profile = sso\n\
                    [profile proc]\ncredential_process = /bin/creds\n\
                    [sso-session corp]\nsso_region = us-east-1\n";
        let info = config_info(text);
        assert_eq!(
            info["sso"],
            ProfileInfo {
                region: Some("ap-northeast-2".into()),
                account: Some("111122223333".into()),
                kind: "SSO",
            }
        );
        assert_eq!(info["role"].account.as_deref(), Some("444455556666"));
        assert_eq!(info["role"].kind, "assume role");
        assert_eq!(info["proc"].kind, "process");
        assert_eq!(info.len(), 3, "sso-session is not a profile");
    }

    #[test]
    fn credentials_sections_are_profile_names() {
        let text = "[default]\naws_access_key_id = x\n[prod]\naws_secret_access_key = y\n";
        assert_eq!(credentials_profiles(text), ["default", "prod"]);
    }
}
