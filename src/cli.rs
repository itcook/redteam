use crate::bundle::{group, select_files};
use crate::config::Config;
use crate::consolidate::{dedup, refute_targets, verify_findings};
use crate::domain::{DossierMeta, Finding, ReviewReport, RunMeta, Severity, Stats, Usage, Verdict};
use crate::evidence::git::FileChange;
use crate::orchestrate::{enrich, run_code_fanout, CellSink, OrchestrateOpts, PlannedModel};
use crate::output::Format;
use crate::positioning::anchor;
use crate::provider::{LensRequest, ReviewProvider};
use crate::verdict::compute_verdict;
use std::path::Path;

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

/// 退出码映射（统一 run_review/run_code_review）：reject→10、approve+对照集不完整→2、干净 approve→0。
pub fn exit_code(verdict: Verdict, failed_models_nonempty: bool) -> i32 {
    match (verdict, failed_models_nonempty) {
        (Verdict::Reject, _) => 10,
        (Verdict::Approve, true) => 2,
        (Verdict::Approve, false) => 0,
        (Verdict::Incomplete, _) => 2,
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
        let mut v: Vec<_> = cfg.providers.iter().collect();
        v.sort_by_key(|p| std::cmp::Reverse(p.1.priority));
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
            priority: cfg.providers.get(n).map(|p| p.priority).unwrap_or(0),
        })
        .collect();
    (models, replicate)
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
fn model_ids_for(names: &[String], cfg: &Config) -> Vec<String> {
    names
        .iter()
        .map(|n| cfg.providers.get(n).map(|p| p.model.clone()).unwrap_or_else(|| n.clone()))
        .collect()
}

/// 扇出之后的共用尾段上下文——`run_code_review`/`run_resume` 两处扇出前逻辑各不相同
/// （单格×model / 直接重放 manifest prompt），但扇出后（anchor → dedup → verify → verdict
/// → 组报告）完全一致，抽出复用（DRY，T7 授权解冻）。
struct TailCtx<'a> {
    /// anchor/verify_findings 用的项目根。
    root: &'a Path,
    provider: &'a dyn ReviewProvider,
    model_names: Vec<String>,
    /// 与 model_names 同序的 model id（cfg.providers.*.model；查无则回退 provider 名）。
    model_ids: Vec<String>,
    block: Severity,
    floor: f32,
    max_concurrency: usize,
    job_id: String,
    duration_ms: u64,
    usage: (u64, u64, u64),
    warnings: Vec<String>,
    failed_models: Vec<String>,
    dismissed: &'a std::collections::HashSet<String>,
    /// 活跃判例卡——refute 判例注入 + 机械预筛（design 2026-07-03 #3）。
    precedents: &'a [crate::db::DismissCard],
    /// 台账加载披露（进 RunMeta.ledger）。
    ledger_meta: Option<crate::domain::LedgerMeta>,
    /// R3 stamp：证据收集刻的 HEAD（code dispatch 回填；resume 为 None）。
    head_sha: Option<String>,
    /// 卷宗覆盖 meta（T01⑥；进 RunMeta.dossier）。code fan-out 路径为 Some，resume 为 None。
    dossier_meta: Option<DossierMeta>,
    /// 二阶段补查触发观测（进 RunMeta.needs，恒写）：(触发格数, 声明条数)；resume 无 fan-out 恒零。
    needs_stats: (u32, u32),
    /// 后台 job 的 phase 打点通路（refute 子阶段；前台/resume 为 None）。
    sink: Option<&'a dyn CellSink>,
}

/// 共用尾段：anchor → dedup → verify → verdict → 组装 report → 退出码。
/// 两处调用方（run_code_review/run_resume）行为 1:1 迁移，不改变既有语义。
async fn assemble_report(findings: Vec<Finding>, mut ctx: TailCtx<'_>) -> anyhow::Result<(ReviewReport, i32)> {
    let anchored = anchor(findings, ctx.root);
    let findings = dedup(anchored);
    let clusters = vec![]; // correlate 已随 doc 模式退场（E10）；clusters 字段留空保持 schema 稳定。
    // refute 子阶段打点（targets 数与 verify_findings 同一真相源函数，非重复实现）。
    let n_targets = refute_targets(&findings, ctx.precedents, ctx.block).len();
    if n_targets > 0 {
        if let Some(s) = ctx.sink {
            s.on_phase(crate::state::job::Phase::Refute, Some(format!("{n_targets} targets")));
        }
    }
    let (findings, refute_warnings, refute_usage) = verify_findings(
        findings, ctx.provider, &ctx.model_names, ctx.block, ctx.max_concurrency,
        ctx.root, ctx.precedents,
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
    let stats = Stats::from_findings(&findings);
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
            ..Default::default()
        },
    };
    // 退出码：reject 优先 → 10；否则 approve 但对照集不完整 → 2；干净 approve → 0。
    let code = exit_code(verdict, !report.run.failed_models.is_empty());
    Ok((report, code))
}

/// pack flat 预算（字节）——S2-b' 提到 256K：0 壳升级整文件后 cli.rs 级大文件（~90KB）
/// 需装入（E7）；256K ≈ 1M-context 模型的 ~6%、256K 模型的 ~25%，安全。per-model 预算函数已落地
/// （常数查表版，见下方 pack_budget_bytes）；T27 完整版（models.dev 策展表+真 tokenizer 校准）仍缓。
const PACK_BUDGET_BYTES: usize = 256_000;
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
        .map(|m| cfg.providers.get(m).and_then(|p| p.context_window_tokens).unwrap_or(128_000))
        .min()
        .unwrap_or(128_000);
    let by_window = (min_ctx.saturating_sub(OUTPUT_RESERVE_TOKENS) as usize)
        .saturating_mul(BYTES_PER_TOKEN)
        .saturating_sub(SHELL_RESERVE_BYTES);
    PACK_BUDGET_BYTES.min(by_window)
}

/// 单格全审默认（E9 判决，2026-07-13 转正）：全变更集恒并成一个 bundle——分格在全景 pack
/// 范式下=N 倍重复喂同一全景（两跑实证 input −77%、质量未见系统性下降）。空输入原样放行
/// （不产出「一个空文件的 bundle」）——上游两处空证据守卫（`run_code_review` 的「no reviewable
/// evidence」警告、`run_incremental_review` 的 delta 全噪声过滤报错）靠 `bundles.is_empty()`
/// 判空，语义须保持不变。
fn merge_bundles(bundles: Vec<crate::bundle::Bundle>) -> Vec<crate::bundle::Bundle> {
    if bundles.is_empty() {
        return bundles;
    }
    vec![crate::bundle::Bundle { files: bundles.into_iter().flat_map(|b| b.files).collect() }]
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

/// 二阶段补查行窗（design §2.3）：lines 命中区间外扩 ±30 行。
const NEED_LINES_WINDOW: usize = 30;

/// 补查证据单条段字节帽（Important 修复：此前无上限——长单行文件上 ±30 行窗可逼近
/// read_capped 的 512KB 读帽，doc 线整文件 need 直接就是整文件，均绕开预算纪律）。
/// 量级对齐 verify.rs::SNIPPET_FULL_FILE_CAP（64KB）。
const NEED_SECTION_BYTE_CAP: usize = 64_000;

/// NeedEntry.lines 解析："a-b"（1-based、a<=b）→ Some((a,b))；缺失/坏格式 → None（整文件语义）。
fn parse_need_lines(lines: Option<&str>) -> Option<(u32, u32)> {
    let (a, b) = lines?.split_once('-')?;
    let a: u32 = a.trim().parse().ok()?;
    let b: u32 = b.trim().parse().ok()?;
    (a >= 1 && a <= b).then_some((a, b))
}

/// 单条 need → `## 补查证据` 段：lines 合法 → ±30 行窗；缺失/坏格式 → 整文件。
/// 路径安全走 ToolExecutor 不变量（绝对路径/`..`/symlink 逃逸/不可读 → None、不 panic）。
fn need_section(ex: &crate::tools::ToolExecutor, need: &crate::domain::NeedEntry) -> Option<String> {
    let text = ex.read_capped(&need.path)?;
    let lines: Vec<&str> = text.lines().collect();
    let n = lines.len();
    if n == 0 {
        return None;
    }
    let (a, b) = match parse_need_lines(need.lines.as_deref()) {
        Some((la, lb)) => (
            (la as usize).saturating_sub(NEED_LINES_WINDOW).max(1).min(n),
            (lb as usize).saturating_add(NEED_LINES_WINDOW).min(n),
        ),
        None => (1, n),
    };
    let b = b.max(a);
    let mut body = lines[a - 1..b].join("\n");
    if body.len() > NEED_SECTION_BYTE_CAP {
        let mut cut = NEED_SECTION_BYTE_CAP;
        while cut > 0 && !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
        body.push_str("\n…（截断）");
    }
    Some(format!("## 补查证据\n{}:{}-{}\n{}\n\n", need.path, a, b, body))
}

/// --spec 参照文档总帽——超则截断+标记（D2）。
const SPEC_BUDGET_BYTES: usize = 64_000;

/// D4 守卫：拒绝把密钥/凭据类文件注入送外部模型。命中=skip+warn。
/// 路径遍历信用户（user-arg），但内容敏感度不信任（防 secret exfil）。
fn is_sensitive_spec_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
    const DENY_EXT: &[&str] = &[".env", ".pem", ".key", ".p12", ".pfx", ".pkcs12", ".crt", ".cer"];
    const DENY_BASE: &[&str] = &["id_rsa", "id_ed25519", "id_dsa", ".netrc", ".htpasswd"];
    const DOC_EXT: &[&str] = &[".md", ".txt", ".rst", ".adoc"];
    if DENY_EXT.iter().any(|e| base.ends_with(e)) {
        return true;
    }
    if DENY_BASE.iter().any(|b| base == *b || base.starts_with(&format!("{b}."))) {
        return true;
    }
    // 名含敏感词且非文档类 → 拒（design.md 这类文档放行）
    let doc = DOC_EXT.iter().any(|e| base.ends_with(e));
    if !doc && ["secret", "credential", "password", "token"].iter().any(|w| base.contains(w)) {
        return true;
    }
    false
}

