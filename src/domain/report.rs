use super::finding::{Cluster, Finding, Severity};
use super::suspicion::Role;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 报告 schema 版本（v4 = M21 疑点扫描地基：RunMeta coverage 五集合 / 模板哈希 / cache 命中 / 在途峰值 / refute 率，
/// cell 记录按角色分叉）。所有 `ReviewReport` 构造点引用此常量，不再散写字面量。
pub const SCHEMA_VERSION: &str = "4";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Approve,
    Reject,
    #[serde(rename = "incomplete")]
    Incomplete,
    /// M21a 批 F-3（D10）：scan-only 报告——只有疑点索引与覆盖自报、不产 finding，退出码 0；M21b 深挖落地前 `review` 的终态。
    ScanOnly,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Approve => "approve",
            Verdict::Reject => "reject",
            Verdict::Incomplete => "incomplete",
            Verdict::ScanOnly => "scan-only",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stats {
    pub critical: u32, pub high: u32, pub medium: u32, pub low: u32, pub total: u32,
    /// M13：仅本轮新发（origin=new）的计数子块；主字段维持合并集口径（verdict 依据）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub new: Option<Box<Stats>>,
}

impl Stats {
    /// 被驳回的不计数（批 D〔D17〕Q9）：`verdict` 已经不认它们（confidence 归零、永不 blocking），
    /// 报告头条数字也不该认——两处口径必须一致。记录仍留在 `findings` 数组里（带 `refuted` 标注）供审计与 M21c 台账回流。
    pub fn from_findings(fs: &[Finding]) -> Self {
        let mut s = Stats::default();
        for f in fs.iter().filter(|f| !f.is_refuted()) {
            match f.severity {
                Severity::Critical => s.critical += 1,
                Severity::High => s.high += 1,
                Severity::Medium => s.medium += 1,
                Severity::Low => s.low += 1,
            }
            s.total += 1;
        }
        s
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// 命中缓存读的输入量（input_tokens 子集）；实付全价 ≈ input − cached。跨 provider 归一，
    /// 缺失/未知字段记 0（可观测：cached=0 且 input 大 = 未命中或字段未知）。L1 缓存观测。
    #[serde(default)]
    pub cached_input_tokens: u64,
    /// refute（skeptic 复核）那趟的 token 分账（子集视角，非加法——顶层 input_tokens 已含 fan-out+refute 总量）。
    /// None = 本次无 refute（targets 空）。镜像 stats.new 的 Option<Box<>> idiom，serde default 向后兼容。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refute: Option<Box<Usage>>,
}

/// 二阶段补查触发观测（design §2.3，E8 裁决 E）：triggered_cells=第一趟 needs 非空的格数、
/// declared=第一趟声明的 needs 总条数（cap 前）。恒序列化——零也写（观测面）。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct NeedsMeta {
    pub triggered_cells: u32,
    pub declared: u32,
}

/// 卷宗覆盖观测（T01⑥）：本次评审卷宗是否生效、多少 bundle 拿到内容——A/B 归因与生产诊断用。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DossierMeta {
    pub bundles: usize,       // 本次 fan-out 的 bundle 总数
    pub injected: usize,      // 拿到非空卷宗的 bundle 数（其余=不支持语言/无符号/调用密度门<5）
}

/// coverage 五集合（design §8.4，D11-7）：每集合按 role 分组的 cell 列表（计数 = 长度），五集合不相交、并 = 全集。
/// selected=计划跑；completed=本轮跑成；reused=resume / 增量复用上轮；failed=重试耗尽（无候补）；waived=预算截断 / 规则豁免。
/// 分级：scanner cell 失败 → verdict 硬 Incomplete；reviewer 失败 → 带债 approve；verifier 失败 → 保守 upheld。
/// 本批只定形状（全 default），填值在 F-5（scanner）/ M21b（reviewer / verifier）。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Coverage {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub selected: BTreeMap<Role, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub completed: BTreeMap<Role, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reused: BTreeMap<Role, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub failed: BTreeMap<Role, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub waived: BTreeMap<Role, Vec<String>>,
}

