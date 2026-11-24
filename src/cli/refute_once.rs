//! `refute-once` 离线单发（M21b 批 D-4，design §8.1「去留由数据决定」〔D17〕）。
//!
//! 只读仓、零写库：读一条 finding 声明 → 建**产品同一份**复核 prompt → 按 `--refuter` 发一轮 →
//! 按产品同一份解析吐判决 JSON。`eval/refute_score.py` 靠它打离线基线——固定集与评分留在 Python，
//! prompt / provider / 解析一律不在 Python 复刻：复刻一份，测的就不是产品了。

use crate::config::{Config, EffectiveModel};
use crate::domain::{Finding, Origin, Severity};

pub struct RefuteOnceArgs<'a> {
    /// finding JSON 路径（`-` = stdin）。
    pub finding: &'a str,
    /// 证据树根（缺省当前目录）；固定集按 `git show <sha>:<path>` 物化到临时树后指过来。
    pub root: Option<&'a str>,
    /// 复核模型 `provider/id`（缺省取配置里 refuter 角色的第一个）。
    pub refuter: Option<&'a str>,
    /// 只打印 prompt，不发请求（零网络）。
    pub dump_prompt: bool,
}

/// finding 声明：只收复核 prompt 与快照真正用到的字段，其余给中性缺省——
/// 固定集（台账导出）本来就缺 body / 行号，缺什么就显式落成模板里的占位，不假造内容。
#[derive(serde::Deserialize, Default)]
#[serde(default, rename_all = "snake_case")]
struct Claim {
    id: String,
    file: String,
    line_start: u32,
    line_end: u32,
    severity: Option<Severity>,
    title: String,
    body: String,
    quote: String,
    confidence: f32,
}

fn claim_to_finding(c: Claim) -> Finding {
    Finding {
        id: c.id,
        severity: c.severity.unwrap_or(Severity::Medium),
        title: c.title,
        body: c.body,
        file: c.file,
        line_start: c.line_start.max(1),
        line_end: c.line_end.max(c.line_start.max(1)),
        confidence: c.confidence,
        recommendation: String::new(),
        models: vec![],
        agreement: 1,
        suspicion_agreement: 0,
        hard_constraint: false,
        quote: c.quote,
        previously_dismissed: false,
        origin: Origin::New,
        outside_window: false,
        cluster_id: None,
        refute: None,
    }
}

/// 选复核模型：显式 `--refuter` 优先按 `provider/id` 取；缺省取 `refuter` 角色首个。
/// **优先 refuter 角色条目**（reasoningEffort 等档位按角色配）；该模型没配 refuter 角色时
/// 回落它的其他角色条目，并在输出里标出用的是哪个角色的档位——离线基线与线上不同档必须看得见。
fn pick_model(cfg: &Config, want: Option<&str>) -> Result<(EffectiveModel, &'static str), String> {
    // `active_roles()` 返回的是自有集合——取一份克隆，调用方不必跟着它的生命周期。
    let roles = cfg.active_roles();
    let find = |set: &[EffectiveModel], name: &str| set.iter().find(|m| m.name == name).cloned();
    match want {
        None => roles
            .refuters
            .first()
            .map(|m| (m.clone(), "refuter"))
            .ok_or_else(|| "配置里没有 refuter 角色的模型；用 --refuter provider/id 指定".to_string()),
        Some(name) => {
            if let Some(m) = find(&roles.refuters, name) {
                return Ok((m, "refuter"));
            }
            if let Some(m) = find(&roles.reviewers, name) {
                return Ok((m, "reviewer"));
            }
            if let Some(m) = find(&roles.scanners, name) {
                return Ok((m, "scanner"));
            }
            Err(format!("配置里没有模型 {name}（须是 provider/id，且该模型至少配了一个角色）"))
        }
    }
}

fn read_claim(path: &str) -> Result<Claim, String> {
    use std::io::Read;
    let mut raw = String::new();
    if path == "-" {
        std::io::stdin().read_to_string(&mut raw).map_err(|e| format!("读 stdin 失败: {e}"))?;
    } else {
        raw = std::fs::read_to_string(path).map_err(|e| format!("读 {path} 失败: {e}"))?;
    }
    serde_json::from_str(&raw).map_err(|e| format!("finding JSON 解析失败: {e}"))
}

