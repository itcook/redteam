//! CLI 评审流水线：`ReviewArgs`/台账/pack 预算/model 选取等留守项 + 全量/resume/job
//! 状态机（并发组 M16 定型，原地不动）。尾段引擎/spec/二阶段补查/增量评审已拆至
//! 同目录子模块（M17-a 尾段统一，零语义 diff）。

use crate::bundle::{group, select_files, Bundle, GroupingOpts, GroupingStats};
use crate::config::{Config, RoleSets};
use crate::domain::{CacheHit, CellSummary, DossierMeta, NeedsMeta, ReviewReport, RiskMeta, Role, RunMeta, ScanGroup, ScanReport, ScanStats, Stats, Usage, Verdict};
use crate::evidence::git::FileChange;
use crate::orchestrate::{run_scan_fanout, scan_cells_from_records, CellSink, ScanCellResult, ScanEntrySpec, ScanFanoutInput, ScanResults};
use crate::output::Format;
use crate::provider::registry::ProviderRegistry;
use crate::state::CellRecord;
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) mod deny;
mod incremental;
mod second_pass;
mod spec;
mod tail;

pub use incremental::run_incremental_review;
pub use tail::exit_code;

use spec::{render_spec_section_capped, SPEC_BUDGET_BYTES};
use tail::model_ids_for;

/// scanners 为空时的配置错误文案（`build_registry` 与 `run_code_review` 共用）。
pub const NO_SCANNER_MSG: &str = "未配置 scanner：在 ~/.redteam/config.json（REDTEAM_HOME 可覆盖）给至少一个模型加 roles.scanner，见 `redteam-next model setup --help`";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ReviewArgs {
    pub base: Option<String>,
    pub scope: String,
    pub spec: Vec<String>,
    pub focus: Option<String>,
    pub block_threshold: Option<String>,
    pub conf_floor: Option<f32>,
    // 前台同步路径是默认路径；`--background` 由 main 层解析并写入本字段。
    pub background: bool,
    pub format: String,
    /// R2.3：`--resume <job_id>`。main.rs 据此路由到 `run_resume`（前台同步，忽略 background）；
    /// 其余字段（base/models/…）在 resume 路径下不生效——run_resume 会重新
    /// `read_args_raw(job_id)` 取原始作业的 ReviewArgs。
    pub resume: Option<String>,
    /// `--dismissed <db路径|csv>`：覆盖台账 db 路径或临时 csv 豁免（design 2026-07-03 #6；
    /// 缺省 zero-config 自动加载 .redteam/proj.db）。`serde(default)` 保证旧 args.json 兼容。
    #[serde(default)]
    pub dismissed: Option<String>,
    /// `--no-dismissed`：本次评审完全关闭台账（豁免/预筛/判例注入全停）。
    #[serde(default)]
    pub no_dismissed: bool,
    /// M13：`--incremental-from <job-id>`——以该作业为基线的增量轮。serde default 兼容旧 args.json。
    #[serde(default)]
    pub incremental_from: Option<String>,
    /// `--tools <off|two-stage|on>`：覆盖 config 的工具档位。`serde(default)` 保旧 args.json
    /// resume/背景 job 重建不炸（旧 job 无此字段 → None → 落 config 默认）。
    #[serde(default)]
    pub tools: Option<String>,
    /// `--test-command <cmd>`：ship 循环的测试命令覆盖；缺省由 `project::infer_test_command` 推断。
    /// 记入 `RunMeta.test_command`。serde default 保旧 args.json。
    #[serde(default)]
    pub test_command: Option<String>,
    /// M21a 批 C：`--pack-only`——只收集证据、分组、装包并落盘，不调 provider（D7 B7 预算仪表）。
    /// `serde(default)` 保旧 args.json（resume / 后台 job）兼容。
    #[serde(default)]
    pub pack_only: bool,
}

/// 台账加载产物（design 2026-07-03 #6 zero-config）：豁免 id 集 + 活跃判例卡 + 披露 meta。
#[derive(Default, Debug)]
pub(crate) struct Ledger {
    /// 豁免 id 集 / 活跃判例卡：M21a scan-only 只披露 meta，不消费（M21c 判例前移 + 豁免裁决消费）。
    #[allow(dead_code)]
    pub ids: std::collections::HashSet<String>,
    #[allow(dead_code)]
    pub cards: Vec<crate::db::DismissCard>,
    pub meta: Option<crate::domain::LedgerMeta>,
}

/// 加载台账：--no-dismissed → 空；缺省 → `.redteam/<db::DB_FILE>` 存在才加载（零副作用，不建库）；
/// `--dismissed <值>` → 文件=db 路径覆盖 / 目录=已废弃形态报错引导迁移 / 否则 csv 仅豁免。
pub(crate) fn load_ledger(
    dismissed: Option<&str>,
    no_dismissed: bool,
    cwd: &Path,
) -> anyhow::Result<Ledger> {
    if no_dismissed {
        return Ok(Ledger::default());
    }
    let from_db = |path: &Path, source: String| -> anyhow::Result<Ledger> {
        let db = crate::db::ProjDb::open_at(path)?;
        let cards = db.active_cards()?;
        let ids = db.active_ids()?;
        let loaded = cards.len();
        Ok(Ledger { ids, cards, meta: Some(crate::domain::LedgerMeta { loaded, source }) })
    };
    match dismissed {
        None => {
            let p = cwd.join(crate::project::PROJECT_DIR).join(crate::db::DB_FILE);
            if !p.exists() {
                return Ok(Ledger::default());
            }
            from_db(&p, format!("{}/{}", crate::project::PROJECT_DIR, crate::db::DB_FILE))
        }
        Some(v) => {
            let p = Path::new(v);
            if p.is_dir() {
                anyhow::bail!(
                    "--dismissed: directory ledgers are retired; migrate cards into .redteam/{} \
                     via 'redteam dismiss add' (see 'redteam dismiss --help')", crate::db::DB_FILE
                );
            }
            if p.is_file() {
                return from_db(p, v.to_string());
            }
            let ids = crate::project::parse_dismissed(v)?;
            let loaded = ids.len();
            Ok(Ledger { ids, cards: Vec::new(), meta: Some(crate::domain::LedgerMeta { loaded, source: "csv".into() }) })
        }
    }
}

pub fn parse_format(s: &str) -> Format {
    if s == "md" {
        Format::Md
    } else {
        Format::Json
    }
}

/// pack flat 预算绝对帽（字节）。史：S2-b' 定 256K（装下 cli.rs 级 90KB 大文件，当时模型窗
/// 普遍 128K）；2026-07-28 四 provider 全声明 1M 窗后提 768K——上限锚在 **gpt-5.6 的 272K
/// 计费悬崖**之下（官方条款：input >272K 整单 2x input/1.5x output，见
/// developers.openai.com/api/docs/models/gpt-5.6-sol）：按刻意保守的 3B/tok 记账
/// 768K≈256K tok + 壳预留 8K tok ≈ 264K < 272K；真实换算 3.5-4B/tok 时实际 input 更低
/// （~200-220K），双层安全垫。未声明窗口的缺省档改由窗口墙管（312K，见测试）。
/// T27 完整版（models.dev 策展表+真 tokenizer 校准）仍缓。
const PACK_BUDGET_BYTES: usize = 768_000;
/// 预算函数三常数（design 2026-07-13 §2.2 逐字）：输出预留（推理型模型 findings JSON +
/// reasoning）、字节/token 保守换算（代码英文 ~3.5-4B/token，往小估防超模型墙）、prompt
/// 壳预留（rules/指令/信任声明，pack 预算外但占窗）。
const OUTPUT_RESERVE_TOKENS: u32 = 16_000;
const BYTES_PER_TOKEN: usize = 3;
const SHELL_RESERVE_BYTES: usize = 24_000;

/// per-model pack 预算：min(字节帽, (启用模型最小窗 − 输出预留)×3 − 壳预留)。
/// 双层墙——字节帽是策略、token limit 是硬墙；多模型取最小窗=一份 pack 全模型安全
/// （常数查表，不复活 S2-c per-model 规划器）。未声明窗口 → 保守 128K。
pub(crate) fn pack_budget_bytes(roles: &RoleSets) -> usize {
    let min_ctx = roles
        .scanners
        .iter()
        .map(|m| m.context_window)
        .min()
        .unwrap_or(128_000);
    let by_window = (min_ctx.saturating_sub(OUTPUT_RESERVE_TOKENS) as usize)
        .saturating_mul(BYTES_PER_TOKEN)
        .saturating_sub(SHELL_RESERVE_BYTES);
    PACK_BUDGET_BYTES.min(by_window)
}

/// bundle → 卷宗段（每 bundle 一份，与 bundles 等长/等序）+ 覆盖 meta（T01⑥）。
/// 唯一路径（E8-E 安乐死 SCIP/Dossier 引擎后 + Task 3 空臂 dev 开关整拆）：
/// `build_pack`（0 壳完整体+省略清单，M15-S1）恒执行，无空臂开关。
/// degrades 静默于：不支持语言/无符号（pack 臂内部回落链承接）。
/// meta.injected 是唯一可观测入口，供归因与生产诊断。
/// `budget`=pack flat 预算（字节）；生产调用点由 `pack_budget_bytes` 按启用模型窗算出。
pub(crate) fn build_packs(
    bundles: &[crate::bundle::Bundle],
    root: &std::path::Path,
    budget: usize,
    opts: &crate::pack::PackOpts,
) -> (Vec<String>, DossierMeta, Vec<crate::pack::PackStats>) {
    // M21a 批 C：bundles 来自引用图分组（design §3.2，门未过时退化为单包），每格只装自家文件——
    // 视野外的改动文件由跨组索引（`{{OTHER_CHANGED_FILES}}`）而非兄弟注入提供。超预算由 build_pack
    // 自身的降级梯子（骨架/hunk 窗/省略清单）承接，分组「宁降不拆」不靠分格避让。
    // 全变更集路径：文件外片段搜索排除（它们已在各自的 cell 里，视野外的由跨组索引提供）。
    let changeset: Vec<String> = bundles.iter().flat_map(|b| &b.files).map(|f| f.path.clone()).collect();
    let (entries, stats): (Vec<String>, Vec<crate::pack::PackStats>) = bundles
        .iter()
        .map(|b| {
            // 预算记账扣本格自己的 diff 字节（单格默认下即全变更集真实 diff）。
            let own_diff_bytes: usize = b.files.iter().map(|fc| fc.diff.len()).sum();
            // 批 F-4：直接产裸 pack 正文（`build_scan_prompt` 自己包 <dossier> 与切片形态前言）。
            let packed = crate::pack::pack_bundle(b, root, budget, own_diff_bytes, opts, &changeset);
            (packed.text, packed.stats)
        })
        .unzip();
    let injected = entries.iter().filter(|s| !s.is_empty()).count();
    let meta = DossierMeta { bundles: bundles.len(), injected };
    (entries, meta, stats)
}

/// 三条路径（全量 / 增量 / `--pack-only`）共用的「pack 预算 + spec 参照段」：spec 帽夹紧到 pack 预算一半
/// （dogfood #2：64K 固定帽会吃穿小窗模型的整个输入窗），扣减后的预算给分组与装包。
/// 返回 (budget, spec_section, warnings)。
pub(crate) fn budget_and_spec(roles: &RoleSets, spec: &[String]) -> (usize, String, Vec<String>) {
    let budget_raw = pack_budget_bytes(roles);
    let (spec_section, spec_used, warnings) = render_spec_section_capped(spec, SPEC_BUDGET_BYTES.min(budget_raw / 2));
    (budget_raw.saturating_sub(spec_used), spec_section, warnings)
}

/// cell 规划产物：分组 + 每组裸 pack 正文（`dossiers`）+ 每组跨组索引 + 每组装包形态（四者等长等序）+ 观测。
pub(crate) struct CellPlan {
    pub bundles: Vec<Bundle>,
    pub dossiers: Vec<String>,
    pub others: Vec<String>,
    pub dossier_meta: DossierMeta,
    pub stats: GroupingStats,
    /// 每组装包形态（批 E K13）：`--pack-only` 消费；RunMeta 接线 F-5。
    pub pack_stats: Vec<crate::pack::PackStats>,
}

/// 筛选后证据 → 引用图分组（design §3.2，预算门喂 pack 预算）→ 装包 → 跨组索引（design §10.6）。
/// 全量 / 增量 / `--pack-only` 的唯一入口——预算仪表量的就是真实评审发出的那份 prompt 素材。
pub(crate) fn plan_cells(
    selected: Vec<FileChange>,
    root: &Path,
    budget: usize,
    grouping_cfg: &crate::config::GroupingCfg,
    pack_cfg: &crate::config::PackCfg,
    extra_paths: &[String],
    delta: Option<&std::collections::BTreeSet<String>>,
) -> CellPlan {
    let grouping = group(selected, Some(root), &grouping_opts(grouping_cfg, budget));
    let bundles = grouping.bundles;
    // `review.pack`（config.json，批 A-2 子表）→ 装包旋钮：缺省 slice〔D8〕、`full` 留旋钮（批 E K1）。
    let (dossiers, dossier_meta, pack_stats) = build_packs(&bundles, root, budget, &crate::pack::PackOpts::from(pack_cfg));
    let others =
        crate::bundle::render_cross_indices(&crate::bundle::cross_index(&bundles, extra_paths, delta), bundles.len());
    CellPlan { bundles, dossiers, others, dossier_meta, stats: grouping.stats, pack_stats }
}

/// `review.grouping`（config.json，批 A-2 子表）→ 分组阈值；预算门由 pack 预算喂（批 C followup「后合者接一行」，2026-09-08 合入）。
/// `semantic` 旋钮（R8）尚无消费点。
pub(crate) fn grouping_opts(g: &crate::config::GroupingCfg, budget: usize) -> GroupingOpts {
    GroupingOpts {
        min_files: g.min_files as usize,
        bundle_line_threshold: g.bundle_line_threshold as usize,
        max_files_per_group: g.max_files_per_group as usize,
        budget_bytes: Some(budget),
    }
}

/// 阶段 0 风险路由（plan_cells 前）。ledger 路径：`--dismissed` 文件覆盖，否则 `.redteam/proj-next.db`（存在才读）。
fn stage0_risk(selected: &[FileChange], root: &Path, args: &ReviewArgs, cfg: &Config, head: Option<&str>) -> Option<RiskMeta> {
    let head = head.unwrap_or("HEAD");
    let base = args.base.as_deref().unwrap_or("HEAD");
    let ledger = match args.dismissed.as_deref() {
        Some(p) if Path::new(p).is_file() => Some(Path::new(p).to_path_buf()),
        None if !args.no_dismissed => {
            let p = root.join(crate::project::PROJECT_DIR).join(crate::db::DB_FILE);
            p.exists().then_some(p)
        }
        _ => None,
    };
    let route = crate::risk::route_from_git(
        selected,
        root,
        head,
        base,
        ledger.as_deref(),
        None,
        cfg.review.reviewers.max_cells,
    );
    Some(RiskMeta {
        hotspots: route.hotspots.len() as u32,
        r: route.r,
        tier: route.tier,
        budget_cells: route.budget_cells_hint,
    })
}

/// scan-only 流水线（M21a 批 F-4 硬切，design §2 / §4 / §5）：证据筛选 → 分组 / 装包 / 跨组索引 → scanner 扇出（组 × 条目 × samples）
/// → 疑点并集 + agreement → scan-only 报告。不产 finding；退出码：0 = scan 完成、3 = 任一 scanner cell 失败（Incomplete，design §8.4）。
/// `--background` 由 main.rs 在 dispatch 之前分叉（spawn worker）；到达这里即路由 bug。
pub async fn run_code_review(
    args: &ReviewArgs,
    registry: &ProviderRegistry,
    cfg: &Config,
    changes: Vec<FileChange>,
    root: &Path,
    head_sha: Option<String>,
    sink: Option<&dyn CellSink>,
) -> anyhow::Result<(ReviewReport, i32)> {
    if args.background {
        anyhow::bail!("--background forks in main.rs before dispatch; reaching here is a routing bug");
    }
    // 台账加载 fail-fast（坏值不烧 LLM）；M21a scan-only 只披露 run.ledger，不消费（M21c 判例前移）。
    let ledger = load_ledger(args.dismissed.as_deref(), args.no_dismissed, root)?;
    let t0 = std::time::Instant::now();

    let selected = select_files(changes);
    let risk = stage0_risk(&selected, root, args, cfg, head_sha.as_deref());
    let roles = cfg.active_roles();
    if roles.scanners.is_empty() {
        anyhow::bail!(NO_SCANNER_MSG);
    }
    let entries: Vec<ScanEntrySpec> = roles.scanners.iter().map(ScanEntrySpec::from).collect();
    let model_names: Vec<String> = entries.iter().map(|e| e.route_key.clone()).collect();

    let focus = args.focus.clone().unwrap_or_default();
    let target_label = if args.scope == "branch" {
        format!("branch {}...HEAD", args.base.as_deref().unwrap_or("?"))
    } else {
        "working tree".to_string()
    };
    if let Some(s) = sink { s.on_meta(&model_names); }
    // --spec 参照段（D2）：从 pack 预算里扣掉它占用的字节。
    let (budget, spec_section, spec_warnings) = budget_and_spec(&roles, &args.spec);
    // 分组 / 装包 / 跨组索引（plan_cells：与 --pack-only 同源）。门未过 → 单包。
    let CellPlan { bundles, dossiers, others, dossier_meta, stats, pack_stats } =
        plan_cells(selected, root, budget, &cfg.review.grouping, &cfg.review.pack, &[], None);
    let input = ScanFanoutInput {
        target_label: &target_label, bundles: &bundles, dossiers: &dossiers, others: &others,
        user_focus: &focus, spec_docs: &spec_section, root, entries: &entries, samples: cfg.review.scanners.samples,
    };
    let results = run_scan_fanout(registry, &input, sink).await;
    let mut warnings: Vec<String> = registry.warnings().to_vec();
    warnings.extend(spec_warnings);
    // 空证据必须可观测（r1 20260703 事故：干净树 0 cell 秒出无警 approve 假绿灯）。
    if bundles.is_empty() {
        warnings.push(format!(
            "no reviewable evidence: empty diff for {target_label} (0 cells) — the scan-only report reflects zero reviewed content"
        ));
    }
    if let Some(s) = sink { s.on_phase(crate::state::Phase::Finalizing, None); }
    let groups: Vec<ScanGroup> = bundles
        .iter()
        .enumerate()
        .map(|(g, b)| ScanGroup {
            label: b.label(),
            files: b.files.iter().map(|f| f.path.clone()).collect(),
            cells: vec![],
            suspicions: 0,
            pack: pack_stats.get(g).cloned().unwrap_or_default(),
        })
        .collect();
    let scanned_coverage = scanned_coverage(&bundles, &results.cells);
    Ok(finish_scan_report(ScanFinish {
        groups,
        scanned_coverage,
        grouping: Some(stats),
        risk,
        dossier_meta: Some(dossier_meta),
        results,
        model_names,
        roles: Some(&roles),
        warnings,
        inflight_peak: registry.inflight_peaks(),
        rate_limit: registry.rate_limit_meta(),
        ledger_meta: ledger.meta.clone(),
        head_sha,
        duration_ms: t0.elapsed().as_millis() as u64,
        test_command: args.test_command.clone().or_else(|| crate::project::infer_test_command(root)),
        job_id: String::new(),
        reused: vec![],
    }))
}

