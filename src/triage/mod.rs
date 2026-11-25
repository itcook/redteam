//! 3.1–3.7 triage 编排：输入 → StandardsMap → 三轴并行 → 锚定/去重/tally → 报告素材。
//!
//! 取消（SIGTERM / 墙钟）由调用方 `select!` 控制：已完成的轴通过 [`Progress`] 交给
//! [`snapshot_finalize`] 收敛成 `partial`（不丢已完成成果；`summary` 只在三轴全成时生成）。

pub mod budget;
pub mod input;
pub mod lenses;
pub mod shot;
pub mod standards;
pub mod tally;

use crate::config::OutputLang;
use crate::domain::Finding;
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
    /// 整轮预算包线（秒，2026-11-25 用户批准 B2）：只收紧 deadline，不改变 manifest 硬上限。
    pub estimate_s: u64,
    /// 绝对截止时刻（manifest 墙钟）：I0 与波次预算都从这里扣除已用时间。
    pub deadline: std::time::Instant,
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
    /// 本次实际采用的整轮预算包线（秒）；写入 `extensions.redteam.budget.estimate_s`。
    pub estimate_s: u64,
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

/// 整轮预算的规模口径：PR 文本（`title + body + linked issues` 的 title/body）与
/// **非空 diff** 的字节/文件数；空 diff 文件不计（与 [`patch_pairs`] 一致）。
pub fn total_estimate(pr: &PrContext, changes: &[FileChange], manifest_cap_s: u64) -> u64 {
    let files = patch_pairs(changes);
    let diff_kb = budget::kb(files.iter().map(|(_, d)| d.len()).sum());
    let text_bytes = pr.title.len()
        + pr.body.len()
        + pr.linked_issues
            .iter()
            .map(|i| i.title.len() + i.body.as_deref().unwrap_or("").len())
            .sum::<usize>();
    budget::total_estimate_s(diff_kb, files.len(), budget::kb(text_bytes), manifest_cap_s)
}

