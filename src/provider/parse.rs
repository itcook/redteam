//! 模型输出 → `LensOutput` 的解析与纠错逻辑。single_shot（原 direct）与 tool_loop
//! 共享的活解析器落地于此，防止两处各自重实现导致行为分叉（B-Q3 迁移）。

use super::tool_loop::AssistantTurn;
use super::ProviderError;
use crate::domain::LensOutput;
use serde_json::Value;

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