/// scan-only 报告收口的输入（全量 / resume / partial 三路共用，批 F-3 / F-5）。
pub(crate) struct ScanFinish<'a> {
    /// 组骨架（label / files / pack 已填；cells / suspicions 计数由本函数填）；resume / partial 无 bundles 时可空，按 cell id 补齐。
    pub groups: Vec<ScanGroup>,
    pub scanned_coverage: Option<f32>,
    pub grouping: Option<GroupingStats>,
    pub risk: Option<RiskMeta>,
    pub dossier_meta: Option<DossierMeta>,
    pub results: ScanResults,
    pub model_names: Vec<String>,
    pub roles: Option<&'a RoleSets>,
    pub warnings: Vec<String>,
    pub inflight_peak: BTreeMap<String, u32>,
    /// D13：撞过 429 的 provider 的收敛记录（`registry.rate_limit_meta()`）；partial 报告为空。
    pub rate_limit: BTreeMap<String, crate::domain::report::RateLimitMeta>,
    pub ledger_meta: Option<crate::domain::LedgerMeta>,
    pub head_sha: Option<String>,
    pub duration_ms: u64,
    pub test_command: Option<String>,
    pub job_id: String,
    /// resume 沿用上次已完成的 cell（coverage `reused`）。
    pub reused: Vec<String>,
}

/// 疑点并集 → ScanReport + RunMeta 观测（design §5 / §8.4 / F-5 表）→ verdict：任一 scanner cell 失败 → `Incomplete`（退出码 3），
/// 否则 `ScanOnly`（0）。usage / needs / coverage 全部从 cell 记录重算（全量 / resume / partial 同一口径）。
pub(crate) fn finish_scan_report(f: ScanFinish<'_>) -> (ReviewReport, i32) {
    let merged = crate::consolidate::merge_suspicions(&f.results.cells);
    let mut groups = f.groups;
    let max_group = f.results.cells.iter().map(|c| c.group + 1).max().unwrap_or(0);
    while groups.len() < max_group {
        groups.push(ScanGroup { label: format!("g{}", groups.len() + 1), ..Default::default() });
    }
    for c in &f.results.cells {
        if let Some(g) = groups.get_mut(c.group) {
            g.cells.push(c.cell_id.clone());
        }
    }
    for m in &merged {
        if let Some(g) = groups.get_mut(m.group) {
            g.suspicions += 1;
        }
    }
    let mut by_category: BTreeMap<String, u32> = BTreeMap::new();
    for m in &merged {
        if let Some(c) = m.suspicion.category {
            *by_category.entry(format!("{c:?}")).or_insert(0) += 1;
        }
    }
    let cells_ok = f.results.cells.iter().filter(|c| c.record.ok).count() as u32;
    let failed: Vec<String> = f.results.cells.iter().filter(|c| !c.record.ok).map(|c| c.cell_id.clone()).collect();
    let (mut needs_declared, mut triggered) = (0u32, 0u32);
    for c in &f.results.cells {
        if let Some(o) = &c.output {
            if !o.needs.is_empty() {
                triggered += 1;
                needs_declared += o.needs.len() as u32;
            }
        }
    }
    let stats = ScanStats {
        total: merged.len() as u32,
        cells_ok,
        cells_failed: failed.len() as u32,
        agreement_ge2: merged.iter().filter(|m| m.agreement >= 2).count() as u32,
        by_category,
        needs_declared,
    };
    let scan = ScanReport { groups, suspicions: merged, scanned_coverage: f.scanned_coverage, stats };

    // coverage 五集合（design §8.4）：selected = 全部；completed = ok 且非沿用；reused = resume 沿用；failed = 失败；waived 空。
    let mut coverage = crate::domain::Coverage::default();
    let selected: Vec<String> = f.results.cells.iter().map(|c| c.cell_id.clone()).collect();
    if !selected.is_empty() {
        coverage.selected.insert(Role::Scanner, selected);
    }
    let completed: Vec<String> = f.results.cells.iter()
        .filter(|c| c.record.ok && !f.reused.contains(&c.cell_id)).map(|c| c.cell_id.clone()).collect();
    if !completed.is_empty() {
        coverage.completed.insert(Role::Scanner, completed);
    }
    let reused: Vec<String> = f.results.cells.iter()
        .filter(|c| c.record.ok && f.reused.contains(&c.cell_id)).map(|c| c.cell_id.clone()).collect();
    if !reused.is_empty() {
        coverage.reused.insert(Role::Scanner, reused);
    }
    if !failed.is_empty() {
        coverage.failed.insert(Role::Scanner, failed.clone());
    }
    let mut usage = Usage::default();
    for c in &f.results.cells {
        usage.input_tokens += c.record.input_tokens;
        usage.output_tokens += c.record.output_tokens;
        usage.cached_input_tokens += c.record.cached_input_tokens;
    }
    let cache_hit_by_cell: Vec<CacheHit> = f.results.cells.iter()
        .filter(|c| c.record.input_tokens > 0)
        .map(|c| CacheHit { cell: c.cell_id.clone(), ratio: c.record.cached_input_tokens as f32 / c.record.input_tokens as f32 })
        .collect();
    let cells_summary: Vec<CellSummary> = f.results.cells.iter().map(|c| CellSummary::from_record(&c.record)).collect();
    let h = crate::prompt::template_hashes();
    let template_hashes: BTreeMap<String, String> = [("shared_head", &h.shared_head), ("scanner_tail", &h.scanner_tail)]
        .into_iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    let mut warnings = f.warnings;
    warnings.extend(f.results.warnings.iter().cloned());
    let verdict = if failed.is_empty() { Verdict::ScanOnly } else {
        warnings.push(format!(
            "scan incomplete: {} scanner cell(s) failed ({}) — verdict hard Incomplete (design §8.4)",
            failed.len(), failed.join(", ")
        ));
        Verdict::Incomplete
    };
    let model_ids = match f.roles {
        Some(r) => model_ids_for(&f.model_names, r),
        None => f.model_names.clone(),
    };
    let run = RunMeta {
        job_id: f.job_id,
        models: f.model_names,
        model_ids,
        duration_ms: f.duration_ms,
        usage,
        warnings,
        failed_models: vec![],
        ledger: f.ledger_meta,
        head_sha: f.head_sha,
        dossier: f.dossier_meta,
        needs: NeedsMeta { triggered_cells: triggered, declared: needs_declared },
        cells: cells_summary,
        coverage,
        template_hashes,
        cache_hit_by_cell,
        provider_inflight_peak: f.inflight_peak,
        provider_rate_limit: f.rate_limit,
        grouping: f.grouping,
        risk: f.risk,
        test_command: f.test_command,
        ..Default::default()
    };
    let cells: Vec<CellRecord> = f.results.cells.into_iter().map(|c| c.record).collect();
    let report = ReviewReport {
        schema_version: crate::domain::SCHEMA_VERSION.into(),
        verdict,
        stats: Stats::default(),
        findings: vec![],
        clusters: vec![],
        run,
        scan: Some(scan),
        cells,
    };
    let code = exit_code(verdict, false);
    (report, code)
}

/// 覆盖自报（design §4 `scanned_areas`，F-5）：改动行 = 各文件 diff hunk 的 `+` 行（0-based 新文件坐标；删除点不计——它在工作树里不存在，
/// scanner 永远覆盖不到，评审修 10）被任一 ok cell 自报区间（`a-b`，1-based）覆盖的比例；无 `+` 行（纯删除 / 空）→ None。
pub(crate) fn scanned_coverage(bundles: &[Bundle], cells: &[ScanCellResult]) -> Option<f32> {
    let mut changed: std::collections::BTreeSet<(String, usize)> = std::collections::BTreeSet::new();
    for b in bundles {
        for fc in &b.files {
            for h in crate::diffsym::parse_hunks(&fc.diff) {
                for (l, _) in h.plus {
                    changed.insert((fc.path.clone(), l));
                }
            }
        }
    }
    if changed.is_empty() {
        return None;
    }
    let mut ranges: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    for c in cells {
        let Some(o) = &c.output else { continue };
        for a in &o.scanned_areas {
            for r in &a.ranges {
                if let Some((s, e)) = r.split_once('-') {
                    if let (Ok(s), Ok(e)) = (s.trim().parse::<usize>(), e.trim().parse::<usize>()) {
                        if s >= 1 && e >= s {
                            ranges.entry(a.file.clone()).or_default().push((s - 1, e - 1));
                        }
                    }
                }
            }
        }
    }
    let hit = changed.iter().filter(|(file, line)| {
        ranges.get(file).is_some_and(|rs| rs.iter().any(|&(s, e)| s <= *line && *line <= e))
    }).count();
    Some(hit as f32 / changed.len() as f32)
}
/// 评审分发（前台 main 与后台 worker 共用）：增量轮路由，否则 git changed_files → run_code_review。
pub async fn run_review_dispatch(
    args: &ReviewArgs,
    registry: &ProviderRegistry,
    cfg: &Config,
    cwd: &Path,
    sink: Option<&dyn CellSink>,
) -> anyhow::Result<(ReviewReport, i32)> {
    // run_review_dispatch 永远同步执行——--background 分叉发生在 main.rs，先于此处；
    // 故强制 background=false（否则 run_code_review 会 bail）。
    let mut sync_args = args.clone();
    sync_args.background = false;
    let branch_scope = sync_args.scope == "branch";
    // M13：增量轮路由（前台与后台 worker 共用本单点；scope 守卫在 preflight 内）。
    if let Some(prior) = sync_args.incremental_from.clone() {
        return run_incremental_review(&sync_args, &prior, cfg, cwd).await;
    }

    let changes = crate::evidence::git::changed_files(cwd, sync_args.base.as_deref(), branch_scope)?;
    // R3：stamp 权威时刻 = 证据收集实跑刻（与 changed_files 同刻同 cwd）；
    // 非 git 目录折 None——前台裸 review 与后台 job 共用本单点，前后台天然同 sha。
    let head_sha = crate::evidence::git::head_sha_at(cwd).ok();
    run_code_review(&sync_args, registry, cfg, changes, cwd, head_sha, sink).await
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 进程内唯一 job-id（pid + secs + cwd 的 fnv 前 12 位）。主程序后台派发与 `--pack-only` 共用。
pub fn gen_job_id(cwd: &Path) -> String {
    let seed = format!("{}-{}", std::process::id(), now_unix());
    let h = crate::hash::fnv1a64_hex(&[&seed, &cwd.to_string_lossy()]);
    h[..12].to_string()
}

/// `--pack-only` 估算换算：代码英文 ~3.5 B/token（D7 B6，网关 usage 低报时以字节为准）。
pub const PACK_BYTES_PER_TOKEN: f64 = 3.5;

/// `--pack-only` 单 cell 记账：`bytes` = 真实发出的 scanner prompt 字节（共享头 system + 四块 user，与真实评审同一
/// `build_scan_prompt`，可与 usage 对账；批 F-4 K15）；`pack_bytes` = 裸 pack 正文；`over_budget` = 分组「宁降不拆」标记。
#[derive(Clone, Debug, serde::Serialize)]
pub struct PackCell {
    /// 1-based，对应 `packs/<job>/cell-<n>.md`。
    pub cell: usize,
    pub label: String,
    pub files: usize,
    pub bytes: usize,
    pub est_tokens: u64,
    pub pack_bytes: usize,
    pub diff_bytes: usize,
    pub over_budget: bool,
    /// 装包形态（批 E K13）：body / sliced / small_full / ladder / folded_lines / outside_*。
    pub pack: crate::pack::PackStats,
}

/// `--pack-only` 报告（stdout 文本行 + `packs/<job>/summary.json` 同源，K12）。
#[derive(Clone, Debug, serde::Serialize)]
pub struct PackOnlyReport {
    pub job_id: String,
    pub head_sha: Option<String>,
    pub target: String,
    pub budget_bytes: usize,
    pub cells: Vec<PackCell>,
    pub grouping: crate::bundle::GroupingStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<RiskMeta>,
    pub packs_dir: String,
    pub warnings: Vec<String>,
}

/// `--pack-only` 预算仪表（M21a 批 C，D7 B7）：证据收集 → 门 / 分组 → 装包 → 按 cell 组 prompt →
/// 落盘 `.redteam/packs/<job_id>/cell-<n>.md` 与 `summary.json`。不建 registry、不读 secrets、不调
/// provider；无任何模型配置也能跑（预算按 128K 窗保守缺省）。
pub fn run_pack_only(args: &ReviewArgs, cfg: &Config, cwd: &Path) -> anyhow::Result<PackOnlyReport> {
    let branch_scope = args.scope == "branch";
    let changes = crate::evidence::git::changed_files(cwd, args.base.as_deref(), branch_scope)?;
    let head_sha = crate::evidence::git::head_sha_at(cwd).ok();
    let target = if branch_scope {
        format!("branch {}...HEAD", args.base.as_deref().unwrap_or("?"))
    } else {
        "working tree".to_string()
    };
    // 预算取自与真实评审同一角色集（`active_roles` → scanners 最小窗），量的才是真实会发出的 prompt；
    // 角色集为空（无配置）时按 128K 窗保守缺省，不报错——仪表无 provider 也能跑。
    let roles = cfg.active_roles();
    let (budget, spec_section, mut warnings) = budget_and_spec(&roles, &args.spec);
    let selected = select_files(changes);
    let risk = stage0_risk(&selected, cwd, args, cfg, head_sha.as_deref());
    let CellPlan { bundles, dossiers, others, stats, pack_stats, .. } =
        plan_cells(selected, cwd, budget, &cfg.review.grouping, &cfg.review.pack, &[], None);
    let focus = args.focus.clone().unwrap_or_default();

    let job_id = gen_job_id(cwd);
    let project_dir = cwd.join(crate::project::PROJECT_DIR);
    let packs_dir = project_dir.join("packs").join(&job_id);
    std::fs::create_dir_all(&packs_dir)?;
    // 未 init 的项目：只补 `.redteam/.gitignore`（镜像 init_project 的 `*`），不建库、不建 rounds。
    let gitignore = project_dir.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, crate::project::GITIGNORE_BODY)?;
    }
    let mut cells = Vec::new();
    for (i, b) in bundles.iter().enumerate() {
        let dossier = dossiers.get(i).map(String::as_str).unwrap_or("");
        let other = others.get(i).map(String::as_str).unwrap_or("");
        let sp = crate::prompt::build_scan_prompt(&crate::prompt::ScanPromptInput {
            target_label: &target, bundle: b, user_focus: &focus, cross_index: other, precedents: "",
            dossier, spec_docs: &spec_section, repo_brief: "",
        });
        // 落盘形态 = system + 四块（openai kind 的 flatten 口径）；字节含 system 段（followup「--pack-only 口径切到 scan prompt」）。
        let prompt = format!("{}\n\n{}", sp.system, sp.flatten());
        std::fs::write(packs_dir.join(format!("cell-{}.md", i + 1)), &prompt)?;
        cells.push(PackCell {
            cell: i + 1,
            label: b.label(),
            files: b.files.len(),
            bytes: prompt.len(),
            est_tokens: (prompt.len() as f64 / PACK_BYTES_PER_TOKEN).round() as u64,
            pack_bytes: dossier.len(),
            diff_bytes: b.files.iter().map(|f| f.diff.len()).sum(),
            over_budget: b.over_budget,
            pack: pack_stats.get(i).cloned().unwrap_or_default(),
        });
    }
    if cells.is_empty() {
        warnings.push(format!("no reviewable evidence: empty diff for {target} (0 cells)"));
    }
    let report = PackOnlyReport {
        job_id,
        head_sha,
        target,
        budget_bytes: budget,
        cells,
        grouping: stats,
        risk,
        packs_dir: packs_dir.to_string_lossy().into_owned(),
        warnings,
    };
    std::fs::write(packs_dir.join("summary.json"), serde_json::to_string_pretty(&report)?)?;
    Ok(report)
}

