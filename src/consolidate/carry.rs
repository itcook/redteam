//! M13 携带引擎（决议 3/5）：上轮合并集 → rename 迁移 → resolved 排除 → 重锚定携带
//! → 与本轮 new 合并去重。纯函数，git/LLM 均不在此层。

use crate::db::ResolveCard;
use crate::domain::{Finding, Origin};
use crate::orchestrate::finding_id;
use std::collections::HashSet;
use std::path::Path;

pub struct CarryOutcome {
    /// 重锚成活的携带项（行号已更新、origin=Carried；severity/agreement/verify/
    /// dismissed 状态原样——refuted/dismissed 本就不 blocking，携带无害）。
    pub carried: Vec<Finding>,
    /// 重锚失败被消灭的 id（使锚失效的改动必在本轮 delta 内，正被审；审计用）。
    pub carried_dropped: Vec<String>,
    /// 命中 resolve 台账被排除携带的 id（销案生效审计）。
    pub resolved_applied: Vec<String>,
}

/// R1 rename 迁移：finding.file 命中旧路径 → 改新路径并按 finding_id 单一真相源重算 id。
/// 不迁则「旧路径读不到 → carry 被杀 + 新 id 不碰撞」——真问题静默蒸发（评审 R1 案）。
pub fn migrate_renames(findings: &mut [Finding], renames: &[(String, String)]) {
    if renames.is_empty() {
        return;
    }
    for f in findings.iter_mut() {
        if let Some((_, new)) = renames.iter().find(|(old, _)| *old == f.file) {
            f.file = new.clone();
            f.id = finding_id(&f.file, &f.quote, &f.title, f.line_start, f.line_end);
        }
    }
}

/// 同规则迁移 resolve 卡——碰撞域（resolved 排除 / resolution_disputed 检测）与
/// findings 必须同一 id 空间。空 quote 卡不重算（无 quote 的 id 含行号，快照行号在
/// 新树无意义——保守保留旧 id；code 模式 quote 是必填契约，实际不会出现）。
pub fn migrate_resolve_cards(cards: &mut [ResolveCard], renames: &[(String, String)]) {
    if renames.is_empty() {
        return;
    }
    for c in cards.iter_mut() {
        if let Some((_, new)) = renames.iter().find(|(old, _)| *old == c.file) {
            c.file = new.clone();
            if !c.quote.trim().is_empty() {
                c.id = finding_id(&c.file, &c.quote, &c.title, 0, 0);
            }
        }
    }
}

/// 决议 3 携带主流程（调用方须先做 rename 迁移）：resolved 排除 → anchor_near 重锚
/// （成活更新行号 + origin=Carried；失败进 carried_dropped）。输出 id 列表排序去重（确定性）。
pub fn carry(prior: Vec<Finding>, resolved_ids: &HashSet<String>, root: &Path) -> CarryOutcome {
    let mut out = CarryOutcome { carried: vec![], carried_dropped: vec![], resolved_applied: vec![] };
    for mut f in prior {
        if resolved_ids.contains(&f.id) {
            out.resolved_applied.push(f.id);
            continue;
        }
        match crate::positioning::anchor_near(root, &f.file, &f.quote, f.line_start) {
            Some((ls, le)) => {
                f.line_start = ls;
                f.line_end = le;
                f.origin = Origin::Carried;
                out.carried.push(f);
            }
            None => out.carried_dropped.push(f.id),
        }
    }
    for v in [&mut out.carried_dropped, &mut out.resolved_applied] {
        v.sort();
        v.dedup();
    }
    out
}