impl Coverage {
    pub fn is_empty(&self) -> bool {
        self.selected.is_empty() && self.completed.is_empty() && self.reused.is_empty()
            && self.failed.is_empty() && self.waived.is_empty()
    }
}

/// 单 cell 的输入缓存命中率（cached_input ÷ input；D10-3 段序翻转与 anthropic 三段界断点的观测面，F-5 填）。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct CacheHit {
    pub cell: String,
    pub ratio: f32,
}

/// 单 cell 运行摘要（诊断呈现：per-model 耗时/轮次/重试/用量）——result 时从 proj.db
/// cells 表回填。旧报告 JSON 无此字段（serde default 空数组），向后兼容。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CellSummary {
    /// cell id（批 F-5：`scan[g<n>]@<provider/id>#<k>`）；旧 JSON 无此字段 → 空。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cell: String,
    pub model: String,
    /// 角色串（`scanner` / `reviewer` / `verifier`），批 F-5 填；旧 JSON 空。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    pub ok: bool,
    /// 胜出 call 的 tool-loop 迭代数（单发单收=1；>1=有 repair/多轮）。
    pub iterations: u32,
    /// 候补链实际尝试的 model 数（成功=第几家成功；整链失败=链长）。
    pub attempts: u32,
    pub duration_ms: u64,
    /// 排队等待毫秒（含在 duration_ms 内；M21a 出口验收）；旧 JSON 0。
    #[serde(default)]
    pub queue_wait_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// 命中缓存读的输入量（批 F-5，cache 命中率归因）。
    #[serde(default)]
    pub cached_input_tokens: u64,
}

impl CellSummary {
    /// 从落库记录派生摘要（前台收口与 `attach_cells` 共用一处映射，评审帽外项）。
    pub fn from_record(r: &crate::state::CellRecord) -> Self {
        CellSummary {
            cell: r.cell.clone(),
            model: r.model.clone(),
            role: r.role().as_str().to_string(),
            ok: r.ok,
            iterations: r.iterations,
            attempts: r.attempts,
            duration_ms: r.duration_ms,
            queue_wait_ms: r.queue_wait_ms,
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
            cached_input_tokens: r.cached_input_tokens,
        }
    }
}

/// scan-only 报告的组摘要（批 F-3）：组标签 / 文件 / cell 列表 / 合并后疑点数 / 装包形态（批 E `PackStats`）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanGroup {
    pub label: String,
    pub files: Vec<String>,
    pub cells: Vec<String>,
    pub suspicions: u32,
    #[serde(default)]
    pub pack: crate::pack::PackStats,
}

/// scan-only 统计（批 F-3）：合并后总数、cell 成败、agreement ≥ 2 的条数、按类别、needs 声明数。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanStats {
    pub total: u32,
    pub cells_ok: u32,
    pub cells_failed: u32,
    pub agreement_ge2: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_category: BTreeMap<String, u32>,
    pub needs_declared: u32,
}