pub async fn run_refute_once(a: RefuteOnceArgs<'_>, cfg: &Config) -> i32 {
    let claim = match read_claim(a.finding) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let f = claim_to_finding(claim);
    let root = std::path::Path::new(a.root.unwrap_or("."));
    // 与生产路径的**两处有意差异**（收口评审 runner-up，别当成漂移）：
    // ① 这里不过 `need_read_allowed_in` 读守卫——`--root` 是评测脚本按 git 对象现物化的单文件树，
    //    路径由维护者的固定集给定、不是模型在线输出，而那棵树根本不是 git 仓（守卫会一律拒）；
    // ② `spec_docs` 恒空：离线基线不注入 spec，两臂同条件可比优先。
    let source_context = crate::consolidate::refute::source_snippet(root, &f);
    let lang = crate::config::OutputLang::resolve_local(cfg);
    let prompt = crate::prompt::build_refute_prompt(&crate::prompt::RefutePromptInput {
        lang: &lang,
        file: &f.file,
        line_start: f.line_start,
        line_end: f.line_end,
        severity: f.severity.as_str(),
        title: &f.title,
        body: &f.body,
        quote: &f.quote,
        source_context: &source_context,
        // 与产品 `run_refute` 同参：判例摘要是 M21c 台账的空槽，repo_brief / spec 不进离线基线。
        precedents: "",
        spec_docs: "",
        repo_brief: "",
    });
    let hash = crate::prompt::refute_template_hash();
    if a.dump_prompt {
        let blocks: Vec<serde_json::Value> = prompt
            .blocks
            .iter()
            .map(|b| serde_json::json!({ "text": b.text, "cache_boundary": b.cache_boundary }))
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "system": prompt.system,
                "blocks": blocks,
                "template_hash": hash,
                "snippet_chars": source_context.chars().count(),
            })
        );
        return 0;
    }

    let (em, role_profile) = match pick_model(cfg, a.refuter) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let key = cfg.resolve_key(&em.provider, &em.id).unwrap_or_default();
    let backend = match crate::provider::backends::backend_from_config(&em, key) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: 构造 backend 失败: {e}");
            return 2;
        }
    };
    let messages = vec![
        crate::provider::tool_loop::ChatMessage::System(prompt.system.clone()),
        crate::provider::tool_loop::ChatMessage::UserBlocks(prompt.blocks.clone()),
    ];
    let t0 = std::time::Instant::now();
    let turn = crate::provider::timed_turn_final(
        backend.as_ref(),
        &messages,
        std::time::Duration::from_secs(em.per_turn_secs()),
        crate::provider::DEFAULT_RETRY_BUDGET,
    )
    .await;
    let ms = t0.elapsed().as_millis() as u64;
    let base = serde_json::json!({
        "id": f.id,
        "model": em.name,
        "role_profile": role_profile,
        "template_hash": hash,
        "snippet_chars": source_context.chars().count(),
        "duration_ms": ms,
    });
    let merge = |extra: serde_json::Value| {
        let mut v = base.clone();
        if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        v
    };
    match turn {
        Ok(t) => {
            // 解析与产品同一份：读不懂 → 保守 upheld（误驳会静默删掉一条真缺陷）。
            let (verdict, reason, parsed) = match crate::provider::parse::parse_refute_output(&t.text) {
                Ok((v, r)) => (v, r, true),
                Err(_) => (crate::domain::RefuteVerdict::Upheld, "verdict unparseable".to_string(), false),
            };
            println!(
                "{}",
                merge(serde_json::json!({
                    "ok": true,
                    "parsed": parsed,
                    "verdict": if verdict == crate::domain::RefuteVerdict::Refuted { "refuted" } else { "upheld" },
                    "reason": reason,
                    "raw": t.text,
                    "input_tokens": t.input_tokens,
                    "output_tokens": t.output_tokens,
                    "cached_input_tokens": t.cached_input_tokens,
                }))
            );
            0
        }
        Err(e) => {
            // 调用失败：产品按保守 upheld 收，离线基线要单独计——把失败混进混淆矩阵会虚高 upheld。
            println!(
                "{}",
                merge(serde_json::json!({
                    "ok": false,
                    "parsed": false,
                    "verdict": "upheld",
                    "reason": "refuter call failed",
                    "error": format!("{e}"),
                }))
            );
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 缺字段的固定集行（台账导出没有 body / 行号）不假造内容：行号夹到 1、severity 缺省 medium，
    /// 空 body / 空 quote 原样交给模板去落占位。
    #[test]
    fn claim_defaults_are_neutral_not_invented() {
        let c: Claim = serde_json::from_str(r#"{"file":"a.rs","title":"t","quote":"q"}"#).unwrap();
        let f = claim_to_finding(c);
        assert_eq!((f.line_start, f.line_end, f.severity), (1, 1, Severity::Medium));
        assert!(f.body.is_empty() && f.recommendation.is_empty(), "缺的字段留空，由模板落占位");
        assert_eq!(f.quote, "q");
    }

    /// `line_end < line_start` 的脏行不产生倒挂区间（快照窗口按 [start,end] 取，倒挂会算出空窗）。
    #[test]
    fn claim_clamps_inverted_range() {
        let c: Claim = serde_json::from_str(r#"{"file":"a.rs","line_start":40,"line_end":3}"#).unwrap();
        let f = claim_to_finding(c);
        assert_eq!((f.line_start, f.line_end), (40, 40));
    }

    /// 选模型：同名模型优先取 refuter 角色条目（档位按角色配），没配 refuter 才回落别的角色并标明。
    #[test]
    fn pick_model_prefers_the_refuter_role_entry() {
        let cfg: Config = serde_json::from_str(
            r#"{"providers":{"p":{"kind":"openai","baseUrl":"https://x","models":{
                "s":{"roles":{"scanner":{}}},
                "v":{"roles":{"refuter":{}}}
            }}}}"#,
        )
        .unwrap();
        assert_eq!(pick_model(&cfg, Some("p/v")).unwrap().1, "refuter");
        assert_eq!(pick_model(&cfg, Some("p/s")).unwrap().1, "scanner", "没配 refuter 角色 → 回落并标明档位来源");
        assert_eq!(pick_model(&cfg, None).unwrap().0.name, "p/v", "缺省取 refuter 角色首个");
        assert!(pick_model(&cfg, Some("p/nope")).is_err());
    }
}
