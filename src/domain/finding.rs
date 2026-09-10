use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Severity { Low, Medium, High, Critical } // 声明顺序即 Ord：Low<Medium<High<Critical

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }
}

/// M13 轮次增量：finding 出身——carried=上轮携带（重锚定成活）、new=本轮 delta 新发。
/// v2 老 JSON 无此键，缺省 New（R6 兼容口径 `(.origin // "new")`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Carried,
    #[default]
    New,
}

/// 模型每次 lens 调用返回的单条 finding（codex 形状 + 可选 hard_constraint）。
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct RawFinding {
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub confidence: f32,
    pub recommendation: String,
    #[serde(default)]
    pub hard_constraint: bool,
    /// 逐字引用出问题的代码原文（positioning 用；prompt 契约要求必填）。
    #[serde(default)]
    pub quote: String,
}

/// needs 条目种类（design §4 / §9.4 L4-B）：evidence=缺证据文件（二阶段补查）、api=缺第三方 callee 契约（M21b 入队）。
/// 旧 JSON 无此键 → 缺省 evidence。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum NeedKind {
    #[default]
    Evidence,
    Api,
}

/// 二阶段补查的证据声明（design 2026-07-13 §2.3，E8 裁决 C）：模型声明「判这个 claim
/// 还缺哪个文件」，非对话式工具调用。lines 可选 "start-end"（超大文件只载区间±上下文）。
/// `Serialize`：随 `CellOutput`（scanner / reviewer 输出）落库。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NeedEntry {
    pub path: String,
    #[serde(default)]
    pub lines: Option<String>,
    pub reason: String,
    #[serde(default)]
    pub kind: NeedKind,
}

/// 模型每次 lens 调用的完整输出。
#[derive(Clone, Debug, Deserialize, JsonSchema, Default)]
pub struct LensOutput {
    pub findings: Vec<RawFinding>,
    /// 证据缺口声明（可选；非空触发二阶段补查趟）。
    #[serde(default)]
    pub needs: Vec<NeedEntry>,
}

/// 本地在 dedup 时 enrich 后的 finding。`PartialEq`：随 `CellOutput` 参与记录相等性断言。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub confidence: f32,
    pub recommendation: String,
    pub models: Vec<String>,
    pub agreement: u32,
    pub hard_constraint: bool,
    /// 逐字引用（positioning 锚定依据，序列化进输出）。
    pub quote: String,
    /// R8.2：命中 --dismissed 清单——verdict 豁免、保留证据（spec §1.2 三件套）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub previously_dismissed: bool,
    /// M13：出身标记（schema v3 显式序列化；反序列化缺省 New）。
    #[serde(default)]
    pub origin: Origin,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<Verify>,
}

fn is_false(b: &bool) -> bool { !*b }

/// targeted-verify（S3-b）的判定：upheld=守住反驳（finding 成立）、refuted=被证伪。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerifyVerdict { Upheld, Refuted }

