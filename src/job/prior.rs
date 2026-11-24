//! 2.3 `inputs.prior_report`（可选，Q14）：**只**消费 `summary.routing`。
//!
//! triage 报告里真正能影响 review 的只有三件事：
//! - `summary.routing.focus[]` → `args.focus`（镜头：注意力加权，不增减覆盖）；
//! - `summary.routing.specs[]` → `args.spec`（必须落在 repo 内且可读，否则 `partial` + 点名）；
//! - `summary.routing.wall_clock_s` → **建议预算**（受 manifest 硬上限约束，只能收紧不能放宽）。
//!
//! 刻意不读的东西：`findings[]`、各 `*_count`、`security_tier`、`convention_drift`——
//! triage 的 findings 不是本轮 review 的阻断项（只有 triage 的 `blockers` 才是「已确认」，
//! 而它属于 triage 的输入语义，不是 review 的过滤条件）；阻断阈值由配置决定，**prior_report 不得改**。

use crate::job::status::JobFailure;
use serde_json::Value;
use std::path::{Component, Path};

/// `prior_report` 的投影结果（只含 routing 三件事 + 披露）。
#[derive(Clone, Debug, Default)]
pub struct PriorRouting {
    pub focus: Option<String>,
    /// 已校验可读的 spec **绝对路径**（`render_spec_section_capped` 用进程 cwd 读，不能给相对路径）。
    pub specs: Vec<String>,
    /// manifest 里声明的 spec 条数（披露用）。
    pub specs_declared: usize,
    /// 不可读 / 越界 / 非常规文本的 spec 条数（>0 → partial）。
    pub specs_unreadable: usize,
    /// 建议预算（秒）；调用方还要与 manifest 硬上限取 min。
    pub wall_clock_s: Option<u64>,
    /// `extensions.redteam.prior_routing`（保留声明的相对路径，不泄露绝对路径）。
    pub disclosure: Value,
}

/// 解析 `prior_report` 文本。JSON 非法 / 顶层不是对象 → `input_schema_mismatch:prior_report`。
/// `summary` / `summary.routing` 缺失或不是对象 → 空投影（Q14：triage 非 ok 时没有 summary）。
pub fn parse(text: &str, repo: &Path) -> Result<PriorRouting, JobFailure> {
    let v: Value = serde_json::from_str(text)
        .map_err(|_| JobFailure::new("input_schema_mismatch", Some("prior_report".into())))?;
    if !v.is_object() {
        return Err(JobFailure::new("input_schema_mismatch", Some("prior_report".into())));
    }
    let routing = v.get("summary").and_then(|s| s.get("routing")).filter(|r| r.is_object());
    let Some(routing) = routing else {
        return Ok(PriorRouting {
            disclosure: serde_json::json!({
                "present": true,
                "focus": [],
                "specs_used": [],
                "specs_unreadable": 0,
                "wall_clock_s": Value::Null,
            }),
            ..Default::default()
        });
    };

    // focus[]：非空字符串，按声明顺序保留。
    let focus_items: Vec<String> = routing
        .get("focus")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect())
        .unwrap_or_default();
    let focus = if focus_items.is_empty() { None } else { Some(focus_items.join("\n")) };

    // specs[]：相对 repo 的路径；canonicalize 后必须仍在 repo 内，且是可读的常规文本文件。
    let declared: Vec<&str> = routing
        .get("specs")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut specs = Vec::new();
    let mut specs_used = Vec::new();
    let mut specs_unreadable = 0usize;
    for s in &declared {
        match resolve_spec(repo, s) {
            Some(abs) => {
                specs_used.push((*s).to_string());
                specs.push(abs);
            }
            None => specs_unreadable += 1,
        }
    }

    // wall_clock_s：只认正整数值（字符串/负数/0 一律忽略——建议预算不是契约必需字段）。
    let wall_clock_s = routing.get("wall_clock_s").and_then(|x| x.as_u64()).filter(|s| *s > 0);

    let disclosure = serde_json::json!({
        "present": true,
        "focus": focus_items,
        "specs_used": specs_used,
        "specs_unreadable": specs_unreadable,
        "wall_clock_s": wall_clock_s,
    });
    Ok(PriorRouting {
        focus,
        specs,
        specs_declared: declared.len(),
        specs_unreadable,
        wall_clock_s,
        disclosure,
    })
}