/// 跑三轴（并发，只受 provider 门约束）。`progress` 在每轴完成时更新，取消后仍可取快照。
/// 两阶段 intent：先串行 I0（claims + overlap），再按 diff 切片 map（兑现 + scope creep）。
/// I0 失败 → intent 快速失败，不占波次门位；slice 未全成 → verification_complete=false。
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
        // 空 patch：没有可审计的注入/漂移面；intent 仍判"声明是否兑现"（没有 diff = 无交付证据）。
        let mut p = progress.lock().unwrap();
        p.security = Some(lenses::AxisRun { ok: true, ..Default::default() });
        p.drift = Some(lenses::AxisRun { ok: true, ..Default::default() });
    }

    // ── I0：串行、小输入（不喂 diff）；预算按文本规模（60–180s），并留 20% 全局余量 ──
    let remaining = req.deadline.saturating_duration_since(std::time::Instant::now());
    let budget_factor = budget::time_factor(req.manifest_cap_s);
    let i0_text_kb = budget::kb(
        req.pr.body.len()
            + req.pr.linked_issues.iter().map(|i| i.body.as_deref().unwrap_or("").len()).sum::<usize>()
            + req.pr.comments.iter().filter(|c| !c.is_bot && c.author != req.pr.author).map(|c| c.body.len()).sum::<usize>(),
    );
    let i0_budget = budget::scaled_i0_timeout(i0_text_kb, budget_factor).min(remaining.mul_f64(0.8));
    let i0_start = std::time::Instant::now();
    let claims_run = lenses::run_intent_claims(registry, &req.route_key, req.lang, req.pr, i0_budget).await;
    let i0_elapsed = i0_start.elapsed().as_millis() as u64;

    // 规模决定分片数：min(4, ceil(total_kb/45), ceil(files/6))；drift 再受 modules 限制。
    let total_kb = budget::kb(files.iter().map(|(_, d)| d.len()).sum());
    let n_sec = budget::shard_count(total_kb, files.len());
    let modules: std::collections::BTreeSet<String> =
        files.iter().map(|(p, _)| standards::module_of(p)).collect();
    let n_drift = n_sec.min(modules.len().max(1));
    let provider_cap = registry
        .provider_of(&req.route_key)
        .and_then(|p| registry.cap_of(p))
        .unwrap_or(4) as usize;

    let claims = claims_run.rows.first().cloned().unwrap_or_default();
    if claims_run.ok {
        let (intent, sec, drift) = tokio::join!(
            // 先 poll intent slices：它们只有少量 cell，先拿门位；不再被分片挤到最后一波。
            async {
                let r = lenses::run_intent_slices(
                    registry,
                    &req.route_key,
                    req.lang,
                    req.pr,
                    &claims.claims,
                    &files,
                    n_sec,
                    (provider_cap / 2).max(1),
                    req.deadline,
                    budget_factor,
                )
                .await;
                let assembled = assemble_intent(&claims, &r);
                let mut out = lenses::AxisRun {
                    ok: r.ok,
                    error: r.error,
                    usage: (claims_run.usage.0 + r.usage.0, claims_run.usage.1 + r.usage.1, claims_run.usage.2 + r.usage.2),
                    cells: claims_run.cells + r.cells,
                    elapsed_ms: i0_elapsed + r.elapsed_ms,
                    ..Default::default()
                };
                out.repair_notes.extend(claims_run.repair_notes.iter().cloned());
                out.repair_notes.extend(r.repair_notes.iter().cloned());
                // 即使 slice 未全成，也保留已抽出的 claims/overlap 与已交付证据（partial 可披露）。
                out.rows.push(assembled);
                progress.lock().unwrap().intent = Some(out);
            },
            async {
                if files.is_empty() {
                    return;
                }
                let r = lenses::run_security(registry, &req.route_key, req.lang, req.pr, &files, n_sec, req.deadline, budget_factor).await;
                progress.lock().unwrap().security = Some(r);
            },
            async {
                if files.is_empty() {
                    return;
                }
                let r = lenses::run_drift(registry, &req.route_key, req.lang, &map, req.repo, &files, n_drift, req.deadline, budget_factor).await;
                progress.lock().unwrap().drift = Some(r);
            }
        );
        let _ = (intent, sec, drift);
    } else {
        // I0 失败：intent 轴直接失败（不占波次门位），security/drift 照常跑。
        {
            let mut p = progress.lock().unwrap();
            p.intent = Some(lenses::AxisRun {
                ok: false,
                error: claims_run.error.clone().or_else(|| Some("intent claims extraction failed".into())),
                usage: claims_run.usage,
                cells: claims_run.cells,
                elapsed_ms: claims_run.elapsed_ms,
                ..Default::default()
            });
        }
        let (sec, drift) = tokio::join!(
            async {
                if files.is_empty() {
                    return;
                }
                let r = lenses::run_security(registry, &req.route_key, req.lang, req.pr, &files, n_sec, req.deadline, budget_factor).await;
                progress.lock().unwrap().security = Some(r);
            },
            async {
                if files.is_empty() {
                    return;
                }
                let r = lenses::run_drift(registry, &req.route_key, req.lang, &map, req.repo, &files, n_drift, req.deadline, budget_factor).await;
                progress.lock().unwrap().drift = Some(r);
            }
        );
        let _ = (sec, drift);
    }

    let snapshot = progress.lock().unwrap().clone();
    finalize(req, &snapshot, true)
}