/// scan-only 报告段（batch F-3，design §4 / §5）：按组的疑点索引 + 覆盖自报（`scanned_areas` ∪ ∩ 改动行 ÷ 改动行）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanReport {
    pub groups: Vec<ScanGroup>,
    pub suspicions: Vec<super::suspicion::MergedSuspicion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scanned_coverage: Option<f32>,
    pub stats: ScanStats,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RunMeta {
    pub job_id: String,
    pub models: Vec<String>,
    /// 与 `models`（provider 名）同序的 model id（config 的 providers.*.model）——
    /// 人读报告显示 model 名而非 provider 名（rN.md 模板,2026-07-03）。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub model_ids: Vec<String>,
    pub duration_ms: u64,
    pub usage: Usage,
    pub warnings: Vec<String>,
    pub failed_models: Vec<String>,
    /// R8.2 派生索引：本次命中 dismissed 的 finding id（权威源是 findings.previously_dismissed）。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub dismissed_reappeared: Vec<String>,
    /// 台账自动加载披露——zero-config 下 verdict 受台账影响必须可见（design 2026-07-03 #6）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ledger: Option<LedgerMeta>,
    /// R3 stamp：证据收集实跑刻的 rev-parse HEAD（code 模式全路径回填；doc 为 None）。
    /// 增量轮以上轮此值为 delta base。serde default 兼容 v2。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub head_sha: Option<String>,
    /// M13：本次是否增量轮（--incremental-from 路径）。
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub incremental: bool,
    /// 增量轮 delta 基线（上轮 stamped HEAD）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub delta_base: Option<String>,
    /// 命中 resolve 台账被排除携带的 id（销案生效审计）。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub resolved: Vec<String>,
    /// 假修复回路：claimed-resolved 但同 id 再现的 finding id（ship 按修复未生效重开）。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub resolution_disputed: Vec<String>,
    /// 重锚定失败被消灭的携带候选 id（使锚失效的改动必在本轮 delta 内，审计用）。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub carried_dropped: Vec<String>,
    /// 卷宗覆盖（code 模式且卷宗路径才有；doc/resume 为 None）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dossier: Option<DossierMeta>,
    /// 二阶段补查触发率（E8-E 指标）：恒写、零也写；serde default 兼容旧 JSON。
    #[serde(default)]
    pub needs: NeedsMeta,
    /// per-cell 运行摘要（诊断呈现）。旧 JSON 无此字段，反序列化回空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cells: Vec<CellSummary>,
    /// v4：coverage 五集合（design §8.4）。空 = 未填（本批）/ 旧 JSON；不序列化。
    #[serde(default, skip_serializing_if = "Coverage::is_empty")]
    pub coverage: Coverage,
    /// v4：本次渲染所用模板哈希（`shared_head` / `scanner` / … → fnv1a64 hex，D-4 填）；改模板必跑回归的归因键。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub template_hashes: BTreeMap<String, String>,
    /// v4：每 cell 输入缓存命中率（F-5 填）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_hit_by_cell: Vec<CacheHit>,
    /// v4：每 provider 在途请求峰值（provider 在途锁观测，F-2 / F-5 填）。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_inflight_peak: BTreeMap<String, u32>,
    /// v4（D13）：每 provider 的 429 收敛记录——只含本轮撞过 429 的 provider；空不序列化（旧消费者零噪音）。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_rate_limit: BTreeMap<String, RateLimitMeta>,
    /// v4：本轮复核的驳回率 = `verify_refuted / (verify_triggered − verify_failed)`（design §8.1「去留由数据决定」）。
    /// 分母只含**真跑成了的**条数：被跳过的（区域共识 / carried / 判例 / 开关关）从没给过 verifier 机会，
    /// 没跑成的（provider 挂了 / 判决读不懂）也没给出过证据——两类进分母都只会冲淡率。
    /// None = 本轮没有任何一条真跑成的复核（不拿 0 冒充「验了没驳回」）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refute_rate: Option<f32>,
    /// ship 循环的测试命令：`--test-command` 覆盖，否则 `project::infer_test_command` 推断；推不出 None
    /// （M21a 批 A K1，与 `redteam test-command` 同一链）。serde default 兼容旧 JSON。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_command: Option<String>,
    /// 批 F-5：引用图分组观测（批 C `GroupingStats`，此前只在 `--pack-only` 可见）；旧 JSON / 无分组路径 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouping: Option<crate::bundle::GroupingStats>,
    /// M21b 批 A：阶段 0 风险路由（H0 区数 / R / 档位 / 预算格）；旧 JSON None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<RiskMeta>,
    /// M21b 批 C：深挖观测；无 reviewer / 旧 JSON None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deep: Option<DeepMeta>,
}