/// 一条 finding 的 verify 结果（refute pass 产出）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Verify {
    pub verdict: VerifyVerdict,
    /// 执行 refute 的 model 名（交叉验证用哪个）。
    pub model: String,
    /// refute 理由（refuted 时为反驳要点；upheld 时可空）。
    /// `default` 关键：空 note 序列化时被 skip，反序列化（read_partial）必须容忍缺键回落空，
    /// 否则含空-note verify 的 CellRecord 会被 filter_map 静默丢弃（数据丢失）。
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cluster {
    pub id: String,
    pub theme: String,
    pub finding_ids: Vec<String>,
    pub relation: String,
    pub note: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn severity_orders_low_to_critical() {
        assert!(Severity::Low < Severity::Medium);
        assert!(Severity::Medium < Severity::High);
        assert!(Severity::High < Severity::Critical);
    }
    #[test]
    fn severity_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Severity::High).unwrap(), "\"high\"");
    }

    #[test]
    fn raw_finding_ignores_legacy_lens_key() {
        // E8-I：模型仍可能输出 lens 键——serde 静默忽略，不炸不存
        let j = r#"{"severity":"high","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":2,"confidence":0.9,"recommendation":"r","quote":"q","lens":"L1"}"#;
        let r: RawFinding = serde_json::from_str(j).unwrap();
        assert_eq!(r.quote, "q");
    }

    #[test]
    fn raw_finding_quote_defaults_empty_when_absent() {
        let j = r#"{"severity":"low","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r"}"#;
        let r: RawFinding = serde_json::from_str(j).unwrap();
        assert_eq!(r.quote, "");
    }

    #[test]
    fn finding_serializes_quote() {
        let f = Finding {
            id: "x".into(), severity: Severity::High, title: "t".into(), body: "b".into(),
            file: "a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), models: vec![], agreement: 1,
            hard_constraint: false, cluster_id: None, quote: "snippet".into(), verify: None,
            previously_dismissed: false, origin: Origin::New,
        };
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(v["quote"], "snippet");
    }

    #[test]
    fn finding_serializes_verify_when_present_and_skips_when_none() {
        let mut f = Finding {
            id: "x".into(), severity: Severity::High, title: "t".into(), body: "b".into(),
            file: "a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), models: vec![], agreement: 1,
            hard_constraint: false, cluster_id: None, quote: "snippet".into(), verify: None,
            previously_dismissed: false, origin: Origin::New,
        };
        // None → 不出现
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert!(v.get("verify").is_none(), "verify=None must be skipped");
        // Some(Refuted) → 出现，verdict 小写
        f.verify = Some(Verify { verdict: VerifyVerdict::Refuted, model: "gpt".into(), note: "guarded above".into() });
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(v["verify"]["verdict"], "refuted");
        assert_eq!(v["verify"]["model"], "gpt");
        assert_eq!(v["verify"]["note"], "guarded above");
    }

    #[test]
    fn verify_empty_note_roundtrips_after_skip() {
        // 空 note → skip_serializing 跳键；反序列化（read_partial 路径）须 default 回 ""，
        // 否则缺键报错 → 整条记录被 filter_map 静默丢弃（Critical 数据丢失回归防护）。
        let v = Verify { verdict: VerifyVerdict::Upheld, model: "m".into(), note: String::new() };
        let j = serde_json::to_string(&v).unwrap();
        assert!(!j.contains("note"), "空 note 应被 skip: {j}");
        let back: Verify = serde_json::from_str(&j).unwrap();
        assert_eq!(back.note, "", "缺 note 键须 default 回空");
        assert_eq!(back.verdict, VerifyVerdict::Upheld);
    }

    #[test]
    fn origin_defaults_new_and_serializes_lowercase() {
        // v2 老 JSON（无 origin 键）反序列化 → 默认 New（R6：缺省视 new）
        let j = r#"{"id":"x","severity":"high","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.9,"recommendation":"r","models":[],"agreement":1,"hard_constraint":false,"quote":"q"}"#;
        let f: Finding = serde_json::from_str(j).unwrap();
        assert_eq!(f.origin, Origin::New);
        // v3 序列化显式携带（契约字段不靠缺省猜）
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(v["origin"], "new");
        let mut c = f.clone();
        c.origin = Origin::Carried;
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(v["origin"], "carried");
    }
}

/// 测试构造器（M21b 批 B 顺手项，followup 实现债）：verify / prompt / suspicion 三份合一——
/// 新增字段时只改这里，不再逐处补。`#[cfg(test)]` 保证不进生产二进制。
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    /// 标准测试 finding：`id` 兼作 title（三处旧构造的口径）、a.rs:1-1、quote "q"。
    pub(crate) fn finding(id: &str, sev: Severity, agreement: u32, model: &str) -> Finding {
        Finding {
            id: id.into(),
            severity: sev,
            title: id.into(),
            body: "b".into(),
            file: "a.rs".into(),
            line_start: 1,
            line_end: 1,
            confidence: 0.9,
            recommendation: "r".into(),
            models: vec![model.into()],
            agreement,
            hard_constraint: false,
            quote: "q".into(),
            previously_dismissed: false,
            origin: Origin::New,
            cluster_id: None,
            verify: None,
        }
    }

    /// evidence 类 needs 条目（reviewer needs 二阶段 / parse 帽测试共用；B-5 互斥断言起消费）。
    #[allow(dead_code)]
    pub(crate) fn need_evidence(path: &str, lines: &str) -> NeedEntry {
        NeedEntry { path: path.into(), lines: Some(lines.to_string()), reason: "verify".into(), kind: NeedKind::Evidence }
    }
}
