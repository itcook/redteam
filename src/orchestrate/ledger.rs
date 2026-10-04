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
/// 只落**带疑点的项**：`members` 为空的项没有 quote 锚、也就没有跨轮身份。
/// H0 规则项的 `members` 恒空（规则不产疑点），每轮由规则重新产生、不需要跨轮记忆——这是设计意图。
///
/// needs 项**不一定**空（批 C 订正：原注释称「needs 补查项 members 恒空、故不会落账」，与实测不符）：
/// `members = spec.notes.clone()`，而 needs 项在合并时会吸收到达请求的 notes（`queue.rs` 的 merge 分支），
/// 于是「与 needs 项落在同一块的 scan 疑点」会跟着 needs 项的身份落账——PR181 库里实有一条 `origin='Needs'` 的行。
/// 缺口本身仍然成立，但要按它的真实形状理解：**纯 needs 项**（没有 scan 疑点并进来的）落不了账，
/// 因为落账身份是「文件 + quote 归一」而补查项的诉求不在任何 quote 上；批 C 的 delta 激活也就捞不回它们。
/// 要捞得先给它一条独立于 quote 的身份（已记 followup）。
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
                status: STATUS_DORMANT.into(),
                first_seen_sha: job_id.to_string(),
                last_seen_sha: job_id.to_string(),
                activations: 0,
                json: json.to_string(),
            });
        }
    }
    out
}

/// 活锁守卫两段门（M21c 批 C〔用户裁 2026-10-02〕，design §8.4）：激活到 `FORCE_DIG` 次仍未晋升 →
/// 本轮强制入必挖集（不受预算截断）；到 `PARK` 次仍未晋升 → 停车交人。
/// 两段式的理由：直接停车放弃了「强制必挖可能一次就挖出来」的自愈机会，只必挖不停车则真挖不动的
/// 疑点每轮占一个必挖位、成为长期税。计数是**激活次数**，不是「被看见次数」。
pub const ACTIVATION_FORCE_DIG: i64 = 3;
pub const ACTIVATION_PARK: i64 = 5;

/// 一条被激活的台账疑点：待入队的疑点本体 + 待回写的台账身份。
#[derive(Clone, Debug, PartialEq)]
pub struct Activated {
    /// 台账行 id（回写 `status` / `activations` 用）。
    pub row_id: String,
    /// 重锚到**当前**文件内容后的疑点（行号已更新，quote / hint / 置信沿用台账）。
    pub suspicion: crate::domain::Suspicion,
    /// 回写后的激活次数（= 原值 + 1）。
    pub activations: i64,
    /// 本轮是否踩到强制必挖门（`activations >= ACTIVATION_FORCE_DIG`）。
    pub force_dig: bool,
    /// 本轮是否踩到停车门（`activations >= ACTIVATION_PARK`）。
    pub park: bool,
}

/// 激活的产物：待入队项 + 两类不激活的计数（都要能在报告里说清楚，别静默吞掉）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActivationOutcome {
    pub activated: Vec<Activated>,
    /// 判据过了但 quote 重锚不上（代码改到锚失效）→ 不入队。锚丢了就没有可靠位置，宁可不挖。
    pub anchor_lost: Vec<String>,
}

