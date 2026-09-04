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
/// 旧扁平形状（`Kimi = "sk-..."`）解析不出新结构——返回空并打 stderr 警告
/// （F1 防静默：write_secret 落盘前会先备份，旧 key 不丢）。
pub fn load_secrets() -> BTreeMap<String, BTreeMap<String, SecretEntry>> {
    let path = secrets_path();
    let Ok(text) = std::fs::read_to_string(&path) else { return BTreeMap::new() };
    match toml::from_str(&text) {
        Ok(m) => m,
        Err(e) => {
            eprintln!(
                "warning: {} 解析失败（{}）——若是 09-04 前的扁平形状请手动迁移为 [provider.\"model_id\"] 节；write_secret 前会自动备份原文件",
                path.display(), e
            );
            BTreeMap::new()
        }
    }
}

/// secrets.toml 是否存在但解析不出新形状（旧扁平/损坏）——write_secret 用它决定备份。
fn legacy_secrets_present() -> bool {
    let path = secrets_path();
    let Ok(text) = std::fs::read_to_string(&path) else { return false };
    if text.trim().is_empty() { return false; }
    toml::from_str::<BTreeMap<String, BTreeMap<String, SecretEntry>>>(&text).is_err()
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
    // F1：旧形状/损坏文件首次写入前备份——load_secrets 解析不出会回空 map，
    // 直接落盘等于物理销毁旧 key（不可逆）。备份后照常写新形状。
    if legacy_secrets_present() {
        let bak = path.with_extension("toml.bak");
        std::fs::copy(&path, &bak)?;
        eprintln!("warning: 旧形状 secrets.toml 已备份至 {}（含点 model_id 须引号：[provider.\"model.id\"]）", bak.display());
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