/// 读 --spec 文档 → 有界参照段（供 [L0] 实现-对照-设计）。返回 (段, 用掉字节)。
/// 路径=用户 CLI 提供=可信路径遍历（D4）→ 直接有界读，允许 out-of-root；但敏感文件 denylist。
// ponytail: 本 task 只交付纯构造器，接线（调用点注入 prompt）留给后续 task；
// 加 allow(dead_code) 是临时的，接线落地后删掉。
#[allow(dead_code)]
pub(crate) fn render_spec_section(spec_paths: &[String]) -> (String, usize) {
    if spec_paths.is_empty() {
        return (String::new(), 0);
    }
    let mut body = String::new();
    let mut used = 0usize;
    for p in spec_paths {
        if is_sensitive_spec_path(p) {
            eprintln!("redteam: 拒绝把疑似密钥/凭据文件注入评审上下文（防泄漏给外部模型）：{p}");
            continue;
        }
        if used >= SPEC_BUDGET_BYTES {
            break;
        }
        let Ok(mut content) = std::fs::read_to_string(p) else {
            continue;
        };
        let remaining = SPEC_BUDGET_BYTES - used;
        let truncated = content.len() > remaining;
        if truncated {
            // 字符边界安全截断（同 need_section 的既有手法，CJK 不 panic）。
            let mut cut = remaining;
            while cut > 0 && !content.is_char_boundary(cut) {
                cut -= 1;
            }
            content.truncate(cut);
        }
        used += content.len();
        body.push_str(&format!(
            "### {p}\n```\n{content}\n```{}\n\n",
            if truncated { "\n（…spec 截断：超参照预算）" } else { "" }
        ));
    }
    if body.is_empty() {
        return (String::new(), 0);
    }
    let section = format!(
        "<spec_docs>\n关联设计/规格文档（供 [L0] 检查实现是否偏离设计意图；评审对象仍是代码、spec 仅作参照）：\n\n{body}</spec_docs>\n"
    );
    (section, used)
}

/// 草稿注入段（design §2.3）：第一趟 findings 以 CONFIRM-or-REVISE 契约随第二趟 prompt 带回；
/// 空草稿也渲染块（契约「不再声明 needs」对零草稿格同样生效）。
fn draft_block(drafts: &[Finding]) -> String {
    let mut s = String::from(
        "\n<first_pass_draft>\nYour first pass produced these draft findings. CONFIRM or REVISE each using the\nnewly added evidence — drop a draft ONLY if the evidence refutes it, and do not\nre-declare needs.\n",
    );
    for d in drafts {
        let quote_head: String = d.quote.chars().take(80).collect();
        s.push_str(&format!(
            "- [{}] {} @ {}:{}（{}）\n",
            d.severity.as_str(), d.title, d.file, d.line_start, quote_head
        ));
    }
    s.push_str("</first_pass_draft>\n");
    s
}

/// 二阶段补查 prompt（code 线，design §2.3 置换而非追加）：整文件 needs 前插 augmented bundle
/// （build_pack 顺序填充先进先占预算 → 自家非 needs 文件预算紧张时自动让位=置换语义）；lines 行窗 needs 走
/// `## 补查证据` 独立段置于卷宗最前；prompt 尾部追加第一趟草稿 CONFIRM-or-REVISE 段。
/// `incremental`：增量轮的 <incremental_round> 段原样带入第二趟（claimed-resolved/dispute
/// 契约不因补跑丢失）；全量路径传空串。全部 needs 逃逸/不可读 → None（不补跑，省一次 LLM 调用）。
#[allow(clippy::too_many_arguments)] // 与 build_bundle_prompt 同源的参数面；收拢 struct 是过度设计
pub(crate) fn second_pass_prompt(
    bundles: &[crate::bundle::Bundle],
    root: &Path,
    budget: usize,
    target_label: &str,
    focus: &str,
    idx: usize,
    needs: &[crate::domain::NeedEntry],
    drafts: &[Finding],
    incremental: &str,
) -> Option<String> {
    let own = bundles.get(idx)?;
    let ex = crate::tools::ToolExecutor::new(root);
    // 两遍走：先收整文件条目——同 path 全文与行窗并存时全文优先（全文是超集，互斥防双重注入）。
    let mut full_paths: Vec<String> = Vec::new();
    for need in needs {
        if parse_need_lines(need.lines.as_deref()).is_none()
            && !full_paths.contains(&need.path)
            && ex.read_capped(&need.path).is_some()
        {
            // ponytail: read_capped 在此只当路径安全验证用（内容由 build_pack 自己读）——
            // needs ≤5 条/格，多读一次成本可忽略，省一个只为验证的旁路 API。
            full_paths.push(need.path.clone());
        }
    }
    let mut evidence = String::new();
    for need in needs {
        if parse_need_lines(need.lines.as_deref()).is_some() && !full_paths.contains(&need.path) {
            if let Some(sec) = need_section(&ex, need) {
                evidence.push_str(&sec);
            }
        }
    }
    if evidence.is_empty() && full_paths.is_empty() {
        return None;
    }
    // augmented bundle：needs 前插（changeset 内的 need 取真 diff，防超预算 def 回落失锚）
    // → 自家文件（单格全审默认下 own 即全变更集，无兄弟可镜像）；预算记账仍只扣自家原 diff。
    let mut files: Vec<FileChange> = full_paths
        .into_iter()
        .map(|p| {
            let diff = bundles.iter().flat_map(|b| &b.files)
                .find(|f| f.path == p).map(|f| f.diff.clone()).unwrap_or_default();
            FileChange { path: p, diff }
        })
        .collect();
    for fc in own.files.iter() {
        if !files.iter().any(|e| e.path == fc.path) {
            files.push(fc.clone());
        }
    }
    let own_diff_bytes: usize = own.files.iter().map(|fc| fc.diff.len()).sum();
    let pb = crate::bundle::Bundle { files };
    // 第二趟恒用 build_pack（唯一路径，E8-E 安乐死 SCIP 后无引擎可选）——needs 只消费
    // 0 壳全文分支（body_section）。
    // Important 修复：evidence（行窗证据段，已在上方按 NEED_SECTION_BYTE_CAP 分段封顶）先
    // 扣出 pack 预算再传入 build_pack——否则 evidence+pack 联合越过每模型字节预算，撞墙全归零
    // （design §2.2）。budget 耗尽时 build_pack 自身的降级链兜底，不会 panic。
    let pack = crate::pack::build_pack(&pb, root, budget.saturating_sub(evidence.len()), own_diff_bytes);
    let dossier = crate::prompt::render_dossier_section(&format!("{evidence}{pack}"));
    let others = crate::bundle::other_files_manifest(bundles, idx);
    let mut p = crate::prompt::build_bundle_prompt(target_label, own, focus, &others, incremental, &dossier);
    p.push_str(&draft_block(drafts));
    Some(p)
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

    // 1. 确定性筛选 + 分包
    // 单格全审默认（E9，2026-07-13）：全变更集并成一个 bundle——分格在全景 pack 范式下
    // =N 倍重复喂全景（两跑实证 input −77%、质量未见下降）。bundle::group 保留（收集与
    // 未来 L4 拆缝用）；超预算由降级梯子（骨架/hunk 窗/省略清单）承接，不拆格。
    let bundles = merge_bundles(group(select_files(changes), Some(root)));

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
    let budget = pack_budget_bytes(cfg, &model_names);
    let (dossiers, dossier_meta) = build_packs(&bundles, root, budget);
    // 二阶段补查构造器（design §2.3）：cell 声明 needs → 置换式重建 prompt 补跑一趟。
    let mk = |idx: usize, needs: &[crate::domain::NeedEntry], drafts: &[Finding]| {
        second_pass_prompt(&bundles, root, budget, &target_label, &focus, idx, needs, drafts, "")
    };
    let mut raw = run_code_fanout(provider, &target_label, &bundles, &focus, "", &dossiers, &oopts, sink, Some(&mk)).await;
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
    };
    if let Some(s) = sink { s.on_phase(crate::state::job::Phase::Finalizing, None); }
    assemble_report(raw.findings, ctx).await
}

