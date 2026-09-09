//! 模型输出 → `LensOutput` 的解析与纠错逻辑。single_shot（原 direct）与 tool_loop
//! 共享的活解析器落地于此，防止两处各自重实现导致行为分叉（B-Q3 迁移）。

use super::tool_loop::AssistantTurn;
use super::ProviderError;
use crate::domain::{Category, LensOutput, NeedEntry, ScanOutput, ScannedArea, Suspicion};
use serde_json::Value;
use std::path::{Component, Path};

/// 公共前处理（批 D-1 抽出，`parse_lens_output` / `parse_scan_output` 共用）：`</think>` 切除 → 围栏剥离 →
/// 整体解析 / 字符串感知配平扫描，返回首个能解析成 JSON **对象**的片段（不做 schema 校验，调用方按各自类型 `from_value`）。
pub(crate) fn extract_json_object(raw: &str) -> Option<Value> {
    // 思考型模型（MiniMax-M3 / DeepSeek-R1 等）即便设 response_format=json_object，仍在最终
    // 答案前吐 <think>…</think> 推理块，其中常含裸 '{'（代码推理）会带偏下面的括号扫描 → 挂。
    // 取最后一个 </think> 之后的内容再解析。ponytail: 极端下 finding 正文字面量含 "</think>"
    // 会误切——评审工具输出里概率近零，够用。
    let raw = match raw.rfind("</think>") {
        Some(i) => &raw[i + "</think>".len()..],
        None => raw,
    };
    let trimmed = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        if v.is_object() {
            return Some(v);
        }
    }
    // 从首个 '{' 到匹配的 '}' 扫描，字符串感知（跳过串内的 '{'/'}'，
    // 正确处理 '\"' 转义）——findings.body/quote 常引用半行代码含不配平括号
    // （如 "match x {" 未闭合），裸计数会数错深度、抠出非法 JSON。
    let start = trimmed.find('{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in trimmed[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let cand = &trimmed[start..start + i + 1];
                    return serde_json::from_str::<Value>(cand).ok().filter(Value::is_object);
                }
            }
            _ => {}
        }
    }
    None
}

/// 兜底：从模型自由文本里抠出符合 schema 的 JSON。
pub fn parse_lens_output(raw: &str) -> Result<LensOutput, ProviderError> {
    if let Some(v) = extract_json_object(raw) {
        if let Ok(out) = serde_json::from_value::<LensOutput>(v) {
            return Ok(out);
        }
    }
    Err(ProviderError::Parse(format!(
        "no schema-valid JSON in: {}",
        &raw.chars().take(120).collect::<String>()
    )))
}

/// scanner 契约 parse 的规整统计（批 D-1）：丢弃 / 截断计数由调用方（F）写进 cell warn，不静默。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanParseStats {
    /// quote 空白 → 丢（design §4：quote 必填，是合并与判例预筛的锚）。
    pub dropped_no_quote: u32,
    /// file 缺失 / 绝对路径 / 含 `..` → 丢。
    pub dropped_bad_path: u32,
    /// 条目不是对象 → 丢。
    pub dropped_malformed: u32,
    /// needs 条目缺 path / reason → 丢。
    pub needs_dropped: u32,
    /// needs 超帽（5）被截掉的条数。
    pub needs_truncated: u32,
    /// scanned_areas.ranges 不形如 `a-b`（a ≤ b）被丢的条数。
    pub ranges_dropped: u32,
}

/// scanner 输出 → `ScanOutput`（design §4 契约，字段级对齐；D11 quote 逐字硬约束在 `positioning::anchor_suspicions`）。
/// 顶层必须有 `suspicions` 数组——缺失即违约（`Parse`，走 `REPAIR_PROMPT_SCAN`），不当空扫描。
pub fn parse_scan_output(raw: &str) -> Result<ScanOutput, ProviderError> {
    parse_scan_output_with_stats(raw).map(|(o, _)| o)
}