/// 台账行 → 本轮该重新入队的疑点（M21c 批 C，design §8.3）。纯粹按判据挑 + 重锚，不写库、不入队。
///
/// 三条判据缺一不可：
/// - `status == "dormant"`：已激活 / 已晋升的不重复激活（状态机单向推进）。
/// - **文件 ∈ 本轮变更集**：由调用方的 `suspicions_for_files(changed_files)` 查询保证，本函数不再判。
/// - `last_seen_sha != 本轮 head`：一条判据挡掉两种假激活——① 本轮自产（本轮刚被截断，激活了也没有预算再挖它）
///   ② 同 HEAD 重跑（没有新 delta，激活无依据；PR181 那种固定靶子四轮同 HEAD，正是这一类）。
///
/// **落账在深挖之后、激活在深挖之前**，所以这里读到的 `last_seen_sha` 必定是上一轮写的——判据③ 靠这个先后成立，
/// 两者的顺序不能调换。调换了本轮自产的疑点会在同一轮里被自己激活一次。
pub fn activations_for(rows: &[crate::db::SuspicionRow], head: &str, root: &std::path::Path) -> ActivationOutcome {
    let mut out = ActivationOutcome::default();
    for r in rows {
        if r.status != STATUS_DORMANT || r.last_seen_sha == head || head.is_empty() {
            continue;
        }
        // 已停车的不再自动激活（dogfood medium，agreement 2）：停车的含义是「机制挖不动它、交人」，
        // 而原先 `park` 只产生一条 warning，下一轮它照样被激活、照样强制必挖、计数继续涨——
        // 「交人」从未真正发生。它仍然留在台账里让维度 3 为假（见 `attach_convergence`），
        // 收敛因此不成立、ship 会停下来问人，这才是设计里那条通道。
        if r.activations >= ACTIVATION_PARK {
            continue;
        }
        let d: serde_json::Value = serde_json::from_str(&r.json).unwrap_or(serde_json::Value::Null);
        let quote = d.get("quote").and_then(|v| v.as_str()).unwrap_or_default();
        if quote.is_empty() {
            continue; // 无 quote = 无锚，落账时本就不该有这种行
        }
        let old_line = d.get("line_start").and_then(serde_json::Value::as_u64).unwrap_or(1) as u32;
        let Some((ls, le)) = crate::positioning::anchor_near(root, &r.file, quote, old_line) else {
            out.anchor_lost.push(r.id.clone());
            continue;
        };
        let activations = r.activations + 1;
        out.activated.push(Activated {
            row_id: r.id.clone(),
            suspicion: crate::domain::Suspicion {
                file: r.file.clone(),
                line_start: ls,
                line_end: le,
                hint: r.hint.clone(),
                category: d.get("category").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or(None),
                confidence: d.get("confidence").and_then(serde_json::Value::as_f64).unwrap_or(0.0) as f32,
                quote: quote.to_string(),
            },
            activations,
            force_dig: activations >= ACTIVATION_FORCE_DIG,
            park: activations >= ACTIVATION_PARK,
        });
    }
    out
}

