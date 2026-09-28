//! 疑点台账落账（M21c 批 A，design §8.2）：把**本轮没挖成的**队列项提炼成跨轮台账行。
//!
//! 为什么要有这一层：预算截断掉的项，其明细本来只活在按作业的 `queue_items` 快照里（resume 的工作区，随作业增长）。
//! 台账要的是**跨轮**的身份——同一处疑点这轮被截、下轮 delta 碰到它就该被激活（批 C）。
//! 身份用「文件 + quote 归一」，与 finding id 同一套归一化：行号无关、免疫 reformat。

use crate::db::SuspicionRow;
use crate::orchestrate::queue::{ItemState, QueueItem};

/// 落账原因：目前只有预算截断一种；批 B 的判例豁免会加第二种。
pub const REASON_BUDGET: &str = "budget";

/// 把队列项里**没挖成**的那些提炼成台账行（每条疑点一行）。
///
/// 只落**带疑点的项**（scanner / needs 来源）：H0 规则项每轮都会由规则重新产生，不需要跨轮记忆，
/// 且它没有 quote 锚、进不了以 quote 为身份的台账。
/// 同一处疑点在一轮里可能由多个家族重复提出 → 按 id 去重，保留第一条（`members` 是到达序）。
pub fn rows_from_skipped(items: &[QueueItem], job_id: &str) -> Vec<SuspicionRow> {
    let mut out: Vec<SuspicionRow> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in items.iter().filter(|i| i.state == ItemState::Skipped) {
        for s in &item.members {
            let quote_norm = crate::orchestrate::normalize_quote(&s.quote);
            if quote_norm.is_empty() {
                continue; // 无 quote 锚 = 没有跨轮身份，落了也对不上
            }
            let id = crate::hash::fnv1a64_hex(&[&s.file, &quote_norm]);
            if !seen.insert(id.clone()) {
                continue;
            }
            let json = serde_json::json!({
                "line_start": s.line_start,
                "line_end": s.line_end,
                "confidence": s.confidence,
                "category": s.category,
                "quote": s.quote,
                "origin": format!("{:?}", item.spec.origin),
                "reason": REASON_BUDGET,
                "item": item.id,
            });
            out.push(SuspicionRow {
                id,
                file: s.file.clone(),
                quote_norm,
                hint: s.hint.clone(),
                status: "dormant".into(),
                first_job: job_id.to_string(),
                last_job: job_id.to_string(),
                activations: 0,
                json: json.to_string(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Suspicion;
    use crate::orchestrate::queue::QueueItem;

    fn sus(file: &str, line: u32, quote: &str, hint: &str, conf: f32) -> Suspicion {
        Suspicion {
            file: file.into(), line_start: line, line_end: line, hint: hint.into(),
            category: None, confidence: conf, quote: quote.into(),
        }
    }

    /// 用 JSON 反序列化造夹具：`QueueItem::new` 是私有的（它要走入队流程算 id / 块 / 家族），
    /// 而这里要的是「把队列项摆成某个终态」，绕开流程更直白。
    fn item(id: &str, state: &str, origin: &str, members: Vec<Suspicion>) -> QueueItem {
        let file = members.first().map(|s| s.file.clone()).unwrap_or_else(|| "a.rs".into());
        let notes = if origin == "rule" { vec![] } else { members.clone() };
        serde_json::from_value(serde_json::json!({
            "id": id,
            "spec": { "file": file, "ranges": [[1, 9]], "origin": origin, "notes": notes },
            "score": 1.0,
            "agreement": 1,
            "state": state,
            "kind": if origin == "rule" { "primary" } else { "suspicion" },
            "dispatch_agreement": 1,
            "landed_agreement": null,
            "families": ["alpha"],
            "members": members,
        }))
        .expect("夹具 JSON 应与 QueueItem 形状一致")
    }

    /// 只落**没挖成**的带疑点项：settled 的不落（已经挖过了）、H0 规则项不落（每轮由规则重新产生、也没有 quote 锚）。
    #[test]
    fn only_skipped_items_with_quotes_are_recorded() {
        let items = vec![
            item("i1", "skipped", "scan", vec![sus("a.rs", 3, "let x = take(v);", "空指针", 0.6)]),
            item("i2", "settled", "scan", vec![sus("b.rs", 4, "drop(guard);", "早释放", 0.7)]),
            item("i3", "skipped", "rule", vec![]),
            item("i4", "skipped", "scan", vec![sus("c.rs", 5, "   ", "无锚", 0.5)]),
        ];
        let rows = rows_from_skipped(&items, "sha1");
        assert_eq!(rows.len(), 1, "只有 i1 该落账：{:?}", rows.iter().map(|r| &r.file).collect::<Vec<_>>());
        let r = &rows[0];
        assert_eq!((r.file.as_str(), r.hint.as_str(), r.status.as_str()), ("a.rs", "空指针", "dormant"));
        assert_eq!((r.first_job.as_str(), r.last_job.as_str(), r.activations), ("sha1", "sha1", 0));
        assert_eq!(r.quote_norm, "letx=take(v);", "quote 去空白即身份");
        let j: serde_json::Value = serde_json::from_str(&r.json).unwrap();
        assert_eq!(j["reason"], REASON_BUDGET);
        assert_eq!(j["item"], "i1");
        assert_eq!(j["line_start"], 3);
        assert!((j["confidence"].as_f64().unwrap() - 0.6).abs() < 1e-6, "{}", j["confidence"]); // f32 → f64 的表示误差
    }

    /// 身份 = 文件 + quote 归一：同一处疑点换行号 / 改缩进仍是同一行；不同家族重复提出只落一行。
    #[test]
    fn identity_is_file_plus_normalized_quote() {
        let a = rows_from_skipped(&[item("i1", "skipped", "scan",
            vec![sus("a.rs", 3, "let x = take(v);", "h", 0.6)])], "sha1");
        let b = rows_from_skipped(&[item("i9", "skipped", "scan",
            vec![sus("a.rs", 77, "  let x  =  take(v);  ", "别的说法", 0.9)])], "sha2");
        assert_eq!(a[0].id, b[0].id, "行号与空白不进身份");
        // 同项内两个家族报同一处 → 去重成一行
        let dup = rows_from_skipped(&[item("i1", "skipped", "scan", vec![
            sus("a.rs", 3, "let x = take(v);", "甲说", 0.6),
            sus("a.rs", 3, "let x = take(v);", "乙说", 0.8),
        ])], "sha1");
        assert_eq!(dup.len(), 1);
        assert_eq!(dup[0].hint, "甲说", "保留到达序里的第一条");
    }
}