/// M13 增量轮主流水线（决议 2-5）：四守卫 → delta fan-out（<incremental_round> 注入）
/// → carry/merge → refute(new-only) → verdict(合并集) → schema v3 报告。
/// 只由 run_review_dispatch 路由进入（前台与后台 worker 共用）。
pub async fn run_incremental_review(
    args: &ReviewArgs,
    prior_job: &str,
    provider: &dyn ReviewProvider,
    cfg: &Config,
    root: &Path,
    sink: Option<&dyn CellSink>,
) -> anyhow::Result<(ReviewReport, i32)> {
    let ledger = load_ledger(args.dismissed.as_deref(), args.no_dismissed, root)?;
    let t0 = std::time::Instant::now();

    // 1. 四守卫 + 基线（违例显式报错拒跑）。
    let base = incremental_preflight(args, prior_job, root)?;
    // 2. 本轮证据 HEAD stamp（R3：与 delta 收集同刻；本轮报告是下一轮的基线）。
    let head_sha = crate::evidence::git::head_sha_at(root).ok();

    // 3. resolve 台账（preflight 刚从 proj.db 读出上轮 result，库必已在；open_at 免建目录副作用）。
    let db_path = root.join(crate::project::PROJECT_DIR).join(crate::db::DB_FILE);
    let mut resolve_cards = if db_path.exists() {
        crate::db::ProjDb::open_at(&db_path)?.resolved_cards()?
    } else {
        Vec::new()
    };

    // 4. R1 rename 迁移：上轮合并集与 resolve 卡同规则迁 file/id（碰撞域一致）。
    let mut prior_findings = base.prior_findings;
    crate::consolidate::migrate_renames(&mut prior_findings, &base.renames);
    crate::consolidate::migrate_resolve_cards(&mut resolve_cards, &base.renames);
    let prior_ids: std::collections::HashSet<String> =
        prior_findings.iter().map(|f| f.id.clone()).collect();
    let resolved_ids: std::collections::HashSet<String> =
        resolve_cards.iter().map(|c| c.id.clone()).collect();

    // 5. carry：resolved 排除 → 就近重锚 → 成活/消灭。
    let outcome = crate::consolidate::carry(prior_findings, &resolved_ids, root);

    // 6. claimed-resolved 清单（防假修复保险）：本谱系已销案（∩上轮合并集）且 quote 仍锚中
    //    ——修真落地的 quote 会消失，仍在场即可疑，搭本轮评审的车顺带验证（零额外 LLM 调用）。
    let claimed: Vec<crate::db::ResolveCard> = resolve_cards
        .iter()
        .filter(|c| prior_ids.contains(&c.id) && !c.quote.trim().is_empty())
        .filter(|c| crate::positioning::anchor_near(root, &c.file, &c.quote, 0).is_some())
        .cloned()
        .collect();

    // 7. delta → bundle + 增量 prompt 段 + fan-out（select_files/group/run_code_fanout 全复用）。
    // 单格全审默认（E9）：增量路径同全量一样收缩到单 bundle（merge_bundles 空输入原样放行，
    // 下面的空判空守卫语义不变）。
    let bundles = merge_bundles(group(select_files(base.delta), Some(root)));
    anyhow::ensure!(
        !bundles.is_empty(),
        "--incremental-from: delta since {} is all noise-filtered (nothing reviewable); run a full `redteam review` or let ship's stall guard decide",
        base.base_sha
    );
    let all_files = crate::evidence::git::branch_files(
        root,
        args.base.as_deref().expect("branch scope enforced by preflight"),
    )?;
    let delta_files: std::collections::BTreeSet<String> =
        bundles.iter().flat_map(|b| b.files.iter().map(|f| f.path.clone())).collect();
    let manifest2 = crate::bundle::two_layer_manifest(&all_files, &delta_files);
    let short_base = &base.base_sha[..12.min(base.base_sha.len())];
    let inc_section = crate::prompt::render_incremental_section(short_base, &manifest2, &claimed);

    let (models, replicate) = resolve_models(args.models.as_deref(), args.models_all, cfg);
    let model_names: Vec<String> = models.iter().map(|m| m.name.clone()).collect();
    let model_ids = model_ids_for(&model_names, cfg);
    let oopts = OrchestrateOpts {
        models,
        replicate,
        no_fallback: false,
        max_concurrency: cfg.review.max_concurrency,
    };
    let focus = args.focus.clone().unwrap_or_default();
    let target_label = format!("fix delta {short_base}..HEAD (incremental round)");
    if let Some(s) = sink { s.on_meta(&model_names); }
    let budget = pack_budget_bytes(cfg, &model_names);
    let (dossiers, dossier_meta) = build_packs(&bundles, root, budget);
    // 二阶段补查构造器：<incremental_round> 段原样带入第二趟——claimed-resolved 顺带验证/
    // dispute 契约不因补跑丢失。
    let mk = |idx: usize, needs: &[crate::domain::NeedEntry], drafts: &[Finding]| {
        second_pass_prompt(&bundles, root, budget, &target_label, &focus, idx, needs, drafts, &inc_section)
    };
    let mut raw = run_code_fanout(provider, &target_label, &bundles, &focus, &inc_section, &dossiers, &oopts, sink, Some(&mk)).await;

    // 8. 尾段：anchor→dedup→clamp（仅 new）→ dispute 检测 → merge(carry∪new)
    //    → refute(new-only) → mark_dismissed → verdict(合并集) → v3 报告。
    let block = args.block_threshold.as_deref().map(parse_sev).unwrap_or(cfg.review.block_threshold);
    let floor = args.conf_floor.unwrap_or(cfg.review.conf_floor);
    if let Some(s) = sink { s.on_phase(crate::state::job::Phase::Finalizing, None); }
    let anchored = anchor(raw.findings, root);
    let new_findings = dedup(anchored);
    let mut resolution_disputed: Vec<String> = new_findings
        .iter()
        .filter(|f| resolved_ids.contains(&f.id))
        .map(|f| f.id.clone())
        .collect();
    resolution_disputed.sort();
    resolution_disputed.dedup();
    let merged = crate::consolidate::merge_carry(outcome.carried, new_findings);

    let n_targets = refute_targets(&merged, &ledger.cards, block).len();
    if n_targets > 0 {
        if let Some(s) = sink {
            s.on_phase(crate::state::job::Phase::Refute, Some(format!("{n_targets} targets")));
        }
    }
    let (findings, refute_warnings, refute_usage) = verify_findings(
        merged, provider, &model_names, block, cfg.review.max_concurrency, root, &ledger.cards,
    ).await;
    let mut warnings = raw.warnings;
    warnings.extend(refute_warnings);
    // T03：同 assemble_report——refute 趟 token 折进总账，不再是账外盈余。
    raw.usage.0 += refute_usage.0;
    raw.usage.1 += refute_usage.1;
    raw.usage.2 += refute_usage.2;
    let mut findings = findings;
    let dismissed_reappeared = mark_dismissed(&mut findings, &ledger.ids);

    let verdict = compute_verdict(&findings, block, floor);
    let new_only: Vec<Finding> = findings
        .iter()
        .filter(|f| f.origin == crate::domain::Origin::New)
        .cloned()
        .collect();
    let mut stats = Stats::from_findings(&findings);
    stats.new = Some(Box::new(Stats::from_findings(&new_only)));
    let report = ReviewReport {
        schema_version: "3".into(),
        verdict,
        stats,
        findings,
        clusters: vec![],
        run: RunMeta {
            job_id: String::new(),
            models: model_names,
            model_ids,
            duration_ms: t0.elapsed().as_millis() as u64,
            usage: Usage {
                input_tokens: raw.usage.0, output_tokens: raw.usage.1, cached_input_tokens: raw.usage.2,
                refute: if refute_usage == (0, 0, 0) { None } else {
                    Some(Box::new(Usage {
                        input_tokens: refute_usage.0, output_tokens: refute_usage.1,
                        cached_input_tokens: refute_usage.2, refute: None,
                    }))
                },
            },
            warnings,
            failed_models: raw.failed_models,
            dismissed_reappeared,
            ledger: ledger.meta.clone(),
            head_sha,
            incremental: true,
            delta_base: Some(base.base_sha),
            resolved: outcome.resolved_applied,
            resolution_disputed,
            carried_dropped: outcome.carried_dropped,
            dossier: Some(dossier_meta),
            needs: crate::domain::NeedsMeta {
                triggered_cells: raw.needs_stats.0, declared: raw.needs_stats.1,
            },
        },
    };
    let code = exit_code(verdict, !report.run.failed_models.is_empty());
    Ok((report, code))
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

/// R2.1 job checkpoint sink：扇出前落 manifest（+刷 progress.total），每 cell 完成落 partial
/// （+刷 last_active/progress.done）。Bug B 修复：现在 on_cell 由并发 fan-out future 直接触发
/// （见 orchestrate/mod.rs），read_state→改→write_state 的 read-modify-write 段必须串行化，
/// 否则并发 cell 各自读到同一旧值、+1 后互相覆盖，丢更新。
/// ponytail：全局互斥锁最省事且正确——cell 完成是低频事件，锁粒度不值当细化。
struct JobCellSink {
    ws: std::path::PathBuf,
    job_id: String,
    lock: std::sync::Mutex<()>,
}

impl CellSink for JobCellSink {
    fn manifest(&self, inputs: &[crate::state::store::CellInput]) {
        let _g = self.lock.lock().unwrap();
        let _ = crate::state::store::write_manifest(&self.ws, &self.job_id, inputs);
        if let Some(mut st) = crate::state::job::read_state(&self.ws, &self.job_id) {
            st.progress.total = inputs.len() as u32;
            st.set_phase(crate::state::job::Phase::Fanout, None);
            let _ = crate::state::job::write_state(&self.ws, &st);
        }
    }

    fn on_cell(&self, rec: crate::state::store::CellRecord) {
        let _g = self.lock.lock().unwrap();
        let _ = crate::state::store::append_partial(&self.ws, &self.job_id, &rec);
        if let Some(mut st) = crate::state::job::read_state(&self.ws, &self.job_id) {
            st.last_active = now_unix();
            st.progress.done += 1;
            let _ = crate::state::job::write_state(&self.ws, &st);
        }
    }

    fn on_phase(&self, phase: crate::state::job::Phase, note: Option<String>) {
        let _g = self.lock.lock().unwrap();
        if let Some(mut st) = crate::state::job::read_state(&self.ws, &self.job_id) {
            st.set_phase(phase, note);
            let _ = crate::state::job::write_state(&self.ws, &st);
        }
    }

    fn on_meta(&self, models: &[String]) {
        let _g = self.lock.lock().unwrap();
        if let Some(mut st) = crate::state::job::read_state(&self.ws, &self.job_id) {
            st.models = models.to_vec();
            let _ = crate::state::job::write_state(&self.ws, &st);
        }
    }
}

/// M13 增量基线（incremental_preflight 产物）。
#[derive(Debug)]
pub(crate) struct IncrementalBaseline {
    /// 上轮 stamped HEAD（delta base）。
    pub base_sha: String,
    /// 上轮合并集（含 refuted/dismissed，原样携带候选）。
    pub prior_findings: Vec<Finding>,
    /// R1 旧→新路径映射。
    pub renames: Vec<(String, String)>,
    /// base..HEAD 逐文件 diff。
    pub delta: Vec<crate::evidence::git::FileChange>,
}

/// 决议 7 前置守卫：违反**显式报错拒跑**（不静默降级全量——成本意外）。
/// ①谱系 ②同族（scope，含 v1 范围收窄 branch）③健康基线（R2 三查）④空 delta。
pub(crate) fn incremental_preflight(
    args: &ReviewArgs,
    prior_job: &str,
    root: &Path,
) -> anyhow::Result<IncrementalBaseline> {
    // v1 范围：branch scope——working-tree 证据不可 stamp，轮间未提交改动会静默漏出 delta。
    anyhow::ensure!(
        args.scope == "branch",
        "--incremental-from requires branch scope (pass --base <branch>): the delta only covers committed changes since the prior round's stamped HEAD"
    );
    anyhow::ensure!(
        args.base.is_some(),
        "--incremental-from requires --base <branch> (the full-changeset manifest is derived from merge-base(base, HEAD))"
    );
    // 脏树守卫（终审 Important）：carry 重锚读活文件而 delta 只含已提交改动——未提交
    // 编辑可使携带项锚失效却不在本轮评审范围内（carried_dropped 审计语义为假，open
    // finding 静默蒸发）。锚定不变式成立前提=评审时点树即 HEAD。硬错误保守视为脏。
    anyhow::ensure!(
        !crate::evidence::git::worktree_dirty(root).unwrap_or(true),
        "--incremental-from requires a clean working tree (uncommitted tracked changes can invalidate carried findings' anchors without being part of the reviewed delta); commit or stash first, then re-run"
    );

    // 载上轮 result（增量只对经 --background 持久化过 result 的作业有意义）。
    let raw = crate::state::store::read_result(root, prior_job).ok_or_else(|| anyhow::anyhow!(
        "--incremental-from: no persisted result for job {prior_job} (prior round must have run via --background)"
    ))?;
    let prior: ReviewReport = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("--incremental-from: cannot parse prior result for {prior_job}: {e}"))?;

    // ③健康基线守卫（R2）：锚定不变式只对「上轮全量审干净」的基线成立。
    anyhow::ensure!(
        prior.verdict != Verdict::Incomplete,
        "--incremental-from: prior round {prior_job} is incomplete (unreviewed bundles have no findings to carry); run a full `redteam review` to establish a clean baseline"
    );
    anyhow::ensure!(
        prior.run.failed_models.is_empty(),
        "--incremental-from: prior round {prior_job} had failed models {:?} (adversarial coverage was incomplete); run a full `redteam review`",
        prior.run.failed_models
    );
    let manifest = crate::state::store::read_manifest(root, prior_job);
    let ok_cells: std::collections::BTreeSet<String> = crate::state::store::read_partial(root, prior_job)
        .into_iter()
        .filter(|r| r.ok)
        .map(|r| r.cell)
        .collect();
    anyhow::ensure!(
        manifest.iter().all(|c| ok_cells.contains(&c.cell)),
        "--incremental-from: prior round {prior_job} has manifest cells without an ok record (partial coverage); run a full `redteam review`"
    );

    // stamp 在场（v2 老报告无 head_sha——先跑一次全量建立基线）。
    let base_sha = prior.run.head_sha.clone().ok_or_else(|| anyhow::anyhow!(
        "--incremental-from: prior result has no head_sha stamp (predates incremental rounds); run one full `redteam review` to establish a stamped baseline"
    ))?;

    // ②同族守卫：scope 与上轮一致（读上轮 args.json）。
    let prior_args: ReviewArgs = crate::state::store::read_args_raw(root, prior_job)
        .and_then(|j| serde_json::from_str(&j).ok())
        .ok_or_else(|| anyhow::anyhow!("--incremental-from: cannot load prior job args for {prior_job}"))?;
    anyhow::ensure!(
        prior_args.scope == args.scope,
        "--incremental-from: scope mismatch with prior round (prior scope '{}', current '{}'); incremental carry requires the same review family",
        prior_args.scope,
        args.scope
    );

    // ①谱系守卫：stamped HEAD 必须是当前 HEAD 祖先（rebase/reset 即锚定不变式失效）。
    // is_ancestor 的硬错误（如 sha 不存在于对象库）同属谱系失效，折同一文案。
    let lineage_ok = crate::evidence::git::is_ancestor(root, &base_sha).unwrap_or(false);
    anyhow::ensure!(
        lineage_ok,
        "--incremental-from: prior stamped HEAD {base_sha} is not an ancestor of HEAD (rebase/reset detected — the anchoring invariant no longer holds); run a full `redteam review`"
    );

    // ④空 delta：HEAD 未动 → 显式报错（ship 按停滞守卫处理，不烧空轮）。
    let renames = crate::evidence::git::rename_map(root, &base_sha)?;
    let delta = crate::evidence::git::changed_files_since(root, &base_sha)?;
    anyhow::ensure!(
        !delta.is_empty(),
        "--incremental-from: empty delta since {base_sha} (HEAD unchanged — nothing new to review); commit fixes first, or run a full `redteam review`"
    );

    Ok(IncrementalBaseline { base_sha, prior_findings: prior.findings, renames, delta })
}

