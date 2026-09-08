//! `secrets.toml`（design §12.2）：
//! ```toml
//! [providers.X]
//! api_key = "…"                 # provider 级
//! "model.id" = "…"              # 模型级覆盖（含点的 id 必须引号）；值也可为 { api_key = "…" }
//! ```
//! 只在 `$REDTEAM_HOME/secrets.toml`；`api_key` 永不出现在 config.toml。解析链见 `Config::resolve_key`。
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecretEntry {
    pub api_key: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderSecrets {
    pub api_key: Option<String>,
    /// model_id → key（模型级覆盖）。
    pub models: BTreeMap<String, SecretEntry>,
}

#[derive(Clone, Debug, Default)]
pub struct Secrets {
    pub providers: BTreeMap<String, ProviderSecrets>,
    /// 守卫产出（调用方决定披露渠道）：裸写带点 id 的子表、旧形状顶层表、类型不对的值。
    pub warnings: Vec<String>,
}

impl Secrets {
    /// 解析文本；永不失败——形状问题进 `warnings`，能读的部分照读。
    pub fn parse(text: &str) -> Secrets {
        let mut out = Secrets::default();
        let root: toml::Value = match toml::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                out.warnings.push(format!("secrets.toml 解析失败（{e}），本次视为空"));
                return out;
            }
        };
        let Some(table) = root.as_table() else {
            out.warnings.push("secrets.toml 顶层不是表".into());
            return out;
        };
        for (k, v) in table {
            if k != "providers" {
                out.warnings.push(format!(
                    "secrets.toml 顶层出现 [{k}]：旧形状（09-04 `[provider.\"model\"]`），请手动迁到 [providers.{k}]（api_key / \"<model_id>\" = key）"
                ));
                continue;
            }
            let Some(provs) = v.as_table() else {
                out.warnings.push("secrets.toml [providers] 不是表".into());
                continue;
            };
            for (prov, pv) in provs {
                let Some(pt) = pv.as_table() else {
                    out.warnings.push(format!("secrets.toml [providers.{prov}] 不是表"));
                    continue;
                };
                let ps = out.providers.entry(prov.clone()).or_default();
                for (key, val) in pt {
                    if key == "api_key" {
                        match val.as_str() {
                            Some(s) => ps.api_key = Some(s.to_string()),
                            None => out.warnings.push(format!("secrets.toml [providers.{prov}].api_key 不是字符串")),
                        }
                        continue;
                    }
                    match val {
                        toml::Value::String(s) => {
                            ps.models.insert(key.clone(), SecretEntry { api_key: s.clone() });
                        }
                        toml::Value::Table(t) => match t.get("api_key").and_then(|x| x.as_str()) {
                            Some(s) => {
                                ps.models.insert(key.clone(), SecretEntry { api_key: s.to_string() });
                            }
                            None => out.warnings.push(format!(
                                "secrets.toml [providers.{prov}] 下出现子表 {key:?}：是否漏了引号？model_id 含点须写成 \"{key}.…\" = \"…\""
                            )),
                        },
                        _ => out.warnings.push(format!(
                            "secrets.toml [providers.{prov}] 的 {key:?} 既不是字符串也不是 {{ api_key = … }}"
                        )),
                    }
                }
            }
        }
        out
    }
}

/// 读取 `$REDTEAM_HOME/secrets.toml`；文件不存在 → 空。
pub fn load_secrets() -> Secrets {
    let path = secrets_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => Secrets::parse(&text),
        Err(_) => Secrets::default(),
    }
}

pub fn secrets_path() -> PathBuf {
    crate::config::redteam_home().join("secrets.toml")
}

