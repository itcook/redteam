use std::collections::BTreeMap;
use std::path::PathBuf;

/// 读取 ~/.config/breakpoints/secrets.toml → { provider_name: key }
pub fn load_secrets() -> BTreeMap<String, String> {
    let path = secrets_path();
    let Ok(text) = std::fs::read_to_string(&path) else { return BTreeMap::new() };
    toml::from_str::<BTreeMap<String, String>>(&text).unwrap_or_default()
}

pub fn secrets_path() -> PathBuf {
    dirs_config().join("breakpoints").join("secrets.toml")
}

pub fn dirs_config() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            home.join(".config")
        })
}