/// R2.2：失败/超时时从 proj.db 的 cell checkpoint 里已完成的 findings 组一份 `incomplete`
/// 报告——绝不因为 dispatch 出错/超时就静默丢掉已经跑完的 cell。M 取 manifest 行数（若尚未
/// 写出 manifest 就已失败，退化用已完成数兜底，不出现 "N/0"）。
fn partial_report(ws: &Path, job_id: &str, reason: String) -> (ReviewReport, i32) {
    let manifest = crate::state::store::read_manifest(ws, job_id);
    let done_records: Vec<_> = crate::state::store::read_partial(ws, job_id)
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
/// `provider.run(LensRequest{model,prompt})`——不重收集证据、不重 build prompt（确定性）；
/// append_partial 落盘后，`read_partial` 取全部 ok:true 的 findings（已完成 + 新补）走 mode
/// 对应的共用尾段（`assemble_report`）组出完整报告。全集补齐→verdict 正常；仍有缺失→`Incomplete`。
///
/// ponytail：候补链 cell（未指定 `--models`）的 `CellInput.model` 只是 manifest 写入时「最高
/// priority 候选」的近似快照（见设计 §6.5 ⚠️）——resume 按此近似重放。若该模型正是触发 resume
/// 的卡死模型，这次重放会再失败，该 cell 仍缺，可再次 `--resume`（渐进收敛，不会更糟）。精确解
/// 是重放完整候补链或把 manifest.model 升级为 `Vec<String>` 快照，本 task 不做（显式/replicate
/// cell 的 manifest.model 已是精确值，dogfood 常见场景不受影响）。
pub async fn run_resume(
    ws: &Path,
    job_id: &str,
    provider: &dyn ReviewProvider,
    cfg: &Config,
    cwd: &Path,
) -> (ReviewReport, i32) {
    let t0 = std::time::Instant::now();
    let manifest = crate::state::store::read_manifest(ws, job_id);
    // 空 manifest = job 在 fan-out 写 manifest 前就崩溃：没有任何 work-list，尾段会把 0 findings
    // 组装成干净 Approve（假阳性——评审工具静默给未经审查的通过）。护栏：直接出 Incomplete。
    if manifest.is_empty() {
        return partial_report(
            ws, job_id,
            "resume: empty manifest (job crashed before fan-out); nothing to resume".into(),
        );
    }
    let args: ReviewArgs = match crate::state::store::read_args_raw(ws, job_id)
        .and_then(|j| serde_json::from_str(&j).ok())
    {
        Some(a) => a,
        None => {
            return partial_report(
                ws, job_id,
                "resume: missing job args (job never ran via --background)".into(),
            );
        }
    };
    // M13：增量 job 的差集重放只补 fan-out cell，不重跑 carry/merge 尾段——直接拒绝，
    // 引导重跑增量评审（守卫与 carry 必须整链执行）。
    if args.incremental_from.is_some() {
        return partial_report(
            ws, job_id,
            "resume: incremental jobs cannot be resumed; re-run `redteam review --incremental-from <prior-job>`".into(),
        );
    }
    let done: std::collections::BTreeSet<String> = crate::state::store::read_partial(ws, job_id)
        .into_iter()
        .filter(|r| r.ok)
        .map(|r| r.cell)
        .collect();
    let pending: Vec<&crate::state::store::CellInput> =
        manifest.iter().filter(|c| !done.contains(&c.cell)).collect();

    let mut usage = (0u64, 0u64, 0u64);
    let mut warnings = Vec::new();
    for cell in &pending {
        let req = LensRequest { model: cell.model.clone(), prompt: cell.prompt.clone() };
        let rec = match provider.run(req).await {
            Ok(call) => {
                usage.0 += call.input_tokens;
                usage.1 += call.output_tokens; usage.2 += call.cached_input_tokens;
                let fs: Vec<Finding> =
                    call.findings.iter().map(|r| enrich(r, &cell.model)).collect();
                crate::state::store::CellRecord::ok(cell.cell.clone(), cell.model.clone(), fs)
            }
            Err(e) => {
                warnings.push(format!("resume cell [{}] model {}: {e}", cell.cell, cell.model));
                crate::state::store::CellRecord {
                    cell: cell.cell.clone(), model: cell.model.clone(), ok: false,
                    findings: vec![], warn: Some(e.to_string()),
                    duration_ms: 0, input_tokens: 0, output_tokens: 0, cached_input_tokens: 0, attempts: 1, iterations: 0,
                    tool_use: Default::default(),
                }
            }
        };
        let _ = crate::state::store::append_partial(ws, job_id, &rec);
    }

    // 合并出报告：全部 ok:true 的 findings（已完成 + 新补）。
    let final_records = crate::state::store::read_partial(ws, job_id);
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
            return partial_report(ws, job_id, format!("resume: bad ledger: {e}"));
        }
    };

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
    };
    match assemble_report(findings, ctx).await {
        Ok((mut report, mut code)) => {
            if still_missing > 0 {
                report.verdict = Verdict::Incomplete;
                report.run.warnings.push(format!("resume: {still_missing} cell(s) still missing after replay"));
                code = exit_code(Verdict::Incomplete, false);
            }
            (report, code)
        }
        Err(e) => partial_report(ws, job_id, format!("resume tail failed: {e}")),
    }
}

/// 落终态部分报告：渲染 json+md、写 result 文件、把 state 转终态并记录错误原因。
fn write_partial_result(
    ws: &Path,
    job_id: &str,
    st: &mut crate::state::job::JobState,
    status: crate::state::job::JobStatus,
    report: &ReviewReport,
    code: i32,
    error: String,
) {
    let json = crate::output::render(report, crate::output::Format::Json);
    let md = crate::output::render(report, crate::output::Format::Md);
    let _ = crate::state::store::write_result(ws, job_id, &json);
    let _ = crate::state::store::write_result_md(ws, job_id, &md);
    st.status = status;
    st.error = Some(error);
    st.exit_code = Some(code);
    st.result_path = Some(format!("proj.db#results/{job_id}"));
    let _ = crate::state::job::write_state(ws, st);
}

/// 状态机包裹评审：running → (评审) → done+result.json / failed+error。返回退出码。
/// 已被 cancel（状态 Cancelled）时不覆盖为 Done，保留 cancelled 终态。
/// `st.deadline_at`（worker 起手设，见 main.rs）到点仍未完成 → TimedOut + 部分报告（R1.2）。
pub async fn run_job(
    ws: &Path,
    job_id: &str,
    args: &ReviewArgs,
    provider: &dyn ReviewProvider,
    cfg: &Config,
    cwd: &Path,
    started_at: u64,
) -> i32 {
    use crate::state::job::{read_state, write_state, JobState, JobStatus};
    use crate::state::store::write_result;

    let mut st = read_state(ws, job_id).unwrap_or_else(|| JobState::new(job_id, started_at));
    // I2：写 Running 前检查是否已被 cancel，若是则直接返回，保留 Cancelled 终态。
    if st.status == JobStatus::Cancelled {
        return st.exit_code.unwrap_or(0);
    }
    st.status = JobStatus::Running;
    st.set_phase(crate::state::job::Phase::Collecting, None);
    let _ = write_state(ws, &st);

    let sink = JobCellSink { ws: ws.to_path_buf(), job_id: job_id.to_string(), lock: std::sync::Mutex::new(()) };
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

    match outcome {
        Outcome::Done(Ok((mut report, code))) => {
            // dispatch 无 job 上下文（TailCtx.job_id 恒空），此处回填——json/md 契约里
            // run.job_id 是消费者的自证 id（r1 20260703 实证恒空串）。
            report.run.job_id = job_id.to_string();
            // 同渲染 json + md 落盘（result 命令按 format 直读，避免反序列化 report 图）。
            let json = crate::output::render(&report, crate::output::Format::Json);
            let md = crate::output::render(&report, crate::output::Format::Md);
            let _ = write_result(ws, job_id, &json);
            let _ = crate::state::store::write_result_md(ws, job_id, &md);
            // 若期间已被 cancel，保留 cancelled；否则 done。
            // Bug C 修复：以刚读到的最新状态（含 sink 在运行期写入的 progress/last_active）
            // 为底稿叠加终态字段，而非复用函数入口处那份陈旧 st（只设过 status=Running），
            // 否则会把 sink 写的 progress/last_active 覆盖回 0/started_at。
            let latest = read_state(ws, job_id);
            let cancelled = matches!(latest.as_ref().map(|s| s.status), Some(JobStatus::Cancelled));
            let mut st = latest.unwrap_or(st);
            st.status = if cancelled { JobStatus::Cancelled } else { JobStatus::Done };
            st.exit_code = Some(code);
            st.models = report.run.models.clone();
            st.result_path = Some(format!("proj.db#results/{job_id}"));
            let _ = write_state(ws, &st);
            code
        }
        Outcome::Done(Err(e)) => {
            let (report, code) = partial_report(ws, job_id, e.to_string());
            // Bug C 修复：同上，叠加终态前先取最新状态做底稿。
            let mut st = read_state(ws, job_id).unwrap_or(st);
            write_partial_result(ws, job_id, &mut st, JobStatus::Failed, &report, code, e.to_string());
            code
        }
        Outcome::TimedOut => {
            let reason = "timed out: deadline exceeded".to_string();
            let (report, code) = partial_report(ws, job_id, reason.clone());
            // Bug C 修复：同上，叠加终态前先取最新状态做底稿。
            let mut st = read_state(ws, job_id).unwrap_or(st);
            write_partial_result(ws, job_id, &mut st, JobStatus::TimedOut, &report, code, reason);
            code
        }
    }
}

