//! 尾段引擎（M17-a 尾段统一）：exit_code 映射、驳回标记、model id 映射、
//! CarryCtx/TailCtx 携带态与共用尾段 `assemble_report`（anchor → dedup → verify → verdict → 组报告）。

use crate::config::Config;
use crate::consolidate::{dedup, refute_targets, verify_findings};
use crate::domain::{DossierMeta, Finding, ReviewReport, RunMeta, Severity, Stats, Usage, Verdict};
use crate::orchestrate::CellSink;
use crate::positioning::anchor;
use crate::provider::ReviewProvider;
use crate::verdict::compute_verdict;
use std::path::Path;

/// 退出码映射（统一 run_review/run_code_review，五档契约·2026-07-27 定稿）：
/// reject→10、Verdict::Incomplete（部分结果，见 partial_report/run_resume 的 still_missing
/// 分支——真实判定来源是 domain::Verdict 本身而非另一标志位）→3、approve 但对照集不完整
/// （failed_models 非空，模型半失败降级）→2、干净 approve→0。命令自身失败（参数/IO/配置错误）
/// 不经此函数，走 main.rs 的 exit(1)。
pub fn exit_code(verdict: Verdict, failed_models_nonempty: bool) -> i32 {
    match (verdict, failed_models_nonempty) {
        (Verdict::Reject, _) => 10,
        (Verdict::Approve, true) => 2,
        (Verdict::Approve, false) => 0,
        (Verdict::Incomplete, _) => 3,
    }
}

/// R8.2：命中标记 + 派生索引（排序去重）。单点应用于共用尾段，两路径（code/resume）同享。
pub(crate) fn mark_dismissed(
    findings: &mut [Finding],
    dismissed: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut reappeared = Vec::new();
    if dismissed.is_empty() {
        return reappeared;
    }
    for f in findings.iter_mut() {
        if dismissed.contains(&f.id) {
            f.previously_dismissed = true;
            reappeared.push(f.id.clone());
        }
    }
    reappeared.sort();
    reappeared.dedup();
    reappeared
}

/// provider 名 → model id 映射（人读报告显示 model 名，rN.md 模板 2026-07-03）。
pub(crate) fn model_ids_for(names: &[String], cfg: &Config) -> Vec<String> {
    names
        .iter()
        .map(|n| cfg.providers.get(n).map(|p| p.model.clone()).unwrap_or_else(|| n.clone()))
        .collect()
}

/// Task 3 消费：增量轮携带态——上轮合并集（carry() 已重锚/排除 resolved）+ resolved 台账
/// id 集（dispute 检测用）+ 审计三件套（resolved_applied/carried_dropped/delta_base）。
/// `carry=None`（`TailCtx.carry`）时 assemble_report 行为与本任务之前逐字一致；`Some` 时
/// 在 dedup 后插入合并/dispute 检测，组装期回填 Stats.new + RunMeta 五字段。
pub(crate) struct CarryCtx {
    /// 上轮携带集（已含 origin=Carried、行号已就近重锚）。
    pub(crate) carried: Vec<Finding>,
    /// resolve 台账 id 集——本轮新 finding 精确命中即 resolution_disputed（假修复回路）。
    pub(crate) resolved_ids: std::collections::HashSet<String>,
    /// 命中 resolved_ids 被排除携带的 id（进 RunMeta.resolved，审计用）。
    pub(crate) resolved_applied: Vec<String>,
    /// 重锚失败被消灭的携带 id（进 RunMeta.carried_dropped，审计用）。
    pub(crate) carried_dropped: Vec<String>,
    /// R3 基线 HEAD（进 RunMeta.delta_base）。
    pub(crate) delta_base: String,
}