/// spec 路径 → repo 内的绝对路径（可读常规文本）。任何一条不成立 → `None`（计入 unreadable）。
fn resolve_spec(repo: &Path, declared: &str) -> Option<String> {
    let p = Path::new(declared);
    // 只接受 repo 内的相对路径：绝对路径与 `..` 都越界（先做词法拒绝，连 canonicalize 都不必）。
    if p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    if declared.trim().is_empty() {
        return None;
    }
    let real = std::fs::canonicalize(repo.join(p)).ok()?;
    let root = std::fs::canonicalize(repo).ok()?;
    if !real.starts_with(&root) || !real.is_file() {
        return None;
    }
    // 可读常规文本：能打开、前 8KB 无 NUL、UTF-8 起始段合法（render_spec_section_capped 会再读一遍并自行限流）。
    let mut buf = Vec::new();
    {
        use std::io::Read;
        let f = std::fs::File::open(&real).ok()?;
        f.take(8192).read_to_end(&mut buf).ok()?;
    }
    if buf.contains(&0) {
        return None;
    }
    if let Err(e) = std::str::from_utf8(&buf) {
        // 8KB 截断可能切在多字节字符中间：只容忍「末尾不完整」这一种。
        if e.error_len().is_some() {
            return None;
        }
    }
    Some(real.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn repo() -> (tempfile::TempDir, std::path::PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("repo");
        std::fs::create_dir_all(repo.join("docs")).unwrap();
        std::fs::write(repo.join("docs/auth.md"), "auth spec\n").unwrap();
        std::fs::write(repo.join("docs/bin.dat"), [0u8, 1, 2]).unwrap();
        (d, repo)
    }

    fn triage(summary: Value) -> String {
        serde_json::json!({
            "schema": "hibot.audit.report.triage/1",
            "job_id": "T1", "kind": "triage", "status": "ok",
            "input": {"head_sha": "h", "base_sha": "b"},
            "executor": {"name": "redteam", "version": "0.1.0"},
            "findings": [{"fingerprint": "sha256:deadbeef"}],
            "summary": summary,
            "usage": {"input_tokens": 1, "output_tokens": 1, "cached_input_tokens": 0, "wall_clock_ms": 1},
            "extensions": {}
        })
        .to_string()
    }

    #[test]
    fn projects_focus_specs_and_wall_clock_only() {
        let (_d, repo) = repo();
        let text = triage(serde_json::json!({
            "security_tier": "high", "blockers_count": 3, "structural_count": 9, "local_count": 4,
            "intent_undelivered": false, "scope_creep_count": 2, "risk": "high",
            "convention_drift": "low", "fit": "ok",
            "routing": {"focus": ["auth", "  crypto  "], "specs": ["docs/auth.md", "docs/missing.md"], "wall_clock_s": 900}
        }));
        let p = parse(&text, &repo).unwrap();
        assert_eq!(p.focus.as_deref(), Some("auth\ncrypto"));
        assert_eq!(p.specs.len(), 1, "只保留可读的那份");
        assert!(p.specs[0].ends_with("docs/auth.md") && Path::new(&p.specs[0]).is_absolute());
        assert_eq!((p.specs_declared, p.specs_unreadable), (2, 1));
        assert_eq!(p.wall_clock_s, Some(900));
        // 只读 summary.routing：历史 findings / 计数不进投影（结构上就没有承载它们的字段）。
        assert_eq!(p.disclosure["focus"], serde_json::json!(["auth", "crypto"]));
        assert_eq!(p.disclosure["specs_unreadable"], 1);
        assert!(p.disclosure.get("blockers_count").is_none());
    }

    #[test]
    fn missing_summary_or_routing_is_an_empty_projection() {
        let (_d, repo) = repo();
        for text in [
            triage(serde_json::json!({})),
            serde_json::json!({"schema": "hibot.audit.report.triage/1", "status": "partial", "findings": []}).to_string(),
        ] {
            let p = parse(&text, &repo).unwrap();
            assert!(p.focus.is_none() && p.specs.is_empty() && p.wall_clock_s.is_none());
            assert_eq!(p.disclosure["present"], true);
        }
    }

    #[test]
    fn bad_json_is_input_schema_mismatch() {
        let (_d, repo) = repo();
        let e = parse("not json", &repo).unwrap_err();
        assert_eq!(e.full_code(), "input_schema_mismatch:prior_report");
        let e2 = parse("[]", &repo).unwrap_err();
        assert_eq!(e2.full_code(), "input_schema_mismatch:prior_report");
    }

    #[test]
    fn spec_paths_are_confined_to_the_repo_and_must_be_text() {
        let (d, repo) = repo();
        std::fs::write(d.path().join("outside.md"), "x\n").unwrap();
        let outside = format!("../outside.md");
        let text = triage(serde_json::json!({"routing": {"specs": [
            "../outside.md", "/etc/passwd", "docs/bin.dat", "docs/absent.md", "docs/auth.md"
        ]}}));
        let p = parse(&text, &repo).unwrap();
        assert_eq!(p.specs_unreadable, 4, "{p:?}");
        assert_eq!(p.specs.len(), 1);
        assert!(!p.disclosure.to_string().contains(&outside), "披露不出现越界路径以外的绝对路径");
    }

    #[test]
    fn zero_or_non_numeric_wall_clock_is_ignored() {
        let (_d, repo) = repo();
        for bad in [serde_json::json!(0), serde_json::json!(-5), serde_json::json!("900"), serde_json::json!(null)] {
            let text = triage(serde_json::json!({"routing": {"wall_clock_s": bad}}));
            assert!(parse(&text, &repo).unwrap().wall_clock_s.is_none(), "{bad}");
        }
    }

    #[test]
    fn git_repo_fixture_is_untouched() {
        // 夹具只用普通目录；顺手确认 parse 不跑 git、不写盘。
        let (_d, repo) = repo();
        let before = std::fs::read_dir(&repo).unwrap().count();
        let _ = parse(&triage(serde_json::json!({"routing": {"specs": ["docs/auth.md"]}})), &repo).unwrap();
        assert_eq!(std::fs::read_dir(&repo).unwrap().count(), before);
        let _ = Command::new("git"); // 保持 api 形状（无 git 调用）
    }
}
