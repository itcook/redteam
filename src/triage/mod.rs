//! 3.1–3.7 triage 编排：输入 → StandardsMap → 三轴并行 → 锚定/去重/tally → 报告素材。
//!
//! 取消（SIGTERM / 墙钟）由调用方 `select!` 控制：已完成的轴通过 [`Progress`] 交给
//! [`snapshot_finalize`] 收敛成 `partial`（不丢已完成成果；`summary` 只在三轴全成时生成）。

pub mod input;
pub mod lenses;
pub mod shot;
pub mod standards;
pub mod tally;

use crate::config::OutputLang;
use crate::domain::{Finding, Role};
use crate::evidence::git::FileChange;
use crate::job::status::JobFailure;
use crate::provider::registry::ProviderRegistry;
use crate::triage::input::PrContext;
use crate::triage::lenses::{DriftRow, IntentOutput, SecurityRow};
use crate::triage::standards::StandardsMap;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// 一次 triage 的输入（repo 已由调用方完成完整性校验）。
pub struct TriageRequest<'a> {
    pub repo: &'a Path,
    pub pr: &'a PrContext,
    pub changes: &'a [FileChange],
    pub lang: &'a OutputLang,
    /// manifest 的墙钟硬上限（秒）；`routing.wall_clock_s` 由它派生。
    pub manifest_cap_s: u64,
    /// 三轴使用的模型（`roles.scanner` 的第一条；grill：不新增配置块）。
    pub route_key: String,
}

/// 三轴已完成的结果（取消后从这里取已完成部分）。
#[derive(Clone, Default)]
pub struct Progress {
    pub map: Option<StandardsMap>,
    pub security: Option<lenses::AxisRun<SecurityRow>>,
    pub drift: Option<lenses::AxisRun<DriftRow>>,
    pub intent: Option<lenses::AxisRun<IntentOutput>>,
}

/// 报告层需要的全部素材。
#[derive(Clone, Debug)]
pub struct TriageOutcome {
    pub completed: bool,
    pub summary: Option<Value>,
    pub findings: Vec<Finding>,
    pub blockers: Vec<Value>,
    pub axes: Vec<Value>,
    pub usage: (u64, u64, u64),
    pub reasons: Vec<JobFailure>,
    /// 合并到 findings 的 model 身份（报告 `source.model`）。
    pub route_key: String,
}

fn changed_files(changes: &[FileChange]) -> BTreeSet<String> {
    changes.iter().filter(|c| !c.diff.trim().is_empty()).map(|c| c.path.clone()).collect()
}

fn patch_pairs(changes: &[FileChange]) -> Vec<(String, String)> {
    changes
        .iter()
        .filter(|c| !c.diff.trim().is_empty())
        .map(|c| (c.path.clone(), c.diff.clone()))
        .collect()
}

/// 跑三轴（并发，只受 provider 门约束）。`progress` 在每轴完成时更新，取消后仍可取快照。
pub async fn run(registry: &ProviderRegistry, req: &TriageRequest<'_>, progress: Arc<Mutex<Progress>>) -> TriageOutcome {
    let changed = changed_files(req.changes);
    let files = patch_pairs(req.changes);
    let map = std::sync::Arc::new(standards::build(
        req.repo,
        &changed.iter().cloned().collect::<Vec<_>>(),
    ));
    {
        let mut p = progress.lock().unwrap();
        p.map = Some((*map).clone());
    }

    if files.is_empty() {
        // 空 patch：没有可审计的注入/漂移面；意图轴仍判"声明是否兑现"。
        let mut p = progress.lock().unwrap();
        p.security = Some(lenses::AxisRun { ok: true, ..Default::default() });
        p.drift = Some(lenses::AxisRun { ok: true, ..Default::default() });
    }

    let (sec, drift, intent) = tokio::join!(
        async {
            if files.is_empty() {
                return;
            }
            let r = lenses::run_security(registry, &req.route_key, req.lang, req.pr, &files).await;
            progress.lock().unwrap().security = Some(r);
        },
        async {
            if files.is_empty() {
                return;
            }
            let r = lenses::run_drift(registry, &req.route_key, req.lang, &map, req.repo, &files).await;
            progress.lock().unwrap().drift = Some(r);
        },
        async {
            let r = lenses::run_intent(registry, &req.route_key, req.lang, req.pr, &files).await;
            progress.lock().unwrap().intent = Some(r);
        }
    );
    let _ = (sec, drift, intent);
    let snapshot = progress.lock().unwrap().clone();
    finalize(req, &snapshot, true)
}