/// 同上，附规整统计。逐条规整而非整体拒绝：一条坏疑点不应让整格作废（快模型契约守约弱，E3 实证）。
pub fn parse_scan_output_with_stats(raw: &str) -> Result<(ScanOutput, ScanParseStats), ProviderError> {
    let v = extract_json_object(raw).ok_or_else(|| {
        ProviderError::Parse(format!("no JSON object in: {}", raw.chars().take(120).collect::<String>()))
    })?;
    let Some(items) = v.get("suspicions").and_then(Value::as_array) else {
        return Err(ProviderError::Parse("scan output has no \"suspicions\" array".into()));
    };
    let mut stats = ScanParseStats::default();
    let mut out = ScanOutput::default();
    for it in items {
        let Some(o) = it.as_object() else {
            stats.dropped_malformed += 1;
            continue;
        };
        let Some(file) = o.get("file").and_then(Value::as_str).and_then(normalize_rel_path) else {
            stats.dropped_bad_path += 1;
            continue;
        };
        let quote = o.get("quote").and_then(Value::as_str).unwrap_or("");
        if quote.trim().is_empty() {
            stats.dropped_no_quote += 1;
            continue;
        }
        // 先夹在 u32 域内再转换：`as u32` 对越界值截断（4294967296 → 0）会违反 line_start ≥ 1（评审 #4）
        let line_start = as_line(o.get("line_start")).unwrap_or(1).clamp(1, u32::MAX as i64) as u32;
        let line_end = (as_line(o.get("line_end")).unwrap_or(0).clamp(0, u32::MAX as i64) as u32).max(line_start);
        let hint: String = o.get("hint").and_then(Value::as_str).unwrap_or("").trim().chars().take(300).collect();
        let category = o
            .get("category")
            .and_then(Value::as_str)
            .and_then(|s| serde_json::from_value::<Category>(Value::String(s.trim().to_uppercase())).ok());
        let confidence = match o.get("confidence") {
            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.5),
            Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(0.5),
            _ => 0.5,
        };
        let confidence = if confidence.is_finite() { confidence.clamp(0.0, 1.0) as f32 } else { 0.5 };
        out.suspicions.push(Suspicion { file, line_start, line_end, hint, category, confidence, quote: quote.to_string() });
    }
    if let Some(needs) = v.get("needs").and_then(Value::as_array) {
        for n in needs {
            // `kind` 大小写宽容（`"Evidence"` 不整条丢；与 category 的 to_uppercase 同宽严，合并后审计 nit）
            let mut n = n.clone();
            if let Some(k) = n.get("kind").and_then(Value::as_str).map(|s| s.trim().to_lowercase()) {
                n["kind"] = Value::String(k);
            }
            match serde_json::from_value::<NeedEntry>(n) {
                Ok(mut e) => match normalize_rel_path(&e.path) {
                    Some(p) => {
                        e.path = p;
                        out.needs.push(e);
                    }
                    None => stats.needs_dropped += 1,
                },
                Err(_) => stats.needs_dropped += 1,
            }
        }
        if out.needs.len() > NEEDS_CAP {
            stats.needs_truncated = (out.needs.len() - NEEDS_CAP) as u32;
            out.needs.truncate(NEEDS_CAP);
        }
    }
    if let Some(areas) = v.get("scanned_areas").and_then(Value::as_array) {
        for a in areas {
            let Some(ao) = a.as_object() else {
                stats.dropped_malformed += 1;
                continue;
            };
            let Some(file) = ao.get("file").and_then(Value::as_str).and_then(normalize_rel_path) else {
                stats.dropped_malformed += 1;
                continue;
            };
            let mut ranges = Vec::new();
            for r in ao.get("ranges").and_then(Value::as_array).into_iter().flatten() {
                match r.as_str().and_then(valid_range) {
                    Some(s) => ranges.push(s),
                    None => stats.ranges_dropped += 1,
                }
            }
            out.scanned_areas.push(ScannedArea { file, ranges });
        }
    }
    Ok((out, stats))
}

/// needs 帽（design §4：cap 5）。
pub const NEEDS_CAP: usize = 5;