#[cfg(test)]
mod job_tests {
    use super::*;
    use crate::state::job::{read_state, JobStatus};
    use crate::state::store::read_result;
    use crate::provider::fake::FakeProvider;
    use crate::domain::{Finding, Origin, RawFinding, Severity};
    use std::collections::BTreeMap;

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
        ReviewArgs {
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
            "[providers.alpha]\nkind=\"openai\"\nmodel=\"a\"\npriority=10\n\
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
        crate::state::job::write_state(&ws, &crate::state::job::JobState::new("j1", 500)).unwrap();

        let code = run_job(&ws, "j1", &code_args(), &provider, &cfg(), dir.path(), 500).await;

        let st = read_state(&ws, "j1").unwrap();
        assert_eq!(st.status, JobStatus::Done);
        assert_eq!(st.exit_code, Some(code));
        assert!(st.result_path.is_some());
        // result.json 落盘且是合法 JSON
        let res = read_result(&ws, "j1").unwrap();
        assert!(res.contains("\"verdict\""));
        // Bug C 回归：run_job 收尾 write_state 必须以 sink 运行期写的最新 progress 为底稿
        // 叠加终态字段，而不是复用函数入口那份陈旧 st（否则 done/total 会被重置回 0/0）。
        assert_eq!(st.progress.done, 1, "收尾不应吞掉 sink 写入的 progress");
        assert_eq!(st.progress.total, 1, "收尾不应吞掉 sink 写入的 progress");
    }