/// 把 I0 的 claims/overlap + 各 slice 的 delivered/scope creep 合成一个 IntentOutput。
fn assemble_intent(claims_output: &lenses::IntentClaimsOutput, slices: &lenses::AxisRun<lenses::IntentSliceOutput>) -> lenses::IntentOutput {
    let mut claims = claims_output.claims.clone();
    let idx: std::collections::BTreeMap<String, usize> = claims
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.clone(), i))
        .collect();
    let mut scope: Vec<lenses::ScopeCreep> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = Default::default();
    let mut notes = claims_output.notes.clone();
    for slice in &slices.rows {
        for d in &slice.delivered {
            if let Some(&i) = idx.get(&d.id) {
                claims[i].delivered = true;
                claims[i].evidence.extend(d.evidence.clone());
                if !d.note.trim().is_empty() {
                    if claims[i].note.is_empty() {
                        claims[i].note = d.note.trim().to_string();
                    } else {
                        claims[i].note.push_str("; ");
                        claims[i].note.push_str(d.note.trim());
                    }
                }
            }
            // 未知 id 忽略（模型幻觉；不猜）。
        }
        for s in &slice.scope_creep {
            let key = s.title.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
            if seen.insert(key) {
                scope.push(s.clone());
            }
        }
        notes.extend(slice.notes.clone());
    }
    lenses::IntentOutput {
        claims,
        scope_creep: scope,
        maintainer_overlap: claims_output.maintainer_overlap.clone(),
        notes,
        verification_complete: slices.ok,
    }
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
        axes.push(axis_json("security", a.ok, a.rows.len(), a.cells, a.elapsed_ms, a.error.as_deref(), a.usage, &a.repair_notes, Value::Null, None));
        if !a.ok {
            reasons.push(JobFailure::new("triage_axis_failed", Some("security".into())));
        }
    } else {
        if completed {
            reasons.push(JobFailure::new("triage_axis_failed", Some("security".into())));
        }
        axes.push(axis_json("security", false, 0, 0, 0, Some("interrupted"), (0, 0, 0), &[], Value::Null, None));
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
        axes.push(axis_json("drift", a.ok, a.rows.len(), a.cells, a.elapsed_ms, a.error.as_deref(), a.usage, &a.repair_notes, standards, None));
        if !a.ok {
            reasons.push(JobFailure::new("triage_axis_failed", Some("drift".into())));
        }
    } else {
        if completed {
            reasons.push(JobFailure::new("triage_axis_failed", Some("drift".into())));
        }
        axes.push(axis_json("drift", false, 0, 0, 0, Some("interrupted"), (0, 0, 0), &[], Value::Null, None));
    }
    if let Some(a) = &p.intent {
        usage.0 += a.usage.0;
        usage.1 += a.usage.1;
        usage.2 += a.usage.2;
        let notes = a.rows.iter().flat_map(|r| r.notes.clone()).collect::<Vec<_>>();
        let disclosure = a.rows.first().map(|r| json!({
            "claims": r.claims.len(),
            "delivered": r.claims.iter().filter(|c| c.delivered).count(),
            "scope_creep": r.scope_creep.len(),
            "verification_complete": r.verification_complete,
        })).unwrap_or(Value::Null);
        axes.push(axis_json("intent", a.ok, a.rows.len(), a.cells, a.elapsed_ms, a.error.as_deref(), a.usage, &a.repair_notes, disclosure, Some(notes)));
        if !a.ok {
            reasons.push(JobFailure::new("triage_axis_failed", Some("intent".into())));
        }
    } else {
        if completed {
            reasons.push(JobFailure::new("triage_axis_failed", Some("intent".into())));
        }
        axes.push(axis_json("intent", false, 0, 0, 0, Some("interrupted"), (0, 0, 0), &[], Value::Null, None));
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
        estimate_s: req.estimate_s,
    }
}

#[allow(clippy::too_many_arguments)]
fn axis_json(
    axis: &str,
    ok: bool,
    rows: usize,
    cells: u32,
    elapsed_ms: u64,
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
        "elapsed_ms": elapsed_ms,
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
        lenses::INTENT_CLAIMS_TPL,
        lenses::INTENT_SLICE_TPL,
    ])
}

/// 三轴使用的模型：`roles.reviewer` 的第一条（2026-11-24 用户调整：triage 用 reviewer 的思考档位，
/// 不再用 scanner；确定性顺序 = 配置的 BTreeMap 顺序）。
pub fn triage_route(cfg: &crate::config::Config) -> Option<crate::config::EffectiveModel> {
    cfg.active_roles().reviewers.into_iter().next()
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

    #[test]
    fn total_estimate_counts_title_issues_and_non_empty_diffs() {
        // text = title(1) + body(5) + issue title(1) + issue body(50KB)；空 diff 文件不计。
        let issue_body = "i".repeat(50 * 1024);
        let json = format!(
            r#"{{"schema":"hibot.audit.pr-context/1","title":"t","body":"hello","author":"a","labels":[],"comments":[],"linked_issues":[{{"number":1,"title":"i","state":"open","body":"{issue_body}"}}],"ci":{{"state":"success","failing":[]}},"changed_files":[{{"path":"a.rs"}}]}}"#
        );
        let pr = input::parse(&json).unwrap();
        let big = format!("diff --git a/a.rs b/a.rs\n{}", "x".repeat(100 * 1024));
        let changes = vec![
            FileChange { path: "a.rs".into(), diff: big.clone() },
            FileChange { path: "b.bin".into(), diff: "  \n".into() },
        ];
        // 220 + 7.5×100.0KB + 7.5×1 file + 2.5×50.0KB ≈ 1103s；
        // 漏掉 title/linked issue 或把空 diff 计入都会改变这个值。
        assert_eq!(total_estimate(&pr, &changes, 1800), 1103);
    }

    #[test]
    fn total_estimate_never_exceeds_manifest() {
        let pr = input::parse(
            r#"{"schema":"hibot.audit.pr-context/1","title":"t","body":"b","author":"a","labels":[],"comments":[],"linked_issues":[],"ci":{"state":"success","failing":[]},"changed_files":[]}"#,
        )
        .unwrap();
        assert_eq!(total_estimate(&pr, &[], 600), 600, "manifest 小于下限");
        assert_eq!(total_estimate(&pr, &[], 1800), 900, "空 diff 取包线下限");
    }

}
