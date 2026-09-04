use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 单条 secret（model 级）：`[provider.model_id]` 节下的字段。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SecretEntry {
    pub api_key: String,
}

/// 读取 ~/.config/redteam/secrets.toml → { provider: { model_id: { api_key } } }。
/// 形状随 09-04-model-config-split 与 config 的 provider/model 分离对齐（key 粒度=model 级）。
pub fn load_secrets() -> BTreeMap<String, BTreeMap<String, SecretEntry>> {
    let path = secrets_path();
    let Ok(text) = std::fs::read_to_string(&path) else { return BTreeMap::new() };
    toml::from_str(&text).unwrap_or_default()
}

pub fn secrets_path() -> PathBuf {
    dirs_config().join("redteam").join("secrets.toml")
}

pub fn dirs_config() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            home.join(".config")
        })
}

/// 写入/更新 secrets.toml 的 [provider.model_id] key，并 chmod 600（密钥物理隔离主配置）。
pub fn write_secret(provider: &str, model_id: &str, key: &str) -> anyhow::Result<()> {
    let path = secrets_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut map = load_secrets();
    map.entry(provider.to_string())
        .or_default()
        .insert(model_id.to_string(), SecretEntry { api_key: key.to_string() });
    let text = toml::to_string(&map)?;
    std::fs::write(&path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_secret_persists_and_chmods_600() {
        let _g = super::super::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: ENV_LOCK 串行化 env 变更
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()); }
        write_secret("agc", "glm-5.3", "sk-test-123").unwrap();
        // 读回（嵌套 provider.model_id 寻址）
        assert_eq!(
            load_secrets().get("agc").and_then(|m| m.get("glm-5.3")).map(|e| e.api_key.as_str()),
            Some("sk-test-123")
        );
        // 再写一个同 provider 的 model，保留旧的
        write_secret("agc", "kimi-k3", "sk-test-456").unwrap();
        assert_eq!(
            load_secrets().get("agc").and_then(|m| m.get("glm-5.3")).map(|e| e.api_key.as_str()),
            Some("sk-test-123")
        );
        assert_eq!(
            load_secrets().get("agc").and_then(|m| m.get("kimi-k3")).map(|e| e.api_key.as_str()),
            Some("sk-test-456")
        );
        // chmod 600
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(secrets_path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "secrets.toml must be chmod 600");
        }
        unsafe { std::env::remove_var("XDG_CONFIG_HOME"); }
    }
}
