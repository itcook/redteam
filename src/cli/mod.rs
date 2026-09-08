//! CLI 评审流水线：`ReviewArgs`/台账/pack 预算/model 选取等留守项 + 全量/resume/job
//! 状态机（并发组 M16 定型，原地不动）。尾段引擎/spec/二阶段补查/增量评审已拆至
//! 同目录子模块（M17-a 尾段统一，零语义 diff）。

use crate::bundle::{group, select_files, GroupingOpts};
use crate::config::Config;
use crate::domain::{DossierMeta, Finding, ReviewReport, RunMeta, Severity, Stats, Usage, Verdict};
use crate::evidence::git::FileChange;
use crate::orchestrate::{enrich, run_code_fanout, CellSink, OrchestrateOpts, PlannedModel};
use crate::output::Format;
use crate::provider::{LensRequest, ReviewProvider};
use std::path::Path;

mod deny;
mod incremental;
mod second_pass;
mod spec;
mod tail;

pub use incremental::run_incremental_review;
pub use tail::exit_code;

use second_pass::second_pass_prompt;
use spec::{render_spec_section_capped, SPEC_BUDGET_BYTES};
use tail::{assemble_report, model_ids_for, TailCtx};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ReviewArgs {
    pub base: Option<String>,
    pub scope: String,
    pub spec: Vec<String>,
    pub focus: Option<String>,
    pub models: Option<String>,
    pub models_all: bool,
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
    /// M21a 批 C：`--pack-only`——只收集证据、分组、装包并落盘，不调 provider（D7 B7 预算仪表）。
    /// `serde(default)` 保旧 args.json（resume / 后台 job）兼容。
    #[serde(default)]
    pub pack_only: bool,
}

/// 台账加载产物（design 2026-07-03 #6 zero-config）：豁免 id 集 + 活跃判例卡 + 披露 meta。
#[derive(Default, Debug)]
pub(crate) struct Ledger {
    pub ids: std::collections::HashSet<String>,
    pub cards: Vec<crate::db::DismissCard>,
    pub meta: Option<crate::domain::LedgerMeta>,
}

/// 加载台账：--no-dismissed → 空；缺省 → `.redteam/proj.db` 存在才加载（零副作用，不建库）；
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
                    "--dismissed: directory ledgers are retired; migrate cards into .redteam/proj.db \
                     via 'redteam dismiss add' (see 'redteam dismiss --help')"
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

fn parse_sev(s: &str) -> Severity {
    match s {
        "low" => Severity::Low,
        "high" => Severity::High,
        "critical" => Severity::Critical,
        _ => Severity::Medium,
    }
}

pub fn parse_format(s: &str) -> Format {
    if s == "md" {
        Format::Md
    } else {
        Format::Json
    }
}