    #[tokio::test]
    async fn run_job_failed_records_error() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        crate::state::job::write_state(ws, &crate::state::job::JobState::new("j2", 0)).unwrap();
        // 非 git 目录 → changed_files 失败 → run_review_dispatch Err → failed
        let provider = FakeProvider { scripted: BTreeMap::new(), ..Default::default() };
        let code = run_job(ws, "j2", &code_args(), &provider, &cfg(), dir.path(), 0).await;
        assert_eq!(code, 2);
        let st = read_state(ws, "j2").unwrap();
        assert_eq!(st.status, JobStatus::Failed);
        assert!(st.error.is_some());
    }

    /// R2.2：dispatch 失败但已有 checkpoint（partial 里一条 ok:true 记录）→ result.json 不能
    /// 静默丢——应写 verdict=incomplete，findings 并入已 checkpoint 的记录，warnings 含 "partial:"。
    #[tokio::test]
    async fn run_job_failure_writes_incomplete_partial_report() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        crate::state::job::write_state(ws, &crate::state::job::JobState::new("j2b", 0)).unwrap();
        // 预置一条 partial（模拟崩溃前已完成 1 cell）
        crate::state::store::append_partial(
            ws, "j2b",
            &crate::state::store::CellRecord::ok("bundle[a]", "alpha", vec![checkpointed_finding("checkpointed")]),
        ).unwrap();

        // 非 git 目录 → changed_files 失败 → run_review_dispatch Err，但已有 partial 记录
        let provider = FakeProvider { scripted: BTreeMap::new(), ..Default::default() };
        let code = run_job(ws, "j2b", &code_args(), &provider, &cfg(), dir.path(), 0).await;

        assert_eq!(code, 2);
        let st = read_state(ws, "j2b").unwrap();
        assert_eq!(st.status, JobStatus::Failed);
        assert!(st.error.is_some());
        let res = read_result(ws, "j2b").unwrap();
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

        let mut st = crate::state::job::JobState::new("j-to", 0);
        st.deadline_at = Some(0); // 已过期（远小于 now）→ select 立即走超时分支
        crate::state::job::write_state(ws, &st).unwrap();
        // 预置一条 partial（模拟 deadline 前已完成 1 cell）
        crate::state::store::append_partial(
            ws, "j-to",
            &crate::state::store::CellRecord::ok("bundle[a]", "alpha", vec![checkpointed_finding("before-deadline")]),
        ).unwrap();

        let provider = HangProvider;
        let code = run_job(ws, "j-to", &code_args(), &provider, &cfg(), dir.path(), 0).await;

        assert_eq!(code, 2);
        let st = read_state(ws, "j-to").unwrap();
        assert_eq!(st.status, JobStatus::TimedOut);
        let res = read_result(ws, "j-to").unwrap();
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
        let mut pre = crate::state::job::JobState::new("j3", 0);
        pre.status = JobStatus::Cancelled;
        pre.exit_code = Some(0);
        crate::state::job::write_state(ws, &pre).unwrap();

        let provider = FakeProvider { scripted: BTreeMap::new(), ..Default::default() };
        let _code = run_job(ws, "j3", &code_args(), &provider, &cfg(), dir.path(), 0).await;

        // 状态仍应是 Cancelled
        let st = read_state(ws, "j3").unwrap();
        assert_eq!(st.status, JobStatus::Cancelled, "pre-cancel 状态不应被覆盖");
        // 不应写 result.json
        assert!(
            crate::state::store::read_result(ws, "j3").is_none(),
            "Cancelled 提前返回不应留下 result.json"
        );
    }

    /// Bug B 回归：`JobCellSink::on_cell` 的 read_state→改→write_state 段必须串行化。
    /// 无锁时并发 N 次调用会各自读到同一旧 progress、各自 +1、后写覆盖先写，最终
    /// `progress.done < N`；加全局锁后必须精确等于 N。用真实 OS 线程（而非 tokio 任务，
    /// 后者在单线程 runtime 下不会真正并发）制造竞争。
    #[test]
    fn job_cell_sink_on_cell_concurrent_no_lost_update() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_path_buf();
        crate::state::job::write_state(&ws, &crate::state::job::JobState::new("jc1", 0)).unwrap();
        let sink = Arc::new(JobCellSink {
            ws: ws.clone(),
            job_id: "jc1".to_string(),
            lock: std::sync::Mutex::new(()),
        });

        let n = 20usize;
        let handles: Vec<_> = (0..n)
            .map(|i| {
                let sink = sink.clone();
                std::thread::spawn(move || {
                    sink.on_cell(crate::state::store::CellRecord::ok(
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

        let st = read_state(&ws, "jc1").unwrap();
        assert_eq!(
            st.progress.done, n as u32,
            "并发 on_cell 不应丢更新（无锁会 < {n}）"
        );
    }
}

#[cfg(test)]
mod resume_tests {
    use super::*;
    use crate::domain::{Finding, Origin, RawFinding, Severity};
    use crate::state::store::CellInput;

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
        crate::state::store::write_manifest(ws, "j", &inputs).unwrap();
        crate::state::store::append_partial(
            ws, "j",
            &crate::state::store::CellRecord::ok("c1", "m", vec![anchorable_finding("issue-c1", "c1.rs")]),
        ).unwrap();

        let args = ReviewArgs {
            base: None, scope: "working-tree".into(), spec: vec![],
            focus: None, models: Some("m".into()), models_all: false,
            block_threshold: None, conf_floor: None, background: true,
            format: "json".into(), resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None,
        };
        crate::state::store::write_args_raw(ws, "j", &serde_json::to_string(&args).unwrap()).unwrap();

        let provider = RecordingProvider::default();
        let cfg = Config::default();

        let (report, _code) = run_resume(ws, "j", &provider, &cfg, dir.path()).await;

        let calls = provider.prompts.lock().unwrap();
        assert_eq!(calls.len(), 2, "只应对缺失的 2 个 cell 调用 provider");
        assert!(calls.contains(&"P2".to_string()), "应补跑 c2");
        assert!(calls.contains(&"P3".to_string()), "应补跑 c3");
        assert!(!calls.contains(&"P1".to_string()), "已完成的 c1 不应重跑");
        drop(calls);

        assert_eq!(report.findings.len(), 3, "合并后应含 3 个 cell 的 findings（1 已完成 + 2 新补）");
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
        crate::state::store::write_manifest(ws, "j", &inputs).unwrap();
        crate::state::store::append_partial(
            ws, "j",
            &crate::state::store::CellRecord::ok("doc@alpha", "alpha", vec![anchorable_finding("issue-alpha", "alpha.rs")]),
        ).unwrap();

        let args = ReviewArgs {
            base: None, scope: "working-tree".into(), spec: vec![],
            focus: None, models: Some("alpha,beta".into()), models_all: false,
            block_threshold: None, conf_floor: None, background: true,
            format: "json".into(), resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None,
        };
        crate::state::store::write_args_raw(ws, "j", &serde_json::to_string(&args).unwrap()).unwrap();

        let provider = RecordingProvider::default();
        let cfg = Config::default();

        let (report, _code) = run_resume(ws, "j", &provider, &cfg, dir.path()).await;

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
        let args = ReviewArgs {
            base: None, scope: "working-tree".into(), spec: vec![],
            focus: None, models: Some("m".into()), models_all: false,
            block_threshold: None, conf_floor: None, background: true,
            format: "json".into(), resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None,
        };
        crate::state::store::write_args_raw(ws, "j", &serde_json::to_string(&args).unwrap()).unwrap();

        let provider = RecordingProvider::default();
        let (report, code) = run_resume(ws, "j", &provider, &Config::default(), dir.path()).await;

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
        "[providers.alpha]\nkind=\"openai\"\nmodel=\"a\"\npriority=10\n\
         [providers.beta]\nkind=\"openai\"\nmodel=\"b\"\npriority=5\n"
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

    #[test]
    fn exit_code_maps_all_cases() {
        use crate::domain::Verdict;
        assert_eq!(exit_code(Verdict::Reject, false), 10);
        assert_eq!(exit_code(Verdict::Reject, true), 10);   // reject 优先
        assert_eq!(exit_code(Verdict::Approve, true), 2);   // approve 但对照集不完整
        assert_eq!(exit_code(Verdict::Approve, false), 0);  // 干净 approve
        assert_eq!(exit_code(Verdict::Incomplete, false), 2); // 部分结果＝对照集不完整
        assert_eq!(exit_code(Verdict::Incomplete, true), 2);
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
        let my_args = ReviewArgs {
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
        ReviewArgs {
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
        let my_args = ReviewArgs { models: None, ..args() };
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

    #[test]
    fn spec_section_empty_when_no_paths() {
        assert_eq!(render_spec_section(&[]), (String::new(), 0));
    }

    #[test]
    fn spec_section_reads_and_wraps() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("design.md");
        std::fs::write(&path, "DESIGN D8: fail-open").unwrap();
        let path_str = path.to_string_lossy().to_string();
        let (section, used) = render_spec_section(std::slice::from_ref(&path_str));
        assert!(section.contains("<spec_docs>"));
        assert!(section.contains(&path_str));
        assert!(section.contains("DESIGN D8: fail-open"));
        assert!(section.contains("检查实现是否偏离设计意图"));
        assert!(used > 0);
    }

    #[test]
    fn spec_section_caps_and_marks_truncation() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("big.md");
        // CJK 多字节字符填充，确保预算切点大概率落在字符中间——验证边界安全截断不 panic。
        let big = "设计文档超长内容测试".repeat(SPEC_BUDGET_BYTES / 10 + 1000);
        assert!(big.len() > SPEC_BUDGET_BYTES);
        std::fs::write(&path, &big).unwrap();
        let path_str = path.to_string_lossy().to_string();
        let (section, used) = render_spec_section(&[path_str]);
        assert!(section.contains("截断"));
        assert!(used <= SPEC_BUDGET_BYTES);
        // 段壳（标题/围栏/提示语）额外开销要有个合理上限，不能无界膨胀。
        assert!(section.len() <= SPEC_BUDGET_BYTES + 1024);
    }

    #[test]
    fn spec_section_rejects_sensitive_files() {
        let tmp = tempfile::tempdir().unwrap();
        let env_path = tmp.path().join("x.env");
        let key_path = tmp.path().join("id_rsa");
        let doc_path = tmp.path().join("design.md");
        std::fs::write(&env_path, "SECRET_KEY=abc123").unwrap();
        std::fs::write(&key_path, "-----BEGIN PRIVATE KEY-----").unwrap();
        std::fs::write(&doc_path, "DESIGN: spec consistency check").unwrap();
        let paths = vec![
            env_path.to_string_lossy().to_string(),
            key_path.to_string_lossy().to_string(),
            doc_path.to_string_lossy().to_string(),
        ];
        let (section, used) = render_spec_section(&paths);
        assert!(section.contains("design.md"));
        assert!(section.contains("DESIGN: spec consistency check"));
        assert!(!section.contains("SECRET_KEY"));
        assert!(!section.contains("BEGIN PRIVATE KEY"));
        assert!(used > 0);
    }

    #[test]
    fn spec_section_all_sensitive_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let pem_path = tmp.path().join("secrets.pem");
        std::fs::write(&pem_path, "-----BEGIN CERTIFICATE-----").unwrap();
        let (section, used) = render_spec_section(&[pem_path.to_string_lossy().to_string()]);
        assert_eq!((section, used), (String::new(), 0));
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
            Bundle { files: vec![FileChange {
                path: "src/a.rs".into(),
                diff: "@@ -1,0 +1,1 @@\n+pub fn target() {}\n".into(),
            }] },
            Bundle { files: vec![FileChange {
                path: "src/b.rs".into(),
                diff: "@@ -1,0 +1,1 @@\n+pub fn other() {}\n".into(),
            }] },
        ];

        let (entries, meta) = build_packs(&bundles, root, PACK_BUDGET_BYTES);
        assert!(entries[0].contains("target") && entries[0].contains("改动源文"), "bundle 1 自家内容非空含 0 壳: {entries:?}");
        assert!(entries[1].contains("other"), "bundle 2 自家内容非空: {entries:?}");
        assert_eq!(meta, DossierMeta { bundles: 2, injected: 2 });
    }

    #[test]
    fn second_pass_prompt_injects_needs_evidence_and_drafts() {
        use crate::bundle::Bundle;
        use crate::domain::NeedEntry;
        // fixture：两 bundle 各一文件 + 一个变更集外文件 evidence.rs
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn own() { let z = 1; }\n").unwrap();
        std::fs::write(root.join("src/b.rs"), "fn sib() {}\n").unwrap();
        std::fs::write(root.join("src/evidence.rs"), "pub const LIMIT: usize = 7;\n").unwrap();
        let bundles = vec![
            Bundle { files: vec![FileChange { path: "src/a.rs".into(), diff: "@@ -1,0 +1,1 @@\n+ let z = 1;\n".into() }] },
            Bundle { files: vec![FileChange { path: "src/b.rs".into(), diff: "@@ -1,0 +1,1 @@\n+fn sib() {}\n".into() }] },
        ];
        let needs = vec![NeedEntry { path: "src/evidence.rs".into(), lines: None, reason: "verify LIMIT".into() }];
        let drafts = vec![]; // 空草稿也合法
        let p = second_pass_prompt(&bundles, root, 1_000_000, "t", "", 0, &needs, &drafts, "").unwrap();
        assert!(p.contains("pub const LIMIT"), "needs 文件全文进包: {p}");
        assert!(p.contains("first_pass_draft"), "草稿段在场");
        assert!(p.contains("CONFIRM or REVISE"), "确认或修订语义");
        assert!(!p.contains("<incremental_round>"), "全量路径第二趟无增量段");
        // 增量轮：inc_section 原样带入第二趟（claimed-resolved/dispute 契约不因补跑丢失）
        let inc = crate::prompt::render_incremental_section("abc123def456", "src/a.rs [delta]", &[]);
        let pi = second_pass_prompt(&bundles, root, 1_000_000, "t", "", 0, &needs, &drafts, &inc).unwrap();
        assert!(pi.contains("<incremental_round>") && pi.contains("abc123def456"), "增量段带入第二趟: {pi}");
        assert!(pi.contains("pub const LIMIT"), "增量第二趟 needs 证据仍在场");
        // 路径逃逸：绝对路径 needs 被 skip、不 panic、其余照常
        let evil = vec![NeedEntry { path: "/etc/passwd".into(), lines: None, reason: "x".into() }];
        let p2 = second_pass_prompt(&bundles, root, 1_000_000, "t", "", 0, &evil, &drafts, "");
        if let Some(p2) = p2 { assert!(!p2.contains("root:")); }
    }

    #[test]
    fn second_pass_prompt_lines_window_and_draft_format() {
        use crate::bundle::Bundle;
        use crate::domain::{NeedEntry, Origin};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn own() {}\n").unwrap();
        let body: String = (1..=200).map(|i| format!("line{i}\n")).collect();
        std::fs::write(root.join("src/long.rs"), &body).unwrap();
        std::fs::write(root.join("src/other.rs"), "pub fn other_evidence() {}\n").unwrap();
        let bundles = vec![Bundle { files: vec![FileChange {
            path: "src/a.rs".into(), diff: "@@ -1,0 +1,1 @@\n+fn own() {}\n".into(),
        }] }];
        let needs = vec![
            // 合法 lines → ±30 行窗独立补查段
            NeedEntry { path: "src/long.rs".into(), lines: Some("100-102".into()), reason: "r".into() },
            // 坏格式 lines → 按整文件（前插 bundle 路径，全文进 0 壳）
            NeedEntry { path: "src/other.rs".into(), lines: Some("abc".into()), reason: "r".into() },
        ];
        let drafts = vec![Finding {
            id: "x".into(), severity: Severity::High, title: "weak hash".into(), body: "b".into(),
            file: "src/a.rs".into(), line_start: 5, line_end: 6, confidence: 0.9,
            recommendation: "r".into(), models: vec![], agreement: 1,
            hard_constraint: false, quote: "md5(pw)".into(), cluster_id: None, verify: None,
            previously_dismissed: false, origin: Origin::New,
        }];
        let p = second_pass_prompt(&bundles, root, 1_000_000, "t", "", 0, &needs, &drafts, "").unwrap();
        assert!(p.contains("## 补查证据"), "行窗补查段在场");
        assert!(p.contains("src/long.rs:70-132"), "±30 行窗头: {p}");
        assert!(p.contains("line70") && p.contains("line132"), "窗内行在场");
        assert!(p.contains("pub fn other_evidence"), "坏格式 lines 走整文件路径（全文进 0 壳）");
        assert!(p.contains("- [high] weak hash @ src/a.rs:5（md5(pw)）"), "草稿行格式: {p}");

        // 互斥：同 path 既有整文件条目又有行窗条目 → 全文优先（超集），行窗段不再重复注入
        let both = vec![
            NeedEntry { path: "src/long.rs".into(), lines: Some("100-102".into()), reason: "r".into() },
            NeedEntry { path: "src/long.rs".into(), lines: None, reason: "r".into() },
        ];
        let pb = second_pass_prompt(&bundles, root, 1_000_000, "t", "", 0, &both, &drafts, "").unwrap();
        assert!(pb.contains("src/long.rs:1-200（全文"), "全文条目在场: {pb}");
        assert!(!pb.contains("## 补查证据"), "同 path 行窗段被全文超集互斥掉: {pb}");
    }

    #[test]
    fn second_pass_prompt_caps_huge_lines_window_evidence() {
        // Important 修复回归：单行超长（如压缩/minified 文件）时 ±30 行窗退化成"整行"，
        // 旧代码无字节帽——body 可逼近 read_capped 的 512KB 读帽。新帽 NEED_SECTION_BYTE_CAP
        // 应把它摁在 64KB，且 evidence 扣减后 pack 仍正常渲染（预算纪律不因证据段被撞穿）。
        use crate::bundle::Bundle;
        use crate::domain::NeedEntry;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn own() {}\n").unwrap();
        let huge_line = "x".repeat(100_000); // 无换行——lines() 只有 1 行
        std::fs::write(root.join("src/minified.rs"), &huge_line).unwrap();
        let bundles = vec![Bundle { files: vec![FileChange {
            path: "src/a.rs".into(), diff: "@@ -1,0 +1,1 @@\n+fn own() {}\n".into(),
        }] }];
        let needs = vec![NeedEntry { path: "src/minified.rs".into(), lines: Some("1-1".into()), reason: "r".into() }];
        let p = second_pass_prompt(&bundles, root, 1_000_000, "t", "", 0, &needs, &[], "").unwrap();
        assert!(p.contains("…（截断）"), "证据段超帽应带截断注记: {}", &p[..p.len().min(200)]);
        assert!(p.contains("fn own()"), "证据段截断后 pack 仍照常渲染自家文件");
        assert!(p.len() < 300_000, "总 prompt 长度被证据帽拘住，未逼近原 100KB 单行体量: {}", p.len());
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
        let bundles = vec![Bundle { files: vec![
            FileChange { path: "src/a.rs".into(), diff: "@@ -1,0 +1,1 @@\n+pub fn alpha_marker() {}\n".into() },
            FileChange { path: "src/b.rs".into(), diff: "@@ -1,0 +2,1 @@\n+    let marker_body = 1;\n".into() },
        ] }];
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
        // design §2.2 预算函数：min(256K, (min_ctx − 16K 输出预留)×3 − 24K 壳预留)。
        let cfg = crate::config::Config::from_layers(&[r#"
[providers.Big]
kind = "openai"
model = "big-ctx"
context_window_tokens = 1000000
[providers.Small]
kind = "openai"
model = "small-ctx"
context_window_tokens = 40000
"#]).unwrap();
        // 未声明窗口的模型 → 保守缺省 128K → (128K−16K)*3−24K=312K → 字节帽 256K 生效
        assert_eq!(pack_budget_bytes(&cfg, &["Unknown".into()]), 256_000);
        // 大窗模型同样被字节帽封顶
        assert_eq!(pack_budget_bytes(&cfg, &["Big".into()]), 256_000);
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
        use crate::state::job::{JobState, Phase, write_state, read_state};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_path_buf();
        write_state(&ws, &JobState::new("j", 0)).unwrap();
        let sink = JobCellSink { ws: ws.clone(), job_id: "j".into(), lock: std::sync::Mutex::new(()) };
        use crate::orchestrate::CellSink;
        sink.manifest(&[crate::state::store::CellInput {
            cell: "c".into(), model: "m".into(), prompt: "p".into() }]);
        assert_eq!(read_state(&ws, "j").unwrap().phase, Some(Phase::Fanout));
    }

    #[tokio::test]
    async fn jobcellsink_on_phase_writes_phase() {
        use crate::state::job::{JobState, Phase, write_state, read_state};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_path_buf();
        write_state(&ws, &JobState::new("j", 0)).unwrap();
        let sink = JobCellSink { ws: ws.clone(), job_id: "j".into(), lock: std::sync::Mutex::new(()) };
        use crate::orchestrate::CellSink;
        sink.on_phase(Phase::Finalizing, None);
        assert_eq!(read_state(&ws, "j").unwrap().phase, Some(Phase::Finalizing));
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
}

#[cfg(test)]
mod incremental_tests {
    use super::*;
    use crate::domain::{RawFinding, Severity, Verdict};
    use crate::orchestrate::enrich;
    use crate::state::store::{append_partial, write_args_raw, write_manifest, CellInput, CellRecord};

    pub(super) fn git_repo(dir: &std::path::Path) {
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git").current_dir(dir).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        run(&["config", "commit.gpgsign", "false"]);
        // ponytail: 镜像真实项目的 .redteam/.gitignore（project::init_project）——固件用
        // state::store 写 proj.db 是测试副作用，不 ignore 会被后续 `git add -A` 扫进 delta，
        // 假性膨胀 changed_files_since 的文件数（与被测逻辑无关的噪音）。
        std::fs::write(dir.join(".gitignore"), ".redteam/\n").unwrap();
    }
    pub(super) fn commit_all(dir: &std::path::Path, msg: &str) -> String {
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git").current_dir(dir).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", msg]);
        run(&["rev-parse", "HEAD"]).trim().to_string()
    }

    pub(super) fn raw(file: &str, quote: &str, title: &str) -> RawFinding {
        RawFinding {
            severity: Severity::High, title: title.into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: quote.into(),
        }
    }

    /// 增量 args（branch scope）。
    pub(super) fn inc_args(base: &str, prior: &str) -> ReviewArgs {
        ReviewArgs {
            base: Some(base.into()), scope: "branch".into(),
            spec: vec![], focus: None, models: Some("m".into()),
            models_all: false, block_threshold: None, conf_floor: None,
            background: false, format: "json".into(), resume: None,
            dismissed: None, no_dismissed: false, incremental_from: Some(prior.into()),
            tools: None,
        }
    }

    /// 上轮固件：result(v3, approve, head_sha=Some(sha)) + args(branch) + manifest/partial 全 ok。
    pub(super) fn write_prior(root: &std::path::Path, job: &str, head_sha: &str, findings: Vec<Finding>) {
        let stats = Stats::from_findings(&findings);
        let report = ReviewReport {
            schema_version: "3".into(), verdict: Verdict::Approve,
            stats, findings, clusters: vec![],
            run: RunMeta {
                job_id: job.into(), models: vec!["m".into()],
                head_sha: Some(head_sha.into()), ..Default::default()
            },
        };
        crate::state::store::write_result(root, job, &serde_json::to_string(&report).unwrap()).unwrap();
        let prior_args = ReviewArgs { incremental_from: None, background: true, ..inc_args("dev0", "unused") };
        write_args_raw(root, job, &serde_json::to_string(&prior_args).unwrap()).unwrap();
        write_manifest(root, job, &[CellInput { cell: "bundle[a.rs]@m".into(), model: "m".into(), prompt: "p".into() }]).unwrap();
        append_partial(root, job, &CellRecord::ok("bundle[a.rs]@m", "m", vec![])).unwrap();
    }

    #[test]
    fn preflight_happy_path_yields_baseline() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "fn f() {\n    db.write(token);\n}\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        let ok = std::process::Command::new("git").current_dir(dir.path())
            .args(["branch", "dev0", &c1]).output().unwrap();
        assert!(ok.status.success());
        write_prior(dir.path(), "jprev", &c1, vec![enrich(&raw("a.rs", "db.write(token);", "unvalidated write"), "m")]);
        std::fs::write(dir.path().join("a.rs"), "fn f() {\n    db.write(token);\n    extra();\n}\n").unwrap();
        commit_all(dir.path(), "fix");
        let base = incremental_preflight(&inc_args("dev0", "jprev"), "jprev", dir.path()).unwrap();
        assert_eq!(base.base_sha, c1);
        assert_eq!(base.prior_findings.len(), 1);
        assert_eq!(base.delta.len(), 1, "delta 只含 c1..HEAD 改动");
        assert!(base.renames.is_empty());
    }

    /// 脏树守卫（终审 Important）：即使其余前提全部健康，工作树有未提交 tracked 编辑
    /// 仍必须拦下——carry 重锚读活文件而 delta 只含已提交改动，脏树使锚定不变式的
    /// 前提（评审时点树即 HEAD）失效。独立 fixture：脏改动只留在本测试作用域内，不
    /// commit 也不清理，不污染后续用例。
    #[test]
    fn preflight_rejects_dirty_working_tree() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "fn f() {\n    db.write(token);\n}\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        std::process::Command::new("git").current_dir(dir.path())
            .args(["branch", "dev0", &c1]).output().unwrap();
        write_prior(dir.path(), "jprev", &c1, vec![enrich(&raw("a.rs", "db.write(token);", "unvalidated write"), "m")]);
        std::fs::write(dir.path().join("a.rs"), "fn f() {\n    db.write(token);\n    extra();\n}\n").unwrap();
        commit_all(dir.path(), "fix");
        // 未提交的 tracked 编辑（不 commit）：净树守卫必须先拦下，不放行到载入上轮 result
        std::fs::write(dir.path().join("a.rs"), "fn f() {\n    db.write(token);\n    extra();\n    more();\n}\n").unwrap();
        let e = incremental_preflight(&inc_args("dev0", "jprev"), "jprev", dir.path()).unwrap_err();
        assert!(e.to_string().contains("clean working tree"), "{e}");
    }

    /// 五类守卫拒绝：每类断言错误文案关键词（文案即裸终端在场文档）。
    #[test]
    fn preflight_rejects_unhealthy_and_mismatched_baselines() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        std::process::Command::new("git").current_dir(dir.path()).args(["branch", "dev0", &c1]).output().unwrap();
        write_prior(dir.path(), "jok", &c1, vec![]);
        // ④空 delta（HEAD 未动）
        let e = incremental_preflight(&inc_args("dev0", "jok"), "jok", dir.path()).unwrap_err();
        assert!(e.to_string().contains("empty delta"), "{e}");
        std::fs::write(dir.path().join("a.rs"), "x\ny\n").unwrap();
        commit_all(dir.path(), "c2");
        // 上轮无 result
        let e = incremental_preflight(&inc_args("dev0", "ghost"), "ghost", dir.path()).unwrap_err();
        assert!(e.to_string().contains("no persisted result"), "{e}");
        // ③R2 健康基线：incomplete
        let mut bad: ReviewReport = serde_json::from_str(&crate::state::store::read_result(dir.path(), "jok").unwrap()).unwrap();
        bad.verdict = Verdict::Incomplete;
        crate::state::store::write_result(dir.path(), "jincomplete", &serde_json::to_string(&bad).unwrap()).unwrap();
        let e = incremental_preflight(&inc_args("dev0", "jincomplete"), "jincomplete", dir.path()).unwrap_err();
        assert!(e.to_string().contains("incomplete"), "{e}");
        // ③R2：failed_models 非空
        let mut half: ReviewReport = serde_json::from_str(&crate::state::store::read_result(dir.path(), "jok").unwrap()).unwrap();
        half.verdict = Verdict::Approve;
        half.run.failed_models = vec!["beta".into()];
        crate::state::store::write_result(dir.path(), "jhalf", &serde_json::to_string(&half).unwrap()).unwrap();
        crate::state::store::write_args_raw(dir.path(), "jhalf", &crate::state::store::read_args_raw(dir.path(), "jok").unwrap()).unwrap();
        let e = incremental_preflight(&inc_args("dev0", "jhalf"), "jhalf", dir.path()).unwrap_err();
        assert!(e.to_string().contains("failed models"), "{e}");
        // ③R2：manifest 有 cell 无 ok 记录
        let mut ok2: ReviewReport = serde_json::from_str(&crate::state::store::read_result(dir.path(), "jok").unwrap()).unwrap();
        ok2.run.failed_models.clear();
        crate::state::store::write_result(dir.path(), "jpartial", &serde_json::to_string(&ok2).unwrap()).unwrap();
        crate::state::store::write_args_raw(dir.path(), "jpartial", &crate::state::store::read_args_raw(dir.path(), "jok").unwrap()).unwrap();
        crate::state::store::write_manifest(dir.path(), "jpartial", &[
            crate::state::store::CellInput { cell: "c-never-ran".into(), model: "m".into(), prompt: "p".into() },
        ]).unwrap();
        let e = incremental_preflight(&inc_args("dev0", "jpartial"), "jpartial", dir.path()).unwrap_err();
        assert!(e.to_string().contains("without an ok record"), "{e}");
        // 无 stamp（v2 老报告）
        let mut nostamp: ReviewReport = serde_json::from_str(&crate::state::store::read_result(dir.path(), "jok").unwrap()).unwrap();
        nostamp.run.head_sha = None;
        crate::state::store::write_result(dir.path(), "jnostamp", &serde_json::to_string(&nostamp).unwrap()).unwrap();
        crate::state::store::write_args_raw(dir.path(), "jnostamp", &crate::state::store::read_args_raw(dir.path(), "jok").unwrap()).unwrap();
        crate::state::store::write_manifest(dir.path(), "jnostamp", &[]).unwrap();
        let e = incremental_preflight(&inc_args("dev0", "jnostamp"), "jnostamp", dir.path()).unwrap_err();
        assert!(e.to_string().contains("no head_sha stamp"), "{e}");
    }

    #[test]
    fn preflight_rejects_scope_mode_and_lineage_violations() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        std::process::Command::new("git").current_dir(dir.path()).args(["branch", "dev0", &c1]).output().unwrap();
        write_prior(dir.path(), "jprev", &c1, vec![]);
        std::fs::write(dir.path().join("a.rs"), "x\ny\n").unwrap();
        commit_all(dir.path(), "c2");
        // v1 范围：working-tree scope 拒（不可 stamp）
        let mut wt = inc_args("dev0", "jprev");
        wt.scope = "working-tree".into();
        wt.base = None;
        let e = incremental_preflight(&wt, "jprev", dir.path()).unwrap_err();
        assert!(e.to_string().contains("branch scope"), "{e}");
        // v1 范围：branch scope 但缺 --base → 显式报错（防 branch_files 处 expect panic）
        let mut nobase = inc_args("dev0", "jprev");
        nobase.base = None;
        let e = incremental_preflight(&nobase, "jprev", dir.path()).unwrap_err();
        assert!(e.to_string().contains("requires --base"), "{e}");
        // ②同族：上轮 scope 与本轮不一致
        let mut prior_wt = ReviewArgs { scope: "working-tree".into(), base: None, ..inc_args("dev0", "x") };
        prior_wt.incremental_from = None;
        crate::state::store::write_args_raw(dir.path(), "jprev", &serde_json::to_string(&prior_wt).unwrap()).unwrap();
        let e = incremental_preflight(&inc_args("dev0", "jprev"), "jprev", dir.path()).unwrap_err();
        assert!(e.to_string().contains("scope mismatch"), "{e}");
        // 恢复同族后：①谱系——stamp 指向不在祖先链上的假 sha
        let good_args = ReviewArgs { incremental_from: None, background: true, ..inc_args("dev0", "x") };
        crate::state::store::write_args_raw(dir.path(), "jprev", &serde_json::to_string(&good_args).unwrap()).unwrap();
        let mut rebased: ReviewReport = serde_json::from_str(&crate::state::store::read_result(dir.path(), "jprev").unwrap()).unwrap();
        rebased.run.head_sha = Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into());
        crate::state::store::write_result(dir.path(), "jprev", &serde_json::to_string(&rebased).unwrap()).unwrap();
        let e = incremental_preflight(&inc_args("dev0", "jprev"), "jprev", dir.path()).unwrap_err();
        assert!(e.to_string().contains("not an ancestor"), "{e}");
    }

    use crate::provider::fake::FakeProvider;

    fn inc_cfg() -> Config {
        Config::from_layers(&[r#"[providers.m]
kind="openai"
model="m1"
priority=10
[review]
default_models=["m"]
"#]).unwrap()
    }

    /// 主链路：F1 携带（quote 存活）、F2 消灭（quote 被修掉）、新 finding 入场 → v3 报告全字段。
    #[tokio::test]
    async fn incremental_carries_drops_and_reports_v3() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "fn f() {\n    db.write(token);\n    md5(pw);\n}\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        std::process::Command::new("git").current_dir(dir.path()).args(["branch", "dev0", &c1]).output().unwrap();
        let f1 = enrich(&raw("a.rs", "db.write(token);", "unvalidated write"), "m");
        let f2 = enrich(&raw("a.rs", "md5(pw);", "weak hash"), "m");
        let (id1, id2) = (f1.id.clone(), f2.id.clone());
        write_prior(dir.path(), "jprev", &c1, vec![f1, f2]);
        // fix：修掉 md5 行，引入新问题行
        std::fs::write(dir.path().join("a.rs"), "fn f() {\n    db.write(token);\n    sha256(pw);\n    leak(secret);\n}\n").unwrap();
        commit_all(dir.path(), "fix r1");
        // 本轮模型只报新问题
        let provider = FakeProvider::with("m", vec![raw("a.rs", "leak(secret);", "secret leak")]);
        let (report, code) = run_incremental_review(&inc_args("dev0", "jprev"), "jprev", &provider, &inc_cfg(), dir.path(), None).await.unwrap();
        assert_eq!(report.schema_version, "3");
        assert!(report.run.incremental);
        assert_eq!(report.run.delta_base.as_deref(), Some(c1.as_str()));
        assert!(report.run.head_sha.is_some(), "本轮同样 stamp（下轮的 base）");
        assert_eq!(report.run.carried_dropped, vec![id2], "quote 被修掉 → 消灭进审计");
        let carried = report.findings.iter().find(|f| f.id == id1).expect("F1 应携带");
        assert_eq!(carried.origin, crate::domain::Origin::Carried);
        let fresh = report.findings.iter().find(|f| f.title == "secret leak").expect("新 finding 应入场");
        assert_eq!(fresh.origin, crate::domain::Origin::New);
        let sn = report.stats.new.as_ref().expect("stats.new 子块");
        assert_eq!((sn.total, report.stats.total), (1, 2), "stats 合并集口径 + new 子块");
        assert_eq!(code, 10, "carried High(agreement=1 但携带原样) 或 new High 均 blocking → reject");
    }

    /// 假修复靶（验收 §8.2 机制层）：resolve 了但代码未动 → claimed 再现 → resolution_disputed。
    #[tokio::test]
    async fn fake_fix_yields_resolution_disputed() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "fn f() {\n    md5(pw);\n    other();\n}\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        std::process::Command::new("git").current_dir(dir.path()).args(["branch", "dev0", &c1]).output().unwrap();
        let f1 = enrich(&raw("a.rs", "md5(pw);", "weak hash"), "m");
        let id1 = f1.id.clone();
        write_prior(dir.path(), "jprev", &c1, vec![f1]);
        // 销案（谎称已修）——快照四件套与 finding 同源
        let mut db = crate::db::ProjDb::open(dir.path()).unwrap();
        db.resolve_upsert(&crate::db::ResolveCard {
            id: id1.clone(), title: "weak hash".into(), file: "a.rs".into(),
            quote: "md5(pw);".into(), severity: "high".into(),
            reason: "假装修了".into(), round: "r1".into(), updated_at: String::new(),
        }).unwrap();
        drop(db);
        // fix commit 不触 md5 行
        std::fs::write(dir.path().join("a.rs"), "fn f() {\n    md5(pw);\n    other_fixed();\n}\n").unwrap();
        commit_all(dir.path(), "fake fix");
        // 模型按 claimed 指令原样再报
        let provider = FakeProvider::with("m", vec![raw("a.rs", "md5(pw);", "weak hash")]);
        let (report, _) = run_incremental_review(&inc_args("dev0", "jprev"), "jprev", &provider, &inc_cfg(), dir.path(), None).await.unwrap();
        assert_eq!(report.run.resolved, vec![id1.clone()], "resolve 生效：不携带");
        assert_eq!(report.run.resolution_disputed, vec![id1.clone()], "同 id 再现 → 假修复回路");
        let disputed = report.findings.iter().find(|f| f.id == id1).expect("disputed 项以 new 身份在场");
        assert_eq!(disputed.origin, crate::domain::Origin::New);
        assert!(report.run.carried_dropped.is_empty());
    }

    /// re-raise 去重：模型重报携带项 → 保 carried、agreement 不涨。
    #[tokio::test]
    async fn incremental_re_raise_dedups_into_carried() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "fn f() {\n    db.write(token);\n}\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        std::process::Command::new("git").current_dir(dir.path()).args(["branch", "dev0", &c1]).output().unwrap();
        let mut f1 = enrich(&raw("a.rs", "db.write(token);", "unvalidated write"), "m");
        f1.agreement = 2;
        f1.models = vec!["m".into(), "m2".into()];
        let id1 = f1.id.clone();
        write_prior(dir.path(), "jprev", &c1, vec![f1]);
        std::fs::write(dir.path().join("a.rs"), "// touch\nfn f() {\n    db.write(token);\n}\n").unwrap();
        commit_all(dir.path(), "fix");
        let provider = FakeProvider::with("m", vec![raw("a.rs", "db.write(token);", "unvalidated write")]);
        let (report, _) = run_incremental_review(&inc_args("dev0", "jprev"), "jprev", &provider, &inc_cfg(), dir.path(), None).await.unwrap();
        assert_eq!(report.findings.len(), 1, "同 id 去重");
        assert_eq!(report.findings[0].origin, crate::domain::Origin::Carried);
        assert_eq!(report.findings[0].agreement, 2, "re-raise 不涨 agreement");
        assert_eq!(report.findings[0].id, id1);
        assert_eq!((report.findings[0].line_start, report.findings[0].line_end), (3, 3), "重锚随注释插入移位");
    }

    #[test]
    fn merge_bundles_flattens_all_files_into_one() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let fc = |p: &str| FileChange { path: p.into(), diff: format!("@@ -1,0 +1,1 @@\n+x // {p}\n") };
        let bs = vec![
            Bundle { files: vec![fc("src/a.rs")] },
            Bundle { files: vec![fc("src/b.rs"), fc("src/c.rs")] },
        ];
        let merged = merge_bundles(bs);
        assert_eq!(merged.len(), 1, "单格全审：全部并成一个 bundle");
        assert_eq!(merged[0].files.len(), 3);
    }

    #[test]
    fn bundles_collapse_to_single_cell_by_default() {
        // E9：单格全审默认——group 产多 bundle 后恒并单格（无 env 门）
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let fc = |p: &str| FileChange { path: p.into(), diff: format!("@@ -1,0 +1,1 @@\n+x // {p}\n") };
        let bs = vec![
            Bundle { files: vec![fc("src/a.rs")] },
            Bundle { files: vec![fc("src/b.rs")] },
        ];
        let merged = merge_bundles(bs);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].files.len(), 2);
    }
}