/// `--pack-only` stdout：每 cell 一行 + 分组统计一行 + packs 目录一行（+ warnings）。固定文本、不按
/// `--format` 切换（机读走 summary.json）。
pub fn render_pack_only(r: &PackOnlyReport) -> String {
    let mut out = String::new();
    for c in &r.cells {
        let p = &c.pack;
        out.push_str(&format!(
            "cell-{} files={} bytes={} est_tokens={} pack_bytes={} diff_bytes={} over_budget={} body={} sliced={} small={} ladder={} folded_lines={} outside_sites={} outside_sigs={}\n",
            c.cell, c.files, c.bytes, c.est_tokens, c.pack_bytes, c.diff_bytes, c.over_budget,
            p.body, p.sliced, p.small_full, p.ladder, p.folded_lines, p.outside_sites, p.outside_sigs
        ));
    }
    if r.cells.is_empty() {
        out.push_str("cells=0\n");
    }
    let g = &r.grouping;
    // Q4：三键 Option——Some(n) 印数字，None（旧报告未采集）印 `-`，不从总数猜分种
    let kind = |v: Option<usize>| v.map(|n| n.to_string()).unwrap_or_else(|| "-".into());
    out.push_str(&format!(
        "grouping gate={} groups={} edges={} cross_edges={} cross_import={} cross_reference={} cross_same_dir={} pooled_small={} isolated={} over_budget={}\n",
        g.gate.as_str(), g.groups, g.edges, g.cross_edges, kind(g.cross_import), kind(g.cross_reference), kind(g.cross_same_dir), kind(g.pooled_small_components), g.isolated, g.over_budget
    ));
    if let Some(risk) = &r.risk {
        out.push_str(&format!(
            "risk hotspots={} r={} tier={} budget_cells={}\n",
            risk.hotspots, risk.r, risk.tier, risk.budget_cells
        ));
    }
    out.push_str(&format!("packs={}\n", r.packs_dir));
    for w in &r.warnings {
        out.push_str(&format!("warning: {w}\n"));
    }
    out
}

/// R2.1 job checkpoint sink：扇出前落 manifest（+刷 progress.total），每 cell 完成落 partial
/// （+刷 last_active/progress.done）。所有 JobState 写走 `update_if_active`（守卫式
/// 读改写）——事务内 `progress.done += 1` 消除并发 cell 的 RMW 丢更新（原进程内 Mutex 只护
/// worker 自身、跨不进 cancel 进程；守卫靠 SQLite 写锁跨进程串行化，故 Mutex 退役）。
/// 终态封盘：job 已终态（如被 cancel）时守卫拒写，sink 的迟到进度不复活状态。
/// M17-c Task 3：`ws: PathBuf`（每次调用现开连接）改持 `Arc<JobStore>`（worker 全生命周期
/// 单连接复用）。
/// M17-c Task 5（design §4 表②级）：checkpoint 写失败吞而有痕——`Err` 分支 eprintln（含
/// job_id/操作名）+ `ckpt_failures` 计数；run_job 收尾读计数塞 `report.run.warnings`
/// （detached worker stderr=null，warnings 才是唯一对外可见通道）。`Ok(false)`（终态封盘
/// 拒写）是正常竞态，不计入失败、不打印——与 run_job 里 ③级终态写对 `Ok(false)` 的处理呼应。
struct JobCellSink {
    store: std::sync::Arc<crate::state::JobStore>,
    job_id: String,
    ckpt_failures: std::sync::atomic::AtomicU32,
}