/// 仓内相对路径规范化（与 `positioning::safe_read` 同口径，parse 层先挡一道）：非绝对、无 `..` 段；去 `./` 与重复分隔符，
/// 按 `/` 重组——同一文件在不同 cell 里写法不同（`./src/a.rs` vs `src/a.rs`）时并集 / coverage 对账按串比对不会错开（评审 #9）。
fn normalize_rel_path(p: &str) -> Option<String> {
    let path = Path::new(p.trim());
    if path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let parts: Vec<&str> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// `scanned_areas.ranges` 项形如 `a-b`（纯数字，1 ≤ a ≤ b，design §4 覆盖自报）→ 规范串 `"a-b"`。
/// 行号：整数 / 浮点（`9.0`）/ 数字串（`"7"`）都认（模型输出形态不一，合并后审计 nit）；其余 → None（调用方取缺省）。
fn as_line(v: Option<&Value>) -> Option<i64> {
    let float = |f: f64| f.is_finite().then_some(f as i64);
    match v? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().and_then(float)),
        Value::String(s) => s.trim().parse::<i64>().ok().or_else(|| s.trim().parse::<f64>().ok().and_then(float)),
        _ => None,
    }
}

fn valid_range(s: &str) -> Option<String> {
    let (a, b) = s.trim().split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    if a.is_empty() || b.is_empty() || !a.chars().all(|c| c.is_ascii_digit()) || !b.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (a, b) = (a.parse::<u32>().ok()?, b.parse::<u32>().ok()?);
    (a >= 1 && a <= b).then(|| format!("{a}-{b}"))
}

/// D1：兼容网关有时把错误塞进 HTTP 200 响应体（不触发 4xx/5xx，`send_classified` 判"成功"）——
/// `parse_anthropic_turn`/`parse_openai_turn`/`parse_openai_stream` 拿不到期望字段 → `None` →
/// 上层误判 `Parse`（不可重试，一次网关异常烧掉整个 cell）。此函数在 parse 之前先探这层：
/// 两种已知形状——OpenAI `{"error":{"type","code","message"}}`；
/// Anthropic `{"type":"error","error":{"type","message"}}`——两者共同点是都有顶层 `error`
/// 对象，故只认这一个键即可覆盖双形状，不必额外判断外层 `type` 字段。
/// 找不到 `error` 键或其值非对象（`error:""` / `error:[]` 等占位、兼容网关、即正常 findings/choices/content 体）→ `None`，不干预。
pub(crate) fn error_body_to_provider_error(v: &Value) -> Option<ProviderError> {
    let err_obj = v.get("error").filter(|e| e.is_object())?;
    let etype = err_obj.get("type").and_then(Value::as_str).unwrap_or("");
    let message = err_obj.get("message").and_then(Value::as_str).unwrap_or("");
    let detail = format!("{etype}: {message}");
    // 子串匹配（而非精确枚举）：OpenAI/Anthropic/网关三方言的 error.type 用词不完全一致
    // （如 "rate_limit_error" vs "rate_limit_exceeded"），子串覆盖两家现有值且对未来变体宽容。
    Some(if etype.contains("rate_limit") || etype.contains("overloaded") {
        ProviderError::RateLimited { retry_after: None }
    } else if etype.contains("timeout") {
        ProviderError::Timeout(detail)
    } else if etype.contains("server_error") || etype.contains("api_error") {
        ProviderError::Server(detail)
    } else if etype.contains("authentication") || etype.contains("permission") {
        ProviderError::Auth(detail)
    } else if etype.contains("invalid_request") || etype.contains("context_length") {
        ProviderError::BadRequest(detail)
    } else {
        // 未知 error 类型：保守可重试——宁可多试一次，不可误把陌生错误当不可重试永久失败。
        ProviderError::Unavailable(detail)
    })
}

pub(crate) const REPAIR_PROMPT: &str = "Your previous response was not valid JSON (it may have been truncated). \
Re-emit ONLY the complete findings JSON object matching the schema. If space is tight, shorten each \
finding's body/recommendation text, but do NOT drop any finding and do NOT omit any required field.";

