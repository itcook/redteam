//! 疑点 schema 与按角色分叉的 cell 输出（design v8 §4 / §8.4，D11-7 R14）。
//! scanner 不产 finding：产疑点索引（`suspicions`）+ 证据缺口（`needs`）+ 覆盖自报（`scanned_areas`）；
//! reviewer 产 findings；verifier 产判决。一 cell 一条记录、类型上区分角色——杜绝「两个列表同时非空」的糊涂态
//! （M15 翻转当天集成 bug 的温床）。字段级校验（quote 逐字、ranges 形状、confidence 区间）在 parse 层（D-1），本模块只定形状。

use super::finding::{Finding, NeedEntry, VerifyVerdict};
use serde::{Deserialize, Serialize};

/// 角色枚举复用配置层定义（批 A `config::Role`：serde 小写、`parse` / `as_str` / `list_key`），domain 不另定义。
pub use crate::config::Role;

/// 规则家族 L0-L5（design §4：category 取 L0-L5；自标可靠性未验，M21c 看台账数据再议）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Category { L0, L1, L2, L3, L4, L5 }

/// 单条疑点：`quote` 必填——跨模型合并与判例预筛的锚（`quote_overlap` 口径复用）；逐字硬约束在 D-1 parse 校验，不在文件即丢弃。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Suspicion {
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    /// 一句话提示（< 50 token）：哪里、什么味道不对。归一化后置 M21c（R4）。
    pub hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
    /// 模型自评置信 0..1——只进优先级公式，不做门（D11-5）。
    pub confidence: f32,
    pub quote: String,
}

/// 覆盖自报（零调用注意力对账）：`ranges` 形如 `"a-b"`（D-1 校验）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScannedArea {
    pub file: String,
    pub ranges: Vec<String>,
}

/// scanner cell 的完整输出（design §4 输出契约）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanOutput {
    pub suspicions: Vec<Suspicion>,
    #[serde(default)]
    pub needs: Vec<NeedEntry>,
    #[serde(default)]
    pub scanned_areas: Vec<ScannedArea>,
}

/// verifier cell 的单条判决（design §8.1：`REFUTED | UPHELD` + 一句理由；analysis 先于 verdict 是 prompt 字段序，不在此）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VerifyEntry {
    /// 被判决的 finding id。
    pub id: String,
    pub verdict: VerifyVerdict,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// 一 cell 一条、按角色分叉（D11-7）。`role` 作内部 tag：缺失即 serde 报错，不默认——记录类型在类型上说清。
/// tag 值与 [`Role`] 的串一致（`scanner` / `reviewer` / `verifier`），是记录角色的**唯一真相源**：
/// `CellRecord::role()` 与 `cells.role` 列都从 [`CellOutput::role`] 派生（评审裁定 2026-09-08，不另存字段）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role")]
pub enum CellOutput {
    #[serde(rename = "scanner")]
    Scan(ScanOutput),
    #[serde(rename = "reviewer")]
    Review {
        findings: Vec<Finding>,
        #[serde(default)]
        needs: Vec<NeedEntry>,
    },
    #[serde(rename = "verifier")]
    Verify { verdicts: Vec<VerifyEntry> },
}

impl CellOutput {
    pub fn role(&self) -> Role {
        match self {
            CellOutput::Scan(_) => Role::Scanner,
            CellOutput::Review { .. } => Role::Reviewer,
            CellOutput::Verify { .. } => Role::Verifier,
        }
    }

    /// 该角色的空输出（失败格占位：`ok=false` 的记录仍须带类型明确的 output）。
    pub fn empty(role: Role) -> Self {
        match role {
            Role::Scanner => CellOutput::Scan(ScanOutput::default()),
            Role::Reviewer => CellOutput::Review { findings: vec![], needs: vec![] },
            Role::Verifier => CellOutput::Verify { verdicts: vec![] },
        }
    }