impl CellSink for JobCellSink {
    fn manifest(&self, inputs: &[crate::state::CellInput]) {
        if let Err(e) = self.store.write_manifest(&self.job_id, inputs) {
            eprintln!("warn: job {} write_manifest 失败: {e}", self.job_id);
            self.ckpt_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let total = inputs.len() as u32;
        if let Err(e) = self.store.update_if_active(&self.job_id, |st| {
            st.progress.total = total;
            st.set_phase(crate::state::Phase::Scan, None);
        }) {
            eprintln!("warn: job {} manifest 阶段 progress/phase 写失败: {e}", self.job_id);
            self.ckpt_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn on_cell(&self, rec: crate::state::CellRecord) {
        if let Err(e) = self.store.append_partial(&self.job_id, &rec) {
            eprintln!("warn: job {} append_partial 失败（cell {}）: {e}", self.job_id, rec.cell);
            self.ckpt_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if let Err(e) = self.store.update_if_active(&self.job_id, |st| {
            st.last_active = now_unix();
            st.progress.done += 1;
        }) {
            eprintln!("warn: job {} on_cell 进度写失败: {e}", self.job_id);
            self.ckpt_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn on_phase(&self, phase: crate::state::Phase, note: Option<String>) {
        if let Err(e) = self.store.update_if_active(&self.job_id, |st| {
            st.set_phase(phase, note);
        }) {
            eprintln!("warn: job {} on_phase 写失败: {e}", self.job_id);
            self.ckpt_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn on_meta(&self, models: &[String]) {
        let models = models.to_vec();
        if let Err(e) = self.store.update_if_active(&self.job_id, |st| {
            st.models = models;
        }) {
            eprintln!("warn: job {} on_meta 写失败: {e}", self.job_id);
            self.ckpt_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// 批 F-2 / F-5：本次作业的角色集（`JobState.roles`，status 可见）。
    fn on_roles(&self, roles: &[Role]) {
        let roles = roles.to_vec();
        if let Err(e) = self.store.update_if_active(&self.job_id, |st| {
            st.roles = roles;
        }) {
            eprintln!("warn: job {} on_roles 写失败: {e}", self.job_id);
            self.ckpt_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// ②级披露拼装（design §4 表②）：计数>0 才追加条目（正常路径 report 逐字节不变，design §6
/// 契约冻结面——「零失败时 report 不变」）；文案含 job_id 与次数，且指出 resume 兜底路径。
fn append_ckpt_failure_warning(report: &mut ReviewReport, job_id: &str, failures: u32) {
    if failures > 0 {
        report.run.warnings.push(format!(
            "job {job_id}: 检查点落盘失败 {failures} 次（状态/进度可能陈旧；resume 可差集重放兜底）"
        ));
    }
}

/// 从 proj.db cells 表把 cell 记录回填进报告：`run.cells` 摘要（诊断呈现）+ 顶层 `cells`（含 output，评分器形态）。
/// 三条终态路径统一调用；旧作业 JSON 无此字段（serde default 空）不受影响。
pub fn attach_cells(store: &crate::state::JobStore, job_id: &str, report: &mut ReviewReport) {
    let records = store.read_partial(job_id);
    report.run.cells = records.iter().map(CellSummary::from_record).collect();
    report.cells = records;
}
/// R2.2：失败/超时时从 proj.db 的 cell checkpoint 组一份 `incomplete` 报告——绝不因为 dispatch 出错/超时就静默丢掉已经跑完的 cell。
/// 全部落盘记录（含失败格）都进收口：失败格进 coverage.failed / cells_failed、烧掉的 usage 入账，与随后 `attach_cells` 回填的
/// `run.cells` / 顶层 `cells` 一致（评审修 2）。M 取 manifest 行数（若尚未写出 manifest 就已失败，退化用已完成数兜底，不出现 "N/0"）。
fn partial_report(store: &crate::state::JobStore, job_id: &str, reason: String) -> (ReviewReport, i32) {
    let manifest = store.read_manifest(job_id);
    let records: Vec<CellRecord> = store.read_partial(job_id);
    let done = records.iter().filter(|r| r.ok).count();
    let total = manifest.len().max(records.len());
    let results = ScanResults { cells: scan_cells_from_records(records), ..Default::default() };
    let (mut report, _) = finish_scan_report(ScanFinish {
        groups: vec![],
        scanned_coverage: None,
        grouping: None,
        risk: None,
        dossier_meta: None,
        results,
        model_names: vec![],
        roles: None,
        warnings: vec![format!("partial: {done}/{total} cells"), reason],
        inflight_peak: BTreeMap::new(),
        rate_limit: BTreeMap::new(),
        ledger_meta: None,
        head_sha: None,
        duration_ms: 0,
        test_command: None,
        job_id: job_id.to_string(),
        reused: vec![],
    });
    report.verdict = Verdict::Incomplete;
    let code = exit_code(Verdict::Incomplete, false);
    (report, code)
}
/// R2.3：resume 差集补跑（批 F-4 scan 版）。读 `manifest`（全集 M）+ `partial`（已完成 ok:true 的 scanner cell 集）→ `待跑 = M − 已完成`；
/// 对待跑 cell 用 manifest 里落盘的 `scan` 请求（system + 分块 + cache 断点）重放 `registry.scan`——不重收集证据、不重 build prompt
/// （确定性）；无 `scan` 字段（旧格式作业）的 cell 记失败。单格收口与扇出同款（[`crate::orchestrate::scan_cell_record`]），模板哈希取 manifest
/// 落盘值（与当前二进制不一致时 warning，评审修 6）。全部记录（已完成 + 新补 + 失败）走 scan-only 收口；组骨架 / 分组观测 / head_sha /
/// 覆盖自报从作业原报告沿用（resume 不重收集证据，评审修 5）；全集补齐 → scan-only；仍有缺失 → `Incomplete`。
/// 旧格式作业（批 B-E 的 reviewer 形态记录）不可 resume（评审修 1）。角色集按当前配置重建（M21b 队列快照落地后改读快照）。
pub async fn run_resume(
    job_id: &str,
    registry: &ProviderRegistry,
    cfg: &Config,
    cwd: &Path,
    store: &crate::state::JobStore,
) -> (ReviewReport, i32) {
    let t0 = std::time::Instant::now();
    let manifest = store.read_manifest(job_id);
    // 空 manifest = job 在扇出写 manifest 前就崩溃：没有任何 work-list → 直接 Incomplete（不假装 scan 完成）。
    if manifest.is_empty() {
        return partial_report(
            store, job_id,
            "resume: empty manifest (job crashed before fan-out); nothing to resume".into(),
        );
    }
    let args: ReviewArgs = match store.read_args_raw(job_id)
        .and_then(|j| serde_json::from_str(&j).ok())
    {
        Some(a) => a,
        None => {
            return partial_report(
                store, job_id,
                "resume: missing job args (job never ran via --background)".into(),
            );
        }
    };
    if args.incremental_from.is_some() {
        return partial_report(
            store, job_id,
            "resume: incremental jobs cannot be resumed; re-run `redteam-next review --incremental-from <prior-job>`".into(),
        );
    }
    let prior_records = store.read_partial(job_id);
    if prior_records.iter().any(|r| !matches!(r.output, crate::domain::CellOutput::Scan(_))) {
        return partial_report(
            store, job_id,
            "resume: job has non-scanner cell records (pre-M21a finding job); cannot resume with the scan-only binary — re-run review".into(),
        );
    }
    let done: std::collections::BTreeSet<String> = prior_records.into_iter().filter(|r| r.ok).map(|r| r.cell).collect();
    let pending: Vec<&crate::state::CellInput> =
        manifest.iter().filter(|c| !done.contains(&c.cell)).collect();
    let roles = cfg.active_roles();
    let entry_of = |name: &str| -> Option<crate::orchestrate::ScanEntrySpec> {
        roles.scanners.iter().find(|m| m.name == name).map(crate::orchestrate::ScanEntrySpec::from)
    };
    let family_of = |name: &str| -> String {
        roles.scanners.iter().chain(&roles.reviewers).chain(&roles.verifiers)
            .find(|m| m.name == name).map(|m| m.family.clone()).unwrap_or_default()
    };
    let current_hash = crate::prompt::scan_template_hash();

    let mut warnings: Vec<String> = registry.warnings().to_vec();
    // 差集 cell 并行补跑（join_all 保序）；provider 在途门在 registry 内（唯一闸，D13）：门等待记 queue_wait_ms，与扇出同口径。
    let outs = futures::future::join_all(pending.iter().map(|cell| async move {
        let t0 = std::time::Instant::now();
        let mut queue_wait_ms = 0u64;
        let res = match &cell.scan {
            Some(p) => match registry.lease(&cell.model).await {
                Ok(lease) => {
                    queue_wait_ms = t0.elapsed().as_millis() as u64;
                    lease.run(p).await
                }
                Err(e) => Err(e),
            },
            None => Err(crate::provider::ProviderError::Unavailable(
                "manifest 无 scan prompt（旧格式作业），无法重放；请重新 review".into(),
            )),
        };
        (cell, res, t0.elapsed(), queue_wait_ms)
    }))
    .await;
    // 排队告警与扇出同口径（D13）：补跑格数 > 该 provider 有效门宽即排队。
    warnings.extend(registry.queue_warnings(pending.iter().filter(|c| c.scan.is_some()).map(|c| c.model.as_str())));
    let mut hash_mismatch = false;
    for (cell, outcome, elapsed, queue_wait_ms) in outs {
        if outcome.is_err() {
            if let Err(e) = &outcome {
                warnings.push(format!("resume cell [{}] model {}: {e}", cell.cell, cell.model));
            }
        }
        let hash = match &cell.template_hash {
            Some(h) => {
                if *h != current_hash {
                    hash_mismatch = true;
                }
                h.clone()
            }
            None => current_hash.clone(),
        };
        let pinned = entry_of(&cell.model).map(|e| e.thinking_pinned).unwrap_or(false);
        let (mut rec, _) = crate::orchestrate::scan_cell_record(
            &cell.cell, &cell.model, &family_of(&cell.model), pinned, cwd, outcome, elapsed.as_millis() as u64, hash,
        );
        rec.queue_wait_ms += queue_wait_ms;
        if let Err(e) = store.append_partial(job_id, &rec) {
            eprintln!("warn: job {job_id} append_partial 失败（cell {}）: {e}", rec.cell);
        }
    }
    if hash_mismatch {
        warnings.push(format!(
            "resume: replayed cells use the manifest's template hash (frozen prompt), which differs from the current binary's {current_hash}; template A/B attribution follows the manifest value"
        ));
    }

    // 收口：全部记录（已完成 + 新补 + 失败）；组骨架 / 分组观测 / head_sha / 覆盖自报 / dossier 从原报告沿用（resume 不重收集证据）。
    let final_records = store.read_partial(job_id);
    let ok_cells: std::collections::BTreeSet<&str> =
        final_records.iter().filter(|r| r.ok).map(|r| r.cell.as_str()).collect();
    let still_missing = manifest.iter().filter(|c| !ok_cells.contains(c.cell.as_str())).count();
    let reused: Vec<String> = final_records.iter().filter(|r| r.ok && done.contains(&r.cell)).map(|r| r.cell.clone()).collect();
    let model_names: Vec<String> = roles.scanners.iter().map(|m| m.name.clone()).collect();
    let ledger = match load_ledger(args.dismissed.as_deref(), args.no_dismissed, cwd) {
        Ok(l) => l,
        Err(e) => {
            return partial_report(store, job_id, format!("resume: bad ledger: {e}"));
        }
    };
    let prior: Option<ReviewReport> = store.read_result(job_id).and_then(|j| serde_json::from_str(&j).ok());
    let groups: Vec<ScanGroup> = prior
        .as_ref()
        .and_then(|r| r.scan.as_ref())
        .map(|sc| sc.groups.iter().map(|g| ScanGroup { label: g.label.clone(), files: g.files.clone(), cells: vec![], suspicions: 0, pack: g.pack.clone() }).collect())
        .unwrap_or_default();
    warnings.extend(registry.gate_warnings()); // D13：补跑期间撞过 429 的 provider 收敛记录
    let results = ScanResults { cells: scan_cells_from_records(final_records), ..Default::default() };
    let (mut report, mut code) = finish_scan_report(ScanFinish {
        groups,
        scanned_coverage: prior.as_ref().and_then(|r| r.scan.as_ref()).and_then(|sc| sc.scanned_coverage),
        grouping: prior.as_ref().and_then(|r| r.run.grouping.clone()),
        risk: prior.as_ref().and_then(|r| r.run.risk.clone()),
        dossier_meta: prior.as_ref().and_then(|r| r.run.dossier.clone()),
        results,
        model_names,
        roles: Some(&roles),
        warnings,
        inflight_peak: registry.inflight_peaks(),
        rate_limit: registry.rate_limit_meta(),
        ledger_meta: ledger.meta.clone(),
        head_sha: prior.as_ref().and_then(|r| r.run.head_sha.clone()),
        duration_ms: t0.elapsed().as_millis() as u64,
        test_command: args.test_command.clone().or_else(|| crate::project::infer_test_command(cwd)),
        job_id: job_id.to_string(),
        reused,
    });
    if still_missing > 0 {
        report.verdict = Verdict::Incomplete;
        report.run.warnings.push(format!("resume: {still_missing} cell(s) still missing after replay"));
        code = exit_code(Verdict::Incomplete, false);
    }
    // 回写 proj.db：resume 产出的合并报告覆盖扇出原始落盘（否则 result 命令永远返回旧报告）。终态字段不动。
    let json = crate::output::render(&report, crate::output::Format::Json);
    let md = crate::output::render(&report, crate::output::Format::Md);
    if let Err(e) = store.write_result(job_id, &json) {
        eprintln!("warn: job {job_id} resume write_result 失败: {e}");
    }
    if let Err(e) = store.write_result_md(job_id, &md) {
        eprintln!("warn: job {job_id} resume write_result_md 失败: {e}");
    }
    (report, code)
}
/// 落终态部分报告：渲染 json+md、写 result 文件、守卫式把 state 转终态并记录错误原因。
/// 走 `update_state_if_active`：若期间已被 cancel（终态），守卫拒写、保留 Cancelled（首个终态封盘）。
fn write_partial_result(
    store: &crate::state::JobStore,
    job_id: &str,
    status: crate::state::JobStatus,
    report: &ReviewReport,
    code: i32,
    error: String,
) {
    let mut report = report.clone();
    attach_cells(store, job_id, &mut report);
    let json = crate::output::render(&report, crate::output::Format::Json);
    let md = crate::output::render(&report, crate::output::Format::Md);
    // ③级终态写：失败必响（design §4）——result/result_md/终态任一落盘失败都不该被静默吞掉，
    // 否则 status/result 命令会永远看不到这份部分报告，且查无踪迹。
    if let Err(e) = store.write_result(job_id, &json) {
        eprintln!("warn: job {job_id} write_result 失败: {e}");
    }
    if let Err(e) = store.write_result_md(job_id, &md) {
        eprintln!("warn: job {job_id} write_result_md 失败: {e}");
    }
    match store.update_if_active(job_id, |st| {
        st.status = status;
        st.error = Some(error);
        st.exit_code = Some(code);
        st.result_path = Some(format!("proj.db#results/{job_id}"));
    }) {
        Ok(false) => eprintln!("warn: job {job_id} 终态写被终态封盘拒绝（可能已被 cancel/收尸）"),
        Err(e) => eprintln!("warn: job {job_id} 终态写失败: {e}"),
        Ok(true) => {}
    }
}

/// 状态机包裹评审：running → (评审) → done+result.json / failed+error。返回退出码。
/// 起手若已是任意终态（Cancelled/Done/Failed/TimedOut）则早退不覆盖，保留首个终态。
/// `st.deadline_at`（worker 起手设，见 main.rs）到点仍未完成 → TimedOut + 部分报告（R1.2）。
/// M17-c Task 3：收 `Arc<JobStore>`（替代 `ws: &Path` + `&JobStore` 双参）——worker 全
/// 生命周期单连接贯穿，`ws` 参随之消失（唯一用途是构造 `JobCellSink`，现克隆 Arc 即可）。
#[allow(clippy::too_many_arguments)] // C-Q4 新增 store 参致第 8 参；收拢 struct 是过度设计
pub async fn run_job(
    job_id: &str,
    args: &ReviewArgs,
    registry: &ProviderRegistry,
    cfg: &Config,
    cwd: &Path,
    started_at: u64,
    store: std::sync::Arc<crate::state::JobStore>,
) -> i32 {
    use crate::state::{JobState, JobStatus};

    let st = store.read_state(job_id).unwrap_or_else(|| JobState::new(job_id, started_at));
    // 终态封盘：写 Running 前检查是否已落任意终态（不止 Cancelled），若是则直接返回，
    // 不再走下面的守卫写（避免用一次注定被拒的写掩盖真实早退语义）。
    if st.status.is_terminal() {
        return st.exit_code.unwrap_or(0);
    }
    // 守卫写 Pending→Running：与上面的检查之间存在窗口——cancel 进程可能在此期间
    // 抢先落 Cancelled 并提交，若仍走裸 write_state 会把 Cancelled 覆回 Running
    // （违反首个终态封盘）。改走 update_if_active，事务内 CAS，拒写不覆盖。
    // 三态显式分流（dogfood F3 抓漏）：此前 Err 被 unwrap_or(false) 折叠进拒写分支，
    // DB 故障会让 worker 沿「重读终态」路径静默退 0（job 悬 Pending、无终态无报错）——
    // DB 错误既不是「已有终态」也不是「可以开跑」，必须报错退 1。
    match store.update_if_active(job_id, |s| {
        s.status = JobStatus::Running;
        s.set_phase(crate::state::Phase::Collecting, None);
    }) {
        Ok(true) => {}
        Ok(false) => {
            // 期间已被别的进程落了终态：重读真终态、返回其 exit_code（与上面早退同款语义）。
            return store.read_state(job_id).and_then(|s| s.exit_code).unwrap_or(0);
        }
        Err(e) => {
            eprintln!("error: job {job_id} Pending→Running 守卫写失败: {e}");
            // dogfood r2 追打：detached worker 的退出码无人观测、stderr 进 null，仅 return
            // 会把 job 行留在 Pending——best-effort 补一次 Failed 终态写（瞬时 DB 故障可
            // 覆盖；持久故障本就无从落盘，残余由 status 惰性收尸兜底，损失仅 exit_code
            // 精度 1→2）。
            match store.update_if_active(job_id, |s| {
                s.status = JobStatus::Failed;
                s.error = Some(format!("Pending→Running 守卫写失败: {e}"));
                s.exit_code = Some(1);
            }) {
                Ok(true) => {}
                Ok(false) => eprintln!("warn: job {job_id} Failed 终态写被终态封盘拒绝"),
                Err(e2) => eprintln!("warn: job {job_id} Failed 终态写亦失败: {e2}"),
            }
            return 1;
        }
    }

    let sink = JobCellSink {
        store: std::sync::Arc::clone(&store),
        job_id: job_id.to_string(),
        ckpt_failures: std::sync::atomic::AtomicU32::new(0),
    };
    let dispatch = run_review_dispatch(args, registry, cfg, cwd, Some(&sink));

    // Outcome 只在本函数内构造/匹配一次，Box 间接层不值当——沿用 main.rs Cmd 枚举同款 allow。
    #[allow(clippy::large_enum_variant)]
    enum Outcome {
        Done(anyhow::Result<(ReviewReport, i32)>),
        TimedOut,
    }

    let outcome = match st.deadline_at {
        Some(deadline_secs) => {
            let remaining = deadline_secs.saturating_sub(now_unix());
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(remaining);
            tokio::select! {
                r = dispatch => Outcome::Done(r),
                _ = tokio::time::sleep_until(deadline) => Outcome::TimedOut,
            }
        }
        None => Outcome::Done(dispatch.await),
    };

    // Task 5（design §4 表②级）：dispatch 生命周期内 sink 累计的 checkpoint 写失败数——
    // dispatch 已 await/被 select 丢弃，&sink 借用随之结束，此处读计数安全。三条 outcome
    // 分支（成功/dispatch 报错/超时）在写 result 前统一塞 report.run.warnings（若非零）。
    let ckpt_failures = sink.ckpt_failures.load(std::sync::atomic::Ordering::Relaxed);

    match outcome {
        Outcome::Done(Ok((mut report, code))) => {
            // dispatch 无 job 上下文（TailCtx.job_id 恒空），此处回填——json/md 契约里
            // run.job_id 是消费者的自证 id（r1 20260703 实证恒空串）。
            report.run.job_id = job_id.to_string();
            append_ckpt_failure_warning(&mut report, job_id, ckpt_failures);
            attach_cells(&store, job_id, &mut report);
            // 同渲染 json + md 落盘（result 命令按 format 直读，避免反序列化 report 图）。
            let json = crate::output::render(&report, crate::output::Format::Json);
            let md = crate::output::render(&report, crate::output::Format::Md);
            // ③级终态写：失败必响（design §4）——不再静默吞，result 缺失/终态未落都要有痕迹。
            if let Err(e) = store.write_result(job_id, &json) {
                eprintln!("warn: job {job_id} write_result 失败: {e}");
            }
            if let Err(e) = store.write_result_md(job_id, &md) {
                eprintln!("warn: job {job_id} write_result_md 失败: {e}");
            }
            let models = report.run.models.clone();
            // 守卫写 Done：若期间已被 cancel（Cancelled 终态），守卫拒写、保留 Cancelled
            // （首个终态封盘）。progress/last_active 由 sink 运行期已写，这里只叠终态字段。
            match store.update_if_active(job_id, |st| {
                st.status = JobStatus::Done;
                st.exit_code = Some(code);
                st.models = models;
                st.result_path = Some(format!("proj.db#results/{job_id}"));
            }) {
                Ok(false) => eprintln!("warn: job {job_id} Done 终态写被终态封盘拒绝（可能已被 cancel）"),
                Err(e) => eprintln!("warn: job {job_id} Done 终态写失败: {e}"),
                Ok(true) => {}
            }
            code
        }
        Outcome::Done(Err(e)) => {
            let (mut report, code) = partial_report(&store, job_id, e.to_string());
            append_ckpt_failure_warning(&mut report, job_id, ckpt_failures);
            write_partial_result(&store, job_id, JobStatus::Failed, &report, code, e.to_string());
            code
        }
        Outcome::TimedOut => {
            let reason = "timed out: deadline exceeded".to_string();
            let (mut report, code) = partial_report(&store, job_id, reason.clone());
            append_ckpt_failure_warning(&mut report, job_id, ckpt_failures);
            write_partial_result(&store, job_id, JobStatus::TimedOut, &report, code, reason);
            code
        }
    }
}

/// 惰性收尸（E12-Q2）：`status`/`list` 读状态时，对 Running/Pending 且已登记 pid 但 worker
/// 进程已死（或 pid 被复用）的 job，就地落 **Failed** 终态并返回更新后状态（无常驻守护进程——
/// 裸终端工具无 daemon，YAGNI）。Pending 一并收（dogfood F4 抓漏）：worker 在写 Running 前
/// 早夭（启动期被杀/args 读失败 exit）会留下 Pending+死 pid 永久悬置；pid 尚未登记的创建
/// 窗口由「无 pid 放过」天然挡住，不会误收。
/// 走守卫写：若此刻 worker 恰好真完成写了 Done，守卫拒写；无论写成功与否都重读 db 真相返回——
/// 绝不落回调用方传入的陈旧 Running 快照 `st`（那会把已完成/已消失的 job 谎报成仍在 Running）。
pub fn reap_if_dead(store: &crate::state::JobStore, st: crate::state::JobState) -> crate::state::JobState {
    use crate::state::JobStatus;
    if st.status != JobStatus::Running && st.status != JobStatus::Pending {
        return st;
    }
    let pid = match st.pid {
        Some(p) => p,
        None => return st, // 无 pid 无从判定，保守放过
    };
    if crate::procinfo::is_worker_alive(pid, st.pid_start) {
        return st;
    }
    let job_id = st.job_id.clone();
    // ③级终态写（收尸）：失败必响——收尸拒写通常是正常竞态（worker 抢先真完成/被 cancel），
    // 但 Err（db 层异常）不该被静默吞掉，否则收尸失效会一直悄悄发生。
    let wrote = match store.update_if_active(&job_id, |s| {
        s.status = JobStatus::Failed;
        s.error = Some("worker process died (reaped)".to_string());
        s.exit_code = Some(2);
    }) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("warn: job {job_id} 收尸写失败: {e}");
            false
        }
    };
    // 成功：拿到刚写的 Failed；被拒（终态封盘生效，说明 worker 在探活与落笔间的极小窗口内
    // 已真完成/被 cancel）：重读拿到真终态，而非误判为 Running 或误写为 Failed。
    match store.read_state(&job_id) {
        Some(fresh) => fresh,
        None => {
            // 重读也扑空（job 状态被并发移除等极端情况）：明确降级返回 Failed，绝不复用
            // 调用方传入的陈旧 Running 快照——那同样会谎报「已消失」的 job 仍在 Running。
            let mut degraded = st;
            degraded.status = JobStatus::Failed;
            degraded.error = Some(if wrote {
                "worker process died (reaped)".to_string()
            } else {
                "job state vanished during reap (degraded)".to_string()
            });
            degraded.exit_code = Some(2);
            degraded
        }
    }
}

/// panic 兜底落终态（E12-Q4）：worker 评审体 panic 时由 panic hook 调用，守卫式把 Running
/// 落 Failed（error 只带 message、不带 backtrace/location——B 下报告要干净）。若 panic 发生在
/// 真完成之后，守卫拒写、保留真终态。
pub fn mark_failed_panic(store: &crate::state::JobStore, job_id: &str, msg: &str) {
    let msg = msg.to_string();
    // ③级终态写：失败必响（design §4）——panic 兜底若也悄悄失败，job 会永久悬在 Running。
    match store.update_if_active(job_id, |st| {
        st.status = crate::state::JobStatus::Failed;
        st.error = Some(format!("panic: {msg}"));
        st.exit_code = Some(2);
    }) {
        Ok(false) => eprintln!("warn: job {job_id} panic 兜底写被终态封盘拒绝（已有更早终态）"),
        Err(e) => eprintln!("warn: job {job_id} panic 兜底写失败: {e}"),
        Ok(true) => {}
    }
}

/// R1.2：worker 总时限——到点仍未完成的作业自我中止为 TimedOut（保留部分报告，见 `run_job`）。
pub const JOB_DEADLINE_SECS: u64 = 40 * 60;

/// worker 起手设 deadline_at（若尚未设置——例如状态文件是旧格式或已被设过），main.rs
/// Cmd::Worker 分支调用。守卫式更新（C-Q4 决策点：从裸覆写收紧）：`update_if_active` 挡终态
/// 覆写（首个终态封盘），闭包内 `is_none` 判挡已设 deadline 被覆写（不应缩短/改动已生效期限）。
pub fn stamp_deadline(store: &crate::state::JobStore, job_id: &str, started: u64) {
    if let Err(e) = store.update_if_active(job_id, |st| {
        if st.deadline_at.is_none() {
            st.deadline_at = Some(started + JOB_DEADLINE_SECS);
        }
    }) {
        eprintln!("warn: job {job_id} deadline_at 回填失败: {e}");
    }
}

#[cfg(test)]
mod job_tests {
    use super::*;
    use crate::provider::tool_loop::{AssistantTurn, ChatBackend, ChatMessage, FakeChatBackend};
    use crate::state::{JobState, JobStatus};
    use std::sync::Arc;

    fn scan_turn(json: &str) -> AssistantTurn {
        AssistantTurn { text: json.into(), tool_calls: vec![], input_tokens: 1, output_tokens: 1, cached_input_tokens: 0, finish_reason: None, reasoning_tokens: None }
    }
    fn scan_registry(route_key: &str, json: &str) -> ProviderRegistry {
        let mut reg = ProviderRegistry::new();
        reg.insert_scan(route_key, "p", 8, Box::new(FakeChatBackend::repeating(scan_turn(json))), std::time::Duration::from_secs(5), 0);
        reg
    }
    /// 预置 checkpoint 用的 scanner 记录（hint 作可观测标记）。
    fn checkpointed_scan(cell: &str, hint: &str) -> CellRecord {
        let out = crate::domain::ScanOutput {
            suspicions: vec![crate::domain::Suspicion { file: "a.rs".into(), line_start: 1, line_end: 1, hint: hint.into(), category: None, confidence: 0.5, quote: "q".into() }],
            ..Default::default()
        };
        CellRecord::ok(cell, "p/alpha", crate::domain::CellOutput::Scan(out))
    }

    fn code_args() -> ReviewArgs {
        ReviewArgs { pack_only: false,
            base: None, scope: "working-tree".into(),
            spec: vec![], focus: None,
            block_threshold: None, conf_floor: None,
            background: true, format: "json".into(),
            resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None, test_command: None,
        }
    }
    /// 在 dir 建 git 仓库并 stage 一个含 content 的文件——doc 模式退场后（E10）run_job 测试改走
    /// code 路，working-tree scope 的 changed_files 把 staged 文件当作待评审改动。
    fn git_staged(dir: &std::path::Path, file: &str, content: &str) {
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git").current_dir(dir).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(dir.join(file), content).unwrap();
        run(&["add", file]);
    }
    fn cfg() -> Config {
        Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"alpha":{"roles":{"scanner":{}}}}}}}"#).unwrap()
    }

    #[tokio::test]
    async fn run_job_done_writes_result_and_state() {
        let dir = tempfile::tempdir().unwrap();
        git_staged(dir.path(), "a.rs", "fn main() {\n    let x = 1;\n}\n");
        // alpha 报一条锚得到的疑点，quote 在 a.rs 第 2 行
        let registry = scan_registry("p/alpha", r#"{"suspicions":[{"file":"a.rs","line_start":1,"line_end":1,"hint":"t","confidence":0.9,"quote":"let x = 1;"}],"scanned_areas":[{"file":"a.rs","ranges":["1-3"]}]}"#);
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let store = Arc::new(crate::state::JobStore::open(&ws).unwrap());
        store.put_state_for_test(&JobState::new("j1", 500)).unwrap();

        let code = run_job("j1", &code_args(), &registry, &cfg(), dir.path(), 500, Arc::clone(&store)).await;

        let st = store.read_state("j1").unwrap();
        assert_eq!(st.status, JobStatus::Done);
        assert_eq!(st.exit_code, Some(code));
        assert!(st.result_path.is_some());
        assert_eq!(st.roles, vec![Role::Scanner], "on_roles 落 JobState.roles");
        assert_eq!(st.phase_history.iter().map(|p| p.phase).collect::<Vec<_>>(), vec![crate::state::Phase::Collecting, crate::state::Phase::Scan, crate::state::Phase::Finalizing]);
        // result.json 落盘且是合法 JSON（scan-only）
        let res = store.read_result("j1").unwrap();
        assert!(res.contains("\"verdict\": \"scan-only\"") && res.contains("\"suspicions\""), "{res}");
        assert_eq!(code, 0);
        // Task 5：零 checkpoint 失败的正常路径，report.run.warnings 不应长出「检查点落盘
        // 失败」条目——契约冻结面「正常路径 report 逐字节不变」的直接锚。
        assert!(!res.contains("检查点落盘失败"), "零失败路径不应出现披露 warning: {res}");
        // Bug C 回归：run_job 收尾 write_state 必须以 sink 运行期写的最新 progress 为底稿
        // 叠加终态字段，而不是复用函数入口那份陈旧 st（否则 done/total 会被重置回 0/0）。
        assert_eq!(st.progress.done, 1, "收尾不应吞掉 sink 写入的 progress");
        assert_eq!(st.progress.total, 1, "收尾不应吞掉 sink 写入的 progress");
    }

    #[tokio::test]
    async fn run_job_failed_records_error() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let store = Arc::new(crate::state::JobStore::open(ws).unwrap());
        store.put_state_for_test(&JobState::new("j2", 0)).unwrap();
        // 非 git 目录 → changed_files 失败 → run_review_dispatch Err → failed
        let registry = ProviderRegistry::new();
        let code = run_job("j2", &code_args(), &registry, &cfg(), dir.path(), 0, Arc::clone(&store)).await;
        assert_eq!(code, 3); // 五档：dispatch 失败落 partial_report → Verdict::Incomplete → 3
        let st = store.read_state("j2").unwrap();
        assert_eq!(st.status, JobStatus::Failed);
        assert!(st.error.is_some());
    }

    /// R2.2：dispatch 失败但已有 checkpoint（partial 里一条 ok:true 记录）→ result.json 不能
    /// 静默丢——应写 verdict=incomplete，findings 并入已 checkpoint 的记录，warnings 含 "partial:"。
    #[tokio::test]
    async fn run_job_failure_writes_incomplete_partial_report() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let store = Arc::new(crate::state::JobStore::open(ws).unwrap());
        store.put_state_for_test(&JobState::new("j2b", 0)).unwrap();
        // 预置一条 partial（模拟崩溃前已完成 1 cell）
        store.append_partial("j2b", &checkpointed_scan("scan[g1]@p/alpha#1", "checkpointed")).unwrap();

        // 非 git 目录 → changed_files 失败 → run_review_dispatch Err，但已有 partial 记录
        let registry = ProviderRegistry::new();
        let code = run_job("j2b", &code_args(), &registry, &cfg(), dir.path(), 0, Arc::clone(&store)).await;

        assert_eq!(code, 3); // 五档：partial 报告 verdict=incomplete → 3
        let st = store.read_state("j2b").unwrap();
        assert_eq!(st.status, JobStatus::Failed);
        assert!(st.error.is_some());
        let res = store.read_result("j2b").unwrap();
        assert!(res.contains("\"incomplete\""), "verdict 应为 incomplete: {res}");
        assert!(res.contains("partial:"), "warnings 应含 partial 标记: {res}");
        assert!(res.contains("checkpointed"), "已 checkpoint 的疑点不应丢失: {res}");
        let parsed: ReviewReport = serde_json::from_str(&res).unwrap();
        assert_eq!(parsed.scan.as_ref().map(|sc| sc.suspicions.len()), Some(1));
    }

    /// 永不完成的 backend——保证 dispatch 分支不可能赢得 deadline race（而非靠真实短
    /// sleep+timeout 的时序假设）。用于 R1.2 deadline 超时测试，0 依赖、0 wall-clock 等待。
    struct HangBackend;

    #[async_trait::async_trait]
    impl ChatBackend for HangBackend {
        async fn turn(&self, _m: &[ChatMessage], _t: &[crate::tools::ToolSpec]) -> Result<AssistantTurn, crate::provider::ProviderError> {
            std::future::pending::<()>().await;
            unreachable!("HangBackend never resolves")
        }
    }

    /// R1.2：`deadline_at` 已过期 + dispatch 永不完成 → run_job 走超时分支，写 TimedOut +
    /// 部分报告（不静默丢 deadline 前已 checkpoint 的 findings）。
    #[tokio::test]
    async fn run_job_timeout_writes_incomplete_partial_report() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        // git 仓库 + staged 改动：dispatch 得以推进到 provider（HangBackend 挂起），
        // 让 deadline 分支赢得 select（非 git 目录会先在 changed_files 处 Err，选不到超时分支）。
        git_staged(dir.path(), "a.rs", "fn main() {\n    let x = 1;\n}\n");

        let mut st = JobState::new("j-to", 0);
        st.deadline_at = Some(0); // 已过期（远小于 now）→ select 立即走超时分支
        let store = Arc::new(crate::state::JobStore::open(ws).unwrap());
        store.put_state_for_test(&st).unwrap();
        // 预置一条 partial（模拟 deadline 前已完成 1 cell）
        store.append_partial("j-to", &checkpointed_scan("scan[g1]@p/alpha#1", "before-deadline")).unwrap();

        let mut registry = ProviderRegistry::new();
        registry.insert_scan("p/alpha", "p", 8, Box::new(HangBackend), std::time::Duration::from_secs(3600), 0);
        let code = run_job("j-to", &code_args(), &registry, &cfg(), dir.path(), 0, Arc::clone(&store)).await;

        assert_eq!(code, 3); // 五档：超时 partial 报告 verdict=incomplete → 3
        let st = store.read_state("j-to").unwrap();
        assert_eq!(st.status, JobStatus::TimedOut);
        let res = store.read_result("j-to").unwrap();
        assert!(res.contains("\"incomplete\""), "verdict 应为 incomplete: {res}");
        assert!(res.contains("partial:"), "warnings 应含 partial 标记: {res}");
        assert!(res.contains("before-deadline"), "deadline 前已 checkpoint 的 finding 不应丢失: {res}");
    }

    /// I2：pre-cancel 场景——run_job 读到 Cancelled 状态时应直接返回，不覆盖为 Running/Done，
    /// 且不写 result.json。
    #[tokio::test]
    async fn run_job_honours_pre_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();

        // 先写一个 Cancelled 状态（模拟 cancel 命令先于 worker 启动）
        let mut pre = JobState::new("j3", 0);
        pre.status = JobStatus::Cancelled;
        pre.exit_code = Some(0);
        let store = Arc::new(crate::state::JobStore::open(ws).unwrap());
        store.put_state_for_test(&pre).unwrap();

        let registry = ProviderRegistry::new();
        let _code = run_job("j3", &code_args(), &registry, &cfg(), dir.path(), 0, Arc::clone(&store)).await;

        // 状态仍应是 Cancelled
        let st = store.read_state("j3").unwrap();
        assert_eq!(st.status, JobStatus::Cancelled, "pre-cancel 状态不应被覆盖");
        // 不应写 result.json
        assert!(
            store.read_result("j3").is_none(),
            "Cancelled 提前返回不应留下 result.json"
        );
    }

    /// 终审 Important 回归：修复前 run_job 的 `== Cancelled` 早退与裸 `write_state` 写
    /// Running 之间存在窗口——本测试钉住修复后的行为：读到 Cancelled（带 exit_code=7）时，
    /// (a) run_job 必须原样返回该 exit_code；(b) 调用后状态仍是 Cancelled，未被覆写为
    /// Running（首个终态封盘）。早退发生在建 sink/dispatch 之前，故不需要真 provider 跑完。
    #[tokio::test]
    async fn run_job_pre_cancel_returns_its_exit_code_and_does_not_overwrite_status() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();

        // 先写一个 Cancelled 状态、带具体 exit_code=7（模拟 cancel 命令先于 worker 启动）
        let mut pre = JobState::new("j-precancel-7", 0);
        pre.status = JobStatus::Cancelled;
        pre.exit_code = Some(7);
        let store = Arc::new(crate::state::JobStore::open(ws).unwrap());
        store.put_state_for_test(&pre).unwrap();

        let registry = ProviderRegistry::new();
        let code = run_job("j-precancel-7", &code_args(), &registry, &cfg(), dir.path(), 0, Arc::clone(&store)).await;

        // (a) 返回值必须是 Cancelled 快照里的 exit_code，而非重新跑出的别的码
        assert_eq!(code, 7, "run_job 应原样返回 pre-cancel 的 exit_code");

        // (b) 状态仍应是 Cancelled，未被裸写覆盖为 Running
        let st = store.read_state("j-precancel-7").unwrap();
        assert_eq!(st.status, JobStatus::Cancelled, "pre-cancel 状态不应被覆写为 Running");
    }

    /// 同一窄洞的 Done 终态变体：非 Cancelled 的终态也应早退，不被覆写为 Running。
    #[tokio::test]
    async fn run_job_pre_done_does_not_overwrite_status() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();

        let mut pre = JobState::new("j-predone", 0);
        pre.status = JobStatus::Done;
        pre.exit_code = Some(0);
        let store = Arc::new(crate::state::JobStore::open(ws).unwrap());
        store.put_state_for_test(&pre).unwrap();

        let registry = ProviderRegistry::new();
        let code = run_job("j-predone", &code_args(), &registry, &cfg(), dir.path(), 0, Arc::clone(&store)).await;

        assert_eq!(code, 0);
        let st = store.read_state("j-predone").unwrap();
        assert_eq!(st.status, JobStatus::Done, "pre-done 状态不应被覆写为 Running");
    }

    /// Bug B 回归：`JobCellSink::on_cell` 的 read_state→改→write_state 段必须串行化。
    /// 无锁时并发 N 次调用会各自读到同一旧 progress、各自 +1、后写覆盖先写，最终
    /// `progress.done < N`；加全局锁后必须精确等于 N。用真实 OS 线程（而非 tokio 任务，
    /// 后者在单线程 runtime 下不会真正并发）制造竞争。
    #[test]
    fn job_cell_sink_on_cell_concurrent_no_lost_update() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_path_buf();
        let store = Arc::new(crate::state::JobStore::open(&ws).unwrap());
        store.put_state_for_test(&JobState::new("jc1", 0)).unwrap();
        let sink = Arc::new(JobCellSink {
            store: Arc::clone(&store),
            job_id: "jc1".to_string(),
            ckpt_failures: std::sync::atomic::AtomicU32::new(0),
        });

        let n = 20usize;
        let handles: Vec<_> = (0..n)
            .map(|i| {
                let sink = sink.clone();
                std::thread::spawn(move || {
                    sink.on_cell(crate::state::CellRecord::ok(
                        format!("cell{i}"),
                        "m".to_string(),
                        crate::domain::CellOutput::review(vec![]),
                    ));
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let st = store.read_state("jc1").unwrap();
        assert_eq!(
            st.progress.done, n as u32,
            "并发 on_cell 不应丢更新（无锁会 < {n}）"
        );
    }

    #[test]
    fn reap_dead_running_job_to_failed() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        // 起一个立即退出的子进程做「死 worker」
        let child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        let mut child = child; child.wait().unwrap();
        let mut st = JobState::new("j", 0);
        st.status = JobStatus::Running;
        st.pid = Some(pid);
        st.pid_start = None; // 降级纯存活探测；pid 已回收 → 死
        let store = crate::state::JobStore::open(ws).unwrap();
        store.put_state_for_test(&st).unwrap();
        let out = reap_if_dead(&store, store.read_state("j").unwrap());
        assert_eq!(out.status, JobStatus::Failed);
        assert_eq!(store.read_state("j").unwrap().status, JobStatus::Failed);
        assert!(out.error.as_deref().unwrap_or("").contains("reaped"));
    }

    #[test]
    fn reap_leaves_alive_running_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let me = std::process::id(); // 本进程恒活
        let mut st = JobState::new("j", 0);
        st.status = JobStatus::Running;
        st.pid = Some(me);
        st.pid_start = crate::procinfo::start_time(me);
        let store = crate::state::JobStore::open(ws).unwrap();
        store.put_state_for_test(&st).unwrap();
        let out = reap_if_dead(&store, store.read_state("j").unwrap());
        assert_eq!(out.status, JobStatus::Running); // 活着 → 不动
    }

    #[test]
    fn reap_ignores_terminal_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut st = JobState::new("j", 0);
        st.status = JobStatus::Done;
        st.pid = Some(999_999); // 不存在的 pid，但已终态 → 不该被碰
        let store = crate::state::JobStore::open(ws).unwrap();
        store.put_state_for_test(&st).unwrap();
        let out = reap_if_dead(&store, store.read_state("j").unwrap());
        assert_eq!(out.status, JobStatus::Done);
    }

    /// dogfood F4 回归：worker 在写 Running 前早夭（启动期被杀/args 读失败 exit(2) 不落终态）
    /// 会留下 Pending + 死 pid——reap 必须一并收掉，否则永久悬置、status/list 永不修复。
    #[test]
    fn reap_dead_pending_job_to_failed() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        let mut child = child; child.wait().unwrap();
        let mut st = JobState::new("j", 0); // JobState::new 即 Pending
        st.pid = Some(pid);
        st.pid_start = None;
        let store = crate::state::JobStore::open(ws).unwrap();
        store.put_state_for_test(&st).unwrap();
        assert_eq!(st.status, JobStatus::Pending, "前提：新建态是 Pending");
        let out = reap_if_dead(&store, store.read_state("j").unwrap());
        assert_eq!(out.status, JobStatus::Failed);
        assert_eq!(store.read_state("j").unwrap().status, JobStatus::Failed);
    }

    /// dogfood F4 边界：pid 尚未登记的创建窗口（父进程两次写之间）——无 pid 无从判定，
    /// 保守放过、原样吐回，不误收刚创建的 job。
    #[test]
    fn reap_leaves_pending_without_pid_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let st = JobState::new("j", 0); // Pending，pid=None
        let store = crate::state::JobStore::open(ws).unwrap();
        store.put_state_for_test(&st).unwrap();
        let out = reap_if_dead(&store, store.read_state("j").unwrap());
        assert_eq!(out.status, JobStatus::Pending);
        assert_eq!(store.read_state("j").unwrap().status, JobStatus::Pending);
    }