/// scanner 契约的 repair 句（批 D-1）：与 `REPAIR_PROMPT` 同句式，指向 suspicions 对象；缺 `suspicions` 键同样走这里。
pub const REPAIR_PROMPT_SCAN: &str = "Your previous response was not valid JSON (it may have been truncated or missing the \
\"suspicions\" array). Re-emit ONLY the complete suspicions JSON object matching the schema: {\"suspicions\": [...], \
\"needs\": [...], \"scanned_areas\": [...]}. If space is tight, shorten each suspicion's hint, but do NOT drop any suspicion \
and do NOT omit any required field (file, line_start, line_end, hint, confidence, quote).";

/// parse 失败诊断化：把 turn.finish_reason 拼进 Parse 错误，区分「截断(length/max_tokens)」
/// vs 其它形状违约——warnings 里能直接看出根因，而不是裸的 "no schema-valid JSON in: …"。
pub(crate) fn diagnostic_parse_error(turn: &AssistantTurn) -> ProviderError {
    ProviderError::Parse(format!(
        "final JSON parse failed (finish_reason={}): {}",
        turn.finish_reason.as_deref().unwrap_or("unknown"),
        turn.text.chars().take(120).collect::<String>()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_strips_code_fence() {
        let raw = "```json\n{\"findings\":[]}\n```";
        assert!(parse_lens_output(raw).unwrap().findings.is_empty());
    }

    fn scan_item(extra: &str) -> String {
        format!("{{\"file\":\"src/a.rs\",\"line_start\":3,\"line_end\":4,\"hint\":\"h\",\"category\":\"L2\",\"confidence\":0.7,\"quote\":\"let x = 1;\"{extra}}}")
    }

    /// 批 D-1 AC1：围栏 / 前言 / think 块 → 解析；逐条规整（坏路径 / 空 quote 丢并计数、行号钳、hint 截断、category 大小写与非法、confidence 夹、
    /// needs 帽与坏项、ranges 过滤）；缺 suspicions 键 / 截断 → Parse 错。
    #[test]
    fn scan_parse_regularizes_each_item_and_counts_drops() {
        let items = [
            scan_item(""),
            scan_item(",\"line_start\":0,\"line_end\":0,\"category\":\"l4\",\"confidence\":7"),
            scan_item(&format!(",\"category\":\"L9\",\"confidence\":\"abc\",\"hint\":\"{}\"", "h".repeat(400))),
            "{\"file\":\"../etc/passwd\",\"line_start\":1,\"line_end\":1,\"hint\":\"h\",\"confidence\":0.5,\"quote\":\"q\"}".to_string(),
            "{\"file\":\"/abs/a.rs\",\"line_start\":1,\"line_end\":1,\"hint\":\"h\",\"confidence\":0.5,\"quote\":\"q\"}".to_string(),
            scan_item(",\"quote\":\"   \""),
            "\"not an object\"".to_string(),
        ];
        let needs: Vec<String> = (0..6).map(|i| format!("{{\"path\":\"src/n{i}.rs\",\"reason\":\"r\"}}")).chain(["{\"reason\":\"no path\"}".to_string()]).collect();
        let raw = format!(
            "<think>thinking {{ braces }}</think>\nSure! ```json\n{{\"suspicions\":[{}],\"needs\":[{}],\"scanned_areas\":[{{\"file\":\"src/a.rs\",\"ranges\":[\"1-3\",\"x\",\"9-2\",\"0-1\"]}},{{\"ranges\":[\"1-1\"]}}]}}\n```",
            items.join(","), needs.join(",")
        );
        let (out, stats) = parse_scan_output_with_stats(&raw).unwrap();
        assert_eq!(out.suspicions.len(), 3, "{out:?}");
        let (a, b, c) = (&out.suspicions[0], &out.suspicions[1], &out.suspicions[2]);
        assert_eq!((a.line_start, a.line_end, a.category, a.confidence), (3, 4, Some(Category::L2), 0.7));
        assert_eq!((b.line_start, b.line_end, b.category, b.confidence), (1, 1, Some(Category::L4), 1.0), "行号钳 / 小写 category / confidence 夹");
        assert_eq!((c.category, c.confidence, c.hint.chars().count()), (None, 0.5, 300), "非法 category → None、非数 confidence → 0.5、hint 截 300");
        assert_eq!(stats.dropped_bad_path, 2);
        assert_eq!(stats.dropped_no_quote, 1);
        assert_eq!(stats.dropped_malformed, 2, "非对象疑点 1 + 无 file 的 scanned_area 1");
        assert_eq!(out.needs.len(), NEEDS_CAP);
        assert_eq!((stats.needs_dropped, stats.needs_truncated), (1, 1));
        assert_eq!(out.needs[0].kind, crate::domain::NeedKind::Evidence, "kind 缺省 evidence");
        assert_eq!(out.scanned_areas.len(), 1);
        assert_eq!(out.scanned_areas[0].ranges, vec!["1-3"]);
        assert_eq!(stats.ranges_dropped, 3);
        // 缺 suspicions 键 = 违约；截断 = 违约
        let err = parse_scan_output("{\"needs\":[]}").unwrap_err().to_string();
        assert!(err.contains("suspicions"), "{err}");
        assert!(parse_scan_output("{\"suspicions\":[{\"file\":\"a\",").is_err());
        assert!(parse_scan_output("no json").is_err());
        assert!(REPAIR_PROMPT_SCAN.contains("suspicions"));
        // 空扫描合法
        assert!(parse_scan_output("{\"suspicions\":[],\"scanned_areas\":[]}").unwrap().suspicions.is_empty());
    }

    /// 评审 #4 #8 #9 #21：行号越界不截成 0；路径去 `./` 重组；ranges 规范化且拒 `+`；needs.path / scanned_areas.file 同规则。
    #[test]
    fn scan_parse_normalizes_paths_lines_and_ranges() {
        let raw = r#"{"suspicions":[{"file":"./src//a.rs","line_start":4294967296,"line_end":1,"hint":"h","confidence":0.5,"quote":"q"}],
            "needs":[{"path":"./n.rs","reason":"r"},{"path":"../n.rs","reason":"r"}],
            "scanned_areas":[{"file":"./src/a.rs","ranges":[" 3 - 5 ","+3-5","3-x"]},{"file":"/abs.rs","ranges":["1-1"]}]}"#;
        let (out, stats) = parse_scan_output_with_stats(raw).unwrap();
        let s = &out.suspicions[0];
        assert_eq!(s.file, "src/a.rs");
        assert_eq!((s.line_start, s.line_end), (u32::MAX, u32::MAX), "clamp 到 u32 域而非截断成 0");
        assert_eq!(out.needs.len(), 1);
        assert_eq!(out.needs[0].path, "n.rs");
        assert_eq!(stats.needs_dropped, 1);
        assert_eq!(out.scanned_areas.len(), 1);
        assert_eq!((out.scanned_areas[0].file.as_str(), out.scanned_areas[0].ranges.as_slice()), ("src/a.rs", ["3-5".to_string()].as_slice()));
        assert_eq!((stats.ranges_dropped, stats.dropped_malformed), (2, 1));
    }

    /// 合并后修补：行号数字串 / 浮点也认；needs.kind 大小写宽容。
    #[test]
    fn scan_parse_tolerates_numeric_strings_and_kind_case() {
        let raw = r#"{"suspicions":[{"file":"a.rs","line_start":"7","line_end":9.0,"hint":"h","confidence":0.5,"quote":"q"}],
            "needs":[{"path":"n.rs","reason":"r","kind":"Evidence"},{"path":"m.rs","reason":"r","kind":" API "}]}"#;
        let (out, stats) = parse_scan_output_with_stats(raw).unwrap();
        assert_eq!((out.suspicions[0].line_start, out.suspicions[0].line_end), (7, 9));
        assert_eq!(out.needs.len(), 2, "{stats:?}");
        assert_eq!((out.needs[0].kind, out.needs[1].kind), (crate::domain::NeedKind::Evidence, crate::domain::NeedKind::Api));
        assert_eq!(stats.needs_dropped, 0);
    }

    #[test]
    fn parse_finds_object_in_prose() {
        let raw = "Here is the result: {\"findings\":[{\"severity\":\"low\",\"title\":\"t\",\"body\":\"b\",\"file\":\"a\",\"line_start\":1,\"line_end\":1,\"confidence\":0.5,\"recommendation\":\"r\"}]} done";
        assert_eq!(parse_lens_output(raw).unwrap().findings.len(), 1);
    }

    #[test]
    fn parse_garbage_errors() {
        assert!(parse_lens_output("no json here").is_err());
    }

    /// 回归：思考型模型（MiniMax-M3/DeepSeek-R1）收尾轮 <think> 块含裸 '{' → 剥到 </think> 后再解析。
    #[test]
    fn parse_strips_reasoning_think_block_with_braces() {
        let raw = "<think>The diff adds fn on_meta(&self) { work(); } so I check callers { maybe }</think>\n{\"findings\":[{\"severity\":\"low\",\"title\":\"t\",\"body\":\"b\",\"file\":\"a.rs\",\"line_start\":1,\"line_end\":1,\"confidence\":0.5,\"recommendation\":\"r\"}]}";
        let out = parse_lens_output(raw).unwrap();
        assert_eq!(out.findings.len(), 1);
    }

    /// 回归：recommendation 含反引号时 fence 剥离不应截断 JSON。
    #[test]
    fn parse_backtick_in_recommendation_survives_fence_strip() {
        let raw = "```json\n{\"findings\":[{\"severity\":\"low\",\"title\":\"t\",\"body\":\"b\",\"file\":\"a.rs\",\"line_start\":1,\"line_end\":1,\"confidence\":0.8,\"recommendation\":\"use `foo` instead\"}]}\n```";
        let out = parse_lens_output(raw).unwrap();
        assert_eq!(out.findings.len(), 1);
        assert!(
            out.findings[0].recommendation.contains('`'),
            "backtick in recommendation should survive: got {:?}",
            out.findings[0].recommendation
        );
    }

    /// 回归：finding.body 内含不配平的 `{`（模型引用半行 Rust 代码时常见，如
    /// `match x {` 未闭合）时，裸括号深度计数会数串内的 `{`，导致抠不出合法
    /// JSON。散文前言 + ```json 围栏模拟 DeepSeek 自收敛输出。
    #[test]
    fn parse_survives_unbalanced_brace_in_body_string() {
        let raw = "Here is my analysis of the code.\n\n```json\n{\"findings\":[{\"severity\":\"low\",\"title\":\"t\",\"body\":\"code: match x { without close\",\"file\":\"a.rs\",\"line_start\":1,\"line_end\":1,\"confidence\":0.5,\"recommendation\":\"r\"}]}\n```\n\nHope that helps!";
        let out = parse_lens_output(raw).unwrap();
        assert_eq!(out.findings.len(), 1);
        assert!(out.findings[0].body.contains('{'));
    }

    // D1 四锚：200+error 体误分类修复。兼容网关把错误塞进 HTTP 200 body，
    // send_classified 判"成功"、旧逻辑 parse_*_turn 拿不到 content/choices → None → Parse
    // （不可重试，烧格）。error_body_to_provider_error 在 parse 之前先探这层。
    #[test]
    fn error_body_openai_rate_limit_is_retryable() {
        let v = json!({"error": {"type": "rate_limit_error", "message": "slow down"}});
        let e = error_body_to_provider_error(&v).unwrap();
        assert!(matches!(e, ProviderError::RateLimited { .. }));
        assert!(e.is_retryable());
    }

    #[test]
    fn error_body_anthropic_overloaded_is_retryable() {
        let v = json!({"type": "error", "error": {"type": "overloaded_error", "message": "capacity"}});
        let e = error_body_to_provider_error(&v).unwrap();
        assert!(matches!(e, ProviderError::RateLimited { .. }));
        assert!(e.is_retryable());
    }

    #[test]
    fn error_body_auth_is_not_retryable() {
        let v = json!({"error": {"type": "authentication_error", "message": "invalid key"}});
        let e = error_body_to_provider_error(&v).unwrap();
        assert!(matches!(e, ProviderError::Auth(_)));
        assert!(!e.is_retryable());
    }

    #[test]
    fn error_body_normal_findings_body_is_none() {
        let v = json!({"findings": []});
        assert!(error_body_to_provider_error(&v).is_none());
    }

    /// D1 回归：兼容网关在成功响应保留 error:null 占位（如某些 OpenAI 代理网关）
    /// 应短路返回 None，不误判为错误体 Unavailable。
    #[test]
    fn error_body_null_field_is_none() {
        let v = json!({"error": null, "choices": [{"message": {"content": "ok"}}]});
        assert!(error_body_to_provider_error(&v).is_none());
    }

    /// D1 回归：兼容网关用非对象占位符（error:""、error:[] 等）在成功响应里
    /// 应短路返回 None，不误判为错误体。
    #[test]
    fn error_body_non_object_placeholder_is_none() {
        let v = json!({"error": "", "choices": [{"message": {"content": "ok"}}]});
        assert!(error_body_to_provider_error(&v).is_none());
    }

    /// 验证空 error 对象（真 error 但无类型字段）保守判为 Unavailable 可重试。
    #[test]
    fn error_body_empty_object_is_unavailable() {
        let v = json!({"error": {}});
        let e = error_body_to_provider_error(&v).unwrap();
        assert!(matches!(e, ProviderError::Unavailable(_)));
        assert!(e.is_retryable(), "空 error 对象应保守可重试");
    }

    // 覆盖映射表其余档：server_error/api_error→Server、invalid_request/context_length→
    // BadRequest（不可重试）、未知类型→Unavailable（保守可重试）。
    #[test]
    fn error_body_maps_remaining_types() {
        let server = json!({"error": {"type": "server_error", "message": "boom"}});
        assert!(matches!(error_body_to_provider_error(&server).unwrap(), ProviderError::Server(_)));

        let api_err = json!({"error": {"type": "api_error", "message": "boom"}});
        assert!(matches!(error_body_to_provider_error(&api_err).unwrap(), ProviderError::Server(_)));

        let timeout = json!({"error": {"type": "timeout_error", "message": "slow"}});
        let t = error_body_to_provider_error(&timeout).unwrap();
        assert!(matches!(t, ProviderError::Timeout(_)));
        assert!(t.is_retryable());

        let bad_req = json!({"error": {"type": "invalid_request_error", "message": "bad shape"}});
        let b = error_body_to_provider_error(&bad_req).unwrap();
        assert!(matches!(b, ProviderError::BadRequest(_)));
        assert!(!b.is_retryable());

        let ctx = json!({"error": {"type": "context_length_exceeded", "message": "too long"}});
        assert!(matches!(error_body_to_provider_error(&ctx).unwrap(), ProviderError::BadRequest(_)));

        let unknown = json!({"error": {"type": "some_new_error_type", "message": "???"}});
        let u = error_body_to_provider_error(&unknown).unwrap();
        assert!(matches!(u, ProviderError::Unavailable(_)));
        assert!(u.is_retryable(), "未知 error 类型必须保守可重试");
    }

    // error.message/type 须体现在 Display 里，便于诊断（不可只留一句 "Unavailable"）。
    #[test]
    fn error_body_message_carried_in_display() {
        let v = json!({"error": {"type": "server_error", "message": "upstream exploded"}});
        let e = error_body_to_provider_error(&v).unwrap();
        let s = e.to_string();
        assert!(s.contains("upstream exploded"), "错误消息应携带，got: {s}");
        assert!(s.contains("server_error"), "错误类型应携带，got: {s}");
    }

    #[test]
    fn parse_lens_output_reads_optional_needs() {
        let with = r#"{"findings": [], "needs": [{"path": "src/state/store.rs", "lines": "40-90", "reason": "verify write ordering"}]}"#;
        let o = parse_lens_output(with).unwrap();
        assert_eq!(o.needs.len(), 1);
        assert_eq!(o.needs[0].path, "src/state/store.rs");
        assert_eq!(o.needs[0].lines.as_deref(), Some("40-90"));
        // 旧输出无 needs 字段 → 空 vec（向后兼容）
        let without = r#"{"findings": []}"#;
        assert!(parse_lens_output(without).unwrap().needs.is_empty());
    }
}