/// 统一 model 选取（S3-a）。返回 (planned_models, replicate)。
/// 显式列表（`--models-all` / `--models` / 非空 `default_models` 任一）→ replicate=true。
/// 三者全空 → 所有已配置 provider 按 priority 降序候补，replicate=false。
/// `--models-all` 优先于 `--models`。
pub fn resolve_models(
    models_flag: Option<&str>,
    models_all: bool,
    cfg: &Config,
) -> (Vec<PlannedModel>, bool) {
    let all_by_priority = || -> Vec<String> {
        // 09-04 分离后：名字空间 = [models.*]，候补链按 model 级 priority。
        let mut v: Vec<_> = cfg.models.iter().collect();
        v.sort_by_key(|m| std::cmp::Reverse(m.1.priority));
        v.into_iter().map(|(k, _)| k.clone()).collect()
    };
    let (names, replicate): (Vec<String>, bool) = if models_all {
        (all_by_priority(), true)
    } else if let Some(s) = models_flag {
        (
            s.split(',').map(|x| x.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            true,
        )
    } else if !cfg.review.default_models.is_empty() {
        (cfg.review.default_models.clone(), true)
    } else {
        (all_by_priority(), false)
    };
    let models = names
        .iter()
        .map(|n| PlannedModel {
            name: n.clone(),
            priority: cfg.models.get(n).map(|m| m.priority).unwrap_or(0),
        })
        .collect();
    (models, replicate)
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
pub(crate) fn pack_budget_bytes(cfg: &Config, model_names: &[String]) -> usize {
    let min_ctx = model_names
        .iter()
        .map(|m| cfg.models.get(m).and_then(|x| x.context_window).unwrap_or(128_000))
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
pub(crate) fn build_packs(bundles: &[crate::bundle::Bundle], root: &std::path::Path, budget: usize) -> (Vec<String>, DossierMeta) {
    // 单格全审默认（E9）：bundles 恒为单元素（全变更集），每格即自家文件——原「兄弟改动
    // 文件全景注入 + 依赖距离排序」随分格一并收缩（无兄弟可注入）。超预算由 build_pack
    // 自身的降级梯子（骨架/hunk 窗/省略清单）承接，不再靠分格避让。
    let entries: Vec<String> = bundles
        .iter()
        .map(|b| {
            // 预算记账扣本格自己的 diff 字节（单格默认下即全变更集真实 diff）。
            let own_diff_bytes: usize = b.files.iter().map(|fc| fc.diff.len()).sum();
            crate::prompt::render_dossier_section(&crate::pack::build_pack(b, root, budget, own_diff_bytes))
        })
        .collect();
    let injected = entries.iter().filter(|s| !s.is_empty()).count();
    let meta = DossierMeta { bundles: bundles.len(), injected };
    (entries, meta)
}

/// code 专属流水线（S2-2b）：bundle × model 工具循环 + positioning 尾段。
/// 退出码：0=approve、10=reject、2=对照集不完整（failed_models 非空）。
/// `--background` 由 main.rs 在 dispatch 之前分叉（spawn worker）；到达这里即路由 bug。
pub async fn run_code_review(
    args: &ReviewArgs,
    provider: &dyn ReviewProvider,
    cfg: &Config,
    changes: Vec<FileChange>,
    root: &Path,
    head_sha: Option<String>,
    sink: Option<&dyn CellSink>,
) -> anyhow::Result<(ReviewReport, i32)> {
    if args.background {
        anyhow::bail!("--background forks in main.rs before dispatch; reaching here is a routing bug");
    }
    // Fix 3（final-review）：台账加载上提到 fan-out 之前 fail-fast——坏值不再烧完 LLM
    // 调用才报错。zero-config：缺省自动加载 .redteam/proj.db（存在才启用）。
    let ledger = load_ledger(args.dismissed.as_deref(), args.no_dismissed, root)?;
    let t0 = std::time::Instant::now();

    // 1. 确定性筛选；分组（design §3.2）推迟到 pack 预算算出之后——组预算门要喂它（K1 / K5）。
    let selected = select_files(changes);

    // 3. 模型集 + replicate 判定（统一 resolve_models）
    let (models, replicate) = resolve_models(args.models.as_deref(), args.models_all, cfg);
    let model_names: Vec<String> = models.iter().map(|m| m.name.clone()).collect();
    let oopts = OrchestrateOpts {
        models,
        replicate,
        no_fallback: false,
        max_concurrency: cfg.review.max_concurrency,
    };

    // 4. fan-out（bundle × model）
    let focus = args.focus.clone().unwrap_or_default();
    let target_label = if args.scope == "branch" {
        format!("branch {}...HEAD", args.base.as_deref().unwrap_or("?"))
    } else {
        "working tree".to_string()
    };
    // job 上下文时由 run_job 传 JobCellSink；直调路径（无 job）传 None，行为不变。
    // 台账不再注入 review prompt（design 2026-07-03 #2，判定后置 consolidate）。
    if let Some(s) = sink { s.on_meta(&model_names); }
    // --spec 参照段（D2）：读 spec_docs → 从 pack 预算里扣掉它占用的字节，让 build_packs +
    // 第二趟补查都用缩后预算——总上下文守界，spec 与 pack 不叠加超模型墙。空 --spec →
    // (String::new(), 0) → budget 原样不变（既有回归行为零漂移）。
    let budget_raw = pack_budget_bytes(cfg, &model_names);
    // spec 帽夹紧到 pack 预算一半（dogfood #2：64K 固定帽会吃穿小窗模型的整个输入窗）。
    let (spec_section, spec_used, spec_warnings) =
        render_spec_section_capped(&args.spec, SPEC_BUDGET_BYTES.min(budget_raw / 2));
    let budget = budget_raw.saturating_sub(spec_used);
    // 2. 引用图分组（M21a 批 C）：双阈值门未过 → 单包（即原 E9 单格全审）；过门 → 连通分量 /
    // 弱缝先切 / 宁降不拆 / 孤立装箱。GroupingStats 暂只在 --pack-only 报（RunMeta 接线 F-5）。
    let bundles = group(selected, Some(root), &GroupingOpts { budget_bytes: Some(budget), ..GroupingOpts::default() }).bundles;
    let (dossiers, dossier_meta) = build_packs(&bundles, root, budget);
    // 跨组索引（design §10.6）：每组一份，与 dossiers 等长等序；全量路径无 delta 标注。
    let others = crate::bundle::render_cross_indices(&crate::bundle::cross_index(&bundles, &[], None), bundles.len());
    // 二阶段补查构造器（design §2.3）：cell 声明 needs → 置换式重建 prompt 补跑一趟。
    let mk = |idx: usize, needs: &[crate::domain::NeedEntry], drafts: &[Finding]| {
        let other = others.get(idx).map(String::as_str).unwrap_or("");
        second_pass_prompt(&bundles, root, budget, &target_label, &focus, idx, needs, drafts, "", &spec_section, other)
    };
    let mut raw = run_code_fanout(provider, &target_label, &bundles, &focus, "", &dossiers, &oopts, sink, Some(&mk), &spec_section, &others).await;
    // T46①：--spec denylist/PEM 拒读 warnings 并入 run.warnings（CI/bot 可见，不仅 eprintln）。
    raw.warnings.extend(spec_warnings);
    // 空证据必须可观测（r1 20260703 事故：干净树 0 cell 秒出无警 approve 假绿灯）。
    if bundles.is_empty() {
        raw.warnings.push(format!(
            "no reviewable evidence: empty diff for {target_label} (0 cells) — the verdict reflects zero reviewed content"
        ));
    }

    // 5-8. 共用尾段：anchor → dedup → verify → verdict → 组报告
    let block = args
        .block_threshold
        .as_deref()
        .map(parse_sev)
        .unwrap_or(cfg.review.block_threshold);
    let floor = args.conf_floor.unwrap_or(cfg.review.conf_floor);
    let ctx = TailCtx {
        root,
        provider,
        model_ids: model_ids_for(&model_names, cfg),
        model_names,
        block,
        floor,
        max_concurrency: cfg.review.max_concurrency,
        job_id: String::new(),
        duration_ms: t0.elapsed().as_millis() as u64,
        usage: raw.usage,
        warnings: raw.warnings,
        failed_models: raw.failed_models,
        dismissed: &ledger.ids,
        precedents: &ledger.cards,
        ledger_meta: ledger.meta.clone(),
        head_sha,
        dossier_meta: Some(dossier_meta),
        needs_stats: raw.needs_stats,
        sink,
        spec_docs: &spec_section,
        carry: None,
    };
    if let Some(s) = sink { s.on_phase(crate::state::Phase::Finalizing, None); }
    assemble_report(raw.findings, ctx).await
}

/// 评审分发（前台 main 与后台 worker 共用）：增量轮路由，否则 git changed_files → run_code_review。
pub async fn run_review_dispatch(
    args: &ReviewArgs,
    provider: &dyn ReviewProvider,
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
        return run_incremental_review(&sync_args, &prior, provider, cfg, cwd, sink).await;
    }

    let changes = crate::evidence::git::changed_files(cwd, sync_args.base.as_deref(), branch_scope)?;
    // R3：stamp 权威时刻 = 证据收集实跑刻（与 changed_files 同刻同 cwd）；
    // 非 git 目录折 None——前台裸 review 与后台 job 共用本单点，前后台天然同 sha。
    let head_sha = crate::evidence::git::head_sha_at(cwd).ok();
    run_code_review(&sync_args, provider, cfg, changes, cwd, head_sha, sink).await
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

/// `--pack-only` 单 cell 记账：`bytes` = 完整 user prompt 字节（与真实评审同一 `build_bundle_prompt`
/// 路径，可与 usage 对账）；`pack_bytes` = dossier 段；`over_budget` = 分组「宁降不拆」标记。
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
    // 预算 = 全部已配置模型的最小窗（K15；批 A 合回后改读角色条目）。
    let model_names: Vec<String> = cfg.models.keys().cloned().collect();
    let budget_raw = pack_budget_bytes(cfg, &model_names);
    let (spec_section, spec_used, mut warnings) =
        render_spec_section_capped(&args.spec, SPEC_BUDGET_BYTES.min(budget_raw / 2));
    let budget = budget_raw.saturating_sub(spec_used);
    let grouping = group(
        select_files(changes),
        Some(cwd),
        &GroupingOpts { budget_bytes: Some(budget), ..GroupingOpts::default() },
    );
    let bundles = grouping.bundles;
    let (dossiers, _meta) = build_packs(&bundles, cwd, budget);
    let others =
        crate::bundle::render_cross_indices(&crate::bundle::cross_index(&bundles, &[], None), bundles.len());
    let focus = args.focus.clone().unwrap_or_default();

    let job_id = gen_job_id(cwd);
    let project_dir = cwd.join(crate::project::PROJECT_DIR);
    let packs_dir = project_dir.join("packs").join(&job_id);
    std::fs::create_dir_all(&packs_dir)?;
    // 未 init 的项目：只补 `.redteam/.gitignore`（镜像 init_project 的 `*`），不建库、不建 rounds。
    let gitignore = project_dir.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, "*\n")?;
    }
    let mut cells = Vec::new();
    for (i, b) in bundles.iter().enumerate() {
        let dossier = dossiers.get(i).map(String::as_str).unwrap_or("");
        let other = others.get(i).map(String::as_str).unwrap_or("");
        let prompt = crate::prompt::build_bundle_prompt(&target, b, &focus, other, "", dossier, &spec_section);
        std::fs::write(packs_dir.join(format!("cell-{}.md", i + 1)), &prompt)?;
        cells.push(PackCell {
            cell: i + 1,
            label: b.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>().join(","),
            files: b.files.len(),
            bytes: prompt.len(),
            est_tokens: (prompt.len() as f64 / PACK_BYTES_PER_TOKEN).round() as u64,
            pack_bytes: dossier.len(),
            diff_bytes: b.files.iter().map(|f| f.diff.len()).sum(),
            over_budget: b.over_budget,
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
        grouping: grouping.stats,
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
        out.push_str(&format!(
            "cell-{} files={} bytes={} est_tokens={} pack_bytes={} diff_bytes={} over_budget={}\n",
            c.cell, c.files, c.bytes, c.est_tokens, c.pack_bytes, c.diff_bytes, c.over_budget
        ));
    }
    if r.cells.is_empty() {
        out.push_str("cells=0\n");
    }
    let g = &r.grouping;
    out.push_str(&format!(
        "grouping gate={} groups={} edges={} weak_cut={} cross_edges={} isolated={} over_budget={}\n",
        g.gate.as_str(), g.groups, g.edges, g.weak_cut, g.cross_edges, g.isolated, g.over_budget
    ));
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
            st.set_phase(crate::state::Phase::Fanout, None);
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

/// 从 proj.db cells 表把 per-cell 运行摘要回填进报告（诊断呈现：慢在哪/几轮/是否重试）。
/// 三条终态路径统一调用；旧作业 JSON 无此字段（serde default 空）不受影响。
pub fn attach_cells(store: &crate::state::JobStore, job_id: &str, report: &mut ReviewReport) {
    report.run.cells = store.read_partial(job_id)
        .into_iter()
        .map(|r| crate::domain::CellSummary {
            model: r.model,
            ok: r.ok,
            iterations: r.iterations,
            attempts: r.attempts,
            duration_ms: r.duration_ms,
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
        })
        .collect();
}

/// R2.2：失败/超时时从 proj.db 的 cell checkpoint 里已完成的 findings 组一份 `incomplete`
/// 报告——绝不因为 dispatch 出错/超时就静默丢掉已经跑完的 cell。M 取 manifest 行数（若尚未
/// 写出 manifest 就已失败，退化用已完成数兜底，不出现 "N/0"）。
fn partial_report(store: &crate::state::JobStore, job_id: &str, reason: String) -> (ReviewReport, i32) {
    let manifest = store.read_manifest(job_id);
    let done_records: Vec<_> = store.read_partial(job_id)
        .into_iter()
        .filter(|r| r.ok)
        .collect();
    let done = done_records.len();
    let total = manifest.len().max(done);
    let findings: Vec<_> = done_records.into_iter().flat_map(|r| r.findings).collect();
    let stats = Stats::from_findings(&findings);
    let report = ReviewReport {
        schema_version: "3".into(),
        verdict: Verdict::Incomplete,
        stats,
        findings,
        clusters: vec![],
        run: RunMeta {
            job_id: job_id.to_string(),
            models: vec![],
            model_ids: vec![],
            duration_ms: 0,
            usage: Usage::default(),
            warnings: vec![format!("partial: {done}/{total} cells"), reason],
            failed_models: vec![],
            dismissed_reappeared: vec![],
            ledger: None,
            ..Default::default()
        },
    };
    let code = exit_code(Verdict::Incomplete, false);
    (report, code)
}

/// R2.3：resume 差集补跑。读 `manifest`（全集 M）+ `partial`（已完成 ok:true 的 cell 集）→
/// `待跑 = M − 已完成`；对待跑 cell **直接用 manifest 里渲染好的 prompt 重放**
/// `provider.run(LensRequest{route_key,prompt})`——不重收集证据、不重 build prompt（确定性）；
/// append_partial 落盘后，`read_partial` 取全部 ok:true 的 findings（已完成 + 新补）走 mode
/// 对应的共用尾段（`assemble_report`）组出完整报告。全集补齐→verdict 正常；仍有缺失→`Incomplete`。
///
/// ponytail：候补链 cell（未指定 `--models`）的 `CellInput.model` 只是 manifest 写入时「最高
/// priority 候选」的近似快照（见设计 §6.5 ⚠️）——resume 按此近似重放。若该模型正是触发 resume
/// 的卡死模型，这次重放会再失败，该 cell 仍缺，可再次 `--resume`（渐进收敛，不会更糟）。精确解
/// 是重放完整候补链或把 manifest.model 升级为 `Vec<String>` 快照，本 task 不做（显式/replicate
/// cell 的 manifest.model 已是精确值，dogfood 常见场景不受影响）。
pub async fn run_resume(
    job_id: &str,
    provider: &dyn ReviewProvider,
    cfg: &Config,
    cwd: &Path,
    store: &crate::state::JobStore,
) -> (ReviewReport, i32) {
    let t0 = std::time::Instant::now();
    let manifest = store.read_manifest(job_id);
    // 空 manifest = job 在 fan-out 写 manifest 前就崩溃：没有任何 work-list，尾段会把 0 findings
    // 组装成干净 Approve（假阳性——评审工具静默给未经审查的通过）。护栏：直接出 Incomplete。
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
    // M13：增量 job 的差集重放只补 fan-out cell，不重跑 carry/merge 尾段——直接拒绝，
    // 引导重跑增量评审（守卫与 carry 必须整链执行）。
    if args.incremental_from.is_some() {
        return partial_report(
            store, job_id,
            "resume: incremental jobs cannot be resumed; re-run `redteam review --incremental-from <prior-job>`".into(),
        );
    }
    let done: std::collections::BTreeSet<String> = store.read_partial(job_id)
        .into_iter()
        .filter(|r| r.ok)
        .map(|r| r.cell)
        .collect();
    let pending: Vec<&crate::state::CellInput> =
        manifest.iter().filter(|c| !done.contains(&c.cell)).collect();

    let mut usage = (0u64, 0u64, 0u64);
    let mut warnings = Vec::new();
    // T54：差集 cell 并行补跑（join_all 保序）——旧串行 for 循环 wall-clock = Σcell，
    // 大 bundle 思考型模型单 cell 10-20min 时双 cell 串行轻易破 40min（2026-09-03 PR181
    // dogfood：resume 三跑 49min 零落盘）。append_partial 内部 Mutex 单连接、逐 cell
    // 覆盖写，天然无并发写冲突；usage/warnings 在 join 后按原序累计，输出与串行版逐字等价。
    // (cell, outcome, elapsed)：elapsed 在 future 内起表——Cells 表的 duration 语义与
    // fanout 侧一致（真实墙钟），不再恒 0（2026-09-03 PR181 补跑实证缺口）。
    let outs: Vec<(&&crate::state::CellInput, Result<crate::provider::LensCall, crate::provider::ProviderError>, std::time::Duration)> =
        futures::future::join_all(pending.iter().map(|cell| async move {
            let t0 = std::time::Instant::now();
            let req = LensRequest { route_key: cell.model.clone(), prompt: cell.prompt.clone() };
            (cell, provider.run(req).await, t0.elapsed())
        }))
        .await;
    for (cell, outcome, elapsed) in outs {
        let rec = match outcome {
            Ok(call) => {
                usage.0 += call.input_tokens;
                usage.1 += call.output_tokens; usage.2 += call.cached_input_tokens;
                let fs: Vec<Finding> =
                    call.findings.iter().map(|r| enrich(r, &cell.model)).collect();
                // 指标随落盘：duration/usage/iterations 来自胜出 call（与 fanout 侧 CellSink 同构）。
                crate::state::CellRecord {
                    cell: cell.cell.clone(), model: cell.model.clone(), ok: true,
                    findings: fs, warn: None,
                    duration_ms: elapsed.as_millis() as u64,
                    input_tokens: call.input_tokens, output_tokens: call.output_tokens,
                    cached_input_tokens: call.cached_input_tokens,
                    attempts: 1, iterations: call.iterations,
                    tool_use: call.tool_use.clone(),
                }
            }
            Err(e) => {
                warnings.push(format!("resume cell [{}] model {}: {e}", cell.cell, cell.model));
                crate::state::CellRecord {
                    cell: cell.cell.clone(), model: cell.model.clone(), ok: false,
                    findings: vec![], warn: Some(e.to_string()),
                    duration_ms: elapsed.as_millis() as u64,
                    input_tokens: 0, output_tokens: 0, cached_input_tokens: 0, attempts: 1, iterations: 0,
                    tool_use: Default::default(),
                }
            }
        };
        // resume 披露清零：checkpoint 写失败不再静默吞掉——foreground 命令，stderr 用户可见
        // （与 line 394 CellSink::report 的写法保持一致，是全库最后一处生产 `let _ =` 静默点）。
        if let Err(e) = store.append_partial(job_id, &rec) {
            eprintln!("warn: job {job_id} append_partial 失败（cell {}）: {e}", rec.cell);
        }
    }

    // 合并出报告：全部 ok:true 的 findings（已完成 + 新补）。
    let final_records = store.read_partial(job_id);
    let ok_cells: std::collections::BTreeSet<&str> =
        final_records.iter().filter(|r| r.ok).map(|r| r.cell.as_str()).collect();
    let still_missing = manifest.iter().filter(|c| !ok_cells.contains(c.cell.as_str())).count();
    let findings: Vec<Finding> =
        final_records.into_iter().filter(|r| r.ok).flat_map(|r| r.findings).collect();

    let (models, _replicate) = resolve_models(args.models.as_deref(), args.models_all, cfg);
    let model_names: Vec<String> = models.iter().map(|m| m.name.clone()).collect();
    let block = args.block_threshold.as_deref().map(parse_sev).unwrap_or(cfg.review.block_threshold);
    let floor = args.conf_floor.unwrap_or(cfg.review.conf_floor);
    // resume 因重读 args.json 自然继承原 --dismissed/--no-dismissed；run_resume 不返回 Result
    //（异步状态机式），故此处不能用 `?`——加载失败按既有约定折成 partial_report。
    let ledger = match load_ledger(args.dismissed.as_deref(), args.no_dismissed, cwd) {
        Ok(l) => l,
        Err(e) => {
            return partial_report(store, job_id, format!("resume: bad ledger: {e}"));
        }
    };

    // Task 4：resume 的 refute 趟同原运行拿 --spec 参照段——从 args.spec 重渲（resume 已按同法
    // 重导 models/block/floor/ledger），与首趟缓存 prompt 里烘干的 spec 保持一致；空 --spec→""。
    // 帽用与首趟同一夹紧公式（cap 不同会导致重渲截断点漂移、与首趟 spec 段不一致）。
    let (spec_section, _spec_used, spec_warnings) = render_spec_section_capped(
        &args.spec,
        SPEC_BUDGET_BYTES.min(pack_budget_bytes(cfg, &model_names) / 2),
    );
    // T46①：resume 重渲 --spec 段时若命中 denylist/PEM 拒读，同样并入 warnings（与首趟一致）。
    warnings.extend(spec_warnings);
    let ctx = TailCtx {
        root: cwd,
        provider,
        model_ids: model_ids_for(&model_names, cfg),
        model_names,
        block,
        floor,
        max_concurrency: cfg.review.max_concurrency,
        job_id: job_id.to_string(),
        duration_ms: t0.elapsed().as_millis() as u64,
        usage,
        warnings,
        failed_models: vec![],
        dismissed: &ledger.ids,
        precedents: &ledger.cards,
        ledger_meta: ledger.meta.clone(),
        head_sha: None,
        dossier_meta: None,
        needs_stats: (0, 0),
        sink: None,
        spec_docs: &spec_section,
        carry: None,
    };
    match assemble_report(findings, ctx).await {
        Ok((mut report, mut code)) => {
            attach_cells(store, job_id, &mut report);
            // 回写 proj.db：resume 产出的合并报告（含新补 cell 的 findings/agreement）必须
            // 覆盖 fanout 原始落盘——否则 result 命令永远返回旧报告、补齐的 findings 取不回
            // （2026-09-03 PR181 实证：stdout 8 findings vs result 6 findings）。终态字段
            // （status/exit_code）不动：作业仍 Done，变的只是报告内容。
            let json = crate::output::render(&report, crate::output::Format::Json);
            let md = crate::output::render(&report, crate::output::Format::Md);
            if let Err(e) = store.write_result(job_id, &json) {
                eprintln!("warn: job {job_id} resume write_result 失败: {e}");
            }
            if let Err(e) = store.write_result_md(job_id, &md) {
                eprintln!("warn: job {job_id} resume write_result_md 失败: {e}");
            }
            if still_missing > 0 {
                report.verdict = Verdict::Incomplete;
                report.run.warnings.push(format!("resume: {still_missing} cell(s) still missing after replay"));
                code = exit_code(Verdict::Incomplete, false);
            }
            (report, code)
        }
        Err(e) => partial_report(store, job_id, format!("resume tail failed: {e}")),
    }
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
    provider: &dyn ReviewProvider,
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
    let dispatch = run_review_dispatch(args, provider, cfg, cwd, Some(&sink));

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
    use crate::state::{JobState, JobStatus};
    use crate::provider::fake::FakeProvider;
    use crate::domain::{Finding, Origin, RawFinding, Severity};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn checkpointed_finding(title: &str) -> Finding {
        Finding {
            id: "f".into(), severity: Severity::Medium, title: title.into(), body: "b".into(),
            file: "a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            models: vec!["alpha".into()], agreement: 1, hard_constraint: false,
            quote: "q".into(), cluster_id: None, verify: None, previously_dismissed: false,
            origin: Origin::New,
        }
    }

    fn code_args() -> ReviewArgs {
        ReviewArgs { pack_only: false,
            base: None, scope: "working-tree".into(),
            spec: vec![], focus: None, models: None, models_all: false,
            block_threshold: None, conf_floor: None,
            background: true, format: "json".into(),
            resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None,
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
        Config::from_layers(&[
            "[providers.alpha_p]\nkind=\"openai\"\n\n[models.alpha]\nprovider=\"alpha_p\"\nid=\"a\"\npriority=10\n\
             [review]\ndefault_models=[\"alpha\"]\n",
        ]).unwrap()
    }

    #[tokio::test]
    async fn run_job_done_writes_result_and_state() {
        let dir = tempfile::tempdir().unwrap();
        git_staged(dir.path(), "a.rs", "fn main() {\n    let x = 1;\n}\n");
        // alpha 报一条锚得到的 low finding（不触发 refute），quote 在 a.rs 第 2 行
        let mut scripted: BTreeMap<String, Vec<RawFinding>> = BTreeMap::new();
        scripted.insert("alpha".into(), vec![RawFinding {
            severity: Severity::Low, title: "t".into(), body: "b".into(), file: "a.rs".into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: "let x = 1;".into(),
        }]);
        let provider = FakeProvider { scripted, ..Default::default() };
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let store = Arc::new(crate::state::JobStore::open(&ws).unwrap());
        store.put_state_for_test(&JobState::new("j1", 500)).unwrap();

        let code = run_job("j1", &code_args(), &provider, &cfg(), dir.path(), 500, Arc::clone(&store)).await;

        let st = store.read_state("j1").unwrap();
        assert_eq!(st.status, JobStatus::Done);
        assert_eq!(st.exit_code, Some(code));
        assert!(st.result_path.is_some());
        // result.json 落盘且是合法 JSON
        let res = store.read_result("j1").unwrap();
        assert!(res.contains("\"verdict\""));
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
        let provider = FakeProvider { scripted: BTreeMap::new(), ..Default::default() };
        let code = run_job("j2", &code_args(), &provider, &cfg(), dir.path(), 0, Arc::clone(&store)).await;
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
        store.append_partial(
            "j2b",
            &crate::state::CellRecord::ok("bundle[a]", "alpha", vec![checkpointed_finding("checkpointed")]),
        ).unwrap();

        // 非 git 目录 → changed_files 失败 → run_review_dispatch Err，但已有 partial 记录
        let provider = FakeProvider { scripted: BTreeMap::new(), ..Default::default() };
        let code = run_job("j2b", &code_args(), &provider, &cfg(), dir.path(), 0, Arc::clone(&store)).await;

        assert_eq!(code, 3); // 五档：partial 报告 verdict=incomplete → 3
        let st = store.read_state("j2b").unwrap();
        assert_eq!(st.status, JobStatus::Failed);
        assert!(st.error.is_some());
        let res = store.read_result("j2b").unwrap();
        assert!(res.contains("\"incomplete\""), "verdict 应为 incomplete: {res}");
        assert!(res.contains("partial:"), "warnings 应含 partial 标记: {res}");
        assert!(res.contains("checkpointed"), "已 checkpoint 的 finding 不应丢失: {res}");
    }

    /// 永不完成的 provider——保证 dispatch 分支不可能赢得 deadline race（而非靠真实短
    /// sleep+timeout 的时序假设）。用于 R1.2 deadline 超时测试，0 依赖、0 wall-clock 等待。
    struct HangProvider;

    #[async_trait::async_trait]
    impl crate::provider::ReviewProvider for HangProvider {
        async fn run(&self, _req: crate::provider::LensRequest) -> Result<crate::provider::LensCall, crate::provider::ProviderError> {
            std::future::pending::<()>().await;
            unreachable!("HangProvider never resolves")
        }
    }

    /// R1.2：`deadline_at` 已过期 + dispatch 永不完成 → run_job 走超时分支，写 TimedOut +
    /// 部分报告（不静默丢 deadline 前已 checkpoint 的 findings）。
    #[tokio::test]
    async fn run_job_timeout_writes_incomplete_partial_report() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        // git 仓库 + staged 改动：dispatch 得以推进到 provider（HangProvider 挂起），
        // 让 deadline 分支赢得 select（非 git 目录会先在 changed_files 处 Err，选不到超时分支）。
        git_staged(dir.path(), "a.rs", "fn main() {\n    let x = 1;\n}\n");

        let mut st = JobState::new("j-to", 0);
        st.deadline_at = Some(0); // 已过期（远小于 now）→ select 立即走超时分支
        let store = Arc::new(crate::state::JobStore::open(ws).unwrap());
        store.put_state_for_test(&st).unwrap();
        // 预置一条 partial（模拟 deadline 前已完成 1 cell）
        store.append_partial(
            "j-to",
            &crate::state::CellRecord::ok("bundle[a]", "alpha", vec![checkpointed_finding("before-deadline")]),
        ).unwrap();

        let provider = HangProvider;
        let code = run_job("j-to", &code_args(), &provider, &cfg(), dir.path(), 0, Arc::clone(&store)).await;

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

        let provider = FakeProvider { scripted: BTreeMap::new(), ..Default::default() };
        let _code = run_job("j3", &code_args(), &provider, &cfg(), dir.path(), 0, Arc::clone(&store)).await;

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

        let provider = FakeProvider { scripted: BTreeMap::new(), ..Default::default() };
        let code = run_job("j-precancel-7", &code_args(), &provider, &cfg(), dir.path(), 0, Arc::clone(&store)).await;

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

        let provider = FakeProvider { scripted: BTreeMap::new(), ..Default::default() };
        let code = run_job("j-predone", &code_args(), &provider, &cfg(), dir.path(), 0, Arc::clone(&store)).await;

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
                        vec![],
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
    use crate::domain::{Finding, Origin, RawFinding, Severity};
    use crate::state::CellInput;

    /// 记录每次 `run` 调用的 prompt，脚本化返回一条低 severity finding（避免触发 verify 的
    /// 二次 provider 调用，测试只关心 resume 对缺失 cell 的调用次数）。finding 的 file 从
    /// prompt 派生（每 cell 独立文件），quote 恒 "marker"——各 cell 落在不同文件 → 通过 code
    /// `anchor` 存活且互不 dedup（doc 模式退场后尾段走 code anchor，2026-07-14）。
    #[derive(Default)]
    struct RecordingProvider {
        prompts: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl crate::provider::ReviewProvider for RecordingProvider {
        async fn run(
            &self,
            req: crate::provider::LensRequest,
        ) -> Result<crate::provider::LensCall, crate::provider::ProviderError> {
            self.prompts.lock().unwrap().push(req.prompt.clone());
            Ok(crate::provider::LensCall {
                findings: vec![RawFinding {
                    severity: Severity::Low,
                    title: format!("issue-{}", req.prompt),
                    body: "b".into(),
                    file: format!("{}.rs", req.prompt.trim()),
                    line_start: 1,
                    line_end: 1,
                    confidence: 0.9,
                    recommendation: "r".into(),
                    hard_constraint: false,
                    quote: "marker".into(),
                }],
                input_tokens: 1,
                output_tokens: 1,
                cached_input_tokens: 0,
                iterations: 1,
                tool_use: Default::default(),
                needs: vec![],
            })
        }
    }

    /// 预置 checkpoint 用的可锚 finding：quote="marker"，file 由调用方给（须已写入根目录）。
    fn anchorable_finding(title: &str, file: &str) -> Finding {
        Finding {
            id: title.into(), severity: Severity::Low, title: title.into(), body: "b".into(),
            file: file.into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), models: vec!["m".into()], agreement: 1,
            hard_constraint: false, quote: "marker".into(), cluster_id: None, verify: None,
            previously_dismissed: false, origin: Origin::New,
        }
    }

    /// T7 核心测试：manifest 3 cell、partial 已完成 1（c1）→ resume 只应对缺失的 c2/c3 调用
    /// provider，最终报告合并出全部 3 个 cell 的 findings。
    #[tokio::test]
    async fn resume_only_runs_missing_cells() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        // 每 cell 一个可锚文件（code anchor 读 root.join(file) 匹配 "marker"）。
        for f in ["c1.rs", "P2.rs", "P3.rs"] {
            std::fs::write(dir.path().join(f), "marker\n").unwrap();
        }

        let inputs = vec![
            CellInput { cell: "c1".into(), model: "m".into(), prompt: "P1".into() },
            CellInput { cell: "c2".into(), model: "m".into(), prompt: "P2".into() },
            CellInput { cell: "c3".into(), model: "m".into(), prompt: "P3".into() },
        ];
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &inputs).unwrap();
        store.append_partial(
            "j",
            &crate::state::CellRecord::ok("c1", "m", vec![anchorable_finding("issue-c1", "c1.rs")]),
        ).unwrap();

        let args = ReviewArgs { pack_only: false,
            base: None, scope: "working-tree".into(), spec: vec![],
            focus: None, models: Some("m".into()), models_all: false,
            block_threshold: None, conf_floor: None, background: true,
            format: "json".into(), resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None,
        };
        store.write_args_raw("j", &serde_json::to_string(&args).unwrap()).unwrap();

        let provider = RecordingProvider::default();
        let cfg = Config::default();

        let (report, _code) = run_resume("j", &provider, &cfg, dir.path(), &store).await;

        let calls = provider.prompts.lock().unwrap();
        assert_eq!(calls.len(), 2, "只应对缺失的 2 个 cell 调用 provider");
        assert!(calls.contains(&"P2".to_string()), "应补跑 c2");
        assert!(calls.contains(&"P3".to_string()), "应补跑 c3");
        assert!(!calls.contains(&"P1".to_string()), "已完成的 c1 不应重跑");
        drop(calls);

        assert_eq!(report.findings.len(), 3, "合并后应含 3 个 cell 的 findings（1 已完成 + 2 新补）");

        // 回写锚：resume 合并报告必须覆盖 proj.db 落盘——否则 result 命令永远返回 fanout
        // 原始报告，补齐的 findings 取不回（2026-09-03 PR181 实证）。md 同步。
        let stored = store.read_result("j").expect("resume 后 result 必须可取且含新报告");
        let parsed: ReviewReport = serde_json::from_str(&stored).unwrap();
        assert_eq!(parsed.findings.len(), 3, "proj.db 里的报告应为合并后的 3 findings");
        assert!(store.read_result_md("j").unwrap().contains("issue-c1"), "md 快照同步回写");
    }

    /// final-review Important 回归：replicate 模式（同 cell 多 model 副本）manifest 的
    /// cell-id 必须带上 model（如 `doc@alpha`/`doc@beta`），否则 resume 的差集判定（join on
    /// cell）会把「同 cell 任一 model 已完成」误判为「该 cell 全部完成」，缺失的 model 副本
    /// 永不补跑，还会静默拼出一份只含部分 model 的「完整」报告。
    /// 这里手工构造 manifest 模拟 replicate fan-out 的输出（2 行，cell 各自带 @model），
    /// partial 只完成 doc@alpha → 断言 doc@beta 必须被补跑，且合并结果含两个 model 的 finding。
    #[tokio::test]
    async fn resume_replicate_missing_model_copy_is_completed() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        for f in ["alpha.rs", "P-beta.rs"] {
            std::fs::write(dir.path().join(f), "marker\n").unwrap();
        }

        let inputs = vec![
            CellInput { cell: "doc@alpha".into(), model: "alpha".into(), prompt: "P-alpha".into() },
            CellInput { cell: "doc@beta".into(), model: "beta".into(), prompt: "P-beta".into() },
        ];
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_manifest("j", &inputs).unwrap();
        store.append_partial(
            "j",
            &crate::state::CellRecord::ok("doc@alpha", "alpha", vec![anchorable_finding("issue-alpha", "alpha.rs")]),
        ).unwrap();

        let args = ReviewArgs { pack_only: false,
            base: None, scope: "working-tree".into(), spec: vec![],
            focus: None, models: Some("alpha,beta".into()), models_all: false,
            block_threshold: None, conf_floor: None, background: true,
            format: "json".into(), resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None,
        };
        store.write_args_raw("j", &serde_json::to_string(&args).unwrap()).unwrap();

        let provider = RecordingProvider::default();
        let cfg = Config::default();

        let (report, _code) = run_resume("j", &provider, &cfg, dir.path(), &store).await;

        let calls = provider.prompts.lock().unwrap();
        assert_eq!(calls.len(), 1, "只应对缺失的 doc@beta 调用 provider（doc@alpha 已完成不得重跑）");
        assert!(calls.contains(&"P-beta".to_string()), "beta 副本必须被补跑，不能被 alpha 的完成状态静默坍缩");
        drop(calls);

        assert_eq!(
            report.findings.len(), 2,
            "合并后应含 alpha+beta 两个 model 副本的 findings，不能只含 alpha 就报完整"
        );
        assert_ne!(
            report.verdict, crate::domain::Verdict::Incomplete,
            "两个 model 副本都已补齐，不应再是 Incomplete"
        );
    }

    /// 假阳性防护：manifest 从未写出（job 在 fan-out 前崩溃）→ resume 不得静默出 Approve，
    /// 必须 Incomplete + 非零退出，且不调 provider。
    #[tokio::test]
    async fn resume_empty_manifest_is_incomplete_not_approve() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        // 只写 args，从不 write_manifest（模拟 fan-out 前崩溃）。
        let args = ReviewArgs { pack_only: false,
            base: None, scope: "working-tree".into(), spec: vec![],
            focus: None, models: Some("m".into()), models_all: false,
            block_threshold: None, conf_floor: None, background: true,
            format: "json".into(), resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None,
        };
        let store = crate::state::JobStore::open(ws).unwrap();
        store.write_args_raw("j", &serde_json::to_string(&args).unwrap()).unwrap();

        let provider = RecordingProvider::default();
        let (report, code) = run_resume("j", &provider, &Config::default(), dir.path(), &store).await;

        assert_eq!(report.verdict, crate::domain::Verdict::Incomplete, "空 manifest 不得静默 Approve");
        assert_ne!(code, 0, "退出码非 0");
        assert_eq!(provider.prompts.lock().unwrap().len(), 0, "无 cell 可跑，不应调 provider");
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    fn cfg(providers: &str, defaults: &str) -> Config {
        let toml = format!("{providers}\n[review]\ndefault_models=[{defaults}]\n");
        Config::from_layers(&[&toml]).unwrap()
    }
    fn provs() -> &'static str {
        "[providers.alpha_p]\nkind=\"openai\"\n\n[models.alpha]\nprovider=\"alpha_p\"\nid=\"a\"\npriority=10\n\
         [providers.beta_p]\nkind=\"openai\"\n\n[models.beta]\nprovider=\"beta_p\"\nid=\"b\"\npriority=5\n"
    }

    #[test]
    fn explicit_models_flag_replicates() {
        let c = cfg(provs(), "");
        let (m, rep) = resolve_models(Some("alpha,beta"), false, &c);
        assert!(rep);
        assert_eq!(m.iter().map(|p| p.name.clone()).collect::<Vec<_>>(), vec!["alpha", "beta"]);
    }

    #[test]
    fn models_all_replicates_all_providers_by_priority() {
        let c = cfg(provs(), "");
        let (m, rep) = resolve_models(None, true, &c);
        assert!(rep);
        // 按 priority 降序：alpha(10) 在 beta(5) 前
        assert_eq!(m.iter().map(|p| p.name.clone()).collect::<Vec<_>>(), vec!["alpha", "beta"]);
    }

    #[test]
    fn nonempty_default_models_replicates() {
        let c = cfg(provs(), "\"beta\",\"alpha\"");
        let (m, rep) = resolve_models(None, false, &c);
        assert!(rep);
        assert_eq!(m.iter().map(|p| p.name.clone()).collect::<Vec<_>>(), vec!["beta", "alpha"]);
    }

    #[test]
    fn empty_everything_falls_back_to_candidate_chain() {
        let c = cfg(provs(), "");
        let (m, rep) = resolve_models(None, false, &c);
        assert!(!rep, "no explicit list → candidate chain (fallback)");
        assert_eq!(m.iter().map(|p| p.name.clone()).collect::<Vec<_>>(), vec!["alpha", "beta"]);
    }

    #[test]
    fn models_all_overrides_models_flag() {
        let c = cfg(provs(), "");
        let (m, rep) = resolve_models(Some("alpha"), true, &c);
        assert!(rep);
        assert_eq!(m.len(), 2, "--models-all takes precedence over --models");
    }
}