    /// dogfood F2 回归（语义级）：父进程 spawn 后的 pid 登记必须走守卫、只补 pid/pid_start
    /// 不动 status——子进程抢先推进到 Running 时不得覆回 Pending；抢先落终态时拒写。
    #[test]
    fn guarded_pid_registration_preserves_status_and_respects_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let store = crate::state::JobStore::open(ws).unwrap();
        // 场景 A：子进程已抢先 Running → 登记只补 pid，status 保持 Running。
        let mut st = JobState::new("j", 0);
        st.status = JobStatus::Running;
        store.put_state_for_test(&st).unwrap();
        let wrote = store.update_if_active("j", |s| {
            s.pid = Some(12345);
            s.pid_start = Some(67890);
        })
        .unwrap();
        assert!(wrote);
        let fresh = store.read_state("j").unwrap();
        assert_eq!(fresh.status, JobStatus::Running, "登记不得把 Running 覆回 Pending");
        assert_eq!(fresh.pid, Some(12345));
        // 场景 B：子进程已极快失败落终态 → 登记被守卫拒绝，终态与字段原封不动。
        let mut st2 = JobState::new("j2", 0);
        st2.status = JobStatus::Failed;
        st2.exit_code = Some(2);
        store.put_state_for_test(&st2).unwrap();
        let wrote2 = store.update_if_active("j2", |s| {
            s.pid = Some(12345);
            s.pid_start = Some(67890);
        })
        .unwrap();
        assert!(!wrote2, "已终态 → 登记拒写（首个终态封盘）");
        let fresh2 = store.read_state("j2").unwrap();
        assert_eq!(fresh2.status, JobStatus::Failed);
        assert_eq!(fresh2.pid, None, "拒写后不得留下部分字段");
    }

    /// 回归（并发完成竞态）：`reap_if_dead` 拿到的 `st` 是调用方早先读到的陈旧 Running 快照；
    /// 在探活与守卫落笔之间的极小窗口内，worker 已经真完成并把 db 现状推进到 Done（首个终态
    /// 封盘生效）。此时守卫写必被拒——绝不能因此把 stale 快照直接吐回去（会谎报「已完成」的
    /// job 仍是 Running），也绝不能强行把它冲成 Failed（会误杀真完成的 job）。必须重读 db 拿到
    /// 真 Done。
    #[test]
    fn reap_concurrent_completion_not_clobbered() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        let mut child = child; child.wait().unwrap();
        // 调用方（如 status 的 list 分支）早先读到的快照：Running + 死 pid。
        let mut stale = JobState::new("j", 0);
        stale.status = JobStatus::Running;
        stale.pid = Some(pid);
        stale.pid_start = None;
        let store = crate::state::JobStore::open(ws).unwrap();
        store.put_state_for_test(&stale).unwrap();
        // 模拟窗口期内 worker 真完成：db 现状推进到 Done（守卫写，此时尚未终态封盘）。
        let wrote = store.update_if_active("j", |s| {
            s.status = JobStatus::Done;
            s.exit_code = Some(0);
        })
        .unwrap();
        assert!(wrote, "模拟的「worker 真完成」写应成功（此时仍是 Running，未终态封盘）");
        // reap_if_dead 拿到的仍是旧的 stale 快照，但 db 里已是 Done：
        // 守卫写应被终态封盘拒绝，必须重读拿到真 Done——不可被冲成 Failed，也不可原样报告 Running。
        let out = reap_if_dead(&store, stale);
        assert_eq!(out.status, JobStatus::Done, "必须重读拿到真 Done，而非陈旧 Running 快照或误杀 Failed");
        assert_eq!(store.read_state("j").unwrap().status, JobStatus::Done, "db 现状不应被 reap 覆盖");
    }

    #[test]
    fn mark_failed_panic_sets_failed_with_message_only() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut st = JobState::new("j", 0);
        st.status = JobStatus::Running;
        let store = crate::state::JobStore::open(ws).unwrap();
        store.put_state_for_test(&st).unwrap();
        mark_failed_panic(&store, "j", "index out of bounds");
        let back = store.read_state("j").unwrap();
        assert_eq!(back.status, JobStatus::Failed);
        let err = back.error.unwrap_or_default();
        assert!(err.contains("panic"), "error 应标 panic: {err}");
        assert!(err.contains("index out of bounds"));
    }

    #[test]
    fn mark_failed_panic_respects_terminal_freeze() {
        // panic 发生在真完成之后 → 守卫拒写、保留 Done
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut st = JobState::new("j", 0);
        st.status = JobStatus::Done;
        let store = crate::state::JobStore::open(ws).unwrap();
        store.put_state_for_test(&st).unwrap();
        mark_failed_panic(&store, "j", "late panic");
        assert_eq!(store.read_state("j").unwrap().status, JobStatus::Done);
    }

    // D5 回归锚：worker 起手回填 deadline_at 曾是裸 RMW（main.rs 直接 write_state），会把并发
    // cancel 抢先落下的 Cancelled 终态覆盖回去（首个终态封盘第三例绕过）。抽成 stamp_deadline
    // 后改走 update_if_active 守卫式更新，以下三例分别钉住：终态不被覆写、未设 deadline 被写、
    // 已设 deadline 不被二次覆写（is_none 判生效）。

    #[test]
    fn stamp_deadline_respects_terminal_freeze() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut st = JobState::new("j", 0);
        st.status = JobStatus::Cancelled;
        st.deadline_at = None;
        let store = crate::state::JobStore::open(ws).unwrap();
        store.put_state_for_test(&st).unwrap();
        stamp_deadline(&store, "j", 0);
        let back = store.read_state("j").unwrap();
        assert_eq!(back.status, JobStatus::Cancelled, "终态封盘：status 不应被 stamp_deadline 动");
        assert!(back.deadline_at.is_none(), "终态封盘：deadline_at 也不应被写入");
    }

    #[test]
    fn stamp_deadline_writes_when_unset() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut st = JobState::new("j", 100);
        st.status = JobStatus::Running;
        st.deadline_at = None;
        let store = crate::state::JobStore::open(ws).unwrap();
        store.put_state_for_test(&st).unwrap();
        stamp_deadline(&store, "j", 100);
        let back = store.read_state("j").unwrap();
        assert_eq!(back.deadline_at, Some(100 + super::JOB_DEADLINE_SECS), "尚未设置时应被写入");
    }

    #[test]
    fn stamp_deadline_does_not_overwrite_existing() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut st = JobState::new("j", 100);
        st.status = JobStatus::Running;
        st.deadline_at = Some(999); // 已设置——例如 worker 重入或状态文件已带值
        let store = crate::state::JobStore::open(ws).unwrap();
        store.put_state_for_test(&st).unwrap();
        stamp_deadline(&store, "j", 100);
        let back = store.read_state("j").unwrap();
        assert_eq!(back.deadline_at, Some(999), "已设置的 deadline 不应被二次覆写（is_none 判生效）");
    }
}