/// 扇出之后的共用尾段上下文——`run_code_review`/`run_resume` 两处扇出前逻辑各不相同
/// （单格×model / 直接重放 manifest prompt），但扇出后（anchor → dedup → verify → verdict
/// → 组报告）完全一致，抽出复用（DRY，T7 授权解冻）。
pub(crate) struct TailCtx<'a> {
    /// anchor/verify_findings 用的项目根。
    pub(crate) root: &'a Path,
    pub(crate) provider: &'a dyn ReviewProvider,
    pub(crate) model_names: Vec<String>,
    /// 与 model_names 同序的 model id（cfg.providers.*.model；查无则回退 provider 名）。
    pub(crate) model_ids: Vec<String>,
    pub(crate) block: Severity,
    pub(crate) floor: f32,
    pub(crate) max_concurrency: usize,
    pub(crate) job_id: String,
    pub(crate) duration_ms: u64,
    pub(crate) usage: (u64, u64, u64),
    pub(crate) warnings: Vec<String>,
    pub(crate) failed_models: Vec<String>,
    pub(crate) dismissed: &'a std::collections::HashSet<String>,
    /// 活跃判例卡——refute 判例注入 + 机械预筛（design 2026-07-03 #3）。
    pub(crate) precedents: &'a [crate::db::DismissCard],
    /// 台账加载披露（进 RunMeta.ledger）。
    pub(crate) ledger_meta: Option<crate::domain::LedgerMeta>,
    /// R3 stamp：证据收集刻的 HEAD（code dispatch 回填；resume 为 None）。
    pub(crate) head_sha: Option<String>,
    /// 卷宗覆盖 meta（T01⑥；进 RunMeta.dossier）。code fan-out 路径为 Some，resume 为 None。
    pub(crate) dossier_meta: Option<DossierMeta>,
    /// 二阶段补查触发观测（进 RunMeta.needs，恒写）：(触发格数, 声明条数)；resume 无 fan-out 恒零。
    pub(crate) needs_stats: (u32, u32),
    /// 后台 job 的 phase 打点通路（refute 子阶段；前台/resume 为 None）。
    pub(crate) sink: Option<&'a dyn CellSink>,
    /// --spec 参照段（Task 4，承 hiapi F2）——透传给 refute 趟；空 --spec=""（行为不变）。
    pub(crate) spec_docs: &'a str,
    /// Task 2（尾段引擎泛化）：增量携带态。None=既有全量/resume 路径逐字不变；
    /// Some=Task 3 消费，assemble_report 内 merge_carry + 回填 RunMeta 五字段。
    pub(crate) carry: Option<CarryCtx>,
}