/// 决议 5 合并去重：同 id（或相似回落，D4）保 carried、丢 new 副本、models 并集、**agreement
/// 不变**（跨轮重看非独立证据；顽固性由「仍在携带集」这一事实表达）。相似回落=quote 漂移
/// 导致 id 不同但实为同一 bug 的补丁（is_same_bug 单一真相源，与 dedup 主循环同阈值），
/// 仅对 out 中 origin==Carried 的项生效——new-vs-new 相似命中不在此处理（上游 dedup 已处理）。
/// new 独有项原样进合并集。
pub fn merge_carry(carried: Vec<Finding>, new: Vec<Finding>) -> Vec<Finding> {
    let mut out = carried;
    for n in new {
        let hit = out.iter_mut().find(|c| {
            c.id == n.id
                || (c.origin == Origin::Carried && crate::consolidate::dedup::is_same_bug(c, &n))
        });
        if let Some(c) = hit {
            for m in &n.models {
                if !c.models.contains(m) {
                    c.models.push(m.clone());
                }
            }
        } else {
            out.push(n);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Severity;
    use crate::orchestrate::enrich;
    use crate::domain::RawFinding;
    use std::io::Write;

    fn raw(file: &str, quote: &str, title: &str) -> RawFinding {
        RawFinding {
            severity: Severity::High, title: title.into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: quote.into(),
        }
    }
    fn f(file: &str, quote: &str, title: &str) -> Finding { enrich(&raw(file, quote, title), "m") }
    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() { std::fs::create_dir_all(parent).unwrap(); }
        std::fs::File::create(p).unwrap().write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn migrate_renames_moves_file_and_recomputes_id() {
        // R1：纯 rename 内容不变 → 迁移后 file=新路径、id 与「模型在新路径重报同问题」一致
        let mut v = vec![f("old.rs", "let x = md5(pw);", "weak hash")];
        migrate_renames(&mut v, &[("old.rs".into(), "new.rs".into())]);
        assert_eq!(v[0].file, "new.rs");
        let reraised = f("new.rs", "let x = md5(pw);", "weak hash");
        assert_eq!(v[0].id, reraised.id, "迁移后 id 必须与新路径重报碰撞（resolved/disputed 机制依赖）");
        // 未涉及 rename 的 finding 不动
        let untouched = f("keep.rs", "q", "t");
        let old_id = untouched.id.clone();
        let mut v2 = vec![untouched];
        migrate_renames(&mut v2, &[("old.rs".into(), "new.rs".into())]);
        assert_eq!((v2[0].file.as_str(), v2[0].id.as_str()), ("keep.rs", old_id.as_str()));
    }

    #[test]
    fn migrate_resolve_cards_same_id_space() {
        let fnd = f("old.rs", "let x = md5(pw);", "weak hash");
        let mut cards = vec![ResolveCard {
            id: fnd.id.clone(), title: "weak hash".into(), file: "old.rs".into(),
            quote: "let x = md5(pw);".into(), severity: "high".into(),
            reason: String::new(), round: "r1".into(), updated_at: String::new(),
        }];
        migrate_resolve_cards(&mut cards, &[("old.rs".into(), "new.rs".into())]);
        let mut fv = vec![fnd];
        migrate_renames(&mut fv, &[("old.rs".into(), "new.rs".into())]);
        assert_eq!(cards[0].id, fv[0].id, "卡与 finding 迁移后必须落在同一 id 空间");
        assert_eq!(cards[0].file, "new.rs");
    }

    #[test]
    fn carry_excludes_resolved_reanchors_and_drops() {
        let dir = tempfile::tempdir().unwrap();
        // 当前树：q1 仍在（行移位到 3）；q2 已被修掉
        write(dir.path(), "a.rs", "// new header\nfn main() {\n    db.write(token);\n}\n");
        let f1 = f("a.rs", "db.write(token);", "unvalidated write"); // 携带成活
        let f2 = f("a.rs", "md5(pw)", "weak hash");                  // quote 消失 → dropped
        let f3 = f("a.rs", "db.write(token);", "already fixed");     // resolved → 排除
        let resolved: HashSet<String> = [f3.id.clone()].into();
        let (id1, id2, id3) = (f1.id.clone(), f2.id.clone(), f3.id.clone());
        let out = carry(vec![f1, f2, f3], &resolved, dir.path());
        assert_eq!(out.carried.len(), 1);
        assert_eq!(out.carried[0].id, id1);
        assert_eq!(out.carried[0].origin, Origin::Carried);
        assert_eq!((out.carried[0].line_start, out.carried[0].line_end), (3, 3), "行号随重锚更新");
        assert_eq!(out.carried_dropped, vec![id2]);
        assert_eq!(out.resolved_applied, vec![id3]);
    }

    #[test]
    fn merge_carry_same_id_keeps_carried_no_agreement_bump() {
        // 决议 5：re-raise 不涨 agreement——跨轮重看非独立证据
        let mut carried = f("a.rs", "q", "t");
        carried.origin = Origin::Carried;
        carried.agreement = 2;
        carried.models = vec!["alpha".into(), "beta".into()];
        let mut reraise = f("a.rs", "q", "t"); // 同 id
        reraise.models = vec!["gamma".into()];
        let brand_new = f("a.rs", "other quote", "other issue");
        let new_id = brand_new.id.clone();
        let merged = merge_carry(vec![carried], vec![reraise, brand_new]);
        assert_eq!(merged.len(), 2);
        let kept = merged.iter().find(|x| x.origin == Origin::Carried).unwrap();
        assert_eq!(kept.agreement, 2, "agreement 不变（单轮内多模型互证原语义）");
        assert!(kept.models.contains(&"gamma".to_string()), "models 并集记录");
        let fresh = merged.iter().find(|x| x.id == new_id).unwrap();
        assert_eq!(fresh.origin, Origin::New);
    }

    #[test]
    fn merge_carry_absorbs_quote_drifted_new_into_carried() {
        // D4：quote 漂移致 id 不同，但 file/行重叠 + 标题高 Jaccard → 相似回落仍吸收进 carried，
        // 不该被当成独立新 bug 双条入报告。severity/confidence/body/rec 保 carried 原值,
        // agreement 不变（相似回落=id 漂移补丁,语义同构于精确 id 命中,非独立证据）。
        let mut carried = f("a.rs", "Q1", "missing auth check");
        carried.line_start = 10;
        carried.line_end = 12;
        carried.origin = Origin::Carried;
        carried.agreement = 2;
        carried.models = vec!["claude".into()];
        let mut drifted = f("a.rs", "Q2", "missing auth check"); // 同义高 Jaccard、quote 不同 → id 不同
        drifted.line_start = 11;
        drifted.line_end = 13;
        drifted.models = vec!["gpt".into()];
        assert_ne!(carried.id, drifted.id, "quote 漂移前提：两条 id 必须不同");

        let carried_severity = carried.severity;
        let carried_confidence = carried.confidence;
        let carried_body = carried.body.clone();
        let carried_rec = carried.recommendation.clone();

        let merged = merge_carry(vec![carried], vec![drifted]);
        assert_eq!(merged.len(), 1, "相似回落吸收，不应双条入报告");
        let kept = &merged[0];
        assert_eq!(kept.origin, Origin::Carried);
        assert_eq!(kept.agreement, 2, "agreement 不变——相似回落语义同构于精确 id 命中");
        assert!(kept.models.contains(&"claude".to_string()) && kept.models.contains(&"gpt".to_string()), "models 并集");
        assert_eq!(kept.severity, carried_severity, "severity 保 carried 原值");
        assert_eq!(kept.confidence, carried_confidence, "confidence 保 carried 原值");
        assert_eq!(kept.body, carried_body, "body 保 carried 原值");
        assert_eq!(kept.recommendation, carried_rec, "recommendation 保 carried 原值");
    }

    #[test]
    fn merge_carry_similarity_only_absorbs_into_carried() {
        // 相似回落仅对 origin==Carried 的 out 项生效；new-vs-new 相似命中不在 merge_carry
        // 职责内（上游 dedup 处理），故两条相似 new 各自独立入 out。
        let mut unrelated_carried = f("z.rs", "Zq", "unrelated issue");
        unrelated_carried.line_start = 1;
        unrelated_carried.line_end = 1;
        unrelated_carried.origin = Origin::Carried;

        let mut new1 = f("a.rs", "Q1", "missing auth check");
        new1.line_start = 10;
        new1.line_end = 12;
        new1.models = vec!["claude".into()];
        let mut new2 = f("a.rs", "Q2", "missing auth check");
        new2.line_start = 11;
        new2.line_end = 13;
        new2.models = vec!["gpt".into()];
        assert_ne!(new1.id, new2.id);

        let merged = merge_carry(vec![unrelated_carried], vec![new1, new2]);
        assert_eq!(merged.len(), 3, "new-vs-new 不在 merge_carry 合并——各自独立入合并集");
    }
}