/// 台账状态机（design §8.3）：**两态**——`dormant`（休眠，可被激活）与 `promoted`（已晋升为 finding，不再激活）。
///
/// 这里原本有第三个中间态 `activated`，批 C 的 dogfood 当场证明它是错的：激活把行改成 `activated`，
/// 而激活判据与回写都只认 `dormant`，于是 `activations` 永远饱和在 1、两段门（3 / 5）**是不可达常量**，
/// 活锁守卫整个是死代码；更糟的是「激活了但没挖成」的行从此既不被激活也不被披露，**彻底消失**。
/// 根因是那个状态机没有回边，而「激活了没挖成」恰恰是最常见的情形（本轮预算同样会截断它）。
///
/// 两态下：激活**不动 status**，只 `activations += 1` 并把 `last_seen_sha` 记成本轮；
/// 同轮幂等由 `last_seen_sha <> 本轮` 保证（见 `ProjDb::suspicion_activate`），
/// 跨轮则每轮各加一次，门才可达。挖出 finding 时才写 `promoted`（见 `promote_rows`）。
///
/// 「当前在队列里挖着」是**轮内**状态，不该写进跨轮台账——这是上面那个设计错误的教训。
pub const STATUS_DORMANT: &str = "dormant";
pub const STATUS_PROMOTED: &str = "promoted";

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
        assert_eq!((r.first_seen_sha.as_str(), r.last_seen_sha.as_str(), r.activations), ("sha1", "sha1", 0));
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

    // ───────────────────────── 批 C：激活判据 ─────────────────────────

    fn row(id: &str, file: &str, status: &str, last_seen_sha: &str, activations: i64, quote: &str) -> crate::db::SuspicionRow {
        crate::db::SuspicionRow {
            id: id.into(),
            file: file.into(),
            quote_norm: crate::orchestrate::normalize_quote(quote),
            hint: "边界没校验".into(),
            status: status.into(),
            first_seen_sha: "sha0".into(),
            last_seen_sha: last_seen_sha.into(),
            activations,
            json: serde_json::json!({ "line_start": 1, "quote": quote, "confidence": 0.62, "category": "L2" }).to_string(),
        }
    }

    /// 写一个两行文件，第 2 行是 quote 的落点（重锚要能找到它）。
    fn repo(body: &str) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("a.rs"), body).unwrap();
        t
    }

    /// 激活正路：往轮落的账 + 本轮 head 不同 → 激活，行号重锚到当前内容，计数 +1。
    #[test]
    fn activates_a_dormant_row_from_a_previous_round() {
        let t = repo("fn f() {\n    let x = take(v);\n}\n");
        let out = activations_for(&[row("r1", "a.rs", STATUS_DORMANT, "sha1", 0, "let x = take(v);")], "sha2", t.path());
        assert_eq!(out.activated.len(), 1, "{out:?}");
        let a = &out.activated[0];
        assert_eq!((a.row_id.as_str(), a.activations, a.force_dig, a.park), ("r1", 1, false, false));
        assert_eq!(a.suspicion.line_start, 2, "重锚到当前内容里 quote 实际所在的行");
        assert_eq!(a.suspicion.hint, "边界没校验", "hint 沿用台账");
        assert!((a.suspicion.confidence - 0.62).abs() < 1e-6);
        assert!(out.anchor_lost.is_empty());
    }

    /// 三类不激活，各自单独成立（判据 ① 与 ③；判据 ② 由调用方的 SQL 保证，不在本函数）。
    #[test]
    fn three_kinds_of_rows_are_not_activated() {
        let t = repo("fn f() {\n    let x = take(v);\n}\n");
        let q = "let x = take(v);";
        // ① 本轮自产：落账在深挖之后、激活在深挖之前，所以 last_seen_sha == 本轮 head 只可能是本轮刚写的
        let own = activations_for(&[row("r1", "a.rs", STATUS_DORMANT, "sha2", 0, q)], "sha2", t.path());
        assert!(own.activated.is_empty(), "本轮刚截断的不在本轮激活（没预算再挖它）: {own:?}");
        // ② 同 HEAD 重跑：同一个 head 第二次跑，台账里 last_seen_sha 已是它——没有新 delta，激活无依据
        let rerun = activations_for(&[row("r1", "a.rs", STATUS_DORMANT, "sha1", 0, q)], "sha1", t.path());
        assert!(rerun.activated.is_empty(), "同 HEAD 重跑不激活: {rerun:?}");
        // ③ 已晋升的退出激活池（挖出过 finding，不该再反复重挖）
        let done = activations_for(&[row("r1", "a.rs", STATUS_PROMOTED, "sha1", 2, q)], "sha2", t.path());
        assert!(done.activated.is_empty(), "promoted 不该再被激活: {done:?}");
        // 无轮次标识（非 git / 无 commit）：与落账同一道门
        let noround = activations_for(&[row("r1", "a.rs", STATUS_DORMANT, "sha1", 0, q)], "", t.path());
        assert!(noround.activated.is_empty(), "无轮次标识不激活");
    }

    /// 重锚不上就不入队，但要记下来——「没激活」有两种原因，合成一个数就查不出是哪种。
    #[test]
    fn rows_whose_quote_no_longer_anchors_are_counted_not_enqueued() {
        let t = repo("fn f() {\n    let y = other();\n}\n"); // quote 已经不在文件里了
        let out = activations_for(&[row("r1", "a.rs", STATUS_DORMANT, "sha1", 0, "let x = take(v);")], "sha2", t.path());
        assert!(out.activated.is_empty());
        assert_eq!(out.anchor_lost, vec!["r1".to_string()], "锚丢了要能看见");
    }

    /// 活锁守卫两段门：到 `ACTIVATION_FORCE_DIG` 置 `force_dig`、到 `ACTIVATION_PARK` 再置 `park`。
    /// 门按**回写后**的次数判（本次激活算数），否则第 N 次激活时门还没响、要等到第 N+1 次。
    #[test]
    fn livelock_guard_has_two_stages() {
        let t = repo("fn f() {\n    let x = take(v);\n}\n");
        let q = "let x = take(v);";
        let at = |prior: i64| {
            let o = activations_for(&[row("r1", "a.rs", STATUS_DORMANT, "sha1", prior, q)], "sha2", t.path());
            let a = o.activated.into_iter().next().unwrap();
            (a.activations, a.force_dig, a.park)
        };
        assert_eq!(at(0), (1, false, false));
        assert_eq!(at(ACTIVATION_FORCE_DIG - 2), (ACTIVATION_FORCE_DIG - 1, false, false), "门前一步还没响");
        assert_eq!(at(ACTIVATION_FORCE_DIG - 1), (ACTIVATION_FORCE_DIG, true, false), "踩到强制必挖门");
        assert_eq!(at(ACTIVATION_PARK - 1), (ACTIVATION_PARK, true, true), "踩到停车门时仍然必挖");

        // **停车之后不再自动激活**（dogfood medium，agreement 2）：停车的含义是「机制挖不动它、交人」。
        // 原先 `park` 只产生一条 warning，下一轮它照样被激活、照样强制必挖、计数继续涨——「交人」从未发生。
        let parked = activations_for(&[row("r1", "a.rs", STATUS_DORMANT, "sha1", ACTIVATION_PARK, q)], "sha2", t.path());
        assert!(parked.activated.is_empty(), "已停车的不该被机器再捞一次: {parked:?}");
        // 它仍留在台账里（status 还是 dormant），维度 3 据此为假、收敛不成立，ship 会停下来问人——
        // 那才是设计里的「停车通道」。
    }
}