#[cfg(test)]
mod resume_tests {
    use super::*;
    use crate::prompt::ScanPrompt;
    use crate::provider::tool_loop::{AssistantTurn, ChatMessage, FakeChatBackend};
    use crate::state::CellInput;
    use std::sync::Arc;

    /// 每次调用都回同一份疑点 JSON（quote "marker" 在各 cell 的文件里可锚）；`Arc` 句柄留在测试侧观测调用次数与 system。
    fn recording_registry(route_key: &str, file: &str) -> (ProviderRegistry, Arc<FakeChatBackend>) {
        let json = format!(r#"{{"suspicions":[{{"file":"{file}","line_start":1,"line_end":1,"hint":"issue","confidence":0.6,"quote":"marker"}}],"scanned_areas":[]}}"#);
        let backend = Arc::new(FakeChatBackend::repeating(AssistantTurn {
            text: json, tool_calls: vec![], input_tokens: 1, output_tokens: 1, cached_input_tokens: 0, finish_reason: None, reasoning_tokens: None,
        }));
        let mut reg = ProviderRegistry::new();
        reg.insert_scan(route_key, "p", 8, Box::new(Arc::clone(&backend)), std::time::Duration::from_secs(5), 0);
        (reg, backend)
    }
    fn input(cell: &str, model: &str, system: &str) -> CellInput {
        CellInput { template_hash: None, cell: cell.into(), model: model.into(), prompt: system.into(), scan: Some(ScanPrompt { system: system.into(), blocks: vec![] }) }
    }
    fn done_scan(cell: &str, model: &str, file: &str) -> CellRecord {
        let out = crate::domain::ScanOutput {
            suspicions: vec![crate::domain::Suspicion { file: file.into(), line_start: 1, line_end: 1, hint: "issue".into(), category: None, confidence: 0.6, quote: "marker".into() }],
            ..Default::default()
        };
        CellRecord::ok(cell, model, crate::domain::CellOutput::Scan(out))
    }
    fn args() -> ReviewArgs {
        ReviewArgs { pack_only: false,
            base: None, scope: "working-tree".into(), spec: vec![],
            focus: None,
            block_threshold: None, conf_floor: None, background: true,
            format: "json".into(), resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None, test_command: None,
        }
    }
    fn systems(b: &FakeChatBackend) -> Vec<String> {
        (0..b.turn_count()).filter_map(|i| match &b.messages_at(i)[0] { ChatMessage::System(s) => Some(s.clone()), _ => None }).collect()
    }

    /// T7 核心（批 F-4 scan 版）：manifest 3 cell、partial 已完成 1 → resume 只对缺失的 2 个 cell 用 manifest 的 scan 请求重放，
    /// 报告含全部 3 个 cell（reused 1 + completed 2）、三组各一疑点、verdict scan-only；回写 proj.db。
    #[tokio::test]
    async fn resume_only_runs_missing_cells() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        std::fs::write(ws.join("c.rs"), "marker\n").unwrap();
        let inputs = vec![input("scan[g1]@p/m#1", "p/m", "P1"), input("scan[g2]@p/m#1", "p/m", "P2"), input("scan[g3]@p/m#1", "p/m", "P3")];
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &inputs).unwrap();
        store.append_partial("j", &done_scan("scan[g1]@p/m#1", "p/m", "c.rs")).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        let (reg, backend) = recording_registry("p/m", "c.rs");

        let (report, code) = run_resume("j", &reg, &Config::default(), ws, &store).await;

        let sys = systems(&backend);
        assert_eq!(sys.len(), 2, "只应对缺失的 2 个 cell 调用: {sys:?}");
        assert!(sys.contains(&"P2".to_string()) && sys.contains(&"P3".to_string()) && !sys.contains(&"P1".to_string()));
        assert_eq!(report.verdict, Verdict::ScanOnly);
        assert_eq!(code, 0);
        let scan = report.scan.as_ref().unwrap();
        assert_eq!(scan.suspicions.len(), 3, "三组各一条（跨组不合并）: {:?}", scan.suspicions);
        assert_eq!(report.cells.len(), 3);
        assert_eq!(report.run.coverage.reused.get(&Role::Scanner).map(Vec::len), Some(1));
        assert_eq!(report.run.coverage.completed.get(&Role::Scanner).map(Vec::len), Some(2));
        let stored: ReviewReport = serde_json::from_str(&store.read_result("j").unwrap()).unwrap();
        assert_eq!(stored.scan.unwrap().suspicions.len(), 3, "proj.db 里的报告应为合并后的 3 条");
        assert!(store.read_result_md("j").unwrap().contains("scan-only"), "md 快照同步回写");
    }

    /// 同组两条目副本（`scan[g1]@alpha#1` / `scan[g1]@beta#1`）：只完成 alpha → beta 必须被补跑，合并结果 agreement 2、非 Incomplete。
    #[tokio::test]
    async fn resume_missing_entry_copy_is_completed() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        std::fs::write(ws.join("c.rs"), "marker\n").unwrap();
        let inputs = vec![input("scan[g1]@p/alpha#1", "p/alpha", "P-alpha"), input("scan[g1]@p/beta#1", "p/beta", "P-beta")];
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &inputs).unwrap();
        store.append_partial("j", &done_scan("scan[g1]@p/alpha#1", "p/alpha", "c.rs").with_family("alpha")).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        let (reg, backend) = recording_registry("p/beta", "c.rs");
        let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"alpha":{"roles":{"scanner":{}}},"beta":{"roles":{"scanner":{}}}}}}}"#).unwrap();

        let (report, _code) = run_resume("j", &reg, &cfg, ws, &store).await;

        assert_eq!(systems(&backend), vec!["P-beta".to_string()], "只补跑缺失的 beta 副本");
        assert_ne!(report.verdict, Verdict::Incomplete);
        let scan = report.scan.unwrap();
        assert_eq!((scan.suspicions.len(), scan.suspicions[0].agreement), (1, 2), "{:?}", scan.suspicions);
        assert_eq!(scan.suspicions[0].families, vec!["alpha", "beta"]);
    }

    /// 评审修 1：旧格式作业（partial 里是 reviewer 形态记录、且全部 ok）→ 不重放、不覆盖为空 scan-only，直接 Incomplete + 理由。
    #[tokio::test]
    async fn resume_refuses_pre_scan_job_with_reviewer_records() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &[CellInput { cell: "bundle[a.rs]@p/m".into(), model: "p/m".into(), prompt: "P".into(), scan: None, template_hash: None }]).unwrap();
        store.append_partial("j", &CellRecord::ok("bundle[a.rs]@p/m", "p/m", crate::domain::CellOutput::review(vec![]))).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        store.write_result("j", "{\"marker\": \"original\"}").unwrap();
        let (reg, backend) = recording_registry("p/m", "c.rs");
        let (report, code) = run_resume("j", &reg, &Config::default(), ws, &store).await;
        assert_eq!((report.verdict, code), (Verdict::Incomplete, 3));
        assert_eq!(backend.turn_count(), 0);
        assert!(report.run.warnings.iter().any(|w| w.contains("non-scanner cell records")), "{:?}", report.run.warnings);
        assert_eq!(store.read_result("j").as_deref(), Some("{\"marker\": \"original\"}"), "原报告不被空 scan-only 覆盖");
    }

    /// 评审修 6：重放 cell 的模板哈希取 manifest 落盘值；与当前二进制不一致 → warning。评审修 5：原报告的组骨架 / head_sha / 覆盖自报沿用。
    #[tokio::test]
    async fn resume_keeps_manifest_template_hash_and_prior_report_fields() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        std::fs::write(ws.join("c.rs"), "marker\n").unwrap();
        let mut inp = input("scan[g1]@p/m#1", "p/m", "P1");
        inp.template_hash = Some("aaaa+bbbb".into());
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &[inp]).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        let prior = ReviewReport {
            schema_version: "4".into(), verdict: Verdict::Incomplete, stats: Stats::default(), findings: vec![], clusters: vec![],
            run: RunMeta { head_sha: Some("deadbeef".into()), grouping: Some(crate::bundle::GroupingStats { gate: crate::bundle::Gate::Single, groups: 1, edges: 0, cross_edges: 0, cross_import: Some(0), cross_reference: Some(0), cross_same_dir: Some(0), pooled_small_components: Some(0), isolated: 0, over_budget: 0 }), ..Default::default() },
            scan: Some(ScanReport { groups: vec![ScanGroup { label: "c.rs".into(), files: vec!["c.rs".into()], cells: vec![], suspicions: 0, pack: Default::default() }], suspicions: vec![], scanned_coverage: Some(0.75), stats: Default::default() }),
            cells: vec![],
        };
        store.write_result("j", &serde_json::to_string(&prior).unwrap()).unwrap();
        let (reg, _backend) = recording_registry("p/m", "c.rs");
        let (report, code) = run_resume("j", &reg, &Config::default(), ws, &store).await;
        assert_eq!((report.verdict, code), (Verdict::ScanOnly, 0));
        assert_eq!(report.cells[0].template_hash.as_deref(), Some("aaaa+bbbb"), "取 manifest 哈希，不盖当前值");
        assert!(report.run.warnings.iter().any(|w| w.contains("manifest's template hash")), "{:?}", report.run.warnings);
        let scan = report.scan.as_ref().unwrap();
        assert_eq!((scan.groups[0].label.as_str(), scan.groups[0].files.len(), scan.groups[0].cells.len()), ("c.rs", 1, 1));
        assert_eq!(scan.scanned_coverage, Some(0.75));
        assert_eq!(report.run.head_sha.as_deref(), Some("deadbeef"));
        assert_eq!(report.run.grouping.as_ref().map(|g| g.groups), Some(1));
    }

    /// 旧格式 manifest（无 scan 请求）→ 该 cell 记失败 → Incomplete；不调 backend。
    #[tokio::test]
    async fn resume_legacy_manifest_without_scan_prompt_is_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &[CellInput { template_hash: None, cell: "bundle[a.rs]".into(), model: "p/m".into(), prompt: "P".into(), scan: None }]).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        let (reg, backend) = recording_registry("p/m", "c.rs");
        let (report, code) = run_resume("j", &reg, &Config::default(), ws, &store).await;
        assert_eq!((report.verdict, code), (Verdict::Incomplete, 3));
        assert_eq!(backend.turn_count(), 0);
        assert!(report.run.warnings.iter().any(|w| w.contains("旧格式作业")), "{:?}", report.run.warnings);
    }

    /// 假阳性防护：manifest 从未写出（job 在扇出前崩溃）→ resume 不得静默 scan-only，必须 Incomplete + 非零退出，且不调 backend。
    #[tokio::test]
    async fn resume_empty_manifest_is_incomplete_not_approve() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        let (reg, backend) = recording_registry("p/m", "c.rs");
        let (report, code) = run_resume("j", &reg, &Config::default(), ws, &store).await;
        assert_eq!(report.verdict, Verdict::Incomplete, "空 manifest 不得静默 scan-only");
        assert_ne!(code, 0, "退出码非 0");
        assert_eq!(backend.turn_count(), 0, "无 cell 可跑，不应调 backend");
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    /// 批 C followup：`review.grouping` 子表接到 `GroupingOpts`（缺省 4 / 200 / 10 与内置一致）。
    #[test]
    fn grouping_opts_read_review_grouping_config() {
        let d = grouping_opts(&Config::default().review.grouping, 1000);
        assert_eq!((d.min_files, d.bundle_line_threshold, d.max_files_per_group, d.budget_bytes), (4, 200, 10, Some(1000)));
        let c = Config::from_json(r#"{"review":{"grouping":{"minFiles":1,"bundleLineThreshold":0,"maxFilesPerGroup":2}}}"#).unwrap();
        let o = grouping_opts(&c.review.grouping, 5);
        assert_eq!((o.min_files, o.bundle_line_threshold, o.max_files_per_group, o.budget_bytes), (1, 0, 2, Some(5)));
    }

}

#[cfg(test)]
mod code_review_tests {
    use super::*;
    use crate::evidence::git::FileChange;
    use crate::provider::tool_loop::{AssistantTurn, FakeChatBackend};
    use std::io::Write;