#[cfg(test)]
mod code_review_tests {
    use super::*;
    use crate::domain::{RawFinding, Verdict};
    use crate::evidence::git::FileChange;
    use crate::provider::fake::FakeProvider;
    use std::io::Write;

    fn write(dir: &std::path::Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() { std::fs::create_dir_all(parent).unwrap(); }
        std::fs::File::create(p).unwrap().write_all(body.as_bytes()).unwrap();
    }

    #[tokio::test]
    async fn lone_blocking_refuted_does_not_reject() {
        use crate::domain::{Severity, Verdict, VerifyVerdict};
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.rs", "fn f() {\n    db.write(token);\n}\n");
        // 单 model alpha：review 报一条 High 孤证(单 model → agreement=1)；refute 退化自审 alpha。
        // 注：FakeProvider 是 prompt-blind 的——对同一 model 的任何调用（无论 review 还是 refute
        // prompt）都返回同一脚本，故 refute 调用返回非空 → refuted → confidence 0 → 不 reject。
        // 交叉 model 选择本身已在 verify.rs 单测验证；本测试只验「证伪翻转 verdict」全链路。
        let cfg = Config::from_layers(&[r#"[review]
default_models=["alpha"]
"#]).unwrap();
        let provider = crate::provider::fake::FakeProvider::with(
            "alpha", vec![raw("src/a.rs", "db.write(token);", Severity::High)],
        );
        let my_args = ReviewArgs { pack_only: false,
            base: None, scope: "working-tree".into(),
            spec: vec![], focus: None, models: None, models_all: false,
            block_threshold: None, conf_floor: None,
            background: false, format: "json".into(),
            resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None,
        };
        let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    db.write(token);".into() }];
        let (report, code) = run_code_review(&my_args, &provider, &cfg, changes, dir.path(), None, None).await.unwrap();
        let f = &report.findings[0];
        assert_eq!(f.verify.as_ref().unwrap().verdict, VerifyVerdict::Refuted);
        assert_eq!(f.confidence, 0.0);
        assert_eq!(report.verdict, Verdict::Approve, "孤证被自审 refute 证伪 → 不翻 verdict");
        assert!(code == 0 || code == 2, "approve → 0 或 2（不 10）");
    }

    fn args() -> ReviewArgs {
        ReviewArgs { pack_only: false,
            base: None, scope: "working-tree".into(),
            spec: vec![], focus: None, models: Some("m".into()),
            models_all: false,
            block_threshold: None, conf_floor: None,
            background: false, format: "json".into(),
            resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None,
        }
    }

    fn raw(file: &str, quote: &str, sev: crate::domain::Severity) -> RawFinding {
        RawFinding {
            severity: sev, title: "issue".into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "fix".into(),
            hard_constraint: false, quote: quote.into(),
        }
    }

    #[tokio::test]
    async fn code_review_anchors_real_drops_fabricated() {
        use crate::domain::Severity;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.rs", "fn f() {\n    db.write(token);\n}\n");
        // 两个 model 都报真 finding → dedup 合并成 agreement=2 → 跳过层① verify，
        // 保持本测试「anchor 砍编造 + High→Reject」的原意（不被 S3-b verify 自审误伤）。
        // 编造 quote 只 m 报，anchor 因 quote 不在文件而丢弃。
        let cfg = Config::from_layers(&[r#"[review]
default_models=["m","m2"]
"#]).unwrap();
        let mut scripted = std::collections::BTreeMap::new();
        scripted.insert("m".to_string(), vec![
            raw("src/a.rs", "db.write(token);", Severity::High),
            raw("src/a.rs", "this is fabricated", Severity::Critical),
        ]);
        scripted.insert("m2".to_string(), vec![raw("src/a.rs", "db.write(token);", Severity::High)]);
        let provider = FakeProvider { scripted, ..Default::default() };
        let my_args = ReviewArgs { pack_only: false, models: None, ..args() };
        let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    db.write(token);".into() }];
        let (report, code) = run_code_review(&my_args, &provider, &cfg, changes, dir.path(), None, None).await.unwrap();
        assert_eq!(report.findings.len(), 1, "fabricated dropped, real merged across models");
        assert_eq!(report.findings[0].file, "src/a.rs");
        assert_eq!(report.findings[0].line_start, 2);
        assert_eq!(report.findings[0].agreement, 2, "two models agree → skips verify (layer ①)");
        assert_eq!(report.verdict, Verdict::Reject); // High agreement≥2，verify 跳过 → Reject
        assert_eq!(code, 10);
    }

    #[tokio::test]
    async fn code_review_empty_changes_approves() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::from_layers(&[r#"[review]
default_models=["m"]
"#]).unwrap();
        let provider = FakeProvider::with("m", vec![]);
        let (report, code) = run_code_review(&args(), &provider, &cfg, vec![], dir.path(), None, None).await.unwrap();
        assert!(report.findings.is_empty());
        assert_eq!(code, 0);
    }

    #[tokio::test]
    async fn code_review_stamps_head_sha_into_runmeta() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::from_layers(&[r#"[review]
default_models=["m"]
"#]).unwrap();
        let provider = FakeProvider::with("m", vec![]);
        let (report, _) = run_code_review(&args(), &provider, &cfg, vec![], dir.path(),
            Some("abc123".into()), None).await.unwrap();
        assert_eq!(report.run.head_sha.as_deref(), Some("abc123"));
        let (report2, _) = run_code_review(&args(), &provider, &cfg, vec![], dir.path(), None, None).await.unwrap();
        assert_eq!(report2.run.head_sha, None, "非 git 环境如实 None，不编造");
    }
}

#[cfg(test)]
mod spec_tests {
    use super::*;
    use spec::render_spec_section;
    // ---- Task 2：--spec 接入第一趟主评审的接线级回归（集成测试，非纯构造器单测）----

    /// 记录每次 `run` 调用的 prompt；空脚本化返回（0 findings）——本组测试只关心 prompt 内容，
    /// 不关心 findings/verify 链路（同 resume_tests::RecordingProvider 手法）。
    #[derive(Default)]
    struct SpecPromptRecorder {
        prompts: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl crate::provider::ReviewProvider for SpecPromptRecorder {
        async fn run(
            &self,
            req: crate::provider::LensRequest,
        ) -> Result<crate::provider::LensCall, crate::provider::ProviderError> {
            self.prompts.lock().unwrap().push(req.prompt.clone());
            Ok(crate::provider::LensCall {
                findings: vec![],
                input_tokens: 1,
                output_tokens: 1,
                cached_input_tokens: 0,
                iterations: 1,
                tool_use: Default::default(),
                needs: vec![],
            })
        }
    }

    fn spec_test_args(spec: Vec<String>) -> ReviewArgs {
        ReviewArgs { pack_only: false,
            base: None, scope: "working-tree".into(), spec, focus: None,
            models: Some("m".into()), models_all: false,
            block_threshold: None, conf_floor: None,
            background: false, format: "json".into(),
            resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None,
        }
    }

    #[tokio::test]
    async fn code_review_with_spec_injects_reference() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn f() {\n    let x = 1;\n}\n").unwrap();
        let spec_path = root.join("design.md");
        std::fs::write(&spec_path, "DESIGN D8: fail-open").unwrap();

        let cfg = Config::from_layers(&[r#"[review]
default_models=["m"]
"#]).unwrap();
        let provider = SpecPromptRecorder::default();
        let args = spec_test_args(vec![spec_path.to_string_lossy().to_string()]);
        let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    let x = 1;".into() }];

        run_code_review(&args, &provider, &cfg, changes, root, None, None).await.unwrap();

        let prompts = provider.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1, "单格全审默认下第一趟只应有一次 provider 调用");
        assert!(prompts[0].contains("<spec_docs>"), "第一趟 prompt 须含 spec_docs 段: {}", prompts[0]);
        assert!(prompts[0].contains("DESIGN D8: fail-open"), "spec 内容须原样注入: {}", prompts[0]);
        assert!(!prompts[0].contains("{{SPEC_DOCS}}"), "no residual placeholder");
    }

    #[tokio::test]
    async fn code_review_without_spec_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn f() {\n    let x = 1;\n}\n").unwrap();

        let cfg = Config::from_layers(&[r#"[review]
default_models=["m"]
"#]).unwrap();
        let provider = SpecPromptRecorder::default();
        let args = spec_test_args(vec![]); // 不带 --spec
        let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    let x = 1;".into() }];

        run_code_review(&args, &provider, &cfg, changes, root, None, None).await.unwrap();

        let prompts = provider.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1);
        // 无 --spec → prompt 无 spec 段内容，仅占位散文（既有评审 prompt 零变化）。
        assert!(prompts[0].contains("(none — no spec docs provided)"), "空 spec 渲染占位: {}", prompts[0]);
        assert!(!prompts[0].contains("DESIGN"), "不应有任何 spec 内容泄漏: {}", prompts[0]);
        // budget 未扣（D2）：空 --spec → render_spec_section 用掉 0 字节 → saturating_sub(0)
        // 是恒等运算，pack 预算与不带本 feature 时完全一致。
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

        let (entries, meta) = build_packs(&bundles, root, PACK_BUDGET_BYTES);
        assert!(entries[0].contains("target") && entries[0].contains("改动源文"), "bundle 1 自家内容非空含 0 壳: {entries:?}");
        assert!(entries[1].contains("other"), "bundle 2 自家内容非空: {entries:?}");
        assert_eq!(meta, DossierMeta { bundles: 2, injected: 2 });
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
        let (on, _) = build_packs(&bundles, root, PACK_BUDGET_BYTES);
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
        let cfg = crate::config::Config::from_layers(&[r#"
[providers.bigp]
kind = "openai"
[providers.smallp]
kind = "openai"
[models.Big]
provider = "bigp"
id = "big-ctx"
context_window = 1000000
[models.Small]
provider = "smallp"
id = "small-ctx"
context_window = 40000
"#]).unwrap();
        // 未声明窗口的模型 → 保守缺省 128K → (128K−16K)*3−24K=312K < 768K → 窗口墙生效
        assert_eq!(pack_budget_bytes(&cfg, &["Unknown".into()]), 312_000);
        // 大窗模型（1M）：窗口墙 ≈2.9M → 绝对帽 768K 封顶（≈256K tok 记账 <272K 悬崖）
        assert_eq!(pack_budget_bytes(&cfg, &["Big".into()]), 768_000);
        // 小窗模型：(40_000−16_000)*3−24_000 = 48_000
        assert_eq!(pack_budget_bytes(&cfg, &["Small".into()]), 48_000);
        // 多模型取最小窗（一份 pack 全模型安全）
        assert_eq!(pack_budget_bytes(&cfg, &["Big".into(), "Small".into()]), 48_000);
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
        assert!(l3.meta.as_ref().unwrap().source.contains("proj.db"));
    }
    #[tokio::test]
    async fn jobcellsink_manifest_sets_phase_fanout() {
        use crate::state::{JobState, Phase};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_path_buf();
        let store = std::sync::Arc::new(crate::state::JobStore::open(&ws).unwrap());
        store.put_state_for_test(&JobState::new("j", 0)).unwrap();
        let sink = JobCellSink { store: std::sync::Arc::clone(&store), job_id: "j".into(), ckpt_failures: std::sync::atomic::AtomicU32::new(0) };
        use crate::orchestrate::CellSink;
        sink.manifest(&[crate::state::CellInput {
            cell: "c".into(), model: "m".into(), prompt: "p".into() }]);
        assert_eq!(store.read_state("j").unwrap().phase, Some(Phase::Fanout));
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
        sink.manifest(&[CellInput { cell: "c".into(), model: "m".into(), prompt: "p".into() }]);
        sink.on_cell(CellRecord::ok("c", "m", vec![]));
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
        sink.on_cell(CellRecord::ok("c", "m", vec![]));
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
            ReviewReport {
                schema_version: "3".into(),
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
