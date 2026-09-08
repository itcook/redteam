//! `secrets.json`（design v9 §12.2〔D12〕）：
//! ```json
//! { "providers": { "agc": { "apiKey": "sk-gateway", "models": { "gemini-3.8-flash": "sk-model-specific" } } } }
//! ```
//! 只在 `$REDTEAM_HOME/secrets.json`；`apiKey` 永不出现在 config.json。解析链见 `Config::resolve_key`。
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderSecrets {
    pub api_key: Option<String>,
    /// model id → key（模型级覆盖）。
    pub models: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default)]
pub struct Secrets {
    pub providers: BTreeMap<String, ProviderSecrets>,
    /// 形状 / 类型问题（调用方决定披露渠道）；能读的部分照读。
    pub warnings: Vec<String>,
}

impl Secrets {
    /// 解析文本；永不失败——JSON 语法错误整档视为空并 warning，形状问题逐条 warning。
    pub fn parse(text: &str) -> Secrets {
        let mut out = Secrets::default();
        if text.trim().is_empty() {
            return out;
        }
        let root: serde_json::Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                out.warnings.push(format!("secrets.json 解析失败（{e}），本次视为空"));
                return out;
            }
        };
        let Some(top) = root.as_object() else {
            out.warnings.push("secrets.json 顶层不是对象".into());
            return out;
        };
        for k in top.keys() {
            if k != "providers" {
                out.warnings.push(format!("secrets.json 顶层键 {k:?} 无定义（形状：{{\"providers\": {{\"<name>\": {{\"apiKey\", \"models\"}}}}}}）"));
            }
        }
        let Some(provs) = top.get("providers").and_then(|p| p.as_object()) else {
            if top.contains_key("providers") {
                out.warnings.push("secrets.json providers 不是对象".into());
            }
            return out;
        };
        for (prov, pv) in provs {
            let Some(po) = pv.as_object() else {
                out.warnings.push(format!("secrets.json providers.{prov} 不是对象"));
                continue;
            };
            let ps = out.providers.entry(prov.clone()).or_default();
            for (key, val) in po {
                match key.as_str() {
                    "apiKey" => match val.as_str() {
                        Some(s) => ps.api_key = Some(s.to_string()),
                        None => out.warnings.push(format!("secrets.json providers.{prov}.apiKey 不是字符串")),
                    },
                    "models" => match val.as_object() {
                        Some(ms) => {
                            for (mid, mv) in ms {
                                match mv.as_str() {
                                    Some(s) => {
                                        ps.models.insert(mid.clone(), s.to_string());
                                    }
                                    None => out.warnings.push(format!("secrets.json providers.{prov}.models.{mid:?} 不是字符串")),
                                }
                            }
                        }
                        None => out.warnings.push(format!("secrets.json providers.{prov}.models 不是对象")),
                    },
                    other => out.warnings.push(format!(
                        "secrets.json providers.{prov}.{other:?} 无定义（只认 apiKey / models；模型级 key 写在 models.<id>）"
                    )),
                }
            }
        }
        out
    }
}

/// 读取 `$REDTEAM_HOME/secrets.json`；文件不存在或配置根不可定位 → 空。
pub fn load_secrets() -> Secrets {
    let Ok(path) = secrets_path() else { return Secrets::default() };
    match std::fs::read_to_string(&path) {
        Ok(text) => Secrets::parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Secrets::default(),
        Err(e) => Secrets { providers: BTreeMap::new(), warnings: vec![format!("读取 {} 失败：{e}", path.display())] },
    }
}

pub fn secrets_path() -> anyhow::Result<PathBuf> {
    Ok(crate::config::redteam_home()?.join("secrets.json"))
}