    fn write(dir: &std::path::Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() { std::fs::create_dir_all(parent).unwrap(); }
        std::fs::File::create(p).unwrap().write_all(body.as_bytes()).unwrap();
    }
    fn args() -> ReviewArgs {
        ReviewArgs { pack_only: false,
            base: None, scope: "working-tree".into(),
            spec: vec![], focus: None,
            block_threshold: None, conf_floor: None,
            background: false, format: "json".into(),
            resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None, test_command: None,
        }
    }
    fn registry(entries: &[(&str, &str)]) -> ProviderRegistry {
        let mut reg = ProviderRegistry::new();
        for (key, json) in entries {
            let turn = AssistantTurn { text: (*json).into(), tool_calls: vec![], input_tokens: 1, output_tokens: 1, cached_input_tokens: 0, finish_reason: None, reasoning_tokens: None };
            reg.insert_scan(*key, "p", 8, Box::new(FakeChatBackend::repeating(turn)), std::time::Duration::from_secs(5), 0);
        }
        reg
    }
    fn susp(hint: &str, quote: &str, conf: f32) -> String {
        format!(r#"{{"file":"src/a.rs","line_start":1,"line_end":1,"hint":"{hint}","confidence":{conf},"quote":"{quote}"}}"#)
    }

    /// 两条目都报真疑点 → 合并 agreement 2；编造 quote 只 m 报、锚定丢弃并记 cell warn；scan-only / 0。
    #[tokio::test]
    async fn scan_review_anchors_real_drops_fabricated_and_merges_families() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.rs", "fn f() {\n    db.write(token);\n}\n");
        let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}},"m2":{"roles":{"scanner":{}}}}}}}"#).unwrap();
        let reg = registry(&[
            ("p/m", &format!(r#"{{"suspicions":[{},{}],"scanned_areas":[]}}"#, susp("write", "db.write(token);", 0.8), susp("fab", "this is fabricated", 0.9))),
            ("p/m2", &format!(r#"{{"suspicions":[{}],"scanned_areas":[]}}"#, susp("unvalidated", "db.write(token);", 0.6))),
        ]);
        let changes = vec![FileChange { path: "src/a.rs".into(), diff: "@@ -2,1 +2,1 @@\n+    db.write(token);".into() }];
        let (report, code) = run_code_review(&args(), &reg, &cfg, changes, dir.path(), None, None).await.unwrap();
        assert_eq!((report.verdict, code), (Verdict::ScanOnly, 0));
        let scan = report.scan.as_ref().unwrap();
        assert_eq!(scan.suspicions.len(), 1, "编造丢弃、真疑点跨家族合并: {:?}", scan.suspicions);
        assert_eq!((scan.suspicions[0].agreement, scan.suspicions[0].suspicion.line_start), (2, 2));
        assert_eq!(scan.suspicions[0].families, vec!["m", "m2"]);
        assert_eq!(report.cells.len(), 2);
        assert!(report.cells[0].warn.as_deref().unwrap_or("").contains("1 suspicions dropped"), "{:?}", report.cells[0].warn);
        assert!(report.cells[1].warn.is_none());
        assert_eq!(report.run.models, vec!["p/m", "p/m2"]);
        assert_eq!(report.run.template_hashes.len(), 2);
        assert_eq!(report.run.grouping.as_ref().map(|g| g.groups), Some(1));
        assert_eq!(scan.groups.len(), 1);
        assert_eq!((scan.groups[0].cells.len(), scan.groups[0].suspicions), (2, 1));
    }

    /// 空变更集：0 cell、warning 可观测、scan-only / 0（不是假绿灯 approve）。
    #[tokio::test]
    async fn scan_review_empty_changes_is_scan_only_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}}}}}}"#).unwrap();
        let reg = registry(&[("p/m", r#"{"suspicions":[],"scanned_areas":[]}"#)]);
        let (report, code) = run_code_review(&args(), &reg, &cfg, vec![], dir.path(), None, None).await.unwrap();
        assert_eq!((report.verdict, code), (Verdict::ScanOnly, 0));
        assert!(report.cells.is_empty() && report.scan.as_ref().unwrap().suspicions.is_empty());
        assert!(report.run.warnings.iter().any(|w| w.contains("no reviewable evidence")), "{:?}", report.run.warnings);
        assert_eq!(report.scan.as_ref().unwrap().scanned_coverage, None);
    }

    /// 未配置 scanner → 配置错误（Err，不产报告）。
    #[tokio::test]
    async fn scan_review_errors_without_scanner_role() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"reviewer":{}}}}}}}"#).unwrap();
        let err = run_code_review(&args(), &ProviderRegistry::new(), &cfg, vec![], dir.path(), None, None).await.unwrap_err();
        assert!(err.to_string().contains("scanner"), "{err}");
    }

    #[tokio::test]
    async fn scan_review_stamps_head_sha_and_test_command_into_runmeta() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}}}}}}"#).unwrap();
        let reg = registry(&[("p/m", r#"{"suspicions":[],"scanned_areas":[]}"#)]);
        // K1：无 --test-command 时按项目推断（Cargo.toml → cargo test）进 RunMeta。
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"t\"\n").unwrap();
        let (report, _) = run_code_review(&args(), &reg, &cfg, vec![], dir.path(), Some("abc123".into()), None).await.unwrap();
        assert_eq!(report.run.head_sha.as_deref(), Some("abc123"));
        assert_eq!(report.run.test_command.as_deref(), Some("cargo test"));
        let over = ReviewArgs { test_command: Some("make check".into()), ..args() };
        let (report2, _) = run_code_review(&over, &reg, &cfg, vec![], dir.path(), None, None).await.unwrap();
        assert_eq!(report2.run.test_command.as_deref(), Some("make check"));
        assert_eq!(report2.run.head_sha, None, "非 git 环境如实 None，不编造");
    }

    /// 覆盖自报：改动行 2 被 `1-3` 覆盖 → 1.0；只报 `5-9` → 0.0；两文件各一改动行、只覆盖其一 → 0.5。
    #[test]
    fn scanned_coverage_ratio_cases() {
        use crate::bundle::Bundle;
        use crate::orchestrate::ScanCellResult;
        let b = |path: &str| Bundle::from_files(vec![FileChange { path: path.into(), diff: "@@ -2,1 +2,1 @@\n+x".into() }]);
        let cell = |areas: Vec<(&str, &str)>| {
            let out = crate::domain::ScanOutput {
                scanned_areas: areas.into_iter().map(|(f, r)| crate::domain::ScannedArea { file: f.into(), ranges: vec![r.into()] }).collect(),
                ..Default::default()
            };
            ScanCellResult { cell_id: "c".into(), group: 0, route_key: "p/m".into(), family: "m".into(), sample: 1, record: CellRecord::ok("c", "p/m", crate::domain::CellOutput::Scan(out.clone())), output: Some(out) }
        };
        assert_eq!(scanned_coverage(&[b("a.rs")], &[cell(vec![("a.rs", "1-3")])]), Some(1.0));
        assert_eq!(scanned_coverage(&[b("a.rs")], &[cell(vec![("a.rs", "5-9")])]), Some(0.0));
        assert_eq!(scanned_coverage(&[b("a.rs"), b("b.rs")], &[cell(vec![("a.rs", "2-2")])]), Some(0.5));
        assert_eq!(scanned_coverage(&[], &[cell(vec![("a.rs", "1-3")])]), None, "无改动行 → None");
        assert_eq!(scanned_coverage(&[b("a.rs")], &[cell(vec![("a.rs", "x-y"), ("a.rs", "0-3")])]), Some(0.0), "坏区间不计");
        // 评审修 10：删除点不进分母——纯删除 → None；混合 diff 只计 + 行
        let del = Bundle::from_files(vec![FileChange { path: "old.rs".into(), diff: "@@ -1,3 +0,0 @@\n-a\n-b\n-c".into() }]);
        assert_eq!(scanned_coverage(std::slice::from_ref(&del), &[cell(vec![])]), None, "纯删除无可扫改动行");
        assert_eq!(scanned_coverage(&[del, b("a.rs")], &[cell(vec![("a.rs", "1-3")])]), Some(1.0), "删除文件不拉低覆盖率");
    }

    /// 评审修 2：partial 报告把失败格也算进 coverage.failed / cells_failed / usage（与 attach_cells 回填的 cells 一致）。
    #[test]
    fn partial_report_keeps_failed_cells_in_coverage_and_usage() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::state::JobStore::open(dir.path()).unwrap();
        let ok = CellRecord::ok("scan[g1]@p/a#1", "p/a", crate::domain::CellOutput::Scan(Default::default()));
        let mut failed = CellRecord::failed("scan[g1]@p/b#1", "p/b", Role::Scanner, "auth error: no key");
        failed.input_tokens = 7;
        store.append_partial("j", &ok).unwrap();
        store.append_partial("j", &failed).unwrap();
        let (r, code) = partial_report(&store, "j", "timed out".into());
        assert_eq!((r.verdict, code), (Verdict::Incomplete, 3));
        assert_eq!(r.run.coverage.failed[&Role::Scanner], vec!["scan[g1]@p/b#1"]);
        assert_eq!(r.run.coverage.completed[&Role::Scanner], vec!["scan[g1]@p/a#1"]);
        assert_eq!(r.run.usage.input_tokens, 7, "失败格烧掉的 usage 入账");
        assert_eq!(r.scan.as_ref().unwrap().stats.cells_failed, 1);
        assert!(r.run.warnings.iter().any(|w| w.starts_with("partial: 1/2")), "{:?}", r.run.warnings);
        assert_eq!(r.cells.len(), 2);
    }

    /// 评审修 9：`thinking_pinned` 只在 (kind, family, id) 真有映射且会发出时为 true。
    #[test]
    fn scan_entry_spec_pinned_requires_a_thinking_mapping() {
        use crate::orchestrate::ScanEntrySpec;
        let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{
            "deepseek-v4-flash":{"roles":{"scanner":{"thinkingLevel":"low"}}},
            "grok-4":{"roles":{"scanner":{"thinkingLevel":"high"}}},
            "gpt-5":{"roles":{"scanner":{"thinkingLevel":"off"}}},
            "qwen3":{"roles":{"scanner":{}}}}}}}"#).unwrap();
        let roles = cfg.active_roles();
        let pinned: std::collections::BTreeMap<String, bool> = roles.scanners.iter().map(|em| (em.id.clone(), ScanEntrySpec::from(em).thinking_pinned)).collect();
        assert!(pinned["deepseek-v4-flash"]);
        assert!(!pinned["grok-4"], "无映射家族不算 pinned");
        assert!(!pinned["gpt-5"], "Off 不算 pinned");
        assert!(!pinned["qwen3"], "未配档位不算 pinned");
    }
}

#[cfg(test)]
mod spec_tests {
    use super::*;
    use crate::provider::tool_loop::{AssistantTurn, ChatMessage, FakeChatBackend};
    use spec::render_spec_section;
    use std::sync::Arc;
    // ---- Task 2：--spec 接入第一趟主评审的接线级回归（集成测试，非纯构造器单测；批 F-4 改看 scan prompt 的 system）----

    fn recorder() -> (ProviderRegistry, Arc<FakeChatBackend>) {
        let backend = Arc::new(FakeChatBackend::repeating(AssistantTurn {
            text: r#"{"suspicions":[],"scanned_areas":[]}"#.into(), tool_calls: vec![], input_tokens: 1, output_tokens: 1, cached_input_tokens: 0, finish_reason: None, reasoning_tokens: None,
        }));
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/m", "p", 8, Box::new(Arc::clone(&backend)), std::time::Duration::from_secs(5), 0);
        (reg, backend)
    }
    fn spec_test_args(spec: Vec<String>) -> ReviewArgs {
        ReviewArgs { pack_only: false,
            base: None, scope: "working-tree".into(), spec, focus: None,
            block_threshold: None, conf_floor: None,
            background: false, format: "json".into(),
            resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None, test_command: None,
        }
    }
    fn system_of(b: &FakeChatBackend) -> String {
        match &b.messages_at(0)[0] { ChatMessage::System(s) => s.clone(), other => panic!("首条须是 system: {other:?}") }
    }

