//! CLI 评审流水线：`ReviewArgs`/台账/pack 预算/model 选取等留守项 + 全量/resume/job
//! 状态机（并发组 M16 定型，原地不动）。尾段引擎/spec/二阶段补查/增量评审已拆至
//! 同目录子模块（M17-a 尾段统一，零语义 diff）。

use crate::bundle::{group, select_files, Bundle, GroupingOpts, GroupingStats};
use crate::config::{Config, RoleSets};
use crate::domain::{CacheHit, CellSummary, DossierMeta, NeedsMeta, ReviewReport, RiskMeta, Role, RunMeta, ScanGroup, ScanReport, ScanStats, Stats, Usage, Verdict};
use crate::evidence::git::FileChange;
use crate::orchestrate::deep::{run_deep, DeepInput};
use crate::orchestrate::{run_scan_fanout, scan_cells_from_records, CellSink, ScanCellResult, ScanEntrySpec, ScanFanoutInput, ScanResults};
use crate::output::Format;
use crate::provider::registry::ProviderRegistry;
use crate::state::CellRecord;
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) mod deny;
mod incremental;
pub(crate) mod second_pass;
pub mod verify_once;
mod spec;
mod tail;

pub use incremental::run_incremental_review;
pub use tail::exit_code;

use spec::{render_spec_section_capped, SPEC_BUDGET_BYTES};
use tail::model_ids_for;

/// scanners 为空时的配置错误文案（`build_registry` 与 `run_code_review` 共用）。
pub const NO_SCANNER_MSG: &str = "未配置 scanner：在 ~/.redteam/config.json（REDTEAM_HOME 可覆盖）给至少一个模型加 roles.scanner，见 `redteam model setup --help`";

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
    // **只读**打开（收口评审 #2）：台账加载是纯读语义，而 `open_at` 现在带一次性迁移——
    // 用读的姿势把「拿来做对拍基线的 dev 库」就地迁成 v4（评审史清空、dev 二进制再也写不了它），
    // 会把 README 里承诺的回退路径本身毁掉。项目库自身的建库 / 迁移由 `init` 与写入口负责。
    let from_db = |path: &Path, source: String| -> anyhow::Result<Ledger> {
        let db = crate::db::ProjDb::open_read_only(path)?;
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

/// 阶段 0 风险路由（plan_cells 前）。ledger 路径：`--dismissed` 文件覆盖，否则 `.redteam/proj.db`（存在才读）。
fn stage0_risk(
    selected: &[FileChange],
    root: &Path,
    args: &ReviewArgs,
    cfg: &Config,
    head: Option<&str>,
) -> (Option<RiskMeta>, Option<crate::risk::RiskRoute>) {
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
    let meta = RiskMeta {
        hotspots: route.hotspots.len() as u32,
        r: route.r,
        tier: route.tier,
        budget_cells: route.budget_cells_hint,
    };
    (Some(meta), Some(route))
}

/// 评审流水线（design §2 / §4 / §5）：证据筛选 → 阶段 0 风险路由 → 分组 / 装包 / 跨组索引 → scanner 扇出（组 × 条目 × samples）
/// → 疑点并集 + agreement → scan 报告收口。配了 reviewer 时深挖与扫描同 task 时间重叠（C2〔D15〕：H0 开跑即挖、scan 格完成即入队、
/// 扫描全部返回即收场），收口后叠加 finding 与 verdict；scanner 有格失败时深挖照跑、判定仍 Incomplete / 3。
/// 未配 reviewer = scan-only：不产 finding，退出码 0 = scan 完成、3 = 任一 scanner cell 失败（design §8.4）。
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
    // 变更文件路径（A-2 披露用）：`selected` 随后被 `plan_cells` 消费掉，先留一份
    let changed_files: Vec<String> = selected.iter().map(|c| c.path.clone()).collect();
    let roles = cfg.active_roles();
    if roles.scanners.is_empty() {
        anyhow::bail!(NO_SCANNER_MSG);
    }
    // 阶段 0 风险路由：放在 scanner 空校验之后（无 scanner 直接 bail，不空跑 git grep/log 一串子进程）。
    // RiskRoute 留下入队（不从 RiskMeta 反推）；无 reviewer 时丢掉区列表，只把 meta 进报告。
    let (risk, route) = stage0_risk(&selected, root, args, cfg, head_sha.as_deref());
    let deep_route = if roles.reviewers.is_empty() { None } else { route };
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
    let (results, mut deep) = if roles.reviewers.is_empty() {
        (run_scan_fanout(registry, &input, sink, None).await, None)
    } else {
        // scanner 与深挖时间重叠（design C2 §2〔D15〕）：同 task 并发——H0 开跑即挖，scan 格完成即经通道入队；
        // 扫描全部返回 → 打 `Deep`、drop 发送端 = 关闭入口 → 深挖以剩余预算收场。scanner 有格失败时深挖照跑（F3，见 `overlay_deep`）。
        let snap = |it: &crate::orchestrate::queue::QueueItem| {
            if let Some(s) = sink {
                s.on_queue_item(it);
            }
        };
        let on_cell = |rec: &crate::state::CellRecord| {
            if let Some(s) = sink {
                s.on_cell(rec.clone());
            }
        };
        let factory = per_cell_backend_factory(cfg);
        let budget_cells = risk.as_ref().map(|r| r.budget_cells).unwrap_or(4);
        // 队列头落在扇出之前（评审 #1 / #2 / #5）：resume 靠它区分「首跑 scan-only」与「深挖开过被 kill」，并沿用同一个 K。
        if let Some(s) = sink {
            s.on_deep_start(crate::state::DeepHeader { budget_cells });
        }
        let (tx, rx) = tokio::sync::mpsc::channel(256);
        let scan = async move {
            let r = run_scan_fanout(registry, &input, sink, Some(&tx)).await;
            if let Some(s) = sink { s.on_phase(crate::state::Phase::Deep, None); }
            drop(tx);
            r
        };
        // 首跑无往轮判决（resume 才有）。
        let no_known = BTreeMap::new();
        let thresholds = verdict_thresholds(args, cfg);
        let deep = run_deep(
            registry,
            DeepInput {
                route: deep_route.as_ref(),
                scan_cells: &[],
                bundles: &bundles,
                root,
                reviewers: &roles.reviewers,
                budget_cells,
                pack: &cfg.review.pack,
                spec_docs: &spec_section,
                target_label: &target_label,
                snapshot: Some(&snap),
                restore: None,
                prior_findings: vec![],
                tools: resolve_tools(args, cfg),
                backend_factory: Some(factory.as_ref()),
                verify: Some(crate::orchestrate::deep::VerifyInput {
                    roles: &roles,
                    enabled: cfg.review.verifiers.enabled,
                    // 与 verdict 同一份阈值（收口评审 #9）：旗标覆盖必须同时作用于两处。
                    block_threshold: thresholds.0,
                    conf_floor: thresholds.1,
                    precedents: &ledger.cards,
                    known: &no_known,
                }),
                on_cell: Some(&on_cell),
                incoming: Some(rx),
                reopen_skipped: false,
            },
            &(),
        );
        // 显式先 poll 扫描一次（design C2 §7.3）：各 scan 格先占门或登记为等门者，深挖首次填门遇等门者即让位——
        // t = 0 的先后不依赖 `join!` 的分支轮询序。
        tokio::pin!(scan);
        match futures::poll!(scan.as_mut()) {
            std::task::Poll::Ready(r) => (r, Some(deep.await)),
            std::task::Poll::Pending => {
                let (r, d) = tokio::join!(scan, deep);
                (r, Some(d))
            }
        }
    };
    if let Some(s) = sink { s.on_phase(crate::state::Phase::Finalizing, None); }
    let mut warnings: Vec<String> = registry.warnings().to_vec();
    warnings.extend(spec_warnings);
    // 空证据必须可观测（r1 20260703 事故：干净树 0 cell 秒出无警 approve 假绿灯）。
    if bundles.is_empty() {
        warnings.push(format!(
            "no reviewable evidence: empty diff for {target_label} (0 cells) — the scan-only report reflects zero reviewed content"
        ));
    }
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
    if let Some(d) = deep.as_mut() {
        // 合并残差对账（design C2 §4〔D15〕）：scan 报告的组内闭包簇被队列逐条合并拆进多项的簇数。
        d.meta.queue_split_clusters = crate::consolidate::scan::split_clusters(&results.cells, &d.items);
        // 疑点落账（M21c 批 A，design §8.2）：本轮被预算截断的疑点进跨轮台账，供下轮 delta 激活（批 C）。
        persist_dormant_suspicions(root, &d.items, head_sha.as_deref().unwrap_or(""), &mut warnings);
    }
    // 披露本轮变更文件上的休眠线索（A-2）：只读、不进 verdict。落账之后读，所以本轮刚截断的也会出现。
    let dormant_leads = read_dormant_leads(root, &changed_files);
    let (report, code) = finish_scan_report(ScanFinish {
        groups,
        scanned_coverage,
        grouping: Some(stats),
        risk: risk.clone(),
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
        dormant_leads,
        reused: vec![],
    });
    match deep {
        Some(deep) => Ok(overlay_deep(report, code, deep, args, cfg, registry, t0)),
        None => Ok((report, code)),
    }
}

/// 读本轮变更文件上的休眠疑点（M21c 批 A，A-2）：**只读、零副作用**——库不在就空，读失败也空（不打扰评审）。
/// 这是披露面，不是判定面：它们没被深挖过、没有危害链论证，当 finding 用会把「没挖」误读成「有问题」。
fn read_dormant_leads(cwd: &Path, changed_files: &[String]) -> Vec<crate::domain::DormantLead> {
    if changed_files.is_empty() {
        return vec![];
    }
    let p = cwd.join(crate::project::PROJECT_DIR).join(crate::db::DB_FILE);
    if !p.exists() {
        return vec![];
    }
    let Ok(db) = crate::db::ProjDb::open_read_only(&p) else { return vec![] };
    let Ok(rows) = db.suspicions_for_files(changed_files) else { return vec![] };
    rows.into_iter()
        .filter(|r| r.status == "dormant")
        .map(|r| {
            let line = serde_json::from_str::<serde_json::Value>(&r.json)
                .ok()
                .and_then(|j| j.get("line_start").and_then(|v| v.as_u64()))
                .unwrap_or(0) as u32;
            crate::domain::DormantLead {
                file: r.file, line, hint: r.hint, first_round: r.first_job, activations: r.activations,
            }
        })
        .collect()
}

/// per-cell backend 工厂（流式旁路的挂点）：按 reviewer 条目取密钥另造实例并挂 delta 回调。
/// 造不出来（缺密钥 / google 未支持）→ `None`，调用方退回共享 backend（无旁路，不致命）。
fn per_cell_backend_factory(cfg: &Config) -> Box<crate::orchestrate::deep::PerCellBackend<'_>> {
    Box::new(move |em, cb| {
        // 没密钥就别造：造出来也只会 401，而 registry 里的共享 backend（真跑时同一把密钥，
        // 测试时是 Fake）才是该用的那个。旁路缺席不影响正确性——终态 parse 才是权威。
        let key = cfg.resolve_key(&em.provider, &em.id)?;
        if key.is_empty() {
            return None;
        }
        crate::provider::backends::backend_from_config_with_delta(em, key, cb).ok()
    })
}

/// `--tools` 旗覆盖 config `review.tools`（R2 二阶段档位）。
fn resolve_tools(args: &ReviewArgs, cfg: &Config) -> crate::config::ToolsMode {
    args.tools.as_deref().and_then(crate::config::ToolsMode::from_flag).unwrap_or(cfg.review.tools)
}

#[allow(clippy::too_many_arguments)] // 报告 / 退出码 / 深挖结果 / 两处配置 / registry / 计时——各不同源，收拢 struct 是过度设计
/// verdict 的两个阈值：`--block-threshold` / `--conf-floor` 覆盖 > 配置（M21a 起这两个旗才有消费者）。
/// **复核触发集与它同源**（design §8.1：「会让本轮 reject 的那批 finding」正是「值得花一次复核的那批」），
/// 所以两处必须读同一份解析结果——旗标只作用于 verdict、不作用于触发集的话，
/// `--conf-floor 0.3` 会让一批 blocking finding 永远不被复核（收口评审 #9）。
pub(crate) fn verdict_thresholds(args: &ReviewArgs, cfg: &Config) -> (crate::domain::Severity, f32) {
    let block = args
        .block_threshold
        .as_deref()
        .and_then(|s| serde_json::from_value(serde_json::Value::String(s.to_string())).ok())
        .unwrap_or(cfg.review.block_threshold);
    (block, args.conf_floor.unwrap_or(cfg.review.conf_floor))
}

fn overlay_deep(
    mut report: ReviewReport,
    scan_code: i32,
    deep: crate::orchestrate::deep::DeepRun,
    args: &ReviewArgs,
    cfg: &Config,
    registry: &ProviderRegistry,
    t0: std::time::Instant,
) -> (ReviewReport, i32) {
    // 深挖期的墙钟 / 在途峰值 / 429 收敛只有跑完才知道——scan 收口时的快照必须在这里刷新，
    // 否则 20 分钟的深挖在报告里查无此事，D13 的 429 披露也断在 scan 段（design §12.3）。
    report.run.duration_ms = t0.elapsed().as_millis() as u64;
    report.run.provider_inflight_peak = registry.inflight_peaks();
    report.run.provider_rate_limit = registry.rate_limit_meta();
    for w in registry.gate_warnings() {
        if !report.run.warnings.contains(&w) {
            report.run.warnings.push(w);
        }
    }
    let findings = deep.findings;
    let skipped = deep.meta.budget_skipped;
    let failed = deep.reviewer_failed;
    // 一格都没挖、没债、也没有往轮挖出来的 finding = 深挖根本没发生（空变更集 / 队列为空）。
    // 此时绝不能重算 verdict：`compute_verdict([], …)` 会把「没挖」说成 Approve——正是
    // r1 20260703 的干净树假绿灯。报告保持 scan-only 形态与退出码，deep 观测段照进（可观测 ≠ 改判）。
    // 注意 findings 非空要走下面的正常收口：resume 全 Settled 时这轮确实 0 格，但成果在。
    // 沿用格（resume 往轮挖过）同样算「深挖发生过」：零 finding 的已完成深挖是 Approve，不是 scan-only。
    if deep.cells.is_empty() && deep.reused_cells.is_empty() && !failed && skipped == 0 && findings.is_empty() {
        report.run.deep = Some(deep.meta);
        report.run.warnings.extend(deep.warnings);
        return (report, scan_code);
    }
    report.findings = findings.clone();
    report.stats = Stats::from_findings(&findings);
    // 本轮复核的驳回率（design §8.1「去留由数据决定」）：分母是**真跑成了的**条数——
    // 被免验跳过的从没给过 verifier 机会，没跑成的（provider 挂了 / 判决读不懂）也没给出过证据，
    // 两类算进分母都只会把率冲淡（收口评审 #6：verifier 全挂会读成「验了 N 条一条没驳」）。
    // 一条都没跑成 → None（不产键），不拿 0 冒充「验了没驳回」。
    let verified = deep.meta.verify_triggered.saturating_sub(deep.meta.verify_failed);
    let verify_ran = deep.meta.verify_triggered > 0;
    report.run.refute_rate = (verified > 0).then(|| deep.meta.verify_refuted as f32 / verified as f32);
    report.run.deep = Some(deep.meta);
    report.run.warnings.extend(deep.warnings);
    if skipped > 0 {
        report.run.warnings.push(format!(
            "deep: skipped(budget) {skipped} item(s) — disclosed, not in verdict"
        ));
    }
    // RunMeta.models 补齐实际参与的 reviewer（有格才算，含失败格与 resume 沿用格）；model_ids 与之同序追加。
    let roles = cfg.active_roles();
    for c in deep.reused_cells.iter().chain(&deep.cells) {
        if !report.run.models.contains(&c.model) {
            report.run.model_ids.extend(model_ids_for(std::slice::from_ref(&c.model), &roles));
            report.run.models.push(c.model.clone());
        }
    }
    fold_role_cells(&mut report, deep.reused_cells, true);
    fold_role_cells(&mut report, deep.cells, false);
    let h = crate::prompt::template_hashes();
    report.run.template_hashes.insert("reviewer_tail".into(), h.reviewer_tail.clone());
    // 复核真跑过才记它的模板哈希（prompt 漂移登记表；没跑过就记等于给报告塞噪音）。
    if verify_ran {
        report.run.template_hashes.insert("verifier_tail".into(), h.verifier_tail.clone());
    }
    let (block, floor) = verdict_thresholds(args, cfg);
    let verdict = crate::verdict::compute_verdict(&findings, block, floor);
    let debt = failed || skipped > 0;
    const DEBT: &str = "reviewer debt: failed reviewer cell, undispatched queue items, or budget skip";
    if report.verdict == Verdict::Incomplete {
        // scanner 有格失败（design C2 §5 F3〔D15〕）：深挖照跑、findings / 观测 / reviewer 格照进，但疑点来源有结构性缺口——
        // 判定保持 Incomplete / 3（优先于 reject 10 与带债 2）；本可 reject 的 blocking 条数与债另行披露。
        if verdict == Verdict::Reject {
            report.run.warnings.push(format!(
                "scan incomplete: {} blocking finding(s) present — a complete review would reject",
                crate::verdict::blocking_count(&findings, block, floor)
            ));
        }
        if debt {
            report.run.warnings.push(DEBT.into());
        }
        return (report, exit_code(Verdict::Incomplete, debt));
    }
    report.verdict = verdict;
    if debt && matches!(verdict, Verdict::Approve) {
        report.run.warnings.push(format!("{DEBT} — approve with exit 2"));
    }
    (report, exit_code(verdict, debt))
}