/// 写入 / 更新 key 并 chmod 600：`model_id = None` 写 `providers.X.apiKey`，否则写 `providers.X.models.<id>`。
/// 其他内容原样保留；现有文件解析失败 → 拒写不覆写（#8，与 `write_setup` 同策略）。
pub fn write_secret(provider: &str, model_id: Option<&str>, key: &str) -> anyhow::Result<PathBuf> {
    let path = secrets_path()?;
    let existing = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => anyhow::bail!("读取 {} 失败：{e}", path.display()),
    };
    let mut root: serde_json::Value = if existing.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&existing)
            .map_err(|e| anyhow::anyhow!("{} 解析失败（{e}）：不覆写现有密钥文件，请先修复", path.display()))?
    };
    let root_o = root.as_object_mut().ok_or_else(|| anyhow::anyhow!("secrets.json 顶层不是对象"))?;
    let provs = root_o.entry("providers").or_insert_with(|| serde_json::json!({}));
    let provs_o = provs.as_object_mut().ok_or_else(|| anyhow::anyhow!("providers 不是对象"))?;
    let pv = provs_o.entry(provider.to_string()).or_insert_with(|| serde_json::json!({}));
    let po = pv.as_object_mut().ok_or_else(|| anyhow::anyhow!("providers.{provider} 不是对象"))?;
    match model_id {
        None => {
            po.insert("apiKey".into(), serde_json::json!(key));
        }
        Some(mid) => {
            let ms = po.entry("models").or_insert_with(|| serde_json::json!({}));
            let mo = ms.as_object_mut().ok_or_else(|| anyhow::anyhow!("providers.{provider}.models 不是对象"))?;
            mo.insert(mid.to_string(), serde_json::json!(key));
        }
    }
    // 原子写 + 临时文件即 0600（评审 #6：无 umask 明文窗口、无半截文件）
    crate::config::write_atomic(&path, &(serde_json::to_string_pretty(&root)? + "\n"), Some(0o600))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_provider_and_model_level_keys_with_warnings() {
        let s = Secrets::parse(r#"{"providers":{"agc":{"apiKey":"sk-gateway","models":{"gemini-3.8-flash":"sk-model","bad":1},"org":"x"}},"extra":1}"#);
        let p = &s.providers["agc"];
        assert_eq!(p.api_key.as_deref(), Some("sk-gateway"));
        assert_eq!(p.models["gemini-3.8-flash"], "sk-model");
        assert!(!p.models.contains_key("bad"));
        assert_eq!(s.warnings.len(), 3, "{:?}", s.warnings);
        assert!(s.warnings.iter().any(|w| w.contains("\"bad\"")) && s.warnings.iter().any(|w| w.contains("\"org\"")) && s.warnings.iter().any(|w| w.contains("\"extra\"")));
        let broken = Secrets::parse("{\"providers\": ");
        assert!(broken.providers.is_empty() && broken.warnings[0].contains("解析失败"));
        assert!(Secrets::parse("  ").warnings.is_empty());
    }

    #[test]
    fn write_secret_both_levels_chmod_and_refuse_on_unparseable() {
        let _g = super::super::ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: ENV_LOCK 串行化 env 变更
        unsafe { std::env::set_var("REDTEAM_HOME", dir.path()); }
        write_secret("agc", None, "sk-prov").unwrap();
        write_secret("agc", Some("glm-5.3"), "sk-model").unwrap();
        let s = load_secrets();
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        assert_eq!(s.providers["agc"].api_key.as_deref(), Some("sk-prov"));
        assert_eq!(s.providers["agc"].models["glm-5.3"], "sk-model");
        write_secret("agc", Some("glm-5.3"), "sk-model2").unwrap();
        let s2 = load_secrets();
        assert_eq!(s2.providers["agc"].models["glm-5.3"], "sk-model2");
        assert_eq!(s2.providers["agc"].api_key.as_deref(), Some("sk-prov"), "其他内容保留");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(secrets_path().unwrap()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "secrets.json must be chmod 600");
        }
        // 解析失败 → 拒写、原文逐字保留（#8）
        let broken = "{\"providers\": {";
        std::fs::write(secrets_path().unwrap(), broken).unwrap();
        let err = write_secret("agc", None, "k").unwrap_err().to_string();
        assert!(err.contains("不覆写"), "{err}");
        assert_eq!(std::fs::read_to_string(secrets_path().unwrap()).unwrap(), broken);
        unsafe { std::env::remove_var("REDTEAM_HOME"); }
    }
}