/// 把（可能不完整的）三轴结果收敛成报告素材。`completed=false` → 永不生成 `summary`。
pub fn snapshot_finalize(req: &TriageRequest<'_>, progress: &Progress, completed: bool) -> TriageOutcome {
    finalize(req, progress, completed)
}

fn finalize(req: &TriageRequest<'_>, p: &Progress, completed: bool) -> TriageOutcome {
    let changed = changed_files(req.changes);
    let map = p.map.clone().unwrap_or_else(|| standards::build(req.repo, &changed.iter().cloned().collect::<Vec<_>>()));
    let zh = req.lang.value.starts_with("zh");

    let mut axes = Vec::new();
    let mut reasons = Vec::new();
    let mut usage = (0u64, 0u64, 0u64);

    let sec_ok = p.security.as_ref().is_some_and(|a| a.ok);
    let drift_ok = p.drift.as_ref().is_some_and(|a| a.ok);
    let intent_ok = p.intent.as_ref().is_some_and(|a| a.ok);
    let all_ok = completed && sec_ok && drift_ok && intent_ok;

    if let Some(a) = &p.security {
        usage.0 += a.usage.0;
        usage.1 += a.usage.1;
        usage.2 += a.usage.2;
        axes.push(axis_json("security", a.ok, a.rows.len(), a.cells, a.error.as_deref(), a.usage, &a.repair_notes, Value::Null, None));
        if !a.ok {
            reasons.push(JobFailure::new("triage_axis_failed", Some("security".into())));
        }
    } else {
        if completed {
            reasons.push(JobFailure::new("triage_axis_failed", Some("security".into())));
        }
        axes.push(axis_json("security", false, 0, 0, Some("interrupted"), (0, 0, 0), &[], Value::Null, None));
    }
    if let Some(a) = &p.drift {
        usage.0 += a.usage.0;
        usage.1 += a.usage.1;
        usage.2 += a.usage.2;
        let standards = json!({
            "modules": map.modules.iter().map(|m| json!({
                "module": m.module,
                "source": m.source.map(|s| s.as_str()),
                "doc": m.doc,
                "linters": m.linters,
                "peers": m.peers,
            })).collect::<Vec<_>>(),
            "no_standard": map.no_standard,
        });
        axes.push(axis_json("drift", a.ok, a.rows.len(), a.cells, a.error.as_deref(), a.usage, &a.repair_notes, standards, None));
        if !a.ok {
            reasons.push(JobFailure::new("triage_axis_failed", Some("drift".into())));
        }
    } else {
        if completed {
            reasons.push(JobFailure::new("triage_axis_failed", Some("drift".into())));
        }
        axes.push(axis_json("drift", false, 0, 0, Some("interrupted"), (0, 0, 0), &[], Value::Null, None));
    }
    if let Some(a) = &p.intent {
        usage.0 += a.usage.0;
        usage.1 += a.usage.1;
        usage.2 += a.usage.2;
        let notes = a.rows.iter().flat_map(|r| r.notes.clone()).collect::<Vec<_>>();
        axes.push(axis_json("intent", a.ok, a.rows.len(), a.cells, a.error.as_deref(), a.usage, &a.repair_notes, Value::Null, Some(notes)));
        if !a.ok {
            reasons.push(JobFailure::new("triage_axis_failed", Some("intent".into())));
        }
    } else {
        if completed {
            reasons.push(JobFailure::new("triage_axis_failed", Some("intent".into())));
        }
        axes.push(axis_json("intent", false, 0, 0, Some("interrupted"), (0, 0, 0), &[], Value::Null, None));
    }

    // 已完成的轴照常参与 findings/blockers；缺失轴按空处理（但 summary 不会生成）。
    let empty_sec: Vec<SecurityRow> = Vec::new();
    let empty_drift: Vec<DriftRow> = Vec::new();
    let empty_intent = IntentOutput::default();
    let sec_rows = p.security.as_ref().map(|a| a.rows.as_slice()).unwrap_or(&empty_sec);
    let drift_rows = p.drift.as_ref().map(|a| a.rows.as_slice()).unwrap_or(&empty_drift);
    let intent_out = p.intent.as_ref().and_then(|a| a.rows.first()).unwrap_or(&empty_intent);
    let prepared = tally::prepare(req.repo, req.pr, &changed, sec_rows, drift_rows, Some(intent_out));
    let tallied = tally::tally(&prepared, &map, req.manifest_cap_s, zh);

    // 丢弃计数**按轴归属**（某个轴丢弃不得挂到其它轴）；每轴附 ≤20 条诊断样本。
    for axis in &mut axes {
        let name = axis.get("axis").and_then(|v| v.as_str()).unwrap_or("");
        let d = match name {
            "security" => &prepared.drops.security,
            "drift" => &prepared.drops.drift,
            _ => &prepared.drops.intent,
        };
        if let Some(o) = axis.as_object_mut() {
            o.insert("dropped".into(), serde_json::to_value(d).unwrap_or(Value::Null));
        }
    }

    let mut findings = tallied.findings;
    for f in &mut findings {
        f.models = vec![req.route_key.clone()];
    }
    let blockers = tallied.blockers;
    TriageOutcome {
        completed,
        summary: if all_ok { tallied.summary } else { None },
        findings,
        blockers,
        axes,
        usage,
        reasons,
        route_key: req.route_key.clone(),
    }
}