/// 深挖观测（design §9）。旧报告不产键。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeepMeta {
    pub queue_depth_peak: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub enqueue_reasons: BTreeMap<String, u32>,
    pub dedup_hits: u32,
    pub budget_skipped: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub item_wait_ms: Vec<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dispatch_agreement: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub landed_agreement: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviewer_output_tokens: Vec<u64>,
    pub needs_two_stage_triggered: u32,
    pub needs_two_stage_skipped: u32,
    pub stream_discarded: u32,
    // ── C2 时间重叠观测（design v11 §6 / §12.3〔D15〕；为零不序列化，旧报告形状不变）──
    /// 收场（关闭入口）时刻，自深挖开跑起毫秒；与扫描同时开跑时即 scanner 段墙钟。resume 与开跑即关闭（无入队通道）时 0。
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub close_ms: u64,
    /// 首次派发时刻（无派发 0；有派发至少记 1，H0 在 t = 0 开挖即 1）。
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub first_dispatch_ms: u64,
    /// 最后一格落地时刻（队列排空；无格 0、有格至少 1）。
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub drain_done_ms: u64,
    /// 复核尾部（批 D 前恒 0）。
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub verify_tail_ms: u64,
    // ── 批 D-3 复核观测（design §8.1〔D17〕）：判决在深挖循环里产生，故与 verify_tail_ms 同层；为零不序列化 ──
    /// 进了复核的 finding 数（触发且未被跳过）。
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub verify_triggered: u32,
    /// 跳过分布：`not_triggered` / `disabled` / `carried` / `precedent` / `consensus`
    /// ——「没触发」与「触发了被免验」必须分得开，否则「复核为什么没跑」无从归因。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub verify_skipped: BTreeMap<String, u32>,
    /// 被驳回条数（`Stats` 与 `verdict` 剔除的正是这批）。注意它数的是**本轮 verifier 给出的驳回判决**，
    /// 不等于「报告里少了几条」：dedup 可能把一条 REFUTED 与一条 UPHELD 并成 upheld（UPHELD 优先），
    /// resume 沿用往轮判决时也不进这个计数（记 `verify_skipped["resumed"]`）。
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub verify_refuted: u32,
    /// 派出去了但**没跑成**的条数（lease 失败 / 调用失败 / 判决读不懂）：判决按保守 upheld 收，
    /// 但它们不是证据——`refute_rate` 的分母要减掉它们，否则「verifier 全挂」会读成「验了 N 条一条没驳」。
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub verify_failed: u32,
    /// 收场前派出的格数（扫描期额度 K − F 下，非必挖项受限）。
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub scan_phase_dispatched: u32,
    /// 收场时剩余预算 `K − 已派发`（收场 drain 的预算参数）。
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub close_budget_remaining: u32,
    /// 组内闭包簇被队列逐条合并拆进多项的簇数（非传递残差，收场对账）。
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub queue_split_clusters: u32,
    /// 主循环同步段（填门：pack 构造 + prompt + 快照；到达处理）单次最长 / 累计阻塞毫秒——同 task 并发下它们会让其余在途格停摆。
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub fill_block_max_ms: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub fill_block_total_ms: u64,
    // ── C3 选项观测（design C3 §7〔D16〕；为零不序列化。逐格数组不加——可由 `DeepRun.items` + scan 报告离线复算）──
    /// 经通道 / 回放到达并被接受的疑点数（到达键重复的 `Duplicate` 不计）。
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub suspicions_arrived: u32,
    /// 收场时 `kind == Suspicion` 的项数；÷ `suspicions_arrived` = 结构化合并的压缩比。
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub items_from_suspicions: u32,
    /// 队列项覆盖的文件数（不分状态）。
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub files_candidate: u32,
    /// 派发过的项（InFlight ∪ Settled）覆盖的文件数。
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub files_selected: u32,
    /// 同一文件派发格数的最大值——文件多样性的辅助门槛直接读它。
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub max_cells_per_file: u32,
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

/// 阶段 0 风险观测（serde default 兼容旧报告）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RiskMeta {
    pub hotspots: u32,
    pub r: f32,
    pub tier: crate::risk::Tier,
    pub budget_cells: u32,
}