/// 写入 / 更新 key 并 chmod 600：`model_id = None` 写 `[providers.X].api_key`，否则写 `"model_id" = key`。
/// 其他内容深合并保留；旧形状（顶层非 providers 表）先备份 `.toml.bak` 再写，不自动迁移（D7 A9）。
pub fn write_secret(provider: &str, model_id: Option<&str>, key: &str) -> anyhow::Result<()> {
    let path = secrets_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut root: toml::Value = match toml::from_str(&existing) {
        Ok(v) => v,
        Err(_) if existing.trim().is_empty() => toml::Value::Table(Default::default()),
        Err(e) => {
            let bak = path.with_extension("toml.bak");
            std::fs::copy(&path, &bak)?;
            eprintln!("warning: 原 secrets.toml 解析失败（{e}），已备份至 {} 后重写", bak.display());
            toml::Value::Table(Default::default())
        }
    };
    let root_t = root.as_table_mut().ok_or_else(|| anyhow::anyhow!("secrets root is not a table"))?;
    if root_t.keys().any(|k| k != "providers") {
        let bak = path.with_extension("toml.bak");
        std::fs::copy(&path, &bak)?;
        eprintln!("warning: secrets.toml 含旧形状顶层表，已备份至 {}；请手动迁到 [providers.X]", bak.display());
    }
    let provs = root_t
        .entry("providers".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let provs_t = provs.as_table_mut().ok_or_else(|| anyhow::anyhow!("[providers] is not a table"))?;
    let pt = provs_t
        .entry(provider.to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let pt = pt.as_table_mut().ok_or_else(|| anyhow::anyhow!("[providers.{provider}] is not a table"))?;
    pt.insert(model_id.unwrap_or("api_key").to_string(), toml::Value::String(key.to_string()));
    std::fs::write(&path, toml::to_string_pretty(&root)?)?;
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
    fn parse_provider_and_model_level_keys() {
        let s = Secrets::parse(r#"[providers.agc]
api_key = "sk-gateway"
"gemini-3.8-flash" = "sk-model"
"kimi-k3" = { api_key = "sk-inline", org = "o" }
"#);
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        let p = &s.providers["agc"];
        assert_eq!(p.api_key.as_deref(), Some("sk-gateway"));
        assert_eq!(p.models["gemini-3.8-flash"].api_key, "sk-model");
        assert_eq!(p.models["kimi-k3"].api_key, "sk-inline");
    }

    #[test]
    fn bare_dotted_id_guard_and_legacy_top_level_warn() {
        // 裸写 [providers.agc.glm-5.3] api_key=… → 子表 "glm-5" 下再嵌 "3"，无 api_key → 引号提示
        let s = Secrets::parse("[providers.agc]\n[providers.agc.glm-5.3]\napi_key = \"k\"\n");
        assert!(s.warnings.iter().any(|w| w.contains("漏了引号")), "{:?}", s.warnings);
        let legacy = Secrets::parse("[agc.\"glm-5.3\"]\napi_key = \"k\"\n");
        assert!(legacy.warnings.iter().any(|w| w.contains("旧形状")), "{:?}", legacy.warnings);
        assert!(legacy.providers.is_empty());
    }

    #[test]
    fn write_secret_persists_both_levels_and_chmods_600() {
        let _g = super::super::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: ENV_LOCK 串行化 env 变更
        unsafe { std::env::set_var("REDTEAM_HOME", dir.path()); }
        write_secret("agc", None, "sk-prov").unwrap();
        write_secret("agc", Some("glm-5.3"), "sk-model").unwrap();
        let s = load_secrets();
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        assert_eq!(s.providers["agc"].api_key.as_deref(), Some("sk-prov"));
        assert_eq!(s.providers["agc"].models["glm-5.3"].api_key, "sk-model");
        // 再写同 model 覆盖、其他保留
        write_secret("agc", Some("glm-5.3"), "sk-model2").unwrap();
        let s2 = load_secrets();
        assert_eq!(s2.providers["agc"].models["glm-5.3"].api_key, "sk-model2");
        assert_eq!(s2.providers["agc"].api_key.as_deref(), Some("sk-prov"));
        // 文件里带点 id 被引号包住
        let raw = std::fs::read_to_string(secrets_path()).unwrap();
        assert!(raw.contains("\"glm-5.3\""), "{raw}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(secrets_path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "secrets.toml must be chmod 600");
        }
        unsafe { std::env::remove_var("REDTEAM_HOME"); }
    }
}
