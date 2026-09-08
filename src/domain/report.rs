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
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Approve => "approve",
            Verdict::Reject => "reject",
            Verdict::Incomplete => "incomplete",
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
    pub fn from_findings(fs: &[Finding]) -> Self {
        let mut s = Stats::default();
        for f in fs {
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
    pub model: String,
    pub ok: bool,
    /// 胜出 call 的 tool-loop 迭代数（单发单收=1；>1=有 repair/多轮）。
    pub iterations: u32,
    /// 候补链实际尝试的 model 数（成功=第几家成功；整链失败=链长）。
    pub attempts: u32,
    pub duration_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
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
    /// v4：本轮 blocking finding 的 refute 率（design §8.1「去留由数据决定」，M21b 填）。None = 无 verifier 段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refute_rate: Option<f32>,
    /// ship 循环的测试命令：`--test-command` 覆盖，否则 `project::infer_test_command` 推断；推不出 None
    /// （M21a 批 A K1，与 `redteam test-command` 同一链）。serde default 兼容旧 JSON。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_command: Option<String>,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::finding::*;
    fn f(sev: Severity) -> Finding {
        Finding { id: "x".into(), severity: sev, title: "t".into(), body: "b".into(),
            file: "a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), models: vec![], agreement: 1,
            hard_constraint: false, quote: "snippet".into(), cluster_id: None, verify: None,
            previously_dismissed: false, origin: Origin::New }
    }
    #[test]
    fn stats_counts_by_severity() {
        let s = Stats::from_findings(&[f(Severity::High), f(Severity::High), f(Severity::Low)]);
        assert_eq!((s.high, s.low, s.total), (2, 1, 3));
    }
    #[test]
    fn verdict_serializes_kebab() {
        assert_eq!(serde_json::to_string(&Verdict::Reject).unwrap(), "\"reject\"");
    }

    #[test]
    fn verdict_incomplete_serializes() {
        assert_eq!(serde_json::to_string(&Verdict::Incomplete).unwrap(), "\"incomplete\"");
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
        for k in ["coverage", "template_hashes", "cache_hit_by_cell", "provider_inflight_peak", "refute_rate"] {
            assert!(!d.contains(k), "默认态不应含 {k}: {d}");
        }
        let mut m = RunMeta { job_id: "j".into(), refute_rate: Some(0.25), ..Default::default() };
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
        assert_eq!(v["template_hashes"]["shared_head"], "deadbeefdeadbeef");
        let back: RunMeta = serde_json::from_str(&j).unwrap();
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