/// v4（D13）：某 provider 本轮撞 429 的收敛记录——`hits` 次数、`cap` 配置门宽、`effective_cap` 收敛后门宽（design §12.3）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimitMeta {
    pub hits: u32,
    pub cap: u32,
    pub effective_cap: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerMeta {
    /// 加载的活跃判例卡数（withdrawn/stale 不计）。
    pub loaded: usize,
    /// 来源描述：db 路径或 "csv"。
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewReport {
    pub schema_version: String,
    pub verdict: Verdict,
    pub stats: Stats,
    pub findings: Vec<Finding>,
    pub clusters: Vec<Cluster>,
    pub run: RunMeta,
    /// 批 F-3：scan-only 段（疑点索引 + 覆盖自报）；finding 路径 / 旧 JSON 为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan: Option<ScanReport>,
    /// 批 F-3 / F-5：全部 cell 记录（含 `output`，tag `role`）——`eval/scan_score.py` 读顶层 `cells[].output.suspicions`
    /// （followup「结果 JSON 加 scanner 输出」）；`run.cells` 保留摘要。旧 JSON 为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cells: Vec<crate::state::CellRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::finding::*;
    fn f(sev: Severity) -> Finding {
        Finding { id: "x".into(), severity: sev, title: "t".into(), body: "b".into(),
            file: "a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), models: vec![], agreement: 1, suspicion_agreement: 0,
            hard_constraint: false, quote: "snippet".into(), cluster_id: None, verify: None,
            previously_dismissed: false, origin: Origin::New }
    }
    #[test]
    fn stats_counts_by_severity() {
        let s = Stats::from_findings(&[f(Severity::High), f(Severity::High), f(Severity::Low)]);
        assert_eq!((s.high, s.low, s.total), (2, 1, 3));
    }
    /// 被驳回的不计数（批 D〔D17〕Q9）：`verdict` 已经不认它们，报告头条数字也不该认——
    /// 两处口径不一致时，用户会看到「2 个 high」却拿到 Approve。记录仍在 `findings` 数组里。
    #[test]
    fn stats_exclude_refuted_findings() {
        let verdict = |v: VerifyVerdict| Some(Verify { verdict: v, model: "p/v".into(), note: String::new() });
        let mut refuted = f(Severity::High);
        refuted.verify = verdict(VerifyVerdict::Refuted);
        let mut upheld = f(Severity::High);
        upheld.verify = verdict(VerifyVerdict::Upheld);
        let s = Stats::from_findings(&[refuted, upheld, f(Severity::Low)]);
        assert_eq!((s.high, s.low, s.total), (1, 1, 2), "驳回的那条既不计 high 也不计 total");
    }
    /// C2 观测字段为零不产键、旧报告（无这些键）照读（design C2 §12 兼容）。
    #[test]
    fn deep_meta_c2_fields_skip_zero_and_default_on_old_json() {
        let old = r#"{"queue_depth_peak":3,"dedup_hits":0,"budget_skipped":1,"needs_two_stage_triggered":0,"needs_two_stage_skipped":0,"stream_discarded":0}"#;
        let m: DeepMeta = serde_json::from_str(old).unwrap();
        assert_eq!((m.queue_depth_peak, m.budget_skipped, m.close_ms, m.scan_phase_dispatched), (3, 1, 0, 0));
        assert_eq!(serde_json::to_string(&m).unwrap(), old, "全零新字段不改旧形状");
        let m = DeepMeta { close_ms: 400, scan_phase_dispatched: 6, fill_block_max_ms: 2, ..m };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!((v["close_ms"].as_u64(), v["scan_phase_dispatched"].as_u64(), v["fill_block_max_ms"].as_u64()), (Some(400), Some(6), Some(2)));
        assert!(v.get("verify_tail_ms").is_none() && v.get("queue_split_clusters").is_none());
    }

    /// C3 选项观测同规矩（design C3 §7〔D16〕）：旧报告缺键照读、全零不产键、非零才出现。
    #[test]
    fn deep_meta_c3_selection_fields_default_and_skip_zero() {
        let old = r#"{"queue_depth_peak":3,"dedup_hits":0,"budget_skipped":1,"needs_two_stage_triggered":0,"needs_two_stage_skipped":0,"stream_discarded":0}"#;
        let m: DeepMeta = serde_json::from_str(old).unwrap();
        assert_eq!((m.suspicions_arrived, m.items_from_suspicions, m.files_candidate, m.files_selected, m.max_cells_per_file), (0, 0, 0, 0, 0));
        assert_eq!(serde_json::to_string(&m).unwrap(), old, "全零新字段不改旧形状");
        let v = serde_json::to_value(DeepMeta { suspicions_arrived: 40, files_selected: 5, max_cells_per_file: 4, ..m }).unwrap();
        assert_eq!(
            (v["suspicions_arrived"].as_u64(), v["files_selected"].as_u64(), v["max_cells_per_file"].as_u64()),
            (Some(40), Some(5), Some(4))
        );
        assert!(v.get("files_candidate").is_none() && v.get("items_from_suspicions").is_none());
    }

    #[test]
    fn verdict_serializes_kebab() {
        assert_eq!(serde_json::to_string(&Verdict::Reject).unwrap(), "\"reject\"");
    }

    #[test]
    fn verdict_incomplete_serializes() {
        assert_eq!(serde_json::to_string(&Verdict::Incomplete).unwrap(), "\"incomplete\"");
    }

    /// 批 F-3：scan-only verdict 串；ScanReport 与顶层 cells round-trip，`cells[].output.role == "scanner"`（评分器形态）；
    /// 默认态不产 scan / cells 键。
    #[test]
    fn scan_only_verdict_and_scan_report_roundtrip() {
        assert_eq!(serde_json::to_string(&Verdict::ScanOnly).unwrap(), "\"scan-only\"");
        assert_eq!(serde_json::from_str::<Verdict>("\"scan-only\"").unwrap(), Verdict::ScanOnly);
        assert_eq!(Verdict::ScanOnly.as_str(), "scan-only");
        let susp = crate::domain::Suspicion { file: "a.rs".into(), line_start: 3, line_end: 3, hint: "h".into(), category: Some(crate::domain::Category::L1), confidence: 0.7, quote: "q".into() };
        let out = crate::domain::ScanOutput { suspicions: vec![susp.clone()], ..Default::default() };
        let rec = crate::state::CellRecord::ok("scan[g1]@p/m#1", "p/m", crate::domain::CellOutput::Scan(out)).with_family("fam");
        let scan = ScanReport {
            groups: vec![ScanGroup { label: "a.rs".into(), files: vec!["a.rs".into()], cells: vec!["scan[g1]@p/m#1".into()], suspicions: 1, pack: crate::pack::PackStats { body: "slice".into(), ..Default::default() } }],
            suspicions: vec![crate::domain::MergedSuspicion { group: 0, suspicion: susp, agreement: 1, families: vec!["fam".into()], cell_ids: vec!["scan[g1]@p/m#1".into()], hints: vec!["h".into()] }],
            scanned_coverage: Some(0.5),
            stats: ScanStats { total: 1, cells_ok: 1, ..Default::default() },
        };
        let r = ReviewReport { schema_version: SCHEMA_VERSION.into(), verdict: Verdict::ScanOnly, stats: Stats::default(), findings: vec![], clusters: vec![], run: RunMeta { grouping: Some(crate::bundle::GroupingStats { gate: crate::bundle::Gate::Grouped, groups: 1, edges: 0, cross_edges: 0, cross_import: Some(0), cross_reference: Some(0), cross_same_dir: Some(0), pooled_small_components: Some(0), isolated: 0, over_budget: 0 }), ..Default::default() }, scan: Some(scan.clone()), cells: vec![rec] };
        let j = serde_json::to_string(&r).unwrap();
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["verdict"], "scan-only");
        assert_eq!(v["cells"][0]["output"]["role"], "scanner");
        assert_eq!(v["cells"][0]["output"]["suspicions"][0]["quote"], "q");
        assert_eq!(v["scan"]["groups"][0]["pack"]["body"], "slice");
        assert_eq!(v["run"]["grouping"]["gate"], "grouped");
        let back: ReviewReport = serde_json::from_str(&j).unwrap();
        assert_eq!(back.scan.unwrap(), scan);
        assert_eq!(back.cells.len(), 1);
        assert_eq!(back.run.grouping.unwrap().groups, 1);
        let d = serde_json::to_string(&ReviewReport { schema_version: "4".into(), verdict: Verdict::Approve, stats: Stats::default(), findings: vec![], clusters: vec![], run: RunMeta::default(), scan: None, cells: vec![] }).unwrap();
        assert!(!d.contains("\"scan\"") && !d.contains("\"cells\"") && !d.contains("grouping"), "默认态不产键: {d}");
    }

    #[test]
    fn report_v2_json_deserializes_with_defaults() {
        // v2 真实形状：无 head_sha/incremental/聚合数组/stats.new/dismissed_reappeared
        let j = r#"{"schema_version":"2","mode":"code","verdict":"approve",
            "stats":{"critical":0,"high":0,"medium":0,"low":0,"total":0},
            "findings":[],"clusters":[],
            "run":{"job_id":"j1","models":["m"],"lenses":["L1"],"duration_ms":1,
                   "usage":{"input_tokens":1,"output_tokens":1},
                   "warnings":[],"failed_models":[]}}"#;
        let r: ReviewReport = serde_json::from_str(j).unwrap();
        assert_eq!(r.verdict, Verdict::Approve);
        assert_eq!(r.run.head_sha, None);
        assert!(!r.run.incremental);
        assert!(r.run.resolved.is_empty() && r.run.carried_dropped.is_empty());
        assert!(r.stats.new.is_none());
    }

    #[test]
    fn usage_refute_roundtrips_and_old_json_defaults_to_none() {
        // T03：Usage.refute 有值 → 序列化带 refute 键；无 refute（旧 JSON）→ 反序列化为 None。
        let u = Usage {
            input_tokens: 100, output_tokens: 20, cached_input_tokens: 5,
            refute: Some(Box::new(Usage { input_tokens: 30, output_tokens: 6, cached_input_tokens: 1, refute: None })),
        };
        let j = serde_json::to_string(&u).unwrap();
        assert!(j.contains("\"refute\":{"), "refute 子块应出现在序列化输出: {j}");
        let back: Usage = serde_json::from_str(&j).unwrap();
        assert_eq!(back.refute.unwrap().input_tokens, 30);

        // 旧结果（无 refute 键）→ 向后兼容，反序列化为 None
        let old = r#"{"input_tokens":1,"output_tokens":1}"#;
        let r: Usage = serde_json::from_str(old).unwrap();
        assert!(r.refute.is_none());
        assert!(!serde_json::to_string(&Usage::default()).unwrap().contains("refute"), "默认态不产 refute 键");
    }

    #[test]
    fn runmeta_v3_fields_roundtrip_and_skip_when_default() {
        let mut m = RunMeta { head_sha: Some("abc".into()), incremental: true,
            delta_base: Some("def".into()), resolved: vec!["1".into()],
            resolution_disputed: vec!["2".into()], carried_dropped: vec!["3".into()],
            ..Default::default() };
        m.job_id = "j".into();
        let j = serde_json::to_string(&m).unwrap();
        assert!(j.contains("\"head_sha\":\"abc\"") && j.contains("\"incremental\":true"));
        // 默认态（非增量）不产键——v2 消费者视角零噪音
        let d = serde_json::to_string(&RunMeta::default()).unwrap();
        for k in ["head_sha", "incremental", "delta_base", "resolved", "resolution_disputed", "carried_dropped"] {
            assert!(!d.contains(k), "默认态不应含 {k}: {d}");
        }
    }

    /// v4 五字段：默认态不产键（v2/v3 消费者零噪音）；赋值 round-trip；旧 v2/v3 JSON 可读且回默认。
    #[test]
    fn runmeta_v4_fields_roundtrip_and_skip_when_default() {
        let d = serde_json::to_string(&RunMeta::default()).unwrap();
        for k in ["coverage", "template_hashes", "cache_hit_by_cell", "provider_inflight_peak", "provider_rate_limit", "refute_rate"] {
            assert!(!d.contains(k), "默认态不应含 {k}: {d}");
        }
        let mut m = RunMeta { job_id: "j".into(), refute_rate: Some(0.25), ..Default::default() };
        m.provider_rate_limit.insert("agc".into(), RateLimitMeta { hits: 2, cap: 20, effective_cap: 5 });
        m.coverage.selected.insert(Role::Scanner, vec!["scan[g1]@x#1".into(), "scan[g2]@x#1".into()]);
        m.coverage.failed.insert(Role::Scanner, vec!["scan[g2]@x#1".into()]);
        m.template_hashes.insert("shared_head".into(), "deadbeefdeadbeef".into());
        m.cache_hit_by_cell.push(CacheHit { cell: "scan[g1]@x#1".into(), ratio: 0.5 });
        m.provider_inflight_peak.insert("agc".into(), 3);
        let j = serde_json::to_string(&m).unwrap();
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["coverage"]["selected"]["scanner"].as_array().unwrap().len(), 2, "role 作字符串键: {j}");
        assert!(v["coverage"].get("completed").is_none(), "空集合不序列化: {j}");
        assert_eq!(v["cache_hit_by_cell"][0]["cell"], "scan[g1]@x#1");
        assert_eq!(v["provider_inflight_peak"]["agc"], 3);
        assert_eq!((v["provider_rate_limit"]["agc"]["hits"].as_u64(), v["provider_rate_limit"]["agc"]["effective_cap"].as_u64()), (Some(2), Some(5)), "{j}");
        assert_eq!(v["template_hashes"]["shared_head"], "deadbeefdeadbeef");
        let back: RunMeta = serde_json::from_str(&j).unwrap();
        assert_eq!(back.provider_rate_limit, m.provider_rate_limit);
        assert_eq!(back.coverage, m.coverage);
        assert_eq!(back.cache_hit_by_cell, m.cache_hit_by_cell);
        assert_eq!(back.refute_rate, Some(0.25));
        assert_eq!(back.provider_inflight_peak, m.provider_inflight_peak);
        // 旧 v2 形状（无任何 v4 键）→ 全默认
        let old = r#"{"job_id":"j1","models":["m"],"lenses":["L1"],"duration_ms":1,
                   "usage":{"input_tokens":1,"output_tokens":1},"warnings":[],"failed_models":[]}"#;
        let r: RunMeta = serde_json::from_str(old).unwrap();
        assert!(r.coverage.is_empty() && r.template_hashes.is_empty() && r.cache_hit_by_cell.is_empty());
        assert!(r.provider_inflight_peak.is_empty() && r.refute_rate.is_none());
    }

    #[test]
    fn runmeta_dossier_roundtrips_and_old_json_defaults_to_none() {
        // T01⑥：dossier 有值 → 序列化带 dossier 键；旧 JSON（无 dossier 键）→ 反序列化为 None。
        let mut m = RunMeta {
            dossier: Some(DossierMeta { bundles: 3, injected: 1 }),
            ..Default::default()
        };
        m.job_id = "j".into();
        let j = serde_json::to_string(&m).unwrap();
        assert!(j.contains("\"dossier\":{\"bundles\":3,\"injected\":1}"), "{j}");

        // 默认态（无 dossier）不产键——v2/v3（无卷宗字段）消费者视角零噪音
        let d = serde_json::to_string(&RunMeta::default()).unwrap();
        assert!(!d.contains("dossier"), "默认态不应含 dossier: {d}");

        // 旧 JSON（无 dossier 键）→ 反序列化为 None（doc/resume 路径同样兼容）
        let old = r#"{"job_id":"j1","models":["m"],"lenses":["L1"],"duration_ms":1,
                   "usage":{"input_tokens":1,"output_tokens":1},
                   "warnings":[],"failed_models":[]}"#;
        let back: RunMeta = serde_json::from_str(old).unwrap();
        assert!(back.dossier.is_none());
    }
}