/// 共用尾段：anchor → dedup → verify → verdict → 组装 report → 退出码。
/// 两处调用方（run_code_review/run_resume）行为 1:1 迁移，不改变既有语义。
pub(crate) async fn assemble_report(findings: Vec<Finding>, mut ctx: TailCtx<'_>) -> anyhow::Result<(ReviewReport, i32)> {
    let anchored = anchor(findings, ctx.root);
    let findings = dedup(anchored);
    // Task 2 插入点（brief 2.3 逐字）：carry=None → 现状路径逐字不变；carry=Some → dispute
    // 检测（精确 id 命中 resolved 台账，须在 merge 前对纯 new 集合判，避免携带项误判）→
    // merge_carry 吸收携带集，refute/mark_dismissed/verdict 作用于合并后的 findings。
    let carry = ctx.carry.take();
    let (findings, resolution_disputed, carry_meta) = match carry {
        Some(c) => {
            let mut disputed: Vec<String> = findings.iter()
                .filter(|f| c.resolved_ids.contains(&f.id))
                .map(|f| f.id.clone()).collect();
            disputed.sort(); disputed.dedup();
            let merged = crate::consolidate::merge_carry(c.carried, findings);
            (merged, disputed, Some((c.resolved_applied, c.carried_dropped, c.delta_base)))
        }
        None => (findings, vec![], None),
    };
    let clusters = vec![]; // correlate 已随 doc 模式退场（E10）；clusters 字段留空保持 schema 稳定。
    // refute 子阶段打点（targets 数与 verify_findings 同一真相源函数，非重复实现）。
    let n_targets = refute_targets(&findings, ctx.precedents, ctx.block).len();
    if n_targets > 0 {
        if let Some(s) = ctx.sink {
            s.on_phase(crate::state::Phase::Refute, Some(format!("{n_targets} targets")));
        }
    }
    let (findings, refute_warnings, refute_usage) = verify_findings(
        findings, ctx.provider, &ctx.model_names, ctx.block, ctx.max_concurrency,
        ctx.root, ctx.precedents, ctx.spec_docs,
    ).await;
    let mut run_warnings = ctx.warnings;
    run_warnings.extend(refute_warnings);
    // T03：refute 趟 token 曾被丢弃，从未入过 run.usage 的账——折进总账，诚实反映全流程成本。
    ctx.usage.0 += refute_usage.0;
    ctx.usage.1 += refute_usage.1;
    ctx.usage.2 += refute_usage.2;

    let mut findings = findings;
    let dismissed_reappeared = mark_dismissed(&mut findings, ctx.dismissed);

    let verdict = compute_verdict(&findings, ctx.block, ctx.floor);
    let mut stats = Stats::from_findings(&findings);
    // carry_meta 为 Some 时填 Stats.new（origin==New 子账，手法参照 run_incremental_review
    // 手抄段 cli.rs:938-939）；None 时沿用现状（无 stats.new 子块）。
    if carry_meta.is_some() {
        let new_only: Vec<Finding> = findings.iter()
            .filter(|f| f.origin == crate::domain::Origin::New)
            .cloned().collect();
        stats.new = Some(Box::new(Stats::from_findings(&new_only)));
    }
    let (incremental, delta_base, resolved, carried_dropped) = match carry_meta {
        Some((resolved_applied, dropped, base)) => (true, Some(base), resolved_applied, dropped),
        None => (false, None, vec![], vec![]),
    };
    let report = ReviewReport {
        schema_version: "3".into(),
        verdict,
        stats,
        findings,
        clusters,
        run: RunMeta {
            job_id: ctx.job_id,
            models: ctx.model_names,
            model_ids: ctx.model_ids,
            duration_ms: ctx.duration_ms,
            usage: Usage {
                input_tokens: ctx.usage.0, output_tokens: ctx.usage.1, cached_input_tokens: ctx.usage.2,
                refute: if refute_usage == (0, 0, 0) { None } else {
                    Some(Box::new(Usage {
                        input_tokens: refute_usage.0, output_tokens: refute_usage.1,
                        cached_input_tokens: refute_usage.2, refute: None,
                    }))
                },
            },
            warnings: run_warnings,
            failed_models: ctx.failed_models,
            dismissed_reappeared,
            ledger: ctx.ledger_meta,
            head_sha: ctx.head_sha,
            dossier: ctx.dossier_meta,
            needs: crate::domain::NeedsMeta {
                triggered_cells: ctx.needs_stats.0, declared: ctx.needs_stats.1,
            },
            incremental,
            delta_base,
            resolved,
            resolution_disputed,
            carried_dropped,
        },
    };
    // 退出码：reject 优先 → 10；否则 approve 但对照集不完整 → 2；干净 approve → 0。
    let code = exit_code(verdict, !report.run.failed_models.is_empty());
    Ok((report, code))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Origin;
    use crate::provider::fake::FakeProvider;

    #[test]
    fn exit_code_maps_all_cases() {
        use crate::domain::Verdict;
        assert_eq!(exit_code(Verdict::Reject, false), 10);
        assert_eq!(exit_code(Verdict::Reject, true), 10);   // reject 优先
        assert_eq!(exit_code(Verdict::Approve, true), 2);   // approve 但对照集不完整（降级 approve）
        assert_eq!(exit_code(Verdict::Approve, false), 0);  // 干净 approve
        assert_eq!(exit_code(Verdict::Incomplete, false), 3); // 不完整报告，与降级 approve 分离
        assert_eq!(exit_code(Verdict::Incomplete, true), 3);  // failed_models 与否不改变不完整判定
    }

    #[test]
    fn dismissed_survives_line_shift_and_is_marked_exempt() {
        use crate::orchestrate::enrich;
        use std::collections::HashSet;
        let raw = |ls: u32, le: u32| crate::domain::RawFinding {
            severity: Severity::High, title: "Weak hash".into(), body: "b".into(),
            file: "src/x.rs".into(), line_start: ls, line_end: le, confidence: 0.9,
            recommendation: "r".into(), hard_constraint: false,
            quote: "md5(pw)".into(),
        };
        let before = enrich(&raw(5, 7), "m");          // 第一轮：驳回它
        let dismissed: HashSet<String> = [before.id.clone()].into();
        let mut after = vec![enrich(&raw(50, 52), "m")]; // 修复位移后再现
        let reappeared = mark_dismissed(&mut after, &dismissed);
        assert_eq!(reappeared, vec![before.id]);
        assert!(after[0].previously_dismissed);
        assert_eq!(compute_verdict(&after, Severity::Medium, 0.5), Verdict::Approve);
    }

    // Task 2（尾段引擎泛化）：CarryCtx 数据扩展 + assemble_report 插入点。
    // 下面两条直构 TailCtx，FakeProvider 空脚本——findings 严格低于 block 阈值，
    // refute_targets 恒空，verify_findings 短路不发起 provider 调用。

    fn tailctx_fixture<'a>(
        root: &'a Path,
        provider: &'a dyn ReviewProvider,
        dismissed: &'a std::collections::HashSet<String>,
        carry: Option<CarryCtx>,
    ) -> TailCtx<'a> {
        TailCtx {
            root,
            provider,
            model_names: vec!["m".into()],
            model_ids: vec!["m1".into()],
            block: Severity::High,
            floor: 0.5,
            max_concurrency: 1,
            job_id: String::new(),
            duration_ms: 0,
            usage: (0, 0, 0),
            warnings: vec![],
            failed_models: vec![],
            dismissed,
            precedents: &[],
            ledger_meta: None,
            head_sha: None,
            dossier_meta: None,
            needs_stats: (0, 0),
            sink: None,
            spec_docs: "",
            carry,
        }
    }

    #[tokio::test]
    async fn assemble_report_carry_none_unchanged() {
        // 现状锚：carry=None 时 assemble_report 行为必须与本任务之前逐字一致——
        // 报告无 stats.new 子块、RunMeta.incremental=false、delta_base=None。
        use crate::orchestrate::enrich;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn f() {\n    minor_issue();\n}\n").unwrap();
        let finding = enrich(&crate::domain::RawFinding {
            severity: Severity::Low, title: "minor".into(), body: "b".into(), file: "a.rs".into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: "minor_issue();".into(),
        }, "m");
        let dismissed = std::collections::HashSet::new();
        let provider = FakeProvider { scripted: Default::default(), ..Default::default() };
        let ctx = tailctx_fixture(dir.path(), &provider, &dismissed, None);
        let (report, _code) = assemble_report(vec![finding], ctx).await.unwrap();
        assert!(report.stats.new.is_none(), "carry=None 不应产出 stats.new 子块");
        assert!(!report.run.incremental, "carry=None → incremental 恒 false");
        assert_eq!(report.run.delta_base, None, "carry=None → delta_base 恒 None");
        assert_eq!(report.run.resolved, Vec::<String>::new());
        assert_eq!(report.run.resolution_disputed, Vec::<String>::new());
        assert_eq!(report.run.carried_dropped, Vec::<String>::new());
    }

    #[tokio::test]
    async fn assemble_report_carry_some_merges_and_fills_meta() {
        // carry=Some：carried 1 条（origin=Carried，不经 anchor）+ resolved_ids 命中本轮一条
        // 再现的 new id → 合并集含 carried、resolution_disputed 命中、Stats.new 只计 New，
        // RunMeta 五字段逐一断言。
        use crate::orchestrate::enrich;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "fn f() {\n    reraised_bug();\n    brand_new_bug();\n}\n",
        ).unwrap();
        let raw = |quote: &str, title: &str| crate::domain::RawFinding {
            severity: Severity::Low, title: title.into(), body: "b".into(), file: "a.rs".into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: quote.into(),
        };
        // 本轮再报一条已"销案"的 id（假修复回路）+ 一条真新 finding。
        let reraised = enrich(&raw("reraised_bug();", "weak hash"), "m");
        let fresh = enrich(&raw("brand_new_bug();", "secret leak"), "m");
        let disputed_id = reraised.id.clone();

        let mut carried_finding = enrich(&crate::domain::RawFinding {
            severity: Severity::Medium, title: "carried issue".into(), body: "b".into(),
            file: "carried.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), hard_constraint: false, quote: "old_bug();".into(),
        }, "m");
        carried_finding.origin = Origin::Carried;

        let mut resolved_ids = std::collections::HashSet::new();
        resolved_ids.insert(disputed_id.clone());
        let carry = CarryCtx {
            carried: vec![carried_finding],
            resolved_ids,
            resolved_applied: vec!["resolved-x".into()],
            carried_dropped: vec!["dropped-y".into()],
            delta_base: "deadbeef".into(),
        };

        let dismissed = std::collections::HashSet::new();
        let provider = FakeProvider { scripted: Default::default(), ..Default::default() };
        let ctx = tailctx_fixture(dir.path(), &provider, &dismissed, Some(carry));
        let (report, _code) = assemble_report(vec![reraised, fresh], ctx).await.unwrap();

        assert_eq!(report.findings.len(), 3, "合并集 = carried(1) + new(2)");
        assert!(report.findings.iter().any(|f| f.origin == Origin::Carried), "carried 项在场");
        assert_eq!(report.run.resolution_disputed, vec![disputed_id], "精确 id 命中已销案 → dispute");
        let sn = report.stats.new.as_ref().expect("carry=Some 应产出 stats.new 子块");
        assert_eq!(sn.total, 2, "stats.new 只计 origin==New（2 条）");
        assert_eq!(report.stats.total, 3, "主 stats 维持合并集口径（3 条）");
        assert!(report.run.incremental, "carry=Some → incremental=true");
        assert_eq!(report.run.delta_base.as_deref(), Some("deadbeef"));
        assert_eq!(report.run.resolved, vec!["resolved-x".to_string()]);
        assert_eq!(report.run.carried_dropped, vec!["dropped-y".to_string()]);
    }
}