    #[tokio::test]
    async fn scan_review_with_spec_injects_reference_into_shared_head() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn f() {\n    let x = 1;\n}\n").unwrap();
        let spec_path = root.join("design.md");
        std::fs::write(&spec_path, "DESIGN D8: fail-open").unwrap();
        let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}}}}}}"#).unwrap();
        let (reg, backend) = recorder();
        let args = spec_test_args(vec![spec_path.to_string_lossy().to_string()]);
        let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    let x = 1;".into() }];

        run_code_review(&args, &reg, &cfg, changes, root, None, None).await.unwrap();

        assert_eq!(backend.turn_count(), 1, "单格全审默认下第一趟只应有一次调用");
        let sys = system_of(&backend);
        assert_eq!(sys.matches("<spec_docs>").count(), 1, "共享头恰一段 spec_docs（无双壳）: {sys}");
        assert!(sys.contains("DESIGN D8: fail-open"), "spec 内容须原样注入: {sys}");
        assert!(!sys.contains("{{SPEC_DOCS}}") && !sys.contains("<\\/spec_docs>"), "无残留占位、真闭合不被中和: {sys}");
    }

    #[tokio::test]
    async fn scan_review_without_spec_renders_placeholder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn f() {\n    let x = 1;\n}\n").unwrap();
        let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}}}}}}"#).unwrap();
        let (reg, backend) = recorder();
        let args = spec_test_args(vec![]);
        let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    let x = 1;".into() }];

        run_code_review(&args, &reg, &cfg, changes, root, None, None).await.unwrap();

        let sys = system_of(&backend);
        assert!(sys.contains("(none — no spec docs provided)"), "空 spec 渲染占位: {sys}");
        assert!(!sys.contains("DESIGN"), "不应有任何 spec 内容泄漏: {sys}");
        let (_, spec_used, _spec_warnings) = render_spec_section(&args.spec);
        assert_eq!(spec_used, 0, "空 spec 不得占用任何预算字节");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_args_ignores_legacy_unknown_keys() {
        // 旧 args.json（--background 落盘）带已退役字段（E8-I 的 "lenses"、E10 移除的
        // "mode"/"no_correlate"/"targets" 等）——serde 无 deny_unknown_fields，未知键一律静默
        // 忽略；此处 "lenses" 代表该机制，旧 job resume 反序列化不炸。
        let j = r#"{"base":null,"scope":"branch","spec":[],"focus":null,
            "lenses":"L1,L2","models":null,"models_all":false,
            "block_threshold":null,"conf_floor":null,"background":true,"format":"json",
            "resume":null}"#;
        let a: ReviewArgs = serde_json::from_str(j).unwrap();
        assert_eq!(a.scope, "branch");
        assert!(a.background);
    }
    #[test]
    fn build_packs_covers_multi_bundle_and_default_has_bodies() {
        // Task 3（空臂 dev 开关整拆）后改写：build_packs 唯一路径 = build_pack（0 壳
        // 完整体），无空臂开关。单格全审默认（E9）下 build_packs 不再做跨 bundle 兄弟注入——
        // 两 bundle 各自以自家内容非空（DossierMeta 的多 bundle 计数仍是通用函数行为，
        // 值得独立守住）。
        use crate::bundle::Bundle;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"),
            "pub fn target() {}\nfn u1() { target(); }\nfn u2() { target(); }\nfn u3() { target(); }\nfn u4() { target(); }\nfn u5() { target(); }\n").unwrap();
        std::fs::write(root.join("src/b.rs"), "pub fn other() {}\n").unwrap();
        let bundles = vec![
            Bundle::from_files(vec![FileChange {
                path: "src/a.rs".into(),
                diff: "@@ -1,0 +1,1 @@\n+pub fn target() {}\n".into(),
            }]),
            Bundle::from_files(vec![FileChange {
                path: "src/b.rs".into(),
                diff: "@@ -1,0 +1,1 @@\n+pub fn other() {}\n".into(),
            }]),
        ];

        let (entries, meta, _) = build_packs(&bundles, root, PACK_BUDGET_BYTES, &crate::pack::PackOpts::full());
        assert!(entries[0].contains("target") && entries[0].contains("改动源文"), "bundle 1 自家内容非空含 0 壳: {entries:?}");
        assert!(entries[1].contains("other"), "bundle 2 自家内容非空: {entries:?}");
        assert_eq!(meta, DossierMeta { bundles: 2, injected: 2 });
    }
    /// 批 F-5：scan-only 收口的 RunMeta 观测面——coverage 五集合（selected / completed / reused / failed）、cache 命中率、模板哈希两键、
    /// 在途峰值、needs 计数、类别分布、agreement ≥ 2、usage 从记录重算；任一 cell 失败 → Incomplete/3 + warning；组骨架按 cell id 补齐。
    #[test]
    fn finish_scan_report_fills_observability_and_flips_incomplete_on_failed_cell() {
        use crate::domain::{Category, CellOutput, NeedEntry, NeedKind, ScanOutput, Suspicion};
        use crate::orchestrate::{ScanCellResult, ScanResults};
        let susp = |file: &str, hint: &str, quote: &str, cat: Category, conf: f32| Suspicion {
            file: file.into(), line_start: 1, line_end: 1, hint: hint.into(), category: Some(cat), confidence: conf, quote: quote.into(),
        };
        let mk = |id: &str, group: usize, family: &str, ok: bool, susps: Vec<Suspicion>, needs: usize, input: u64, cached: u64| {
            let out = ScanOutput {
                suspicions: susps,
                needs: (0..needs).map(|i| NeedEntry { path: format!("n{i}.rs"), lines: None, reason: "r".into(), kind: NeedKind::Evidence }).collect(),
                scanned_areas: vec![],
            };
            let mut rec = if ok { CellRecord::ok(id, "p/x", CellOutput::Scan(out.clone())) } else { CellRecord::failed(id, "p/x", Role::Scanner, "boom") };
            rec.input_tokens = input;
            rec.cached_input_tokens = cached;
            rec.output_tokens = 5;
            ScanCellResult { cell_id: id.into(), group, route_key: "p/x".into(), family: family.into(), sample: 1, record: rec, output: if ok { Some(out) } else { None } }
        };
        let cells = vec![
            mk("scan[g1]@p/x#1", 0, "alpha", true, vec![susp("a.rs", "h1", "q1", Category::L1, 0.5)], 2, 100, 50),
            mk("scan[g1]@p/y#1", 0, "beta", true, vec![susp("a.rs", "h1b", "q1", Category::L1, 0.7)], 0, 100, 0),
            mk("scan[g2]@p/x#1", 1, "alpha", true, vec![susp("b.rs", "h2", "q2", Category::L3, 0.4)], 0, 0, 0),
        ];
        let roles = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"x":{"roles":{"scanner":{}}},"y":{"roles":{"scanner":{}}}}}}}"#).unwrap().active_roles();
        let base = |cells: Vec<ScanCellResult>, reused: Vec<String>| ScanFinish {
            groups: vec![], scanned_coverage: Some(0.5), grouping: None,
            risk: Some(crate::domain::RiskMeta { hotspots: 2, r: 1.25, tier: crate::risk::Tier::Mid, budget_cells: 12 }),
            dossier_meta: None,
            results: ScanResults { cells, ..Default::default() },
            model_names: vec!["p/x".into(), "p/y".into()], roles: Some(&roles), warnings: vec![],
            inflight_peak: [("p".to_string(), 2u32)].into_iter().collect(), rate_limit: BTreeMap::new(), ledger_meta: None, head_sha: None,
            duration_ms: 7, test_command: None, job_id: "j".into(), reused,
        };
        let (r, code) = finish_scan_report(base(cells.clone(), vec!["scan[g1]@p/y#1".into()]));
        assert_eq!((r.verdict, code), (Verdict::ScanOnly, 0));
        let scan = r.scan.as_ref().unwrap();
        assert_eq!(scan.groups.len(), 2, "组骨架按 cell id 补齐");
        assert_eq!((scan.groups[0].cells.len(), scan.groups[0].suspicions, scan.groups[1].suspicions), (2, 1, 1));
        assert_eq!((scan.stats.total, scan.stats.agreement_ge2, scan.stats.needs_declared, scan.stats.cells_ok, scan.stats.cells_failed), (2, 1, 2, 3, 0));
        assert_eq!((scan.stats.by_category.get("L1"), scan.stats.by_category.get("L3")), (Some(&1), Some(&1)));
        assert_eq!(scan.scanned_coverage, Some(0.5));
        let cov = &r.run.coverage;
        assert_eq!(cov.selected[&Role::Scanner].len(), 3);
        assert_eq!(cov.completed[&Role::Scanner], vec!["scan[g1]@p/x#1", "scan[g2]@p/x#1"]);
        assert_eq!(cov.reused[&Role::Scanner], vec!["scan[g1]@p/y#1"]);
        assert!(cov.failed.is_empty() && cov.waived.is_empty());
        assert_eq!(r.run.cache_hit_by_cell.len(), 2, "input 0 的不计");
        assert_eq!((r.run.cache_hit_by_cell[0].cell.as_str(), r.run.cache_hit_by_cell[0].ratio), ("scan[g1]@p/x#1", 0.5));
        assert_eq!(r.run.template_hashes.len(), 2);
        assert_eq!(r.run.provider_inflight_peak.get("p"), Some(&2));
        assert_eq!((r.run.needs.triggered_cells, r.run.needs.declared), (1, 2));
        assert_eq!((r.run.usage.input_tokens, r.run.usage.cached_input_tokens, r.run.usage.output_tokens), (200, 50, 15));
        assert_eq!(r.run.model_ids, vec!["x", "y"]);
        let risk = r.run.risk.as_ref().expect("risk 段");
        assert_eq!((risk.hotspots, risk.budget_cells), (2, 12));
        assert_eq!(risk.tier, crate::risk::Tier::Mid);
        assert_eq!(r.run.cells.len(), 3);
        assert_eq!((r.run.cells[0].role.as_str(), r.run.cells[0].cached_input_tokens), ("scanner", 50));
        assert_eq!(r.cells.len(), 3);
        assert!(r.run.failed_models.is_empty() && r.findings.is_empty());
        // 一 cell 失败 → Incomplete / 3 + coverage.failed + warning；失败格烧掉的 usage 入账
        let mut failing = cells;
        failing.push(mk("scan[g2]@p/y#1", 1, "beta", false, vec![], 0, 3, 0));
        let (r2, code2) = finish_scan_report(base(failing, vec![]));
        assert_eq!((r2.verdict, code2), (Verdict::Incomplete, 3));
        assert_eq!(r2.run.coverage.failed[&Role::Scanner], vec!["scan[g2]@p/y#1"]);
        assert!(r2.run.warnings.iter().any(|w| w.contains("verdict hard Incomplete")), "{:?}", r2.run.warnings);
        assert_eq!(r2.scan.as_ref().unwrap().stats.cells_failed, 1);
        assert_eq!(r2.run.usage.input_tokens, 203);
    }

    #[test]
    fn run_meta_needs_always_serialized_zeros_included() {
        // E8-E 观测面：needs 统计恒写、零也写。
        let v = serde_json::to_value(RunMeta::default()).unwrap();
        assert_eq!(v["needs"]["triggered_cells"], 0);
        assert_eq!(v["needs"]["declared"], 0);
    }
    #[test]
    fn build_packs_single_bundle_over_budget_file_falls_back_to_def_body_with_real_diff() {
        // E9 单格默认后改写：不再有跨 bundle 兄弟注入——E8-D 回归锚从「兄弟」换成
        // 「同格文件超预算」：同一 bundle 内一个小文件 + 一个超预算大文件，大文件必走
        // def 锚回落，而锚定消费的是 fc.diff——若被塞空 diff 占位（E8 裁决 D 回归），
        // def 锚不到、本块静默消失。
        use crate::bundle::Bundle;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "pub fn alpha_marker() {}\n").unwrap();
        // 同格 b.rs 撑超 pack 预算：全文分支放不下 → 必走 def 锚回落。
        let filler_line = "// 填充行：撑爆预算迫入 def 锚回落\n";
        let big = format!(
            "pub fn beta_marker() {{\n    let marker_body = 1;\n}}\n{}",
            filler_line.repeat(PACK_BUDGET_BYTES / filler_line.len() + 10)
        );
        std::fs::write(root.join("src/b.rs"), &big).unwrap();
        let bundles = vec![Bundle::from_files(vec![
            FileChange { path: "src/a.rs".into(), diff: "@@ -1,0 +1,1 @@\n+pub fn alpha_marker() {}\n".into() },
            FileChange { path: "src/b.rs".into(), diff: "@@ -1,0 +2,1 @@\n+    let marker_body = 1;\n".into() },
        ])];
        // full 档：本测试锚的是回落梯子的真 diff 穿透（slice 档由 pack::tests 覆盖）
        let (on, _, _) = build_packs(&bundles, root, PACK_BUDGET_BYTES, &crate::pack::PackOpts::full());
        assert!(on[0].contains("alpha_marker"), "小文件全文在场");
        // 真 diff 穿透证明（集成级）：超预算文件迫入 def 锚回落分支（changed_def_ranges
        // 消费其真 diff）——空 diff 占位回归下无锚可寻，以下两条断言全灭。
        assert!(on[0].contains("（def 体，全文超预算回落）"), "超预算文件迫入 def 回落分支");
        assert!(on[0].contains("let marker_body = 1;"), "超预算文件改动 def 体在场（真 diff 锚定）");
        assert!(!on[0].contains("填充行"), "回落只给改动 def 体，不带填充");
        // 同 path 不重复
        assert_eq!(on[0].matches("src/b.rs").count(), 1, "同格文件条目 src/b.rs 不重复");
    }
    #[test]
    fn pack_budget_respects_model_token_wall() {
        // design §2.2 预算函数：min(768K 绝对帽, (min_ctx − 16K 输出预留)×3 − 24K 壳预留)。
        // 2026-07-28 提帽 256K→768K（1M 窗时代，锚 gpt-5.6 272K 计费悬崖之下）——
        // 128K 缺省档从「帽封顶 256K」翻为「窗口墙生效 312K」是本次显式入账的行为变化。
        let mk = |models: &str| crate::config::Config::from_json(
            &r#"{"providers":{"p":{"kind":"openai","models":{MODELS}}}}"#.replace("MODELS", models),
        ).unwrap().active_roles();
        let big = r#""big-ctx":{"contextWindow":1000000,"roles":{"scanner":{}}}"#;
        let small = r#""small-ctx":{"contextWindow":40000,"roles":{"scanner":{}}}"#;
        let unknown = r#""u":{"roles":{"scanner":{}}}"#;
        // 未声明窗口的模型 → 保守缺省 128K → (128K−16K)*3−24K=312K < 768K → 窗口墙生效
        assert_eq!(pack_budget_bytes(&mk(unknown)), 312_000);
        // 大窗模型（1M）：窗口墙 ≈2.9M → 绝对帽 768K 封顶（≈256K tok 记账 <272K 悬崖）
        assert_eq!(pack_budget_bytes(&mk(big)), 768_000);
        // 小窗模型：(40_000−16_000)*3−24_000 = 48_000
        assert_eq!(pack_budget_bytes(&mk(small)), 48_000);
        // 多模型取最小窗（一份 pack 全模型安全）
        assert_eq!(pack_budget_bytes(&mk(&format!("{big},{small}"))), 48_000);
        // 无 scanner → 缺省 128K 档
        assert_eq!(pack_budget_bytes(&crate::config::RoleSets::default()), 312_000);
    }
    #[test]
    fn load_ledger_no_dismissed_and_missing_db_are_empty_without_side_effects() {
        let tmp = tempfile::tempdir().unwrap();
        let l = load_ledger(None, true, tmp.path()).unwrap();
        assert!(l.ids.is_empty() && l.cards.is_empty() && l.meta.is_none());
        // zero-config：proj.db 不存在 → 空台账且不建库/不建目录（零副作用）
        let l2 = load_ledger(None, false, tmp.path()).unwrap();
        assert!(l2.ids.is_empty() && l2.meta.is_none());
        assert!(!tmp.path().join(".redteam").exists(), "自动加载不得建 .redteam/");
    }
    #[test]
    fn load_ledger_csv_db_and_retired_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // csv：仅豁免，meta.source=csv
        let l = load_ledger(Some("d2b371819297f98a,20ba9b3025a8b421"), false, tmp.path()).unwrap();
        assert_eq!(l.ids.len(), 2);
        assert!(l.cards.is_empty());
        assert_eq!(l.meta.as_ref().unwrap().source, "csv");
        // db 文件路径覆盖：加载活跃卡
        let dbp = tmp.path().join("x.db");
        let mut db = crate::db::ProjDb::open_at(&dbp).unwrap();
        db.dismiss_upsert(&crate::db::DismissCard {
            id: "1111111111111111".into(), status: "dismissed".into(), title: "t".into(),
            file: "a.rs".into(), quote: "q".into(), severity: "high".into(),
            reason: "r".into(), round: "r1".into(), updated_at: String::new(),
        }).unwrap();
        drop(db);
        let l2 = load_ledger(Some(dbp.to_str().unwrap()), false, tmp.path()).unwrap();
        assert_eq!((l2.ids.len(), l2.cards.len()), (1, 1));
        assert_eq!(l2.meta.as_ref().unwrap().loaded, 1);
        // dir 形态已退役 → 明确报错引导迁移
        let err = load_ledger(Some(tmp.path().to_str().unwrap()), false, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("retired"), "{err}");
        // zero-config 命中：cwd 下有 .redteam/proj.db → 自动加载
        let root = tmp.path().join("proj");
        let mut db2 = crate::db::ProjDb::open(&root).unwrap();
        db2.dismiss_upsert(&crate::db::DismissCard {
            id: "2222222222222222".into(), status: "dismissed".into(), title: "t".into(),
            file: "b.rs".into(), quote: "q".into(), severity: "low".into(),
            reason: "r".into(), round: String::new(), updated_at: String::new(),
        }).unwrap();
        drop(db2);
        let l3 = load_ledger(None, false, &root).unwrap();
        assert_eq!(l3.cards.len(), 1);
        assert!(l3.meta.as_ref().unwrap().source.contains(crate::db::DB_FILE), "{}", l3.meta.as_ref().unwrap().source);
    }
    #[tokio::test]
    async fn jobcellsink_manifest_sets_phase_scan() {
        use crate::state::{JobState, Phase};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_path_buf();
        let store = std::sync::Arc::new(crate::state::JobStore::open(&ws).unwrap());
        store.put_state_for_test(&JobState::new("j", 0)).unwrap();
        let sink = JobCellSink { store: std::sync::Arc::clone(&store), job_id: "j".into(), ckpt_failures: std::sync::atomic::AtomicU32::new(0) };
        use crate::orchestrate::CellSink;
        sink.manifest(&[crate::state::CellInput { template_hash: None,
            cell: "c".into(), model: "m".into(), prompt: "p".into(), scan: None }]);
        assert_eq!(store.read_state("j").unwrap().phase, Some(Phase::Scan));
    }
    #[tokio::test]
    async fn jobcellsink_on_phase_writes_phase() {
        use crate::state::{JobState, Phase};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_path_buf();
        let store = std::sync::Arc::new(crate::state::JobStore::open(&ws).unwrap());
        store.put_state_for_test(&JobState::new("j", 0)).unwrap();
        let sink = JobCellSink { store: std::sync::Arc::clone(&store), job_id: "j".into(), ckpt_failures: std::sync::atomic::AtomicU32::new(0) };
        use crate::orchestrate::CellSink;
        sink.on_phase(Phase::Finalizing, None);
        assert_eq!(store.read_state("j").unwrap().phase, Some(Phase::Finalizing));
    }

    /// Task 5 / ①（design §4 表②级）：正常写成功路径下 `ckpt_failures` 恒为 0——四个
    /// on_* 全跑一遍不应误增计数。真实 db 写失败注入在 SQLite WAL 模式下不可靠（连接持有
    /// 的 fd 在 open() 之后不受文件系统层 chmod/删除/uchg/截断影响，实测四种手法均未能
    /// 制造出真实写错误，参见 task-c5-report.md「注入不可行」记录）——按 brief 5.1 降级为
    /// 计数器语义测试：本测锚「成功不误计」，下一测锚「终态封盘拒写（Ok(false)）不误计入
    /// 失败」（这才是 on_* 里 `if let Err(e)` 分支唯二会被跳过的两种非计数场景）。
    #[tokio::test]
    async fn jobcellsink_ckpt_failures_stays_zero_on_success() {
        use crate::state::{CellInput, CellRecord, JobState, Phase};
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(crate::state::JobStore::open(dir.path()).unwrap());
        store.put_state_for_test(&JobState::new("j", 0)).unwrap();
        let sink = JobCellSink {
            store: std::sync::Arc::clone(&store),
            job_id: "j".into(),
            ckpt_failures: std::sync::atomic::AtomicU32::new(0),
        };
        use crate::orchestrate::CellSink;
        sink.manifest(&[CellInput { template_hash: None, cell: "c".into(), model: "m".into(), prompt: "p".into(), scan: None }]);
        sink.on_cell(CellRecord::ok("c", "m", crate::domain::CellOutput::review(vec![])));
        sink.on_phase(Phase::Finalizing, None);
        sink.on_meta(&["m".to_string()]);
        assert_eq!(
            sink.ckpt_failures.load(std::sync::atomic::Ordering::Relaxed), 0,
            "全部写成功时不应误增失败计数"
        );
    }

    /// Task 5 / ①（design §4 表②级）：job 已落终态（首个终态封盘）时，`update_if_active`
    /// 返回 `Ok(false)`（守卫拒写，非错误）——这是正常竞态（sink 迟到进度撞上已 cancel/
    /// done 的 job），不应被误计为 checkpoint 失败（否则每个正常收尾的 job 都会被污染出
    /// 一条假警告）。`manifest` 因同时含 write_manifest（不受终态封盘影响，仍会真写）+
    /// update_if_active（受影响）——本测只挑 on_cell/on_phase/on_meta 三个纯 update_if_active
    /// 路径，避免 write_manifest 那一半干扰断言。
    #[tokio::test]
    async fn jobcellsink_ckpt_failures_not_counted_on_terminal_guard_reject() {
        use crate::state::{CellRecord, JobState, JobStatus, Phase};
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(crate::state::JobStore::open(dir.path()).unwrap());
        let mut st = JobState::new("j", 0);
        st.status = JobStatus::Done; // 已终态：守卫对后续所有 update_if_active 一律拒写
        store.put_state_for_test(&st).unwrap();
        let sink = JobCellSink {
            store: std::sync::Arc::clone(&store),
            job_id: "j".into(),
            ckpt_failures: std::sync::atomic::AtomicU32::new(0),
        };
        use crate::orchestrate::CellSink;
        sink.on_cell(CellRecord::ok("c", "m", crate::domain::CellOutput::review(vec![])));
        sink.on_phase(Phase::Finalizing, None);
        sink.on_meta(&["m".to_string()]);
        assert_eq!(
            sink.ckpt_failures.load(std::sync::atomic::Ordering::Relaxed), 0,
            "终态封盘拒写（Ok(false)）是正常竞态，不应计入 checkpoint 失败"
        );
        // 状态本身确实原样保留（未被迟到的 sink 写复活成 Running）——佐证守卫确实生效。
        assert_eq!(store.read_state("j").unwrap().status, JobStatus::Done);
    }

    /// Task 5 / ②（design §4 表②级）：warnings 拼装——计数>0 才追加、含 job_id 与次数；
    /// 计数=0 时 report 不变（design §6 契约冻结面「正常路径逐字节不变」的直接落点）。
    #[test]
    fn append_ckpt_failure_warning_only_appends_when_nonzero() {
        fn empty_report() -> ReviewReport {
            ReviewReport { scan: None, cells: vec![],
                schema_version: crate::domain::SCHEMA_VERSION.into(),
                verdict: Verdict::Approve,
                stats: Stats::from_findings(&[]),
                findings: vec![],
                clusters: vec![],
                run: RunMeta { warnings: vec!["pre-existing".into()], ..Default::default() },
            }
        }
        // 计数=0：warnings 原样不变（不多不少）。
        let mut r0 = empty_report();
        append_ckpt_failure_warning(&mut r0, "j1", 0);
        assert_eq!(r0.run.warnings, vec!["pre-existing".to_string()]);

        // 计数>0：追加一条含 job_id 与次数的简体中文条目，既有 warnings 不动。
        let mut r3 = empty_report();
        append_ckpt_failure_warning(&mut r3, "j1", 3);
        assert_eq!(r3.run.warnings.len(), 2);
        assert_eq!(r3.run.warnings[0], "pre-existing");
        assert!(r3.run.warnings[1].contains("j1"), "warning 应含 job_id: {}", r3.run.warnings[1]);
        assert!(r3.run.warnings[1].contains('3'), "warning 应含次数: {}", r3.run.warnings[1]);
        assert!(r3.run.warnings[1].contains("检查点落盘失败"), "warning 应含文案: {}", r3.run.warnings[1]);
    }
}