#[allow(clippy::too_many_arguments)]
fn axis_json(
    axis: &str,
    ok: bool,
    rows: usize,
    cells: u32,
    error: Option<&str>,
    usage: (u64, u64, u64),
    repair_notes: &[String],
    extra: Value,
    notes: Option<Vec<String>>,
) -> Value {
    let mut v = json!({
        "axis": axis,
        "ok": ok,
        "rows": rows,
        "cells": cells,
        "usage": { "input_tokens": usage.0, "output_tokens": usage.1, "cached_input_tokens": usage.2 },
    });
    if let Some(e) = error {
        v["error"] = json!(e);
    }
    if !repair_notes.is_empty() {
        v["repair_notes"] = json!(repair_notes);
    }
    if !extra.is_null() {
        if let Some(o) = v.as_object_mut() {
            if let Some(extra_obj) = extra.as_object() {
                for (k, val) in extra_obj {
                    o.insert(k.clone(), val.clone());
                }
            }
        }
    }
    if let Some(n) = notes {
        if !n.is_empty() {
            v["notes"] = json!(n);
        }
    }
    v
}

/// 附录 D：`template_hashes.triage` = 三轴模板原文（不含槽值）的确定性哈希。
pub fn template_hash() -> String {
    crate::hash::fnv1a64_hex(&[
        lenses::SYSTEM_TPL,
        lenses::SECURITY_TPL,
        lenses::DRIFT_TPL,
        lenses::INTENT_TPL,
    ])
}

/// 三轴使用的模型：`roles.scanner` 的第一条（确定性顺序 = 配置的 BTreeMap 顺序）。
pub fn scanner_route(cfg: &crate::config::Config) -> Option<crate::config::EffectiveModel> {
    cfg.active_roles().scanners.into_iter().next()
}

/// 轻量校验：三轴都要 scanner 角色（P2）；缺 → `missing_role`。
pub fn ensure_roles(cfg: &crate::config::Config) -> Result<(), JobFailure> {
    if cfg.active_roles().scanners.is_empty() {
        return Err(JobFailure::new("missing_role", Some(Role::Scanner.as_str().to_string())));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_files_ignore_empty_diffs() {
        let changes = vec![
            FileChange { path: "a.rs".into(), diff: "diff --git a/a.rs b/a.rs\n+x\n".into() },
            FileChange { path: "b.bin".into(), diff: String::new() },
        ];
        assert_eq!(changed_files(&changes).len(), 1);
        assert_eq!(patch_pairs(&changes).len(), 1);
    }
}
