//! 模型输出 → `LensOutput` 的解析与纠错逻辑。single_shot（原 direct）与 tool_loop
//! 共享的活解析器落地于此，防止两处各自重实现导致行为分叉（B-Q3 迁移）。

use super::tool_loop::AssistantTurn;
use super::ProviderError;
use crate::domain::LensOutput;

/// 兜底：从模型自由文本里抠出符合 schema 的 JSON。
pub fn parse_lens_output(raw: &str) -> Result<LensOutput, ProviderError> {
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
    if let Ok(v) = serde_json::from_str::<LensOutput>(trimmed) {
        return Ok(v);
    }
    // 从首个 '{' 到匹配的 '}' 扫描，字符串感知（跳过串内的 '{'/'}'，
    // 正确处理 '\"' 转义）——findings.body/quote 常引用半行代码含不配平括号
    // （如 "match x {" 未闭合），裸计数会数错深度、抠出非法 JSON。
    if let Some(start) = trimmed.find('{') {
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
                        if let Ok(v) = serde_json::from_str::<LensOutput>(cand) {
                            return Ok(v);
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    Err(ProviderError::Parse(format!(
        "no schema-valid JSON in: {}",
        &raw.chars().take(120).collect::<String>()
    )))
}

pub(crate) const REPAIR_PROMPT: &str = "Your previous response was not valid JSON (it may have been truncated). \
Re-emit ONLY the complete findings JSON object matching the schema. If space is tight, shorten each \
finding's body/recommendation text, but do NOT drop any finding and do NOT omit any required field.";

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

    #[test]
    fn parse_strips_code_fence() {
        let raw = "```json\n{\"findings\":[]}\n```";
        assert!(parse_lens_output(raw).unwrap().findings.is_empty());
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