    /// reviewer 形态便捷构造（旧 finding 路径 F-4 前沿用；测试用）。
    pub fn review(findings: Vec<Finding>) -> Self {
        CellOutput::Review { findings, needs: vec![] }
    }

    /// 消费点统一访问器：只有 reviewer 输出带 findings，其余为空切片。
    pub fn findings(&self) -> &[Finding] {
        match self {
            CellOutput::Review { findings, .. } => findings,
            _ => &[],
        }
    }

    /// reviewer 输出的 findings 可变视图；非 reviewer 形态为空切片（深挖回填落地时刻疑点共识用）。
    pub fn findings_mut(&mut self) -> &mut [Finding] {
        match self {
            CellOutput::Review { findings, .. } => findings,
            _ => &mut [],
        }
    }

    pub fn into_findings(self) -> Vec<Finding> {
        match self {
            CellOutput::Review { findings, .. } => findings,
            _ => vec![],
        }
    }

    pub fn needs(&self) -> &[NeedEntry] {
        match self {
            CellOutput::Scan(s) => &s.needs,
            CellOutput::Review { needs, .. } => needs,
            CellOutput::Verify { .. } => &[],
        }
    }
}

/// 合并后的疑点（批 F-3，design §5）：同组内跨 cell 以 quote 为锚并集；`agreement` = 家族数（同家族多 samples 计一次）；
/// `suspicion` = 代表（confidence 最高者）；`hints` 全部保留不归一（M21c R4）；`cell_ids` 追溯来源。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MergedSuspicion {
    /// 组索引（0-based，对应报告 `scan.groups[group]`）。
    pub group: usize,
    pub suspicion: Suspicion,
    pub agreement: u32,
    pub families: Vec<String>,
    pub cell_ids: Vec<String>,
    pub hints: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::finding::{NeedKind, Severity};

    fn finding(id: &str) -> Finding {
        crate::domain::finding::fixtures::finding(id, Severity::High, 1, "m")
    }

    fn scan() -> CellOutput {
        CellOutput::Scan(ScanOutput {
            suspicions: vec![Suspicion {
                file: "src/a.rs".into(), line_start: 3, line_end: 5, hint: "unchecked index".into(),
                category: Some(Category::L2), confidence: 0.4, quote: "v[i]".into(),
            }],
            needs: vec![NeedEntry { path: "src/b.rs".into(), lines: Some("1-9".into()), reason: "callee".into(), kind: NeedKind::Api }],
            scanned_areas: vec![ScannedArea { file: "src/a.rs".into(), ranges: vec!["1-40".into()] }],
        })
    }

    /// 三变体 round-trip：序列化 → 反序列化 → 再序列化字节相同，且值相等。
    #[test]
    fn cell_output_three_variants_roundtrip() {
        let review = CellOutput::Review { findings: vec![finding("f1")], needs: vec![] };
        let verify = CellOutput::Verify { verdicts: vec![VerifyEntry { id: "f1".into(), verdict: VerifyVerdict::Refuted, note: "guarded".into() }] };
        for (out, tag) in [(scan(), "scanner"), (review, "reviewer"), (verify, "verifier")] {
            let j = serde_json::to_string(&out).unwrap();
            let v: serde_json::Value = serde_json::from_str(&j).unwrap();
            assert_eq!(v["role"], tag, "tag 值须与 Role 串一致: {j}");
            let back: CellOutput = serde_json::from_str(&j).unwrap();
            assert_eq!(back, out);
            assert_eq!(serde_json::to_string(&back).unwrap(), j, "二次序列化字节稳定");
            assert_eq!(back.role().as_str(), tag);
        }
    }

    #[test]
    fn scan_variant_flattens_fields_next_to_tag() {
        // newtype 变体 + 内部 tag：ScanOutput 字段与 role 平铺在同一对象，不套一层 "Scan"。
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&scan()).unwrap()).unwrap();
        assert_eq!(v["role"], "scanner");
        assert_eq!(v["suspicions"][0]["category"], "L2");
        assert_eq!(v["suspicions"][0]["quote"], "v[i]");
        assert_eq!(v["needs"][0]["kind"], "api");
        assert_eq!(v["scanned_areas"][0]["ranges"][0], "1-40");
        assert!(v.get("Scan").is_none());
    }

    #[test]
    fn missing_role_tag_is_error_not_default() {
        // 无 tag 的旧形状（findings 平铺）与无 tag 的疑点形状都必须报错——记录类型不靠猜。
        for j in [r#"{"findings":[]}"#, r#"{"suspicions":[],"needs":[],"scanned_areas":[]}"#, r#"{"role":"lens","findings":[]}"#] {
            assert!(serde_json::from_str::<CellOutput>(j).is_err(), "应报错: {j}");
        }
    }

    #[test]
    fn category_serializes_l0_to_l5() {
        assert_eq!(serde_json::to_string(&Category::L0).unwrap(), "\"L0\"");
        assert_eq!(serde_json::to_string(&Category::L5).unwrap(), "\"L5\"");
        assert_eq!(serde_json::from_str::<Category>("\"L3\"").unwrap(), Category::L3);
        assert!(serde_json::from_str::<Category>("\"l3\"").is_err());
        // category 可缺（Option）：缺键 → None、None 不序列化
        let s: Suspicion = serde_json::from_str(
            r#"{"file":"a","line_start":1,"line_end":1,"hint":"h","confidence":0.5,"quote":"q"}"#).unwrap();
        assert_eq!(s.category, None);
        assert!(!serde_json::to_string(&s).unwrap().contains("category"));
    }

    #[test]
    fn need_entry_without_kind_defaults_evidence() {
        // 旧 JSON（无 kind 键）仍可读，缺省 evidence；新 JSON 显式携带。
        let n: NeedEntry = serde_json::from_str(r#"{"path":"a.rs","reason":"r"}"#).unwrap();
        assert_eq!(n.kind, NeedKind::Evidence);
        assert_eq!(n.lines, None);
        let j = serde_json::to_string(&n).unwrap();
        assert!(j.contains("\"kind\":\"evidence\""), "{j}");
        assert_eq!(serde_json::from_str::<NeedKind>("\"api\"").unwrap(), NeedKind::Api);
    }

    #[test]
    fn role_tag_matches_config_role_strings() {
        // 复用 config::Role：serde 串、as_str、parse 与 CellOutput tag 三者同一张表。
        for r in Role::ALL {
            assert_eq!(serde_json::to_string(&r).unwrap(), format!("\"{}\"", r.as_str()));
            assert_eq!(Role::parse(r.as_str()), Some(r));
            let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&CellOutput::empty(r)).unwrap()).unwrap();
            assert_eq!(v["role"], r.as_str());
        }
    }

    #[test]
    fn accessors_only_review_has_findings() {
        let review = CellOutput::review(vec![finding("f1")]);
        assert_eq!(review.findings().len(), 1);
        assert!(scan().findings().is_empty());
        assert_eq!(scan().needs().len(), 1);
        assert!(CellOutput::empty(Role::Verifier).findings().is_empty());
        assert_eq!(review.clone().into_findings().len(), 1);
        for r in [Role::Scanner, Role::Reviewer, Role::Verifier] {
            assert_eq!(CellOutput::empty(r).role(), r);
        }
    }

    #[test]
    fn verify_entry_empty_note_skipped_and_defaults() {
        let e = VerifyEntry { id: "x".into(), verdict: VerifyVerdict::Upheld, note: String::new() };
        let j = serde_json::to_string(&e).unwrap();
        assert!(!j.contains("note"), "{j}");
        let back: VerifyEntry = serde_json::from_str(&j).unwrap();
        assert_eq!(back, e);
    }
}