/// 把 scan-only 收口之外的格（reviewer / verifier）并进报告：coverage 按记录自身角色记（追加、不覆盖同角色已有项）——
/// selected 全列，然后**失败优先**分桶：失败 → failed，成功且沿用 → reused，成功且本轮跑 → completed；
/// usage、`run.cells` 摘要、顶层 `cells` 同步。
///
/// 「失败优先」是批 A 统一的口径：原先这里是「复用优先」（`(reused, _)` 一律进 reused），
/// 而 scanner 侧收口是失败优先——同一个「失败的复用格」在两处会落进不同的桶，且 reviewer 侧会把失败**静默记成复用**。
/// scan-only 收口只吃 scanner 格：把别的角色喂进去会被 `scan_cells_from_records` 判成失败的 scanner 格（假 `scan incomplete`）。
fn fold_role_cells(report: &mut ReviewReport, recs: Vec<CellRecord>, reused: bool) {
    let cov = &mut report.run.coverage;
    for rec in &recs {
        let role = rec.role();
        cov.selected.entry(role).or_default().push(rec.cell.clone());
        let bucket = match (rec.ok, reused) {
            (false, _) => &mut cov.failed,
            (true, true) => &mut cov.reused,
            (true, false) => &mut cov.completed,
        };
        bucket.entry(role).or_default().push(rec.cell.clone());
        report.run.usage.input_tokens += rec.input_tokens;
        report.run.usage.output_tokens += rec.output_tokens;
        report.run.usage.cached_input_tokens += rec.cached_input_tokens;
        report.run.cells.push(CellSummary::from_record(rec));
    }
    report.cells.extend(recs);
    if let Err(e) = report.run.coverage.check_partition() {
        report.run.warnings.push(format!("coverage 划分不变式被破坏（design §8.4）：{e}"));
    }
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
    /// 休眠疑点线索（M21c 批 A，A-2）：只披露、不进 verdict。
    pub dormant_leads: Vec<crate::domain::DormantLead>,
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
    // 五集合划分自检（design §8.4，M21c 批 A）：破了不该静默——它意味着某个格被漏分类或重复分类，
    // 而 coverage 正是 verdict 分级（scanner 失败 → 硬 Incomplete / 深挖失败 → 带债 approve）的依据。
    if let Err(e) = coverage.check_partition() {
        warnings.push(format!("coverage 划分不变式被破坏（design §8.4）：{e}"));
    }
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
        dormant_leads: f.dormant_leads,
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
    let (risk, _route) = stage0_risk(&selected, cwd, args, cfg, head_sha.as_deref());
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
            "risk hotspots={} r={:.3} tier={} budget_cells={}\n",
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
        // 进度分母是 manifest 里的 scanner 格，分子只数 scanner 格（评审 #12）：C2 起深挖与扫描同时跑，
        // reviewer 格也计会在扫描期显示「1/1」这种超分母进度；深挖进展看 phase 与报告的 run.deep。
        let scanner = rec.role() == Role::Scanner;
        if let Err(e) = self.store.update_if_active(&self.job_id, |st| {
            st.last_active = now_unix();
            if scanner {
                st.progress.done += 1;
            }
        }) {
            eprintln!("warn: job {} on_cell 进度写失败: {e}", self.job_id);
            self.ckpt_failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn on_deep_start(&self, header: crate::state::DeepHeader) {
        if let Err(e) = self.store.update_if_active(&self.job_id, |st| {
            st.deep = Some(header);
        }) {
            eprintln!("warn: job {} 深挖队列头写失败: {e}", self.job_id);
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

    /// M21b 批 C：队列项状态转换写 `queue_items` 快照（resume 用）。写失败 warning 不败 job（design §7.1）。
    fn on_queue_item(&self, item: &crate::orchestrate::queue::QueueItem) {
        if let Err(e) = self.store.put_queue_item(&self.job_id, item) {
            eprintln!("warn: job {} queue snapshot 失败: {e}", self.job_id);
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
/// 疑点落账（M21c 批 A）：把本轮**没挖成**的疑点 upsert 进项目库的跨轮台账。
///
/// 三条边界：
/// - **不建库**：项目没 `init` 过就跳过（与 `load_ledger` 的 zero-config 同口径，评审不该有建库副作用）；
/// - **不败评审**：写失败只记 warning（与队列快照同口径，design §7.1）——台账是增益，不该让一轮评审失败；
/// - 轮次标识用 `head_sha`：前台裸跑没有 job_id，而台账要的本就是「第一次/最近一次见到它是哪个 HEAD」。
fn persist_dormant_suspicions(cwd: &Path, items: &[crate::orchestrate::queue::QueueItem], round: &str, warnings: &mut Vec<String>) {
    let rows = crate::orchestrate::ledger::rows_from_skipped(items, round);
    if rows.is_empty() {
        return;
    }
    let p = cwd.join(crate::project::PROJECT_DIR).join(crate::db::DB_FILE);
    if !p.exists() {
        return;
    }
    let db = match crate::db::ProjDb::open_at(&p) {
        Ok(db) => db,
        Err(e) => {
            warnings.push(format!("疑点台账未落账（打不开项目库 {}）：{e}", p.display()));
            return;
        }
    };
    let mut failed = 0usize;
    for row in &rows {
        if db.suspicion_upsert(row).is_err() {
            failed += 1;
        }
    }
    if failed > 0 {
        warnings.push(format!("疑点台账落账部分失败：{failed}/{} 行未写入", rows.len()));
    }
}

/// 全部落盘记录（含失败格）都进收口：失败格进 coverage.failed / cells_failed、烧掉的 usage 入账，与随后 `attach_cells` 回填的
/// `run.cells` / 顶层 `cells` 一致（评审修 2）。M 取 manifest 行数（若尚未写出 manifest 就已失败，退化用已完成数兜底，不出现 "N/0"）。
fn partial_report(store: &crate::state::JobStore, job_id: &str, reason: String) -> (ReviewReport, i32) {
    let manifest = store.read_manifest(job_id);
    // manifest 只列 scanner 格：进度与 scan-only 收口都只数 scanner 记录；深挖期落盘的 reviewer 格按自己的角色另记。
    let (records, deep_records): (Vec<CellRecord>, Vec<CellRecord>) =
        store.read_partial(job_id).into_iter().partition(|r| r.role() == Role::Scanner);
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
        dormant_leads: vec![], // partial / resume 收口没有本轮变更集上下文，不披露（A-2）
        reused: vec![],
    });
    fold_role_cells(&mut report, deep_records, false);
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
            "resume: incremental jobs cannot be resumed; re-run `redteam review --incremental-from <prior-job>`".into(),
        );
    }
    let prior_records = store.read_partial(job_id);
    let mut queue_snap = store.read_queue_items(job_id);
    if queue_snap.is_empty()
        && prior_records.iter().any(|r| !matches!(r.output, crate::domain::CellOutput::Scan(_)))
    {
        return partial_report(
            store, job_id,
            "resume: job has non-scanner cell records (pre-M21a finding job); cannot resume with the scan-only binary — re-run review".into(),
        );
    }
    let done: std::collections::BTreeSet<String> = prior_records.into_iter().filter(|r| r.ok).map(|r| r.cell).collect();
    let pending: Vec<&crate::state::CellInput> =
        manifest.iter().filter(|c| !done.contains(&c.cell)).collect();
    // 本次要补跑 scanner（不论补跑成败）= 首跑收场时疑点集不全：深挖续跑时被截项复位重选（design C2 §6 RS1〔D15〕）。
    let replayed = !pending.is_empty();
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

    // 收口：全部 scanner 记录（已完成 + 新补 + 失败）；组骨架 / 分组观测 / head_sha / 覆盖自报 / dossier 从原报告沿用（resume 不重收集证据）。
    // scanner 格走 scan-only 收口；深挖落盘的 reviewer 格另记（喂进 scan 收口会被判成失败的 scanner 格 → 假 Incomplete）。
    let (scan_records, deep_records): (Vec<CellRecord>, Vec<CellRecord>) =
        store.read_partial(job_id).into_iter().partition(|r| r.role() == Role::Scanner);
    let ok_cells: std::collections::BTreeSet<&str> =
        scan_records.iter().filter(|r| r.ok).map(|r| r.cell.as_str()).collect();
    let still_missing = manifest.iter().filter(|c| !ok_cells.contains(c.cell.as_str())).count();
    let reused: Vec<String> = scan_records.iter().filter(|r| r.ok && done.contains(&r.cell)).map(|r| r.cell.clone()).collect();
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
    let scan_cells = scan_cells_from_records(scan_records);
    let results = ScanResults { cells: scan_cells.clone(), ..Default::default() };
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
        dormant_leads: vec![], // resume 收口没有本轮变更集上下文，不披露（A-2）
        reused,
    });
    if still_missing > 0 {
        report.verdict = Verdict::Incomplete;
        report.run.warnings.push(format!("resume: {still_missing} cell(s) still missing after replay"));
        code = exit_code(Verdict::Incomplete, false);
    }
    // 深挖离线续跑（design C2 §6 RS1〔D15〕）：配了 reviewer **且首跑确实开过深挖**才进——不要求 scan 补齐（仍缺格时判定已是 Incomplete，
    // 深挖照跑、findings 照进，与首跑 F3 同口径），也不要求有队列快照（扫描期被 kill 时深挖可能还没入过队：空快照 = 新队列，H0 按当前 diff 重建）。
    // 「首跑开过深挖」= 有队列头（C2 起扇出前落）或有队列快照（C1 起的旧作业）；两者都无 = 首跑是 scan-only 作业，
    // 事后给配置加了 reviewer 也不改判它（评审 #5：那会整轮深挖并覆盖库内原结果）。
    // 注意「全 Settled」也必须进：run_deep 会 0 次派发直接把已挖出的 findings 带回报告；
    // 跳过它 → report.findings 停在 scan-only 的空集 → 尾段 write_result 用一份无 finding 的
    // 报告覆盖掉上一轮的成果（deep 已跑完才被 kill 的作业就是这样丢结果的）。
    let deep_header = store.read_state(job_id).and_then(|st| st.deep);
    if !roles.reviewers.is_empty() && (deep_header.is_some() || !queue_snap.is_empty()) {
        // 带标签块：diff 重建失败时也要走到函数尾部回写结果（`--resume` 只打印不落库，早 return 会让库里停在旧报告）。
        'deep: {
            let (reopened, reconciled) = reconcile_queue_with_cells(&mut queue_snap, &deep_records);
            // 对账改过状态的项写回快照（下一次 resume 不必再对一遍）；重挖的项派发时 run_deep 会再写。
            for it in reconciled.iter().filter_map(|id| queue_snap.iter().find(|it| &it.id == id)) {
                if let Err(e) = store.put_queue_item(job_id, it) {
                    eprintln!("warn: job {job_id} resume queue snapshot 失败: {e}");
                }
            }
            let branch_scope = args.scope == "branch";
            // 已落盘 reviewer 格里的 finding——深挖的权威成果，无论后面走哪条路都不能丢。
            // **判决在这里就贴上**（收口评审 #1）：下面「diff 重建不了」那条路会直接 `break 'deep`
            // 用这批 finding 造报告，走不到 run_deep 的回填；漏贴 = 往轮已被驳回的 finding
            // 在本轮报告里复活成拦路 blocking，往轮的驳回记录从 JSON 里消失。
            // 往轮已判过的不重验（〔D17〕Q11）：verifier 格按 finding id 回填。
            let known = crate::orchestrate::deep::known_verdicts(&deep_records);
            let prior_findings: Vec<crate::domain::Finding> = deep_records
                .iter()
                .flat_map(|r| r.output.findings().to_vec())
                .map(|mut f| {
                    if let Some(v) = known.get(&f.id) {
                        f.set_verdict(v.clone());
                    }
                    f
                })
                .collect();
            // 未完 = 有没挖完的项，或有成功 scanner 格的疑点从没到过队列（补跑出的新疑点、首跑到达丢失）——
            // diff 重建失败时据此计债，不把「从没挖过」收成干净通过（评审 #1）。
            let unfinished = crate::orchestrate::deep::unfinished(&queue_snap, &scan_cells);
            let changes = match crate::evidence::git::changed_files(cwd, args.base.as_deref(), branch_scope) {
                Ok(c) => c,
                Err(e) => {
                    // diff 重建不了 → 未完项没法重跑，但已挖出的 finding 照样进报告（未完 = 带债）。
                    report.run.warnings.push(format!("resume deep: cannot rebuild diff: {e}"));
                    let deep = crate::orchestrate::deep::DeepRun {
                        findings: crate::consolidate::dedup(prior_findings),
                        cells: vec![],
                        // 截断数取自快照（评审修 #3）：空 meta 会让首跑被预算截掉的项既不计债也不披露。
                        meta: crate::domain::DeepMeta {
                            queue_split_clusters: crate::consolidate::scan::split_clusters(&scan_cells, &queue_snap),
                            ..crate::orchestrate::deep::restored_meta(&queue_snap)
                        },
                        warnings: vec![],
                        reviewer_failed: unfinished,
                        reused_cells: deep_records.iter().filter(|r| r.ok).cloned().collect(),
                        items: queue_snap.clone(),
                    };
                    let over = overlay_deep(report, code, deep, &args, cfg, registry, t0);
                    (report, code) = over;
                    break 'deep;
                }
            };
            let selected = select_files(changes);
            // 阶段 0 路由按当前 diff 重算（design C2 §6〔D15〕）：H0 每轮幂等重入队（快照为空时等于重建）；连队列头与原报告 risk 段
            // 都没有时，预算也退到重算值（缺省 4 会把首跑留下的项截掉）。
            let prior_risk = prior.as_ref().and_then(|r| r.run.risk.clone());
            let known_budget = deep_header.map(|h| h.budget_cells).or_else(|| prior_risk.as_ref().map(|r| r.budget_cells));
            // 每轮都按当前 diff 重算并重入队 H0（评审 #8）：到达键保证幂等，首跑入队快照写失败的 H0 靠这一步补回。
            let (risk_now, route_now) = stage0_risk(&selected, cwd, &args, cfg, prior.as_ref().and_then(|r| r.run.head_sha.as_deref()));
            // 预算优先取首跑落的队列头：被 kill 的作业没有原报告，按当前配置 / 工作树重算 K 会把首跑已选中的项截掉（评审 #5）。
            let budget_cells = known_budget.or_else(|| risk_now.as_ref().map(|r| r.budget_cells)).unwrap_or(4);
            if report.run.risk.is_none() {
                report.run.risk = risk_now;
            }

            // spec 段随重跑一起带回：resume 的 reviewer prompt 必须与首跑同构，缺了契约段
            // 同一 cell 两次判读口径就不一样。
            let (budget, spec_section, spec_warnings) = budget_and_spec(&roles, &args.spec);
            report.run.warnings.extend(spec_warnings);
            let CellPlan { bundles, .. } =
                plan_cells(selected, cwd, budget, &cfg.review.grouping, &cfg.review.pack, &[], None);
            // 「re-digging」只在真会重挖时披露（评审第 2 档 #7）：变更集为空时 run_deep 早退、另报「无法重跑」并计债。
            if reopened > 0 && !bundles.is_empty() {
                report.run.warnings.push(format!(
                    "resume deep: {reopened} settled item(s) have no successful reviewer cell — re-digging"
                ));
            }
            let target_label = if branch_scope {
                format!("branch {}...HEAD", args.base.as_deref().unwrap_or("?"))
            } else {
                "working tree".to_string()
            };
            let snap2 = |it: &crate::orchestrate::queue::QueueItem| {
                if let Err(e) = store.put_queue_item(job_id, it) {
                    eprintln!("warn: job {job_id} resume queue snapshot 失败: {e}");
                }
            };
            // 往轮格里的 finding（按格名）：重挖失败时同名 upsert 会用空输出的失败格覆盖掉它们。
            let carriers: BTreeMap<&str, &[crate::domain::Finding]> = deep_records
                .iter()
                .filter(|r| !r.output.findings().is_empty())
                .map(|r| (r.cell.as_str(), r.output.findings()))
                .collect();
            let on_cell2 = |rec: &crate::state::CellRecord| {
                // 重挖失败不抹掉往轮载体（评审修 #2 连带）：本轮报告靠事先读出的 prior findings 侥幸完整，
                // 下一次 resume 就永久丢失——失败格照写（债与重挖资格不变），但带上往轮 finding。
                let carried;
                let rec = match carriers.get(rec.cell.as_str()) {
                    Some(fs) if !rec.ok && rec.output.findings().is_empty() => {
                        let mut r = rec.clone();
                        r.output = crate::domain::CellOutput::Review { findings: fs.to_vec(), needs: vec![] };
                        carried = r;
                        &carried
                    }
                    _ => rec,
                };
                if let Err(e) = store.append_partial(job_id, rec) {
                    eprintln!("warn: job {job_id} resume reviewer cell 落盘失败（{}）: {e}", rec.cell);
                }
            };
            let factory = per_cell_backend_factory(cfg);
            let thresholds = verdict_thresholds(&args, cfg);
            let deep = run_deep(
                registry,
                DeepInput {
                    route: route_now.as_ref(),
                    // 全部成功 scanner 格按到达键幂等入队：首跑已到达的疑点跳过，补跑出的新疑点照常入队；旧快照（无到达键）走谓词合并兜底
                    scan_cells: &scan_cells,
                    bundles: &bundles,
                    root: cwd,
                    reviewers: &roles.reviewers,
                    budget_cells,
                    pack: &cfg.review.pack,
                    spec_docs: &spec_section,
                    target_label: &target_label,
                    snapshot: Some(&snap2),
                    restore: Some(&queue_snap),
                    prior_findings,
                    tools: resolve_tools(&args, cfg),
                    backend_factory: Some(factory.as_ref()),
                    verify: Some(crate::orchestrate::deep::VerifyInput {
                        roles: &roles,
                        enabled: cfg.review.verifiers.enabled,
                        block_threshold: thresholds.0,
                        conf_floor: thresholds.1,
                        precedents: &ledger.cards,
                        known: &known,
                    }),
                    on_cell: Some(&on_cell2),
                    incoming: None,
                    reopen_skipped: replayed,
                },
                &(),
            )
            .await;
            let mut deep = deep;
            // 沿用 = 往轮的成功格且本轮没重挖（同名格被本轮 upsert 覆盖的以本轮为准）；往轮失败格的项已按
            // 「Settled 以成功格为准」重开——要么本轮重挖覆盖，要么（换了 reviewer）旧失败格作废、不再报。
            let redone: std::collections::BTreeSet<&str> = deep.cells.iter().map(|c| c.cell.as_str()).collect();
            deep.reused_cells = deep_records.iter().filter(|r| r.ok && !redone.contains(r.cell.as_str())).cloned().collect();
            // 拆簇对账与首跑同口径（评审 #12）：resume 不算就会用 0 覆盖首跑落库的读数
            deep.meta.queue_split_clusters = crate::consolidate::scan::split_clusters(&scan_cells, &deep.items);
            // 复核账把**复用格**也算进来（M21c 批 A-4，M21b 第 6 项实测的债）：
            // 原先 meta 只统计本轮新验的，于是崩溃续跑后报告里「复核触发 0 / 驳回 0」——
            // 极端情况（杀在复核尾部、续跑无事可做）整组计数全空，出口验收若按报告字段判会误判成「一次没验」。
            crate::orchestrate::deep::fold_reused_verify(&mut deep.meta, &deep.reused_cells);
            // 不可重建的照实说，别静默留 null：`close_ms` 是首跑扇出关闭的时刻、`budget_cells` 是首跑的预算档，
            // 两者都不在 cell 记录里，续跑重算只会得到本轮的数、与首跑语义不同。
            if deep.meta.close_ms == 0 {
                report.run.warnings.push(
                    "resume: deep.close_ms / budget_cells 属首跑时刻量，cell 记录里没有、续跑不可重建（复核与 findings 账已按复用格补齐）".into(),
                );
            }
            let over = overlay_deep(report, code, deep, &args, cfg, registry, t0);
            report = over.0;
            code = over.1;
        }
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
/// resume 快照对账：队列项状态**双向以成功 reviewer 格为准**（格名以 `review[{item.id}]@` 开头）。快照只记状态，格才是 finding 的载体。
/// - 有成功格 → Settled（评审修 #4）：kill 落在「格已落盘、快照仍 InFlight / Pending」之间，或快照写失败只 warning——该项已挖完，
///   不重挖。旧口径会重挖，重挖失败时同名 upsert 把成功格覆盖成失败格，finding 失去载体，下一次 resume 永久丢失。
/// - Settled 却无成功格 → InFlight 重挖（批 C1-1）：无格 = cell 落盘失败而快照写成功（旧作业的 `review[n]@` 格名也落在这里，重挖一次）；
///   只有失败格 = 首跑那格失败或二阶段失败——不重挖，它的债在 resume 报告里被抹成干净通过（scanner resume 同口径：失败格重跑）。
///   记 InFlight（= 往轮派发过、未完成）而非 Pending（C2-3）：`run_deep` 把往轮在途项计入已派发、不交给收场 drain——
///   记成 Pending 会与从没派发过的项一起按剩余预算重选，被截成 Skipped 就既不重挖也不计债。
///
/// 返回（改回待挖的项数，状态被改过的项 id）。
fn reconcile_queue_with_cells(items: &mut [crate::orchestrate::queue::QueueItem], records: &[CellRecord]) -> (usize, Vec<String>) {
    use crate::orchestrate::queue::ItemState;
    let mut reopened = 0;
    let mut changed = Vec::new();
    for it in items.iter_mut() {
        let prefix = crate::orchestrate::deep::review_cell_prefix(&it.id);
        let has_ok = records.iter().any(|r| r.ok && r.cell.starts_with(&prefix));
        let next = match (has_ok, it.state) {
            (true, ItemState::Settled) => continue,
            (true, _) => ItemState::Settled,
            (false, ItemState::Settled) => {
                reopened += 1;
                ItemState::InFlight
            }
            (false, _) => continue,
        };
        it.state = next;
        changed.push(it.id.clone());
    }
    (reopened, changed)
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

    /// 配了 reviewer 的作业阶段时间线（design C2 §2.3〔D15〕）：Collecting → Scan（manifest）→ Deep（扫描全部返回时）→ Finalizing。
    #[tokio::test]
    async fn run_job_with_reviewer_phases_scan_deep_finalizing() {
        let dir = tempfile::tempdir().unwrap();
        git_staged(dir.path(), "a.rs", "fn main() {\n    let x = 1;\n}\n");
        let mut registry = scan_registry("p/alpha", r#"{"suspicions":[{"file":"a.rs","line_start":2,"line_end":2,"hint":"t","confidence":0.9,"quote":"let x = 1;"}],"scanned_areas":[]}"#);
        registry.insert_scan("p/r", "p", 8, Box::new(FakeChatBackend::repeating(scan_turn(r#"{"findings":[],"needs":[]}"#))), std::time::Duration::from_secs(5), 0);
        let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"alpha":{"roles":{"scanner":{}}},"r":{"roles":{"reviewer":{}}}}}}}"#).unwrap();
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let store = Arc::new(crate::state::JobStore::open(&ws).unwrap());
        store.put_state_for_test(&JobState::new("j1", 500)).unwrap();

        let code = run_job("j1", &code_args(), &registry, &cfg, dir.path(), 500, Arc::clone(&store)).await;

        let st = store.read_state("j1").unwrap();
        assert_eq!((st.status, code), (JobStatus::Done, 0));
        use crate::state::Phase;
        assert_eq!(st.phase_history.iter().map(|p| p.phase).collect::<Vec<_>>(), [Phase::Collecting, Phase::Scan, Phase::Deep, Phase::Finalizing]);
        // 进度只数 scanner 格（评审 #12）：深挖与扫描同时跑，reviewer 格也计会显示超分母进度
        assert_eq!((st.progress.done, st.progress.total), (1, 1));
        // 队列头在扇出前落盘（评审 #5）：resume 据此判「首跑开过深挖」并沿用同一个 K
        assert_eq!(st.deep, Some(crate::state::DeepHeader { budget_cells: 4 }));
        let res = store.read_result("j1").unwrap();
        assert!(res.contains("\"verdict\": \"approve\"") && res.contains("review[a.rs:"), "{res}");
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
                    // scanner 格：进度分子只数它们（评审 #12），并发下仍不能丢更新
                    sink.on_cell(crate::state::CellRecord::ok(
                        format!("scan[g{i}]@p/m#1"),
                        "p/m".to_string(),
                        crate::domain::CellOutput::Scan(Default::default()),
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

    /// deep 已跑完才被 kill：队列快照全 Settled、reviewer 格已落盘 → resume 必须把
    /// 已挖出的 finding 带进报告并回写 proj.db。跳过深挖收口 = 用一份空 findings 的
    /// scan-only 报告覆盖上一轮成果。
    #[tokio::test]
    async fn resume_after_deep_done_keeps_findings() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        std::fs::write(ws.join("c.rs"), "marker\n").unwrap();
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &[input("scan[g1]@p/m#1", "p/m", "P1")]).unwrap();
        store.append_partial("j", &done_scan("scan[g1]@p/m#1", "p/m", "c.rs")).unwrap();
        // 已挖完的 reviewer 格（finding 的权威载体；格名按队列项，与快照项对得上）
        let f = crate::domain::finding::fixtures::finding("f1", crate::domain::Severity::Low, 1, "p/r");
        let rev_rec = CellRecord::ok(
            "review[c.rs:rule:1-2]@p/r", "p/r",
            crate::domain::CellOutput::Review { findings: vec![f], needs: vec![] },
        ).with_family("r");
        store.append_partial("j", &rev_rec).unwrap();
        // 全 Settled 队列快照
        let spec = crate::pack::region::QueueItemSpec::new(
            "c.rs".into(), vec![(1, 2)], crate::pack::region::Origin::Rule, vec![],
        ).unwrap();
        let item = crate::orchestrate::queue::QueueItem {
            id: "c.rs:rule:1-2".into(), spec, score: 1.0, agreement: 1,
            state: crate::orchestrate::queue::ItemState::Settled,
            kind: crate::orchestrate::queue::ItemKind::Primary,
            dispatch_agreement: 1, landed_agreement: Some(1), families: vec![], enqueued_ms: None,
            arrivals: vec![], members: vec![], member_families: vec![], block: Default::default(), peak_agreement: 0, risk_band: 0,
        };
        store.put_queue_item("j", &item).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}},"r":{"roles":{"reviewer":{}}}}}}}"#).unwrap();
        let (reg, _backend) = recording_registry("p/m", "c.rs");

        let (report, _code) = run_resume("j", &reg, &cfg, ws, &store).await;

        assert_eq!(report.findings.len(), 1, "已挖出的 finding 必须留在报告里: {:?}", report.run.warnings);
        // diff 重建失败分支（ws 非 git 仓）同样不许把 reviewer 格当失败的 scanner 格
        assert!(!report.run.coverage.failed.contains_key(&Role::Scanner), "{:?}", report.run.coverage);
        assert_eq!(report.run.coverage.reused.get(&Role::Reviewer), Some(&vec!["review[c.rs:rule:1-2]@p/r".to_string()]));
        if let Some(stored) = store.read_result("j") {
            let stored: ReviewReport = serde_json::from_str(&stored).unwrap();
            assert_eq!(stored.findings.len(), 1, "回写 proj.db 的报告不得把 finding 清空");
        }
    }

    fn git(root: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git").current_dir(root).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// 带未提交改动的 git 仓：各文件先提交、再追加一行——resume 深挖段要重建工作树 diff。
    fn repo_with_changes(root: &std::path::Path, files: &[(&str, &str)]) {
        git(root, &["init", "-q"]);
        for (path, body) in files {
            std::fs::write(root.join(path), body).unwrap();
        }
        git(root, &["add", "."]);
        git(root, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
        for (path, body) in files {
            std::fs::write(root.join(path), format!("{body}// changed\n")).unwrap();
        }
    }

    /// H0 主区形态的队列快照项（id 与 `QueueItem::new` 同形）。
    fn rule_item(file: &str, state: crate::orchestrate::queue::ItemState) -> crate::orchestrate::queue::QueueItem {
        let spec = crate::pack::region::QueueItemSpec::new(file.into(), vec![(1, 2)], crate::pack::region::Origin::Rule, vec![]).unwrap();
        crate::orchestrate::queue::QueueItem {
            id: format!("{file}:rule:1-2"), spec, score: 1.0, agreement: 1, state,
            kind: crate::orchestrate::queue::ItemKind::Primary,
            dispatch_agreement: 1, landed_agreement: None, families: vec![], enqueued_ms: None,
            arrivals: vec![], members: vec![], member_families: vec![], block: Default::default(), peak_agreement: 0, risk_band: 0,
        }
    }

    fn review_json(file: &str, quote: &str, title: &str) -> String {
        format!(r#"{{"findings":[{{"severity":"low","title":"{title}","body":"b","file":"{file}","line_start":2,"line_end":2,"confidence":0.4,"recommendation":"r","quote":"{quote}"}}],"needs":[]}}"#)
    }

    fn deep_cfg() -> Config {
        Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}},"r":{"roles":{"reviewer":{}}}}}}}"#).unwrap()
    }

    /// scanner `p/m` + reviewer `p/r`（每次都回同一份 finding JSON；`Arc` 句柄留给测试数调用次数）。
    fn deep_registry(review: &str) -> (ProviderRegistry, Arc<FakeChatBackend>) {
        let turn = |text: &str| AssistantTurn {
            text: text.into(), tool_calls: vec![], input_tokens: 1, output_tokens: 1, cached_input_tokens: 0, finish_reason: Some("stop".into()), reasoning_tokens: None,
        };
        let rev = Arc::new(FakeChatBackend::repeating(turn(review)));
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/m", "p", 8, Box::new(FakeChatBackend::repeating(turn(r#"{"suspicions":[],"scanned_areas":[]}"#))), std::time::Duration::from_secs(5), 0);
        reg.insert_scan("p/r", "p", 8, Box::new(Arc::clone(&rev)), std::time::Duration::from_secs(5), 0);
        (reg, rev)
    }

    /// scan 段已补齐的作业骨架（manifest 一格且已完成、**零疑点** + args）。零疑点是为了与手写的队列快照自洽：C2 起 resume 把全部
    /// scanner 疑点按到达键幂等入队，快照里没记到达的疑点会被当成未挖的新疑点挖掉（`resume_*` 深挖用例只关心快照里的项）。
    /// 首跑开过深挖的作业头（C2 起扇出前落；resume 据此区分「首跑 scan-only」与「深挖开过被 kill」，并沿用同一个 K）。
    fn deep_started(store: &crate::state::JobStore, budget_cells: u32) {
        let st = crate::state::JobState {
            deep: Some(crate::state::DeepHeader { budget_cells }),
            ..crate::state::JobState::new("j", 0)
        };
        store.put_state_for_test(&st).unwrap();
    }

    fn scan_done_job(store: &crate::state::JobStore) {
        store.write_manifest("j", &[input("scan[g1]@p/m#1", "p/m", "P1")]).unwrap();
        let empty = CellRecord::ok("scan[g1]@p/m#1", "p/m", crate::domain::CellOutput::Scan(Default::default()));
        store.append_partial("j", &empty).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
    }

    /// 批 C resume 覆盖丢 finding（C1-1 修）：首跑 x.rs 已挖完落盘、y.rs 派出后被 kill → resume 两次。
    /// 批 C 的 reviewer cell_id 按单次调用的派发序（`review[1]@m`，每次调用从 1 起），`cell_put` 按名 upsert：
    /// 第一次 resume 挖 y.rs 时覆盖了 x.rs 的格——当次报告靠事前读出的 prior findings 侥幸完整，
    /// 第二次 resume 时 x.rs 的 finding 已无载体、永久丢失。
    #[tokio::test]
    async fn resume_twice_keeps_findings_of_earlier_cells() {
        use crate::orchestrate::queue::ItemState;
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        repo_with_changes(ws, &[("x.rs", "fn x() {\n    x_marker();\n}\n"), ("y.rs", "fn y() {\n    y_marker();\n}\n")]);
        let store = crate::state::JobStore::open(ws).unwrap();
        scan_done_job(&store);
        // 首跑现场：x.rs 的格按批 C 形态命名，y.rs 在途（快照 InFlight、无格）
        let mut fx = crate::domain::finding::fixtures::finding("x issue", crate::domain::Severity::Low, 1, "p/r");
        (fx.file, fx.quote, fx.line_start, fx.line_end) = ("x.rs".into(), "x_marker();".into(), 2, 2);
        let first_cell = CellRecord::ok("review[1]@p/r", "p/r", crate::domain::CellOutput::Review { findings: vec![fx], needs: vec![] });
        store.append_partial("j", &first_cell.with_family("r")).unwrap();
        store.put_queue_item("j", &rule_item("x.rs", ItemState::Settled)).unwrap();
        store.put_queue_item("j", &rule_item("y.rs", ItemState::InFlight)).unwrap();
        let (reg, _rev) = deep_registry(&review_json("y.rs", "y_marker();", "y issue"));

        let (first, _) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;
        let (second, _) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

        let files = |r: &ReviewReport| r.findings.iter().map(|f| f.file.clone()).collect::<std::collections::BTreeSet<_>>();
        assert!(files(&first).contains("x.rs") && files(&first).contains("y.rs"), "{:?}", first.findings);
        assert!(files(&second).contains("x.rs"), "第二次 resume 丢了首跑 x.rs 的 finding: {:?}", second.findings);
        let carried = store.read_partial("j").iter().any(|r| r.output.findings().iter().any(|f| f.file == "x.rs"));
        assert!(carried, "x.rs 的 finding 在 cells 表里必须仍有载体");
    }

    /// 快照状态双向以成功格为准（评审修 #4）：kill 落在「格已落盘、快照仍 InFlight」之间 → 该项已挖完，resume 不重挖。
    /// 旧口径会重挖；重挖失败时同名 upsert 把往轮成功格覆盖成失败格，finding 失去载体，下一次 resume 永久丢失。
    #[tokio::test]
    async fn resume_treats_inflight_item_with_ok_cell_as_settled() {
        use crate::orchestrate::queue::ItemState;
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        repo_with_changes(ws, &[("x.rs", "fn x() {\n    x_marker();\n}\n")]);
        let store = crate::state::JobStore::open(ws).unwrap();
        scan_done_job(&store);
        let mut fx = crate::domain::finding::fixtures::finding("x issue", crate::domain::Severity::Low, 1, "p/r");
        (fx.file, fx.quote, fx.line_start, fx.line_end) = ("x.rs".into(), "x_marker();".into(), 2, 2);
        let fid = fx.id.clone();
        let landed = CellRecord::ok("review[x.rs:rule:1-2]@p/r", "p/r", crate::domain::CellOutput::Review { findings: vec![fx], needs: vec![] });
        store.append_partial("j", &landed).unwrap();
        // 往轮已复核过这条 finding（A-4：复用格的复核账要并回 meta，否则续跑报告里「复核触发 0」）
        store.append_partial("j", &CellRecord::ok(
            format!("verify[{fid}]@p/v"), "p/v",
            crate::domain::CellOutput::Verify { verdicts: vec![crate::domain::VerifyEntry {
                id: fid.clone(), verdict: crate::domain::VerifyVerdict::Upheld, note: "看过了".into(),
            }] },
        )).unwrap();
        store.put_queue_item("j", &rule_item("x.rs", ItemState::InFlight)).unwrap();
        // reviewer 一调就失败：若被重挖，失败格会同名覆盖掉上面的成功格
        let (reg, rev) = deep_registry("not-json");

        let (first, code1) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;
        let (second, code2) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

        assert_eq!(rev.turn_count(), 0, "已有成功格的项不重挖");
        // A-4：往轮的 verify 格是「复用」的，它的账必须并回 meta——否则续跑报告里「复核触发 0 / 驳回 0」，
        // 按报告字段判会误判成「一次没验」（M21b 出口验收第 6 项实测）。不可重建的那些要照实说。
        for r in [&first, &second] {
            let deep = r.run.deep.as_ref().expect("续跑有深挖段");
            assert_eq!(deep.verify_triggered, 1, "复用的复核格要计入触发数: {deep:?}");
            assert!(r.run.warnings.iter().any(|w| w.contains("close_ms") && w.contains("不可重建")),
                    "不可重建的字段要照实说: {:?}", r.run.warnings);
        }
        for (r, code) in [(&first, code1), (&second, code2)] {
            assert_eq!(r.findings.len(), 1, "{:?}", r.run.warnings);
            assert_eq!(code, 0, "{:?}", r.run.warnings);
        }
        assert!(store.read_partial("j").iter().any(|r| r.ok && r.cell == "review[x.rs:rule:1-2]@p/r"), "成功格原样在库");
    }

    /// 重挖失败不抹掉往轮载体（评审修 #2 连带）：往轮失败格仍携带 finding（二阶段失败保留的首轮结果）→ resume 重挖又失败，
    /// 同名 upsert 不得用空输出把它覆盖掉——否则本轮报告靠事先读出的 prior findings 侥幸完整，下一次 resume 丢失。
    #[tokio::test]
    async fn failed_redig_keeps_prior_findings_carrier() {
        use crate::orchestrate::queue::ItemState;
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        repo_with_changes(ws, &[("x.rs", "fn x() {\n    x_marker();\n}\n")]);
        let store = crate::state::JobStore::open(ws).unwrap();
        scan_done_job(&store);
        let mut fx = crate::domain::finding::fixtures::finding("x issue", crate::domain::Severity::Low, 1, "p/r");
        (fx.file, fx.quote, fx.line_start, fx.line_end) = ("x.rs".into(), "x_marker();".into(), 2, 2);
        let mut debt_cell = CellRecord::ok("review[x.rs:rule:1-2]@p/r", "p/r", crate::domain::CellOutput::Review { findings: vec![fx], needs: vec![] });
        debt_cell.ok = false;
        debt_cell.warn = Some("second-stage round failed: boom — first-round findings kept".into());
        store.append_partial("j", &debt_cell).unwrap();
        store.put_queue_item("j", &rule_item("x.rs", ItemState::Settled)).unwrap();
        let (reg, rev) = deep_registry("not-json");

        let (first, code1) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;
        let (second, code2) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

        assert!(rev.turn_count() > 0, "失败格的项要重挖");
        for (r, code) in [(&first, code1), (&second, code2)] {
            assert_eq!(r.findings.len(), 1, "往轮 finding 不丢: {:?}", r.run.warnings);
            assert_eq!(code, 2, "重挖失败仍是债");
        }
        let rec = store.read_partial("j").into_iter().find(|r| r.cell == "review[x.rs:rule:1-2]@p/r").unwrap();
        assert!(!rec.ok);
        assert_eq!(rec.output.findings().len(), 1, "库里的失败格仍携带往轮 finding");
    }

    /// resume 时 diff 重建不了（改动已提交 → 变更集为空；或不在 git 仓 → 重建报错），首跑被预算截掉、从没挖过的项必须仍计债并披露（评审修 #3）。
    /// 旧实现两条路径的深挖 meta 都是空的（`budget_skipped = 0`），往轮沿用格又让报告走正常收口 → Approve / exit 0，截断悄悄消失。
    #[tokio::test]
    async fn resume_without_rebuildable_diff_still_discloses_budget_skip() {
        use crate::orchestrate::queue::ItemState;
        for committed in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let ws = dir.path();
            std::fs::write(ws.join("x.rs"), "fn x() {\n    x_marker();\n}\n").unwrap();
            if committed {
                git(ws, &["init", "-q"]);
                git(ws, &["add", "."]);
                git(ws, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
            }
            let store = crate::state::JobStore::open(ws).unwrap();
            scan_done_job(&store);
            store.put_queue_item("j", &rule_item("x.rs", ItemState::Settled)).unwrap();
            store.put_queue_item("j", &rule_item("y.rs", ItemState::Skipped)).unwrap();
            let clean = CellRecord::ok("review[x.rs:rule:1-2]@p/r", "p/r", crate::domain::CellOutput::Review { findings: vec![], needs: vec![] });
            store.append_partial("j", &clean).unwrap();
            let (reg, _rev) = deep_registry(r#"{"findings":[],"needs":[]}"#);

            let (report, code) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

            let shape = if committed { "变更集为空" } else { "diff 重建报错" };
            assert_eq!(code, 2, "{shape}: 截断仍是债: {:?}", report.run.warnings);
            assert_eq!(report.run.deep.as_ref().map(|d| d.budget_skipped), Some(1), "{shape}");
            assert!(report.run.warnings.iter().any(|w| w.contains("skipped(budget) 1")), "{shape}: {:?}", report.run.warnings);
        }
    }

    /// 「re-digging」只在真会重挖时披露（评审第 2 档 #7）：diff 重建不了（变更集为空 / 不在 git 仓）时重开的项跑不了，
    /// 旧实现照样先推「re-digging」，与同一份报告里的「无法重跑」自相矛盾。
    #[tokio::test]
    async fn resume_does_not_claim_re_digging_when_diff_cannot_be_rebuilt() {
        use crate::orchestrate::queue::ItemState;
        for committed in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let ws = dir.path();
            std::fs::write(ws.join("x.rs"), "fn x() {\n    x_marker();\n}\n").unwrap();
            if committed {
                git(ws, &["init", "-q"]);
                git(ws, &["add", "."]);
                git(ws, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
            }
            let store = crate::state::JobStore::open(ws).unwrap();
            scan_done_job(&store);
            store.put_queue_item("j", &rule_item("x.rs", ItemState::Settled)).unwrap();
            store.append_partial("j", &CellRecord::failed("review[x.rs:rule:1-2]@p/r", "p/r", Role::Reviewer, "boom")).unwrap();
            let (reg, rev) = deep_registry(r#"{"findings":[],"needs":[]}"#);

            let (report, code) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

            let shape = if committed { "变更集为空" } else { "diff 重建报错" };
            assert_eq!(rev.turn_count(), 0, "{shape}: 跑不了");
            assert_eq!(code, 2, "{shape}: 未完项计债: {:?}", report.run.warnings);
            assert!(!report.run.warnings.iter().any(|w| w.contains("re-digging")), "{shape}: {:?}", report.run.warnings);
        }
    }

    /// resume 收口只把 scanner 格喂 scan-only 收口：批 C 把 cells 表全量喂进去，首跑落盘的 reviewer 格被当成
    /// 「失败的 scanner 格」→ 假 `scan incomplete` + Incomplete / exit 3。深挖跑完、零 finding、收尾时被 kill 的作业
    /// resume 后应是 Approve / 0，首跑的 reviewer 格记 `coverage.reused[Reviewer]`（与 scanner 沿用对称）。
    #[tokio::test]
    async fn resume_reports_prior_reviewer_cells_as_reused_not_failed_scanners() {
        use crate::orchestrate::queue::ItemState;
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        repo_with_changes(ws, &[("x.rs", "fn x() {\n    x_marker();\n}\n"), ("y.rs", "fn y() {\n    y_marker();\n}\n")]);
        let store = crate::state::JobStore::open(ws).unwrap();
        scan_done_job(&store);
        store.put_queue_item("j", &rule_item("x.rs", ItemState::Settled)).unwrap();
        let clean = CellRecord::ok("review[x.rs:rule:1-2]@p/r", "p/r", crate::domain::CellOutput::Review { findings: vec![], needs: vec![] });
        store.append_partial("j", &clean).unwrap();
        // y.rs：只有往轮失败格 → 本轮重挖、同名覆盖——报告里只算本轮完成；往轮失败格既不算沿用、也不重复进报告
        store.put_queue_item("j", &rule_item("y.rs", ItemState::Settled)).unwrap();
        store.append_partial("j", &CellRecord::failed("review[y.rs:rule:1-2]@p/r", "p/r", Role::Reviewer, "boom")).unwrap();
        let (reg, rev) = deep_registry(r#"{"findings":[],"needs":[]}"#);

        let (report, code) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

        assert_eq!(rev.turn_count(), 1, "只重挖只有失败格的 y.rs");
        assert_eq!((report.verdict, code), (Verdict::Approve, 0), "warnings={:?}", report.run.warnings);
        assert!(!report.run.warnings.iter().any(|w| w.contains("scan incomplete")), "{:?}", report.run.warnings);
        let cov = &report.run.coverage;
        assert!(cov.failed.is_empty(), "{cov:?}");
        assert_eq!(cov.reused.get(&Role::Scanner), Some(&vec!["scan[g1]@p/m#1".to_string()]));
        assert_eq!(cov.reused.get(&Role::Reviewer), Some(&vec!["review[x.rs:rule:1-2]@p/r".to_string()]));
        assert_eq!(cov.completed.get(&Role::Reviewer), Some(&vec!["review[y.rs:rule:1-2]@p/r".to_string()]));
        assert_eq!(cov.selected.get(&Role::Reviewer).map(Vec::len), Some(2));
        assert!(!cov.selected.get(&Role::Scanner).into_iter().flatten().any(|c| c.starts_with("review[")), "{cov:?}");
        assert_eq!(report.cells.len(), 3, "scanner 格 + 沿用 x + 本轮 y，各一条、不重复");
    }

    /// 「Settled 以成功格为准」（C1-1）：快照 Settled 但没有该项的成功 reviewer 格 → resume 重挖。
    /// 无格 = cell 落盘失败而快照写成功（`on_cell` 只 warning）；只有失败格 = 首跑那格失败——
    /// 不重挖的话它的债（exit 2）在 resume 报告里被抹成干净通过。有成功格的项不动。
    #[tokio::test]
    async fn resume_redigs_settled_items_without_ok_cell() {
        use crate::orchestrate::queue::ItemState;
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        repo_with_changes(ws, &[
            ("x.rs", "fn x() {\n    x_marker();\n}\n"),
            ("y.rs", "fn y() {\n    y_marker();\n}\n"),
            ("z.rs", "fn z() {\n    z_marker();\n}\n"),
        ]);
        let store = crate::state::JobStore::open(ws).unwrap();
        scan_done_job(&store);
        for f in ["x.rs", "y.rs", "z.rs"] {
            store.put_queue_item("j", &rule_item(f, ItemState::Settled)).unwrap();
        }
        let failed = CellRecord::failed("review[y.rs:rule:1-2]@p/r", "p/r", Role::Reviewer, "boom");
        store.append_partial("j", &failed).unwrap();
        let z_done = CellRecord::ok("review[z.rs:rule:1-2]@p/r", "p/r", crate::domain::CellOutput::Review { findings: vec![], needs: vec![] });
        store.append_partial("j", &z_done).unwrap();
        let (reg, rev) = deep_registry(&review_json("x.rs", "x_marker();", "x issue"));

        let (report, code) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

        assert_eq!(rev.turn_count(), 2, "x.rs（无格）与 y.rs（只有失败格）重挖，z.rs 不动");
        assert!(
            report.run.warnings.iter().any(|w| w == "resume deep: 2 settled item(s) have no successful reviewer cell — re-digging"),
            "真重挖时照常披露: {:?}", report.run.warnings
        );
        let cov = &report.run.coverage;
        let names = |m: &BTreeMap<Role, Vec<String>>| m.get(&Role::Reviewer).cloned().unwrap_or_default();
        assert_eq!(names(&cov.completed), vec!["review[x.rs:rule:1-2]@p/r", "review[y.rs:rule:1-2]@p/r"], "warnings={:?}", report.run.warnings);
        assert_eq!(names(&cov.reused), vec!["review[z.rs:rule:1-2]@p/r"], "z.rs 的成功格沿用");
        assert!(cov.failed.is_empty(), "y.rs 首跑的失败格已被重挖覆盖，不再报失败: {cov:?}");
        assert_eq!(code, 0, "重挖成功 → 首跑失败格的债已还清: {:?}", report.run.warnings);
    }

    // ── C2-5：resume 离线续跑（design §6 RS1〔D15〕）────────────────────────────

    /// scanner 条目 `p/m`（每格回 `scan_json`）+ reviewer `p/r`（每格回 `review`）；`Arc` 句柄留给测试数调用次数。
    fn rs_registry(scan_json: &str, review: &str) -> (ProviderRegistry, Arc<FakeChatBackend>, Arc<FakeChatBackend>) {
        let turn = |text: &str| AssistantTurn {
            text: text.into(), tool_calls: vec![], input_tokens: 1, output_tokens: 1, cached_input_tokens: 0, finish_reason: Some("stop".into()), reasoning_tokens: None,
        };
        let scan = Arc::new(FakeChatBackend::repeating(turn(scan_json)));
        let rev = Arc::new(FakeChatBackend::repeating(turn(review)));
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/m", "p", 8, Box::new(Arc::clone(&scan)), std::time::Duration::from_secs(5), 0);
        reg.insert_scan("p/r", "p", 8, Box::new(Arc::clone(&rev)), std::time::Duration::from_secs(5), 0);
        (reg, scan, rev)
    }

    fn susp_json(file: &str, line: u32, quote: &str) -> String {
        format!(r#"{{"suspicions":[{{"file":"{file}","line_start":{line},"line_end":{line},"hint":"h","confidence":0.6,"quote":"{quote}"}}],"scanned_areas":[]}}"#)
    }

    /// 首跑已落盘的 scanner 格（疑点已锚定：`file` 第 `line` 行 `quote`）。
    fn scan_rec(cell: &str, file: &str, line: u32, quote: &str) -> CellRecord {
        let out = crate::domain::ScanOutput {
            suspicions: vec![crate::domain::Suspicion { file: file.into(), line_start: line, line_end: line, hint: "h".into(), category: None, confidence: 0.6, quote: quote.into() }],
            ..Default::default()
        };
        CellRecord::ok(cell, "p/m", crate::domain::CellOutput::Scan(out)).with_family("m")
    }

    /// 修 C1 缺口（design §6 RS1 ③）：扫描期被 kill、队列快照为空（深挖还没入过队）、配了 reviewer 的作业——resume 补跑缺失 scanner 格后
    /// 必须接着深挖（疑点入队 → 收场 drain → 挖），不能出 scan-only / 退出码 0。
    #[tokio::test]
    async fn resume_with_empty_queue_snapshot_still_digs() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        repo_with_changes(ws, &[("x.rs", "fn x() {\n    x_marker();\n}\n"), ("y.rs", "fn y() {\n    y_marker();\n}\n")]);
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &[input("scan[g1]@p/m#1", "p/m", "P1"), input("scan[g2]@p/m#1", "p/m", "P2")]).unwrap();
        store.append_partial("j", &scan_rec("scan[g1]@p/m#1", "x.rs", 2, "x_marker();")).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        deep_started(&store, 4);
        let (reg, scan, rev) = rs_registry(&susp_json("y.rs", 2, "y_marker();"), r#"{"findings":[],"needs":[]}"#);

        let (report, code) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

        assert_eq!(scan.turn_count(), 1, "只补跑缺失的 g2");
        assert_eq!(rev.turn_count(), 2, "两条疑点都挖: {:?}", report.run.warnings);
        assert_eq!((report.verdict, code), (Verdict::Approve, 0), "{:?}", report.run.warnings);
        assert_eq!(report.run.coverage.completed.get(&Role::Reviewer).map(Vec::len), Some(2));
        let deep = report.run.deep.as_ref().expect("深挖观测段");
        assert_eq!((deep.enqueue_reasons.get("scan"), deep.budget_skipped), (Some(&2), 0));
    }

    fn rs_cfg(max_cells: u32) -> Config {
        Config::from_json(
            &r#"{"review":{"reviewers":{"maxCells":MAX}},"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}},"r":{"roles":{"reviewer":{}}}}}}}"#
                .replace("MAX", &max_cells.to_string()),
        )
        .unwrap()
    }

    /// 疑点形态的队列快照项（经 `EnqueueReq::scan` 构造：笔记 / 到达键 / id 与首跑同形；家族 `m`、到达键 `{cell}#0`）。
    fn snap_susp(file: &str, line: u32, quote: &str, cell: &str, state: crate::orchestrate::queue::ItemState) -> crate::orchestrate::queue::QueueItem {
        use crate::orchestrate::queue::{DeepQueue, EnqueueReq};
        let s = crate::domain::Suspicion { file: file.into(), line_start: line, line_end: line, hint: "h".into(), category: None, confidence: 0.6, quote: quote.into() };
        let mut q = DeepQueue::new();
        q.enqueue(EnqueueReq::scan(&s, "m", crate::consolidate::scan::arrival_key(cell, 0, &s)).unwrap());
        let mut it = q.items()[0].clone();
        (it.state, it.dispatch_agreement) = (state, 1);
        it
    }

    fn ripple(file: &str, state: crate::orchestrate::queue::ItemState) -> crate::orchestrate::queue::QueueItem {
        let mut it = rule_item(file, state);
        it.kind = crate::orchestrate::queue::ItemKind::Ripple;
        it
    }

    /// `<name>.rs` 第 2 行 `<name>_marker();` 上的一条 low finding（reviewer 格载体）。
    fn marker_finding(name: &str) -> crate::domain::Finding {
        let mut f = crate::domain::finding::fixtures::finding(&format!("{name} issue"), crate::domain::Severity::Low, 1, "p/r");
        (f.file, f.quote, f.line_start, f.line_end) = (format!("{name}.rs"), format!("{name}_marker();"), 2, 2);
        f
    }

    fn review_rec(item_id: &str, findings: Vec<crate::domain::Finding>) -> CellRecord {
        CellRecord::ok(format!("review[{item_id}]@p/r"), "p/r", crate::domain::CellOutput::Review { findings, needs: vec![] })
    }

    fn marker_repo(ws: &std::path::Path, names: &[&str]) {
        let files: Vec<(String, String)> = names.iter().map(|n| (format!("{n}.rs"), format!("fn {n}() {{\n    {n}_marker();\n}}\n"))).collect();
        repo_with_changes(ws, &files.iter().map(|(p, b)| (p.as_str(), b.as_str())).collect::<Vec<_>>());
    }

    fn reviewer_names(bucket: &BTreeMap<Role, Vec<String>>) -> Vec<String> {
        bucket.get(&Role::Reviewer).cloned().unwrap_or_default()
    }

    /// RS1 ①（design §6）：扫描期被 kill——H0 一项已挖完（格带 finding）、一项在途，g1 的疑点已到达未派，g2 未落盘。resume 补跑 g2 →
    /// g1 疑点幂等跳过、g2 疑点新入队 → 收场 drain（往轮在途占位）→ 挖在途 H0 与两条疑点；往轮 finding 不丢不双计。再 resume 一次：无事可做、结果不变。
    #[tokio::test]
    async fn resume_after_scan_phase_kill_replays_enqueues_and_digs() {
        use crate::orchestrate::queue::ItemState;
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        marker_repo(ws, &["a", "b", "x", "y"]);
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &[input("scan[g1]@p/m#1", "p/m", "P1"), input("scan[g2]@p/m#1", "p/m", "P2")]).unwrap();
        store.append_partial("j", &scan_rec("scan[g1]@p/m#1", "x.rs", 2, "x_marker();")).unwrap();
        store.append_partial("j", &review_rec("a.rs:rule:1-2", vec![marker_finding("a")])).unwrap();
        store.put_queue_item("j", &rule_item("a.rs", ItemState::Settled)).unwrap();
        store.put_queue_item("j", &rule_item("b.rs", ItemState::InFlight)).unwrap();
        store.put_queue_item("j", &snap_susp("x.rs", 2, "x_marker();", "scan[g1]@p/m#1", ItemState::Pending)).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        let (reg, scan, rev) = rs_registry(&susp_json("y.rs", 2, "y_marker();"), r#"{"findings":[],"needs":[]}"#);

        let (report, code) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

        assert_eq!((scan.turn_count(), rev.turn_count()), (1, 3), "补跑 g2；挖 b + x + y: {:?}", report.run.warnings);
        assert_eq!((report.verdict, code), (Verdict::Approve, 0), "{:?}", report.run.warnings);
        assert_eq!(report.findings.iter().map(|f| f.title.as_str()).collect::<Vec<_>>(), ["a issue"], "往轮 finding 不丢不双计");
        let cov = &report.run.coverage;
        assert_eq!(reviewer_names(&cov.reused), ["review[a.rs:rule:1-2]@p/r"]);
        assert_eq!(reviewer_names(&cov.completed).len(), 3);
        let deep = report.run.deep.as_ref().unwrap();
        assert_eq!((deep.enqueue_reasons.get("scan"), deep.dedup_hits, deep.budget_skipped), (Some(&1), 0, 0), "g1 疑点幂等跳过、g2 疑点新入队");
        assert!(store.read_queue_items("j").iter().all(|it| it.state == ItemState::Settled));

        let (again, code2) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;
        assert_eq!((scan.turn_count(), rev.turn_count()), (1, 3), "再 resume 无事可做");
        assert_eq!((again.findings.len(), code2), (1, 0), "{:?}", again.run.warnings);
    }

    /// RS1 ②：首跑 scanner g2 失败（F3）带 Skipped 收场——resume 补跑 g2 → 被截项复位、与新疑点一起经收场 drain 重选。
    /// K = 4：已挖 x（必挖）占 1 → 剩余 3、保底 1 留给新疑点 y；复位的三条涟漪按优先级补到 3 → k3 再次被截。
    #[tokio::test]
    async fn resume_replay_reopens_skipped_for_reselection() {
        use crate::orchestrate::queue::ItemState;
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        marker_repo(ws, &["x", "y", "k1", "k2", "k3"]);
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &[input("scan[g1]@p/m#1", "p/m", "P1"), input("scan[g2]@p/m#1", "p/m", "P2")]).unwrap();
        store.append_partial("j", &scan_rec("scan[g1]@p/m#1", "x.rs", 2, "x_marker();")).unwrap();
        store.append_partial("j", &CellRecord::failed("scan[g2]@p/m#1", "p/m", Role::Scanner, "boom")).unwrap();
        let mut x = snap_susp("x.rs", 2, "x_marker();", "scan[g1]@p/m#1", ItemState::Settled);
        // 往轮那条是「两家族对上同一处」的必挖项：C3〔D16〕起必挖看项内轴，夹具得在项内轴上表达
        // （只抬区域级 agreement 会让它算成非必挖，往轮就白占了保底位、本轮保底归零）。
        (x.agreement, x.dispatch_agreement, x.peak_agreement) = (2, 2, 2);
        x.member_families = vec![vec!["m".into(), "n".into()]];
        store.append_partial("j", &review_rec(&x.id, vec![])).unwrap();
        store.put_queue_item("j", &x).unwrap();
        for k in ["k1.rs", "k2.rs", "k3.rs"] {
            store.put_queue_item("j", &ripple(k, ItemState::Skipped)).unwrap();
        }
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        let (reg, scan, rev) = rs_registry(&susp_json("y.rs", 2, "y_marker();"), r#"{"findings":[],"needs":[]}"#);

        let (report, code) = run_resume("j", &reg, &rs_cfg(4), ws, &store).await;

        assert_eq!((scan.turn_count(), rev.turn_count()), (1, 3), "{:?}", report.run.warnings);
        let dug = reviewer_names(&report.run.coverage.completed);
        assert_eq!(dug, ["review[k1.rs:rule:1-2]@p/r", "review[k2.rs:rule:1-2]@p/r", "review[y.rs:scan:1-3]@p/r"], "疑点项区间 = 稳定块（C3〔D16〕）：y.rs 第 2 行落在 1-3 行的 def 块内");
        assert_eq!(report.run.deep.as_ref().map(|d| d.budget_skipped), Some(1));
        assert_eq!((report.verdict, code), (Verdict::Approve, 2), "预算截断计债: {:?}", report.run.warnings);
        let k3 = store.read_queue_items("j").into_iter().find(|it| it.spec.file == "k3.rs").unwrap();
        assert_eq!(k3.state, ItemState::Skipped);
    }

    /// RS1 ④a：并发多项在途时被 kill，其中两项的格已落盘（带 finding）、快照仍 InFlight——对账成 Settled 不重挖，
    /// 只重挖没格的一项；finding 各一条、不丢不双计。再 resume 一次结果不变。
    #[tokio::test]
    async fn resume_after_concurrent_inflight_kill_keeps_findings_once() {
        use crate::orchestrate::queue::ItemState;
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        marker_repo(ws, &["a", "b", "c"]);
        let store = crate::state::JobStore::open(ws).unwrap();
        scan_done_job(&store);
        for f in ["a.rs", "b.rs", "c.rs"] {
            store.put_queue_item("j", &rule_item(f, ItemState::InFlight)).unwrap();
        }
        store.append_partial("j", &review_rec("a.rs:rule:1-2", vec![marker_finding("a")])).unwrap();
        store.append_partial("j", &review_rec("b.rs:rule:1-2", vec![marker_finding("b")])).unwrap();
        let (reg, rev) = deep_registry(r#"{"findings":[],"needs":[]}"#);

        for round in 0..2 {
            let (report, code) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;
            assert_eq!(rev.turn_count(), 1, "round {round}: 只重挖没格的 c.rs");
            let mut titles: Vec<&str> = report.findings.iter().map(|f| f.title.as_str()).collect();
            titles.sort();
            assert_eq!(titles, ["a issue", "b issue"], "round {round}: {:?}", report.run.warnings);
            assert_eq!((report.verdict, code), (Verdict::Approve, 0), "round {round}: {:?}", report.run.warnings);
        }
    }

    /// RS1 ④b：深挖期被 kill、scanner 无需补跑——被截项不复位（沿用、照计债），首跑留下的待挖项一条不少；预算取首跑落的队列头
    /// （K = 5），**不**按 resume 时的配置重算（这里配置已改成 maxCells 4，评审 #5）。首跑 K = 5（F = 2、额度 3）：x 必挖 + p1、p2 扫描期派出，
    /// 收场剩余 2 → 疑点 k 占保底、p3 补位、p4 被截；kill 时 x、p1 已结案、p2 在途。resume：已派发 3 → 剩余 2 → 挖 p2、p3、k；按 K = 4 算会把 p3 截掉。
    #[tokio::test]
    async fn resume_during_deep_without_replay_keeps_skipped_and_pending() {
        use crate::orchestrate::queue::ItemState;
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        marker_repo(ws, &["x", "p1", "p2", "p3", "p4", "k"]);
        let store = crate::state::JobStore::open(ws).unwrap();
        scan_done_job(&store);
        store.put_queue_item("j", &rule_item("x.rs", ItemState::Settled)).unwrap();
        store.append_partial("j", &review_rec("x.rs:rule:1-2", vec![])).unwrap();
        store.put_queue_item("j", &ripple("p1.rs", ItemState::Settled)).unwrap();
        store.append_partial("j", &review_rec("p1.rs:rule:1-2", vec![])).unwrap();
        store.put_queue_item("j", &ripple("p2.rs", ItemState::InFlight)).unwrap();
        store.put_queue_item("j", &ripple("p3.rs", ItemState::Pending)).unwrap();
        store.put_queue_item("j", &snap_susp("k.rs", 2, "k_marker();", "scan[g9]@p/m#1", ItemState::Pending)).unwrap();
        store.put_queue_item("j", &ripple("p4.rs", ItemState::Skipped)).unwrap();
        deep_started(&store, 5);
        let (reg, rev) = deep_registry(r#"{"findings":[],"needs":[]}"#);

        let (report, code) = run_resume("j", &reg, &rs_cfg(4), ws, &store).await;

        assert_eq!(rev.turn_count(), 3, "{:?}", report.run.warnings);
        assert_eq!(
            reviewer_names(&report.run.coverage.completed),
            ["review[p2.rs:rule:1-2]@p/r", "review[p3.rs:rule:1-2]@p/r", "review[k.rs:scan:2-2]@p/r"]
        );
        assert_eq!(report.run.deep.as_ref().map(|d| d.budget_skipped), Some(1));
        assert_eq!(code, 2);
        let p4 = store.read_queue_items("j").into_iter().find(|it| it.spec.file == "p4.rs").unwrap();
        assert_eq!(p4.state, ItemState::Skipped, "不补跑 scanner 不复位");
    }

    /// RS1 ④c：旧快照（C1 起的作业，队列项无到达键）可 resume——回放的 scanner 疑点经谓词并入已有项（去重命中、不新增项、不重挖），
    /// 并补记到达键；再 resume 一次即幂等（不再计去重）。
    #[tokio::test]
    async fn resume_old_snapshot_without_arrival_keys() {
        use crate::orchestrate::queue::ItemState;
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        marker_repo(ws, &["x"]);
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &[input("scan[g1]@p/m#1", "p/m", "P1")]).unwrap();
        store.append_partial("j", &scan_rec("scan[g1]@p/m#1", "x.rs", 2, "x_marker();")).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        let mut old = snap_susp("x.rs", 2, "x_marker();", "scan[g1]@p/m#1", ItemState::Settled);
        old.arrivals.clear();
        store.append_partial("j", &review_rec(&old.id, vec![marker_finding("x")])).unwrap();
        store.put_queue_item("j", &old).unwrap();
        let (reg, rev) = deep_registry(r#"{"findings":[],"needs":[]}"#);

        let (report, code) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;
        assert_eq!(rev.turn_count(), 0, "{:?}", report.run.warnings);
        let deep = report.run.deep.as_ref().unwrap();
        assert_eq!((deep.dedup_hits, deep.enqueue_reasons.get("scan")), (1, None), "谓词并入、不新增项");
        assert_eq!((report.findings.len(), code), (1, 0));
        let items = store.read_queue_items("j");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].arrivals.len(), 1);
        assert!(items[0].arrivals[0].starts_with("scan[g1]@p/m#1#0:"), "补记到达键（带内容指纹）: {:?}", items[0].arrivals);

        let (again, _) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;
        assert_eq!(again.run.deep.as_ref().map(|d| d.dedup_hits), Some(0), "幂等");
    }

    /// RS1 + F3：补跑后 scanner 仍缺格——深挖照跑、findings 照进，判定 Incomplete / 3，本可 reject 的 blocking 条数照报。
    #[tokio::test]
    async fn resume_with_scanner_still_missing_digs_but_stays_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        marker_repo(ws, &["x", "y"]);
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &[input("scan[g1]@p/m#1", "p/m", "P1"), input("scan[g2]@p/m#1", "p/m", "P2")]).unwrap();
        store.append_partial("j", &scan_rec("scan[g1]@p/m#1", "x.rs", 2, "x_marker();")).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        deep_started(&store, 4);
        let blocking = r#"{"findings":[{"severity":"high","title":"x blocks","body":"b","file":"x.rs","line_start":2,"line_end":2,"confidence":0.95,"recommendation":"r","quote":"x_marker();"}],"needs":[]}"#;
        let turn = |text: &str| AssistantTurn {
            text: text.into(), tool_calls: vec![], input_tokens: 1, output_tokens: 1, cached_input_tokens: 0, finish_reason: Some("stop".into()), reasoning_tokens: None,
        };
        let rev = Arc::new(FakeChatBackend::repeating(turn(blocking)));
        let mut reg = ProviderRegistry::new();
        let down = FakeChatBackend::new_results(vec![Err(crate::provider::ProviderError::Unavailable("still down".into()))]);
        reg.insert_scan("p/m", "p", 8, Box::new(down), std::time::Duration::from_secs(5), 0);
        reg.insert_scan("p/r", "p", 8, Box::new(Arc::clone(&rev)), std::time::Duration::from_secs(5), 0);

        let (report, code) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

        let w = &report.run.warnings;
        assert_eq!(rev.turn_count(), 1, "g1 的疑点照挖: {w:?}");
        assert_eq!((report.verdict, code), (Verdict::Incomplete, 3), "{w:?}");
        assert_eq!(report.findings.len(), 1);
        assert!(w.iter().any(|x| x == "resume: 1 cell(s) still missing after replay"), "{w:?}");
        assert!(w.iter().any(|x| x == "scan incomplete: 1 blocking finding(s) present — a complete review would reject"), "{w:?}");
    }

    /// RS1 ③ 续：空快照时 H0 按当前 diff 重建（design §6）——扫描期被 kill 在深挖入队之前，resume 仍挖到 H0 区；重算的风险段进报告。
    #[tokio::test]
    async fn resume_with_empty_snapshot_rebuilds_h0() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let before = "pub fn check(a: u8) -> bool {\n    if a > 1 {\n        return true;\n    }\n    false\n}\n";
        let after = "pub fn check(a: u16) -> bool {\n    if a > 1 {\n        return true;\n    }\n    false\n}\n";
        std::fs::create_dir_all(ws.join("src")).unwrap();
        git(ws, &["init", "-q"]);
        std::fs::write(ws.join("src/auth.rs"), before).unwrap();
        git(ws, &["add", "."]);
        git(ws, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
        std::fs::write(ws.join("src/auth.rs"), after).unwrap();
        let store = crate::state::JobStore::open(ws).unwrap();
        scan_done_job(&store);
        deep_started(&store, 4);
        let (reg, rev) = deep_registry(r#"{"findings":[],"needs":[]}"#);

        let (report, code) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

        let dug = reviewer_names(&report.run.coverage.completed);
        assert!(!dug.is_empty() && dug.iter().all(|c| c.starts_with("review[src/auth.rs:rule:")), "{dug:?} {:?}", report.run.warnings);
        assert_eq!(rev.turn_count(), dug.len());
        assert!(report.run.risk.as_ref().is_some_and(|r| r.hotspots >= 1), "{:?}", report.run.risk);
        assert_eq!((report.verdict, code), (Verdict::Approve, 0), "{:?}", report.run.warnings);
    }

    /// diff 重建不了（改动已提交 / 不在 git 仓）时按「有没有没挖过的东西」计债（评审 #1 / #5）：
    /// 有成功 scanner 格的疑点从没到过队列 → 计债（approve / 2）；没有 → 干净收口，不编造债。
    #[tokio::test]
    async fn resume_without_diff_counts_debt_only_when_something_is_undug() {
        let undug = |with_suspicion: bool| async move {
            let dir = tempfile::tempdir().unwrap();
            let ws = dir.path();
            let store = crate::state::JobStore::open(ws).unwrap();
            store.write_manifest("j", &[input("scan[g1]@p/m#1", "p/m", "P1")]).unwrap();
            let rec = if with_suspicion {
                scan_rec("scan[g1]@p/m#1", "x.rs", 2, "x_marker();")
            } else {
                CellRecord::ok("scan[g1]@p/m#1", "p/m", crate::domain::CellOutput::Scan(Default::default()))
            };
            store.append_partial("j", &rec).unwrap();
            store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
            deep_started(&store, 4);
            let (reg, rev) = deep_registry(r#"{"findings":[],"needs":[]}"#);
            let (report, code) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;
            assert_eq!(rev.turn_count(), 0);
            assert!(report.run.warnings.iter().any(|w| w.starts_with("resume deep: cannot rebuild diff")), "{:?}", report.run.warnings);
            // 回写必须发生（评审 #7）：`--resume` 只打印不落库，早 return 会让库里停在旧报告
            assert!(store.read_result("j").is_some(), "diff 失败分支也要回写结果");
            (report.verdict, code)
        };
        assert_eq!(undug(true).await, (Verdict::Approve, 2), "补跑 / 首跑留下的疑点没挖 → 带债");
        assert_eq!(undug(false).await, (Verdict::ScanOnly, 0), "零疑点、空快照 → 深挖本就没东西可挖");
    }

    /// 首跑那次 H0 入队的快照写失败（只 eprintln）、随后被 kill：快照非空，但那个 H0 项一条记录都没有。
    /// resume 每轮按当前 diff 幂等重入队 H0（到达键保证不重复），它才补得回来（评审 #8）。
    #[tokio::test]
    async fn resume_rebuilds_h0_missing_from_the_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        std::fs::create_dir_all(ws.join("src")).unwrap();
        git(ws, &["init", "-q"]);
        std::fs::write(ws.join("src/auth.rs"), "pub fn check(a: u8) -> bool {\n    if a > 1 {\n        return true;\n    }\n    false\n}\n").unwrap();
        std::fs::write(ws.join("x.rs"), "fn x() {\n    x_marker();\n}\n").unwrap();
        git(ws, &["add", "."]);
        git(ws, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
        std::fs::write(ws.join("src/auth.rs"), "pub fn check(a: u16) -> bool {\n    if a > 1 {\n        return true;\n    }\n    false\n}\n").unwrap();
        std::fs::write(ws.join("x.rs"), "fn x() {\n    x_marker();\n}\n// changed\n").unwrap();
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &[input("scan[g1]@p/m#1", "p/m", "P1")]).unwrap();
        store.append_partial("j", &scan_rec("scan[g1]@p/m#1", "x.rs", 2, "x_marker();")).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        deep_started(&store, 4);
        // 快照只有疑点项（H0 那次入队的快照写失败）
        let x = snap_susp("x.rs", 2, "x_marker();", "scan[g1]@p/m#1", crate::orchestrate::queue::ItemState::Settled);
        store.append_partial("j", &review_rec(&x.id, vec![])).unwrap();
        store.put_queue_item("j", &x).unwrap();
        let (reg, rev) = deep_registry(r#"{"findings":[],"needs":[]}"#);

        let (report, code) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

        let dug = reviewer_names(&report.run.coverage.completed);
        assert!(dug.iter().any(|c| c.starts_with("review[src/auth.rs:rule:")), "H0 补回并挖: {dug:?} {:?}", report.run.warnings);
        assert_eq!((rev.turn_count(), code), (dug.len(), 0), "{:?}", report.run.warnings);
        assert_eq!(report.run.deep.as_ref().map(|d| d.dedup_hits), Some(0), "疑点项按到达键幂等、不计去重命中");
    }

    /// 格落盘失败后 resume 重放该格、模型输出变了：同下标的新疑点不能撞上旧到达键被当成「已到达」（评审 #8）——
    /// 到达键带内容指纹，新疑点照常入队开挖。
    #[tokio::test]
    async fn resume_replay_with_changed_output_enqueues_the_new_suspicion() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        marker_repo(ws, &["x", "y"]);
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &[input("scan[g1]@p/m#1", "p/m", "P1")]).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        deep_started(&store, 4);
        // 首跑：g1 的疑点 x 已挖完并记了到达键，但该格的 cell 落盘失败（partial 里没有它）→ resume 会重放
        let x = snap_susp("x.rs", 2, "x_marker();", "scan[g1]@p/m#1", crate::orchestrate::queue::ItemState::Settled);
        store.append_partial("j", &review_rec(&x.id, vec![])).unwrap();
        store.put_queue_item("j", &x).unwrap();
        let (reg, scan, rev) = rs_registry(&susp_json("y.rs", 2, "y_marker();"), r#"{"findings":[],"needs":[]}"#);

        let (report, code) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

        assert_eq!((scan.turn_count(), rev.turn_count()), (1, 1), "重放该格、挖它的新疑点: {:?}", report.run.warnings);
        assert_eq!(reviewer_names(&report.run.coverage.completed), ["review[y.rs:scan:1-3]@p/r"], "疑点项区间 = 稳定块（C3〔D16〕）");
        assert_eq!((report.verdict, code), (Verdict::Approve, 0), "{:?}", report.run.warnings);
    }

    /// resume 也算拆簇数（评审 #12）：不算就会用 0 覆盖首跑落库的读数。
    /// 三格各报同一 `tick();` 的第 1 / 4 / 7 行（A~B、B~C、A≁C）；快照里 A、B 一项、C 另一项 → 1 簇被拆开。
    #[tokio::test]
    async fn resume_recomputes_split_clusters() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let body = "tick();\na1();\na2();\ntick();\nb1();\nb2();\ntick();\n";
        repo_with_changes(ws, &[("tick.rs", body)]);
        let store = crate::state::JobStore::open(ws).unwrap();
        let cells = [("scan[g1]@p/alpha#1", 1u32), ("scan[g1]@p/beta#1", 7), ("scan[g1]@p/gamma#1", 4)];
        store.write_manifest("j", &cells.iter().map(|(c, _)| input(c, "p/m", "P")).collect::<Vec<_>>()).unwrap();
        for (cell, line) in cells {
            store.append_partial("j", &scan_rec(cell, "tick.rs", line, "tick();")).unwrap();
        }
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        deep_started(&store, 4);
        // 首跑的队列：A（第 1 行）与后到的 B（第 4 行）并成一项，C（第 7 行）单独一项
        let mut a = snap_susp("tick.rs", 1, "tick();", "scan[g1]@p/alpha#1", crate::orchestrate::queue::ItemState::Settled);
        let b = snap_susp("tick.rs", 4, "tick();", "scan[g1]@p/gamma#1", crate::orchestrate::queue::ItemState::Settled);
        a.arrivals.extend(b.arrivals.clone());
        let c = snap_susp("tick.rs", 7, "tick();", "scan[g1]@p/beta#1", crate::orchestrate::queue::ItemState::Settled);
        for it in [&a, &c] {
            store.append_partial("j", &review_rec(&it.id, vec![])).unwrap();
            store.put_queue_item("j", it).unwrap();
        }
        let (reg, rev) = deep_registry(r#"{"findings":[],"needs":[]}"#);

        let (report, _) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

        assert_eq!(rev.turn_count(), 0, "没有要挖的: {:?}", report.run.warnings);
        assert_eq!(report.run.deep.as_ref().map(|d| d.queue_split_clusters), Some(1));
    }

    /// 旧快照（C1 起、项无到达键）遇上 diff 重建失败：到达键无从复算 → 只按项状态判未完，不因「对不上到达键」编造债。
    #[tokio::test]
    async fn resume_legacy_snapshot_without_diff_is_not_debt() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        std::fs::write(ws.join("x.rs"), "fn x() {\n    x_marker();\n}\n").unwrap();
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &[input("scan[g1]@p/m#1", "p/m", "P1")]).unwrap();
        store.append_partial("j", &scan_rec("scan[g1]@p/m#1", "x.rs", 2, "x_marker();")).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        let mut old = snap_susp("x.rs", 2, "x_marker();", "scan[g1]@p/m#1", crate::orchestrate::queue::ItemState::Settled);
        old.arrivals.clear();
        store.append_partial("j", &review_rec(&old.id, vec![])).unwrap();
        store.put_queue_item("j", &old).unwrap();
        let (reg, rev) = deep_registry(r#"{"findings":[],"needs":[]}"#);

        let (report, code) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

        assert_eq!((rev.turn_count(), code), (0, 0), "{:?}", report.run.warnings);
        assert!(report.run.warnings.iter().any(|w| w.starts_with("resume deep: cannot rebuild diff")), "{:?}", report.run.warnings);
    }

    /// 首跑是 scan-only 作业（没落队列头、快照为空）：事后给配置加了 reviewer 再 resume 也不改判它（评审 #5）——
    /// 否则会按当前 diff 整轮深挖，并用新报告覆盖库内原结果。
    #[tokio::test]
    async fn resume_of_scan_only_job_stays_scan_only_after_reviewer_configured() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        marker_repo(ws, &["x"]);
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &[input("scan[g1]@p/m#1", "p/m", "P1")]).unwrap();
        store.append_partial("j", &scan_rec("scan[g1]@p/m#1", "x.rs", 2, "x_marker();")).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        let (reg, rev) = deep_registry(r#"{"findings":[],"needs":[]}"#);

        let (report, code) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

        assert_eq!((rev.turn_count(), report.verdict, code), (0, Verdict::ScanOnly, 0), "{:?}", report.run.warnings);
        assert!(report.run.deep.is_none(), "深挖没发生 → 不产观测段");
    }

    /// 首跑 F3（scanner 一格失败）后补跑成功、但 diff 重建不了：补跑出的疑点从没挖过 → 计债，不能洗成干净通过（评审 #1）。
    #[tokio::test]
    async fn resume_replayed_suspicion_without_diff_is_debt() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        // 非 git 目录（diff 重建失败）但文件在：补跑出的疑点能锚定
        for n in ["x", "y"] {
            std::fs::write(ws.join(format!("{n}.rs")), format!("fn {n}() {{\n    {n}_marker();\n}}\n")).unwrap();
        }
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &[input("scan[g1]@p/m#1", "p/m", "P1"), input("scan[g2]@p/m#1", "p/m", "P2")]).unwrap();
        store.append_partial("j", &scan_rec("scan[g1]@p/m#1", "x.rs", 2, "x_marker();")).unwrap();
        store.append_partial("j", &CellRecord::failed("scan[g2]@p/m#1", "p/m", Role::Scanner, "boom")).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        deep_started(&store, 4);
        // 首跑把 g1 的疑点挖完了（快照全 Settled、到达键在项上）
        let mut x = snap_susp("x.rs", 2, "x_marker();", "scan[g1]@p/m#1", crate::orchestrate::queue::ItemState::Settled);
        x.dispatch_agreement = 1;
        store.append_partial("j", &review_rec(&x.id, vec![])).unwrap();
        store.put_queue_item("j", &x).unwrap();
        let (reg, scan, rev) = rs_registry(&susp_json("y.rs", 2, "y_marker();"), r#"{"findings":[],"needs":[]}"#);

        let (report, code) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

        assert_eq!((scan.turn_count(), rev.turn_count()), (1, 0), "补跑 g2 成功、但没法重挖");
        assert_eq!((report.verdict, code), (Verdict::Approve, 2), "补跑出的 y 从没挖过 → 带债: {:?}", report.run.warnings);
    }

    /// 变更集为空（改动已提交）时 run_deep 早退：仍有没挖过的疑点 → 计债，不回到 scan-only / 0（评审 #2）。
    #[tokio::test]
    async fn resume_with_committed_changes_and_undug_suspicion_is_debt() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        marker_repo(ws, &["x"]);
        git(ws, &["add", "."]);
        git(ws, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "wip"]);
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &[input("scan[g1]@p/m#1", "p/m", "P1")]).unwrap();
        store.append_partial("j", &scan_rec("scan[g1]@p/m#1", "x.rs", 2, "x_marker();")).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args()).unwrap()).unwrap();
        deep_started(&store, 4);
        let (reg, rev) = deep_registry(r#"{"findings":[],"needs":[]}"#);

        let (report, code) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

        assert_eq!(rev.turn_count(), 0);
        assert_eq!((report.verdict, code), (Verdict::Approve, 2), "{:?}", report.run.warnings);
        assert!(report.run.warnings.iter().any(|w| w.contains("变更集为空")), "{:?}", report.run.warnings);
    }

    /// 重挖的项不被 resume 收场 drain 截掉（C2-3）：「Settled 却无成功格」是往轮派发过的项，对账成「往轮在途」、占预算位——
    /// K = 4、两条主区已挖完 + 三条涟漪无成功格时，对账成 Pending 会被剩余 2 格截掉一条（变 Skipped、既不重挖也不计债）。
    #[tokio::test]
    async fn resume_redig_is_not_cut_by_close_drain() {
        use crate::orchestrate::queue::{ItemKind, ItemState};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let names = ["m1", "m2", "r1", "r2", "r3"];
        let files: Vec<(String, String)> = names.iter().map(|n| (format!("{n}.rs"), format!("fn {n}() {{\n    {n}_marker();\n}}\n"))).collect();
        repo_with_changes(ws, &files.iter().map(|(p, b)| (p.as_str(), b.as_str())).collect::<Vec<_>>());
        let store = crate::state::JobStore::open(ws).unwrap();
        scan_done_job(&store);
        for n in ["m1", "m2"] {
            store.put_queue_item("j", &rule_item(&format!("{n}.rs"), ItemState::Settled)).unwrap();
            let ok = CellRecord::ok(format!("review[{n}.rs:rule:1-2]@p/r"), "p/r", crate::domain::CellOutput::Review { findings: vec![], needs: vec![] });
            store.append_partial("j", &ok).unwrap();
        }
        for n in ["r1", "r2", "r3"] {
            let mut it = rule_item(&format!("{n}.rs"), ItemState::Settled);
            it.kind = ItemKind::Ripple;
            store.put_queue_item("j", &it).unwrap();
        }
        let (reg, rev) = deep_registry(r#"{"findings":[],"needs":[]}"#);

        let (report, code) = run_resume("j", &reg, &deep_cfg(), ws, &store).await;

        assert_eq!(rev.turn_count(), 3, "三条涟漪全部重挖: {:?}", report.run.warnings);
        assert_eq!(report.run.deep.as_ref().map(|d| d.budget_skipped), Some(0));
        assert_eq!(code, 0, "{:?}", report.run.warnings);
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
    use std::time::Duration;

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


    /// 触发集与 verdict 同源，**含 CLI 覆盖**（2026-09-21 收口评审 #9）：`--conf-floor 0.3` 让一条
    /// 置信 0.4 的 finding 成为 blocking，它就必须同时进复核触发集。两处读不同的阈值时，
    /// 这条会被 verdict 判成拦路、却永远不被复核——`verify.rs` 里写着的「与 compute_verdict 同源」当场失效。
    #[tokio::test]
    async fn cli_threshold_overrides_reach_the_verify_trigger_set() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.rs", "fn f() {\n    db.write(token);\n}\n");
        let cfg = Config::from_json(
            r#"{"review":{"verifiers":{"enabled":true}},"providers":{"p":{"kind":"openai","models":{
                "s":{"roles":{"scanner":{}}},
                "r":{"roles":{"reviewer":{}}},
                "v":{"roles":{"verifier":{}}}
            }}}}"#,
        )
        .unwrap();
        let scan_json = format!(r#"{{"suspicions":[{}],"scanned_areas":[]}}"#, susp("write", "db.write(token);", 0.8));
        // 置信 0.4：配置缺省 floor 0.5 之下、`--conf-floor 0.3` 之上
        let rev_json = r#"{"findings":[{"severity":"medium","title":"t","body":"b","file":"src/a.rs","line_start":2,"line_end":2,"confidence":0.4,"recommendation":"r","quote":"db.write(token);"}],"needs":[]}"#;
        let t = |s: &str| AssistantTurn { text: s.into(), tool_calls: vec![], input_tokens: 1, output_tokens: 1, cached_input_tokens: 0, finish_reason: Some("stop".into()), reasoning_tokens: None };
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/s", "p", 8, Box::new(FakeChatBackend::repeating(t(&scan_json))), std::time::Duration::from_secs(5), 0);
        reg.insert_scan("p/r", "p", 8, Box::new(FakeChatBackend::repeating(t(rev_json))), std::time::Duration::from_secs(5), 0);
        reg.insert_scan("p/v", "p", 8, Box::new(FakeChatBackend::repeating(t(r#"{"analysis":"a","verdict":"UPHELD","reason":"成立"}"#))), std::time::Duration::from_secs(5), 0);
        let changes = vec![FileChange { path: "src/a.rs".into(), diff: "@@ -2,1 +2,1 @@\n+    db.write(token);".into() }];

        let mut a = args();
        a.conf_floor = Some(0.3);
        let (report, code) = run_code_review(&a, &reg, &cfg, changes, dir.path(), None, None).await.unwrap();

        let deep = report.run.deep.as_ref().expect("有 reviewer → 有 deep 段");
        assert_eq!(
            deep.verify_triggered, 1,
            "0.4 在 --conf-floor 0.3 之上 → 既是 blocking 也该进触发集；skipped={:?}",
            deep.verify_skipped
        );
        assert_eq!((report.verdict, code), (Verdict::Reject, 10), "同一条同时是 blocking");
    }

    /// 有 reviewer：疑点入队 → Fake 产 finding → Approve + deep 段。
    #[tokio::test]
    async fn deep_review_emits_findings_and_approve() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.rs", "fn f() {\n    db.write(token);\n}\n");
        let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"s":{"roles":{"scanner":{}}},"r":{"roles":{"reviewer":{}}}}}}}"#).unwrap();
        let scan_json = format!(r#"{{"suspicions":[{}],"scanned_areas":[]}}"#, susp("write", "db.write(token);", 0.8));
        let rev_json = r#"{"findings":[{"severity":"low","title":"t","body":"b","file":"src/a.rs","line_start":2,"line_end":2,"confidence":0.4,"recommendation":"r","quote":"db.write(token);"}],"needs":[]}"#;
        let mut reg = ProviderRegistry::new();
        let scan_turn = AssistantTurn { text: scan_json, tool_calls: vec![], input_tokens: 1, output_tokens: 1, cached_input_tokens: 0, finish_reason: None, reasoning_tokens: None };
        let rev_turn = AssistantTurn { text: rev_json.into(), tool_calls: vec![], input_tokens: 2, output_tokens: 3, cached_input_tokens: 0, finish_reason: Some("stop".into()), reasoning_tokens: None };
        reg.insert_scan("p/s", "p", 8, Box::new(FakeChatBackend::repeating(scan_turn)), std::time::Duration::from_secs(5), 0);
        reg.insert_scan("p/r", "p", 8, Box::new(FakeChatBackend::repeating(rev_turn)), std::time::Duration::from_secs(5), 0);
        let changes = vec![FileChange { path: "src/a.rs".into(), diff: "@@ -2,1 +2,1 @@\n+    db.write(token);".into() }];
        let (report, code) = run_code_review(&args(), &reg, &cfg, changes, dir.path(), None, None).await.unwrap();
        assert_eq!(code, 0, "warnings={:?} verdict={:?}", report.run.warnings, report.verdict);
        assert_eq!(report.verdict, Verdict::Approve);
        assert!(!report.findings.is_empty(), "findings={:?}", report.findings);
        assert!(report.run.deep.is_some());
        assert!(report.cells.iter().any(|c| c.cell.starts_with("review[")), "cells={:?}", report.cells.iter().map(|c| &c.cell).collect::<Vec<_>>());
        assert_eq!((report.run.models.clone(), report.run.model_ids.clone()), (vec!["p/s".to_string(), "p/r".to_string()], vec!["s".to_string(), "r".to_string()]), "实际参与的 reviewer 进 RunMeta.models（C1-3）");
    }

    #[tokio::test]
    async fn deep_reviewer_failure_is_approve_exit_2() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.rs", "fn f() {\n    db.write(token);\n}\n");
        let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"s":{"roles":{"scanner":{}}},"r":{"roles":{"reviewer":{}}}}}}}"#).unwrap();
        let scan_json = format!(r#"{{"suspicions":[{}],"scanned_areas":[]}}"#, susp("write", "db.write(token);", 0.8));
        let mut reg = ProviderRegistry::new();
        let scan_turn = AssistantTurn { text: scan_json, tool_calls: vec![], input_tokens: 1, output_tokens: 1, cached_input_tokens: 0, finish_reason: None, reasoning_tokens: None };
        let bad = AssistantTurn { text: "not-json".into(), tool_calls: vec![], input_tokens: 1, output_tokens: 1, cached_input_tokens: 0, finish_reason: Some("stop".into()), reasoning_tokens: None };
        reg.insert_scan("p/s", "p", 8, Box::new(FakeChatBackend::repeating(scan_turn)), std::time::Duration::from_secs(5), 0);
        reg.insert_scan("p/r", "p", 8, Box::new(FakeChatBackend::new(vec![bad.clone(), bad])), std::time::Duration::from_secs(5), 0);
        let changes = vec![FileChange { path: "src/a.rs".into(), diff: "@@ -2,1 +2,1 @@\n+    db.write(token);".into() }];
        let (report, code) = run_code_review(&args(), &reg, &cfg, changes, dir.path(), None, None).await.unwrap();
        assert_eq!(report.verdict, Verdict::Approve);
        assert_eq!(code, 2);
        assert!(report.run.coverage.failed.contains_key(&Role::Reviewer));
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

    /// 空变更集 + **配了 reviewer**：深挖一格没挖 → 必须仍是 ScanOnly / 0，不能被
    /// `compute_verdict([], …)` 改判成 Approve（r1 20260703 干净树假绿灯）。与无 reviewer 同构。
    #[tokio::test]
    async fn empty_changes_with_reviewer_stays_scan_only() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"s":{"roles":{"scanner":{}}},"r":{"roles":{"reviewer":{}}}}}}}"#).unwrap();
        let reg = registry(&[("p/s", r#"{"suspicions":[],"scanned_areas":[]}"#), ("p/r", r#"{"findings":[],"needs":[]}"#)]);
        let (report, code) = run_code_review(&args(), &reg, &cfg, vec![], dir.path(), None, None).await.unwrap();
        assert_eq!((report.verdict, code), (Verdict::ScanOnly, 0), "warnings={:?}", report.run.warnings);
        assert!(report.findings.is_empty());
        assert!(report.run.warnings.iter().any(|w| w.contains("no reviewable evidence")), "{:?}", report.run.warnings);
        assert!(report.run.deep.is_some(), "深挖观测段照进——可观测 ≠ 改判");
    }

    /// reviewer cell 必须经 `CellSink::on_cell` 落盘：后台 job 的 `attach_cells` 用
    /// `read_partial` 全量覆盖 cells，不落盘 = 报告里 coverage 指向一批不存在的格。
    #[tokio::test]
    async fn reviewer_cells_reach_the_sink() {
        use std::sync::Mutex as StdMutex;
        struct Rec(StdMutex<Vec<String>>);
        impl crate::orchestrate::CellSink for Rec {
            fn manifest(&self, _inputs: &[crate::state::CellInput]) {}
            fn on_cell(&self, rec: crate::state::CellRecord) {
                self.0.lock().unwrap().push(rec.cell);
            }
        }
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.rs", "fn f() {\n    db.write(token);\n}\n");
        let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"s":{"roles":{"scanner":{}}},"r":{"roles":{"reviewer":{}}}}}}}"#).unwrap();
        let scan_json = format!(r#"{{"suspicions":[{}],"scanned_areas":[]}}"#, susp("write", "db.write(token);", 0.8));
        let rev_json = r#"{"findings":[{"severity":"low","title":"t","body":"b","file":"src/a.rs","line_start":2,"line_end":2,"confidence":0.4,"recommendation":"r","quote":"db.write(token);"}],"needs":[]}"#;
        let mut reg = ProviderRegistry::new();
        let scan_turn = AssistantTurn { text: scan_json, tool_calls: vec![], input_tokens: 1, output_tokens: 1, cached_input_tokens: 0, finish_reason: None, reasoning_tokens: None };
        let rev_turn = AssistantTurn { text: rev_json.into(), tool_calls: vec![], input_tokens: 2, output_tokens: 3, cached_input_tokens: 0, finish_reason: Some("stop".into()), reasoning_tokens: None };
        reg.insert_scan("p/s", "p", 8, Box::new(FakeChatBackend::repeating(scan_turn)), std::time::Duration::from_secs(5), 0);
        reg.insert_scan("p/r", "p", 8, Box::new(FakeChatBackend::repeating(rev_turn)), std::time::Duration::from_secs(5), 0);
        let changes = vec![FileChange { path: "src/a.rs".into(), diff: "@@ -2,1 +2,1 @@\n+    db.write(token);".into() }];
        let sink = Rec(StdMutex::new(Vec::new()));
        let (report, _) = run_code_review(&args(), &reg, &cfg, changes, dir.path(), None, Some(&sink)).await.unwrap();
        let seen = sink.0.lock().unwrap();
        assert!(seen.iter().any(|c| c.starts_with("review[")), "reviewer cell 未落盘: {seen:?}");
        // 落盘的格与报告里的格同名——attach_cells 覆盖后 coverage 才不会指空。
        for c in report.run.coverage.selected.get(&Role::Reviewer).into_iter().flatten() {
            assert!(seen.contains(c), "coverage 里的 {c} 没进 sink: {seen:?}");
        }
    }

    /// `refute_rate` 的分母是**真发出去验了的**条数，不是全部 blocking（批 D-5）：
    /// 被免验跳过的从没给过 verifier 机会，算进分母只会把率冲淡；一条都没触发 → 不产这个键
    /// （拿 0 冒充「验了没驳回」会让「去留由数据决定」读到假数据）。
    #[test]
    fn refute_rate_counts_only_what_was_actually_verified() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::default();
        let reg = ProviderRegistry::new();
        let deep_with = |triggered: u32, refuted: u32, skipped: u32, failed: u32| crate::orchestrate::deep::DeepRun {
            findings: vec![],
            cells: vec![crate::state::CellRecord::ok("c", "m", crate::domain::CellOutput::Review { findings: vec![], needs: vec![] })],
            meta: crate::domain::DeepMeta {
                verify_triggered: triggered,
                verify_refuted: refuted,
                verify_failed: failed,
                verify_skipped: [("consensus".to_string(), skipped)].into_iter().collect(),
                ..Default::default()
            },
            warnings: vec![], reviewer_failed: false, reused_cells: vec![], items: vec![],
        };
        let blank = || ReviewReport { run: Default::default(), verdict: Verdict::Approve, stats: Default::default(),
            findings: vec![], scan: None, cells: vec![], clusters: Default::default(), schema_version: Default::default() };
        let (rep, _) = overlay_deep(blank(), 0, deep_with(4, 1, 9, 0), &args(), &cfg, &reg, std::time::Instant::now());
        assert_eq!(rep.run.refute_rate, Some(0.25), "4 条进了复核、驳回 1 条 → 25%（跳过的 9 条不进分母）");
        let (rep2, _) = overlay_deep(blank(), 0, deep_with(0, 0, 9, 0), &args(), &cfg, &reg, std::time::Instant::now());
        assert_eq!(rep2.run.refute_rate, None, "一条都没触发复核 → 不产键");
        // 收口评审 #6：没跑成的（provider 挂了 / 判决读不懂）也不进分母，否则「verifier 全挂」
        // 会读成「验了 N 条一条没驳」——而这个数正是下次复议「去留」时要读的。
        let (rep3, _) = overlay_deep(blank(), 0, deep_with(4, 1, 0, 2), &args(), &cfg, &reg, std::time::Instant::now());
        assert_eq!(rep3.run.refute_rate, Some(0.5), "4 条派出去、2 条没跑成 → 分母是 2");
        let (rep4, _) = overlay_deep(blank(), 0, deep_with(3, 0, 0, 3), &args(), &cfg, &reg, std::time::Instant::now());
        assert_eq!(rep4.run.refute_rate, None, "全挂 → 不产键，不拿 0.0 冒充「验了没驳回」");
        drop(dir);
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

    /// 深挖期超时 / 失败的 partial 报告：reviewer 格按自己的角色进 coverage，不冒充失败的 scanner 格
    /// （否则 warnings 里出现点名 reviewer 格的 `scan incomplete`，失败归因指错阶段）。
    #[test]
    fn partial_report_files_reviewer_cells_under_their_own_role() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::state::JobStore::open(dir.path()).unwrap();
        let scan = CellRecord::ok("scan[g1]@p/a#1", "p/a", crate::domain::CellOutput::Scan(Default::default()));
        let mut rev_ok = CellRecord::ok("review[a.rs:rule:1-2]@p/r", "p/r", crate::domain::CellOutput::Review { findings: vec![], needs: vec![] });
        rev_ok.input_tokens = 5;
        let rev_bad = CellRecord::failed("review[b.rs:rule:1-2]@p/r", "p/r", Role::Reviewer, "boom");
        for rec in [&scan, &rev_ok, &rev_bad] {
            store.append_partial("j", rec).unwrap();
        }
        let (r, code) = partial_report(&store, "j", "timed out: deadline exceeded".into());
        assert_eq!((r.verdict, code), (Verdict::Incomplete, 3));
        let cov = &r.run.coverage;
        assert!(!cov.failed.contains_key(&Role::Scanner), "{cov:?}");
        assert_eq!(cov.completed[&Role::Scanner], vec!["scan[g1]@p/a#1"]);
        assert_eq!(cov.completed[&Role::Reviewer], vec!["review[a.rs:rule:1-2]@p/r"]);
        assert_eq!(cov.failed[&Role::Reviewer], vec!["review[b.rs:rule:1-2]@p/r"]);
        assert!(!r.run.warnings.iter().any(|w| w.contains("scan incomplete")), "{:?}", r.run.warnings);
        assert!(r.run.warnings.iter().any(|w| w.starts_with("partial: 1/1")), "scan 进度只数 scanner 格: {:?}", r.run.warnings);
        assert_eq!(r.scan.as_ref().unwrap().stats.cells_failed, 0);
        assert_eq!(r.run.usage.input_tokens, 5, "reviewer 格的 usage 照样入账");
        assert_eq!(r.cells.len(), 3);
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

    // ── C2-4：scanner 与深挖时间重叠 ─────────────────────────────────────────

    fn turn_of(text: &str) -> AssistantTurn {
        AssistantTurn { text: text.into(), tool_calls: vec![], input_tokens: 1, output_tokens: 1, cached_input_tokens: 0, finish_reason: Some("stop".into()), reasoning_tokens: None }
    }

    /// 每格落盘与每次阶段转换的虚拟时刻（manifest 按 `JobCellSink` 口径记 `Scan`）。
    struct Timeline {
        t0: tokio::time::Instant,
        cells: std::sync::Mutex<Vec<(String, u64)>>,
        phases: std::sync::Mutex<Vec<(crate::state::Phase, u64)>>,
    }

    impl Timeline {
        fn new() -> Self {
            Timeline { t0: tokio::time::Instant::now(), cells: Default::default(), phases: Default::default() }
        }
        fn now(&self) -> u64 {
            self.t0.elapsed().as_millis() as u64
        }
        fn landed(&self, prefix: &str) -> Vec<u64> {
            self.cells.lock().unwrap().iter().filter(|(c, _)| c.starts_with(prefix)).map(|(_, t)| *t).collect()
        }
    }

    impl crate::orchestrate::CellSink for Timeline {
        fn manifest(&self, _inputs: &[crate::state::CellInput]) {
            self.phases.lock().unwrap().push((crate::state::Phase::Scan, self.now()));
        }
        fn on_cell(&self, rec: CellRecord) {
            let t = self.now();
            self.cells.lock().unwrap().push((rec.cell, t));
        }
        fn on_phase(&self, phase: crate::state::Phase, _note: Option<String>) {
            let t = self.now();
            self.phases.lock().unwrap().push((phase, t));
        }
    }

    /// 带未提交改动的 git 仓（阶段 0 风险路由要读 git 才出 H0 区）：各文件先提交 `before`、再改成 `after`，返回工作树 diff。
    fn git_changes(root: &Path, files: &[(&str, &str, &str)]) -> Vec<FileChange> {
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git").current_dir(root).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        for (path, before, _) in files {
            write(root, path, before);
        }
        git(&["add", "."]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
        for (path, _, after) in files {
            write(root, path, after);
        }
        crate::evidence::git::changed_files(root, None, false).unwrap()
    }

    fn scanner_and_reviewer_cfg() -> Config {
        Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"s":{"roles":{"scanner":{}}},"r":{"roles":{"reviewer":{}}}}}}}"#).unwrap()
    }

    /// 一个 H0 区（敏感路径 + 签名变更、含控制流 → 分数过门槛）+ 一条 scanner 疑点（新增的 `db.write(token);`）的改动。
    fn h0_and_suspicion_repo(root: &Path) -> Vec<FileChange> {
        let before = "pub fn check(a: u8) -> bool {\n    if a > 1 {\n        return true;\n    }\n    false\n}\n";
        let after = "pub fn check(a: u16) -> bool {\n    if a > 1 {\n        return true;\n    }\n    db.write(token);\n    false\n}\n";
        git_changes(root, &[("src/auth.rs", before, after)])
    }

    const AUTH_SUSPICION: &str =
        r#"{"suspicions":[{"file":"src/auth.rs","line_start":5,"line_end":5,"hint":"write","confidence":0.8,"quote":"db.write(token);"}],"scanned_areas":[]}"#;

    /// scanner 与深挖时间重叠（design C2 §2〔D15〕）：scanner 格 1000 ms、reviewer 格 10 ms → H0 区 t = 0 开挖、scanner 返回前就落地；
    /// 疑点在 scanner 落盘后到达、随即挖掉。阶段 Scan（manifest）→ Deep（扫描返回时）→ Finalizing（深挖排空后）；`close_ms` = scanner 段墙钟。
    #[tokio::test(start_paused = true)]
    async fn h0_is_dug_while_scanner_still_runs() {
        let dir = tempfile::tempdir().unwrap();
        let changes = h0_and_suspicion_repo(dir.path());
        let mut reg = ProviderRegistry::new();
        let slow_scan = FakeChatBackend::repeating(turn_of(AUTH_SUSPICION)).with_delay(Duration::from_millis(1000));
        let fast_review = FakeChatBackend::repeating(turn_of(r#"{"findings":[],"needs":[]}"#)).with_delay(Duration::from_millis(10));
        reg.insert_scan("p/s", "p", 8, Box::new(slow_scan), Duration::from_secs(5), 0);
        reg.insert_scan("p/r", "p", 8, Box::new(fast_review), Duration::from_secs(5), 0);
        let sink = Timeline::new();

        let (report, code) = run_code_review(&args(), &reg, &scanner_and_reviewer_cfg(), changes, dir.path(), None, Some(&sink)).await.unwrap();

        assert_eq!((report.verdict, code), (Verdict::Approve, 0), "{:?}", report.run.warnings);
        let deep = report.run.deep.as_ref().unwrap();
        assert!(deep.enqueue_reasons.get("rule").is_some_and(|&n| n >= 1), "构造前提：有 H0 区 {deep:?}");
        let h0 = sink.landed("review[src/auth.rs:rule:");
        assert!(!h0.is_empty() && h0.iter().all(|&t| t < 1000), "H0 格在 scanner 返回前落地: {:?}", sink.cells.lock().unwrap());
        assert_eq!(sink.landed("scan["), [1000]);
        assert_eq!(sink.landed("review[src/auth.rs:scan:"), [1010], "疑点到达即挖");
        assert_eq!((deep.first_dispatch_ms, deep.close_ms, deep.drain_done_ms), (1, 1000, 1010));
        // 最后一格的疑点与通道关闭同一次唤醒到达（排空后再填门）→ 疑点算在收场之后派出；它仍在收场 drain 的候选里，派发时刻不变
        assert_eq!(deep.scan_phase_dispatched as usize, h0.len(), "收场前派出的是 H0 格");
        let phases = sink.phases.lock().unwrap().clone();
        use crate::state::Phase;
        assert_eq!(phases, [(Phase::Scan, 0), (Phase::Deep, 1000), (Phase::Finalizing, 1010)]);
    }

    /// 共门 t = 0 先后（design C2 §7.3〔D15〕）：scanner 与 reviewer 同一 provider、门宽 1 → 显式先 poll 扫描，scanner 格先入门；
    /// H0 等 scanner 放门（100 ms）才开挖。
    #[tokio::test(start_paused = true)]
    async fn shared_gate_scanner_enters_before_h0_at_t0() {
        let dir = tempfile::tempdir().unwrap();
        let changes = h0_and_suspicion_repo(dir.path());
        let mut reg = ProviderRegistry::new();
        let scan = FakeChatBackend::repeating(turn_of(r#"{"suspicions":[],"scanned_areas":[]}"#)).with_delay(Duration::from_millis(100));
        let review = FakeChatBackend::repeating(turn_of(r#"{"findings":[],"needs":[]}"#)).with_delay(Duration::from_millis(10));
        reg.insert_scan("p/s", "p", 1, Box::new(scan), Duration::from_secs(5), 0);
        reg.insert_scan("p/r", "p", 1, Box::new(review), Duration::from_secs(5), 0);
        let sink = Timeline::new();

        let (report, _) = run_code_review(&args(), &reg, &scanner_and_reviewer_cfg(), changes, dir.path(), None, Some(&sink)).await.unwrap();

        assert_eq!(sink.landed("scan["), [100], "{:?}", report.run.warnings);
        let h0 = sink.landed("review[src/auth.rs:rule:");
        assert_eq!(h0.first(), Some(&110), "H0 在 scanner 放门后才入门: {:?}", sink.cells.lock().unwrap());
    }

    /// scanner 失败口径 F3（design C2 §5〔D15〕）：一个 scanner 条目不可达（格失败）时深挖照跑——reviewer 产出的 blocking finding 进报告，
    /// 但判定保持 Incomplete / 3（不是 reject 10），warning 报本可 reject 的条数；reviewer 也带债时债照披露、不提「approve with exit 2」。
    #[tokio::test]
    async fn scanner_failure_still_digs_but_stays_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.rs", "fn f() {\n    db.write(token);\n}\n");
        let cfg = Config::from_json(
            r#"{"providers":{"p":{"kind":"openai","models":{"s":{"roles":{"scanner":{}}},"ghost":{"roles":{"scanner":{}}},"r":{"roles":{"reviewer":{}}}}}}}"#,
        )
        .unwrap();
        let scan_json = format!(r#"{{"suspicions":[{}],"scanned_areas":[]}}"#, susp("write", "db.write(token);", 0.8));
        let blocking = r#"{"findings":[{"severity":"high","title":"token written unvalidated","body":"b","file":"src/a.rs","line_start":2,"line_end":2,"confidence":0.95,"recommendation":"r","quote":"db.write(token);"}],"needs":[]}"#;
        let changes = || vec![FileChange { path: "src/a.rs".into(), diff: "@@ -2,1 +2,1 @@\n+    db.write(token);".into() }];
        let reg_with = |review: FakeChatBackend| {
            let mut reg = ProviderRegistry::new();
            reg.insert_scan("p/s", "p", 8, Box::new(FakeChatBackend::repeating(turn_of(&scan_json))), Duration::from_secs(5), 0);
            reg.insert_scan("p/r", "p", 8, Box::new(review), Duration::from_secs(5), 0);
            reg
        };

        let reg = reg_with(FakeChatBackend::repeating(turn_of(blocking)));
        let (report, code) = run_code_review(&args(), &reg, &cfg, changes(), dir.path(), None, None).await.unwrap();
        let w = &report.run.warnings;
        assert_eq!((report.verdict, code), (Verdict::Incomplete, 3), "{w:?}");
        assert_eq!(report.findings.len(), 1, "深挖照跑、findings 进报告: {w:?}");
        assert_eq!(report.run.coverage.failed.get(&Role::Scanner), Some(&vec!["scan[g1]@p/ghost#1".to_string()]));
        assert_eq!(report.run.coverage.completed.get(&Role::Reviewer).map(Vec::len), Some(1));
        assert!(w.iter().any(|x| x == "scan incomplete: 1 blocking finding(s) present — a complete review would reject"), "{w:?}");
        assert!(w.iter().any(|x| x.contains("verdict hard Incomplete")), "scan 段失败披露照旧: {w:?}");

        let bad = turn_of("not-json");
        let reg = reg_with(FakeChatBackend::new(vec![bad.clone(), bad]));
        let (report, code) = run_code_review(&args(), &reg, &cfg, changes(), dir.path(), None, None).await.unwrap();
        let w = &report.run.warnings;
        assert_eq!((report.verdict, code), (Verdict::Incomplete, 3), "带债也不降成 exit 2: {w:?}");
        assert!(w.iter().any(|x| x == "reviewer debt: failed reviewer cell, undispatched queue items, or budget skip"), "{w:?}");
        assert!(!w.iter().any(|x| x.contains("approve with exit 2") || x.contains("blocking finding(s) present")), "{w:?}");
    }

    /// 合并残差对账（design C2 §4〔D15〕；C3〔D16〕结构化合并后重写夹具）：三家族各报同一 `tick();` 的一处——
    /// A 第 2 行、B 第 4 行（同在 `f1` 块内）、C 第 7 行（`f2` 块内），A~B、B~C、A≁C。
    /// 到达序 A、C、B（10 / 20 / 30 ms）→ 按块并项：A 与 B 同块并成一项、C 另一项 → 闭包簇落在 2 项上 → `queue_split_clusters` = 1；
    /// scan 报告仍是一簇、agreement 3。（C2 夹具的三条疑点在 C3 落进同一行带，会并成一项、拆簇恒 0——测不到对账。）
    ///
    /// 注意**成因已变**：C2 立这个指标量的是「逐条到达的非传递残差」（到达序不同结果不同）；C3 结构化合并之后
    /// 同块疑点无论到达序都并成一项，非传递残差在同块内构造上不可能，现在它量的是「一个闭包簇跨了两个块」。
    /// 本用例测的也是后者——两个 def 块各一项。
    #[tokio::test(start_paused = true)]
    async fn arrival_order_split_cluster_is_reconciled() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.rs", "fn f1() {\n    tick();\n    pad();\n    tick();\n}\nfn f2() {\n    tick();\n}\n");
        let cfg = Config::from_json(
            r#"{"providers":{"p":{"kind":"openai","models":{"alpha":{"roles":{"scanner":{}}},"beta":{"roles":{"scanner":{}}},"gamma":{"roles":{"scanner":{}}},"r":{"roles":{"reviewer":{}}}}}}}"#,
        )
        .unwrap();
        let at_line = |line: u32| format!(r#"{{"suspicions":[{{"file":"src/a.rs","line_start":{line},"line_end":{line},"hint":"h","confidence":0.5,"quote":"tick();"}}],"scanned_areas":[]}}"#);
        let mut reg = ProviderRegistry::new();
        for (key, line, ms) in [("p/alpha", 2, 10), ("p/beta", 7, 20), ("p/gamma", 4, 30)] {
            let b = FakeChatBackend::repeating(turn_of(&at_line(line))).with_delay(Duration::from_millis(ms));
            reg.insert_scan(key, "p", 8, Box::new(b), Duration::from_secs(5), 0);
        }
        reg.insert_scan("p/r", "p", 8, Box::new(FakeChatBackend::repeating(turn_of(r#"{"findings":[],"needs":[]}"#))), Duration::from_secs(5), 0);
        let changes = vec![FileChange { path: "src/a.rs".into(), diff: "@@ -1,8 +1,8 @@\n+    tick();\n+    tick();\n+    tick();".into() }];

        let (report, _) = run_code_review(&args(), &reg, &cfg, changes, dir.path(), None, None).await.unwrap();

        let scan = report.scan.as_ref().unwrap();
        assert_eq!(scan.suspicions.len(), 1, "scan 报告疑点并集不变: {:?}", scan.suspicions);
        assert_eq!(scan.suspicions[0].agreement, 3);
        let deep = report.run.deep.as_ref().unwrap();
        assert_eq!((deep.enqueue_reasons.get("scan"), deep.dedup_hits), (Some(&2), 1), "{deep:?}");
        assert_eq!(deep.queue_split_clusters, 1, "{:?}", report.run.warnings);
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
            dormant_leads: vec![crate::domain::DormantLead {
                file: "a.rs".into(), line: 42, hint: "历史截断项".into(), first_round: "sha0".into(), activations: 1,
            }],
        };
        let (r, code) = finish_scan_report(base(cells.clone(), vec!["scan[g1]@p/y#1".into()]));
        assert_eq!((r.verdict, code), (Verdict::ScanOnly, 0));
        let scan = r.scan.as_ref().unwrap();
        assert_eq!(scan.groups.len(), 2, "组骨架按 cell id 补齐");
        assert_eq!((scan.groups[0].cells.len(), scan.groups[0].suspicions, scan.groups[1].suspicions), (2, 1, 1));
        assert_eq!((scan.stats.total, scan.stats.agreement_ge2, scan.stats.needs_declared, scan.stats.cells_ok, scan.stats.cells_failed), (2, 1, 2, 3, 0));
        assert_eq!((scan.stats.by_category.get("L1"), scan.stats.by_category.get("L3")), (Some(&1), Some(&1)));
        assert_eq!(scan.scanned_coverage, Some(0.5));
        // 休眠线索进报告但不改判定（A-2）：verdict / code 与上面的断言一致，findings 仍为空
        assert_eq!(r.run.dormant_leads.len(), 1, "披露段要进报告");
        assert_eq!((r.run.dormant_leads[0].file.as_str(), r.run.dormant_leads[0].line), ("a.rs", 42));
        assert!(r.findings.is_empty(), "线索不是 finding");
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
        // 账目自相矛盾要留痕（M21c 批 A）：同一个 cell id 既有成功记录又有失败记录
        // → 它会同时落进 completed 与 failed，coverage 不再是划分，warnings 必须看得见（coverage 是 verdict 分级的依据）。
        let mut dup_cells = cells.clone();
        dup_cells.push(mk("scan[g1]@p/x#1", 0, "alpha", false, vec![], 0, 0, 0));
        let (dup, _) = finish_scan_report(base(dup_cells, vec![]));
        let w = dup.run.warnings.join(" | ");
        assert!(w.contains("划分不变式被破坏") && w.contains("scan[g1]@p/x#1"), "{w}");

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
    /// `--dismissed <dev 库>` 是**纯读**语义（收口评审 #2）：拿旧 schema 库做对拍基线时，
    /// 不得把它就地迁成 v4——那会清空评审史、让 dev 二进制再也写不了它，回退路径本身被毁。
    /// 分桶口径统一为**失败优先**（M21c 批 A）：`resume` 沿用的那批里若混进失败格，
    /// 必须进 `failed` 而不是 `reused`——原先 reviewer 侧是「复用优先」，会把失败**静默记成复用**，
    /// 而 coverage 正是 verdict 分级的依据（深挖失败 → 带债 approve）。同时不变式必须仍成立。
    #[test]
    fn folded_failed_reused_cell_goes_to_failed_not_reused() {
        use crate::domain::Role;
        let mut report = ReviewReport {
            run: Default::default(), verdict: Verdict::Approve, stats: Default::default(),
            findings: vec![], scan: None, cells: vec![], clusters: Default::default(), schema_version: Default::default(),
        };
        let ok_reused = CellRecord::ok("review[a.rs:rule:1-2]@m", "m", crate::domain::CellOutput::empty(Role::Reviewer));
        let bad_reused = CellRecord::failed("review[b.rs:rule:3-4]@m", "m", Role::Reviewer, "boom");
        fold_role_cells(&mut report, vec![ok_reused, bad_reused], true);

        let cov = &report.run.coverage;
        let names = |m: &std::collections::BTreeMap<Role, Vec<String>>| -> Vec<String> {
            m.get(&Role::Reviewer).cloned().unwrap_or_default()
        };
        assert_eq!(names(&cov.reused), ["review[a.rs:rule:1-2]@m"], "只有成功的沿用格算 reused");
        assert_eq!(names(&cov.failed), ["review[b.rs:rule:3-4]@m"], "失败优先：失败的沿用格进 failed");
        assert_eq!(names(&cov.selected).len(), 2, "selected 全列");
        assert!(cov.check_partition().is_ok(), "{:?}", cov.check_partition());
        assert!(!report.run.warnings.iter().any(|w| w.contains("划分不变式")), "不该报不变式破坏");
    }

    /// 不变式破坏要**留痕**而不是静默（M21c 批 A）：同一个格被并入两次（一次当沿用、一次当本轮），
    /// 它会同时落进 reused 与 completed —— coverage 是 verdict 分级的依据，这种账目错必须在 warnings 里看得见。
    /// 休眠线索只披露、不判定（M21c 批 A，A-2）：台账里落在本轮变更文件上的历史截断项进 `run.dormant_leads`，
    /// 但 **verdict / 退出码 / findings 一个都不动**——它们没被深挖过，当 finding 用会把「没挖」误读成「有问题」。
    #[test]
    fn dormant_leads_are_disclosure_only() {
        let tmp = tempfile::tempdir().unwrap();
        crate::project::init_project(tmp.path()).unwrap();
        let db = crate::db::ProjDb::open(tmp.path()).unwrap();
        let row = |id: &str, file: &str, status: &str| crate::db::SuspicionRow {
            id: id.into(), file: file.into(), quote_norm: "letx=1;".into(), hint: "可疑的边界".into(),
            status: status.into(), first_job: "sha0".into(), last_job: "sha1".into(), activations: 1,
            json: r#"{"line_start":42}"#.into(),
        };
        db.suspicion_upsert(&row("s1", "a.rs", "dormant")).unwrap();
        db.suspicion_upsert(&row("s2", "other.rs", "dormant")).unwrap();   // 不在本轮变更集
        db.suspicion_upsert(&row("s3", "a.rs", "promoted")).unwrap();      // 已晋升，不再是线索
        drop(db);

        let leads = read_dormant_leads(tmp.path(), &["a.rs".into()]);
        assert_eq!(leads.len(), 1, "只披露本轮变更文件上、仍处 dormant 的那条: {leads:?}");
        assert_eq!((leads[0].file.as_str(), leads[0].line, leads[0].activations), ("a.rs", 42, 1));
        assert_eq!(leads[0].first_round, "sha0", "首次见到它的那轮");

        // 零副作用：没 init 过的目录、空变更集都返回空且不建库
        let bare = tempfile::tempdir().unwrap();
        assert!(read_dormant_leads(bare.path(), &["a.rs".into()]).is_empty());
        assert!(!bare.path().join(".redteam").exists(), "披露不得建库");
        assert!(read_dormant_leads(tmp.path(), &[]).is_empty(), "空变更集不该退化成全表");
    }

    /// 落账的两条边界（M21c 批 A）：**不建库**（项目没 init 过就跳过，评审不该有建库副作用，
    /// 与 `load_ledger` 的 zero-config 同口径）；库在就写得进去，且没有可落的行时一个字节都不碰。
    #[test]
    fn persist_dormant_suspicions_never_creates_the_db() {
        use crate::orchestrate::queue::QueueItem;
        let skipped: QueueItem = serde_json::from_value(serde_json::json!({
            "id": "a.rs:scan:1-9", "score": 1.0, "agreement": 1, "state": "skipped", "kind": "suspicion",
            "dispatch_agreement": 1, "landed_agreement": null, "families": ["alpha"],
            "spec": { "file": "a.rs", "ranges": [[1, 9]], "origin": "scan",
                      "notes": [{"file":"a.rs","line_start":3,"line_end":3,"hint":"空指针","confidence":0.6,"quote":"let x = take(v);"}] },
            "members": [{"file":"a.rs","line_start":3,"line_end":3,"hint":"空指针","confidence":0.6,"quote":"let x = take(v);"}],
        })).unwrap();

        // 没 init 过：不建 .redteam/、不建库、不报 warning
        let tmp = tempfile::tempdir().unwrap();
        let mut warnings = vec![];
        persist_dormant_suspicions(tmp.path(), std::slice::from_ref(&skipped), "sha1", &mut warnings);
        assert!(!tmp.path().join(".redteam").exists(), "评审不得建库");
        assert!(warnings.is_empty(), "{warnings:?}");

        // init 过：落得进去
        let tmp2 = tempfile::tempdir().unwrap();
        crate::project::init_project(tmp2.path()).unwrap();
        persist_dormant_suspicions(tmp2.path(), std::slice::from_ref(&skipped), "sha1", &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        let db = crate::db::ProjDb::open(tmp2.path()).unwrap();
        let rows = db.suspicions_for_files(&["a.rs".into()]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].status.as_str(), rows[0].last_job.as_str()), ("dormant", "sha1"));
    }

    #[test]
    fn coverage_violation_is_recorded_as_a_warning() {
        use crate::domain::Role;
        let mut report = ReviewReport {
            run: Default::default(), verdict: Verdict::Approve, stats: Default::default(),
            findings: vec![], scan: None, cells: vec![], clusters: Default::default(), schema_version: Default::default(),
        };
        let rec = || CellRecord::ok("review[a.rs:rule:1-2]@m", "m", crate::domain::CellOutput::empty(Role::Reviewer));
        fold_role_cells(&mut report, vec![rec()], true);
        assert!(report.run.warnings.is_empty(), "第一次并入是自洽的");
        fold_role_cells(&mut report, vec![rec()], false);
        let w = report.run.warnings.join(" | ");
        assert!(w.contains("划分不变式被破坏") && w.contains("review[a.rs:rule:1-2]@m"), "{w}");
        assert!(w.contains("reused") && w.contains("completed"), "要说清是哪两个桶: {w}");
    }

    #[test]
    fn load_ledger_db_override_never_migrates_the_target() {
        let tmp = tempfile::tempdir().unwrap();
        let dbp = tmp.path().join("legacy.db");
        // dev 形状：台账四表与 v4 同构（所以 open_at 会「带账迁移」它）+ 评审史有数据
        let c = rusqlite::Connection::open(&dbp).unwrap();
        c.execute_batch(
            "CREATE TABLE dismissed (id TEXT PRIMARY KEY, status TEXT NOT NULL, title TEXT NOT NULL DEFAULT '',
               file TEXT NOT NULL DEFAULT '', quote TEXT NOT NULL DEFAULT '', severity TEXT NOT NULL DEFAULT '',
               reason TEXT NOT NULL, round TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL DEFAULT '');
             CREATE TABLE results (job_id TEXT PRIMARY KEY, json TEXT NOT NULL);
             INSERT INTO dismissed (id,status,reason) VALUES ('a1b2c3d4e5f60718','dismissed','误报');
             INSERT INTO results VALUES ('j1','{}');",
        ).unwrap();
        drop(c);
        let before = std::fs::read(&dbp).unwrap();

        let l = load_ledger(Some(dbp.to_str().unwrap()), false, tmp.path()).unwrap();
        assert_eq!(l.ids.len(), 1, "旧库的台账照样读得出来");

        assert_eq!(std::fs::read(&dbp).unwrap(), before, "对拍基线一个字节都不该变");
        let c = rusqlite::Connection::open(&dbp).unwrap();
        assert_eq!(c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), 0, "不该被写成 v4");
        assert_eq!(c.query_row("SELECT count(*) FROM results", [], |r| r.get::<_, i64>(0)).unwrap(), 1, "评审史不该被清");
        let snaps: Vec<_> = std::fs::read_dir(tmp.path()).unwrap()
            .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.contains("legacy-")).collect();
        assert!(snaps.is_empty(), "只读路径不该留快照: {snaps:?}");
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
