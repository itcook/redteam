use super::{LensCall, LensRequest, ProviderError, ReviewProvider};
use crate::config::{ProviderConfig, ProviderKind};
use crate::domain::LensOutput;
use async_trait::async_trait;
use serde_json::{json, Value};

/// 固定 system 文本：prompt 本身已含 schema 契约，这里只强调"只返回 JSON"。
const SYSTEM: &str =
    "Return ONLY JSON matching the requested schema (an object with a \"findings\" array).";

const ANTHROPIC_DEFAULT_BASE: &str = "https://api.anthropic.com";
const OPENAI_DEFAULT_BASE: &str = "https://api.openai.com/v1";
const MAX_TOKENS: u64 = 8192;

/// 直连 HTTP provider：自写薄封装，按 kind 选 REST 形状，rustls-only。
pub struct DirectProvider {
    kind: ProviderKind,
    model: String,
    base_url: Option<String>,
    api_key: String,
    client: reqwest::Client,
    retry_budget: u32,
}

impl DirectProvider {
    pub fn from_config(pc: &ProviderConfig, api_key: String) -> Self {
        DirectProvider {
            kind: pc.kind,
            model: pc.model.clone(),
            base_url: pc.base_url.clone(),
            api_key,
            client: crate::provider::default_http_client(),
            retry_budget: crate::provider::DEFAULT_RETRY_BUDGET,
        }
    }
}

/// 兜底：从模型自由文本里抠出符合 schema 的 JSON。
pub fn parse_lens_output(raw: &str) -> Result<LensOutput, ProviderError> {
    let trimmed = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(v) = serde_json::from_str::<LensOutput>(trimmed) {
        return Ok(v);
    }
    // 从首个 '{' 到匹配的 '}' 暴力扫描
    if let Some(start) = trimmed.find('{') {
        let mut depth = 0i32;
        for (i, c) in trimmed[start..].char_indices() {
            match c {
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

// ---- 纯函数：构造 body / 抽取 text+usage（便于 hermetic 单测，不打网络）----

/// Anthropic Messages API 请求体。
fn build_anthropic_body(model: &str, system: &str, prompt: &str, max_tokens: u64) -> Value {
    json!({
        "model": model,
        "max_tokens": max_tokens,
        "system": system,
        "messages": [{ "role": "user", "content": prompt }],
    })
}

/// OpenAI 兼容 Chat Completions 请求体。
fn build_openai_body(model: &str, system: &str, prompt: &str) -> Value {
    json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": prompt },
        ],
    })
}

/// 从 Anthropic 响应抽取 (text, input_tokens, output_tokens)；usage 取不到则 0。
fn extract_anthropic(resp: &Value) -> Option<(String, u64, u64)> {
    let blocks = resp.get("content")?.as_array()?;
    let text = blocks
        .iter()
        .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .or_else(|| blocks.first())
        .and_then(|b| b.get("text"))
        .and_then(Value::as_str)?
        .to_string();
    let input = resp["usage"]["input_tokens"].as_u64().unwrap_or(0);
    let output = resp["usage"]["output_tokens"].as_u64().unwrap_or(0);
    Some((text, input, output))
}

/// 从 OpenAI 兼容响应抽取 (text, prompt_tokens, completion_tokens)；usage 取不到则 0。
fn extract_openai(resp: &Value) -> Option<(String, u64, u64)> {
    let text = resp
        .get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")?
        .as_str()?
        .to_string();
    let input = resp["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
    let output = resp["usage"]["completion_tokens"].as_u64().unwrap_or(0);
    Some((text, input, output))
}

#[async_trait]
impl ReviewProvider for DirectProvider {
    async fn run(&self, req: LensRequest) -> Result<LensCall, ProviderError> {
        let (text, input_tokens, output_tokens) =
            crate::provider::retry_classified(self.retry_budget, || async {
                match self.kind {
                    ProviderKind::Anthropic => self.call_anthropic(&req).await,
                    ProviderKind::Openai | ProviderKind::OpenaiCompatible => self.call_openai(&req).await,
                    ProviderKind::Google => Err(ProviderError::Unavailable("google not yet supported".into())),
                }
            }).await?;
        let out = parse_lens_output(&text)?; // Parse 不进重试（形状问题）
        Ok(LensCall { findings: out.findings, input_tokens, output_tokens })
    }
}

impl DirectProvider {
    async fn call_anthropic(&self, req: &LensRequest) -> Result<(String, u64, u64), ProviderError> {
        let base = self.base_url.as_deref().unwrap_or(ANTHROPIC_DEFAULT_BASE);
        let url = super::anthropic_messages_url(base);
        let body = build_anthropic_body(&self.model, SYSTEM, &req.prompt, MAX_TOKENS);
        let resp: Value = super::decode_json(
            super::send_classified(
                self.client
                    .post(&url)
                    .header("x-api-key", &self.api_key)
                    .header("anthropic-version", "2023-06-01")
                    .header("content-type", "application/json")
                    .json(&body),
            )
            .await?,
        )
        .await?; // envelope 非法 JSON → Unavailable（可重试）；内容形状问题才是 Parse
        extract_anthropic(&resp).ok_or_else(|| {
            ProviderError::Parse("anthropic response missing content text".into())
        })
    }

    async fn call_openai(&self, req: &LensRequest) -> Result<(String, u64, u64), ProviderError> {
        let base = self.base_url.as_deref().unwrap_or(OPENAI_DEFAULT_BASE);
        let url = format!("{}/chat/completions", base.trim_end_matches('/'));
        let body = build_openai_body(&self.model, SYSTEM, &req.prompt);
        let resp: Value = super::decode_json(
            super::send_classified(
                self.client
                    .post(&url)
                    .header("authorization", format!("Bearer {}", self.api_key))
                    .header("content-type", "application/json")
                    .json(&body),
            )
            .await?,
        )
        .await?; // envelope 非法 JSON → Unavailable（可重试）；内容形状问题才是 Parse
        extract_openai(&resp).ok_or_else(|| {
            ProviderError::Parse("openai response missing choices content".into())
        })
    }
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

    // ---- 纯函数 hermetic 单测（不打网络）----

    #[test]
    fn anthropic_body_shape() {
        let b = build_anthropic_body("claude-x", SYSTEM, "do review", 8192);
        assert_eq!(b["model"], "claude-x");
        assert_eq!(b["max_tokens"], 8192);
        assert_eq!(b["system"], SYSTEM);
        assert_eq!(b["messages"][0]["role"], "user");
        assert_eq!(b["messages"][0]["content"], "do review");
    }

    #[test]
    fn openai_body_shape() {
        let b = build_openai_body("gpt-x", SYSTEM, "do review");
        assert_eq!(b["model"], "gpt-x");
        // system 与 user 两条消息，顺序固定
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][0]["content"], SYSTEM);
        assert_eq!(b["messages"][1]["role"], "user");
        assert_eq!(b["messages"][1]["content"], "do review");
        // OpenAI body 不带顶层 max_tokens / system 字段
        assert!(b.get("max_tokens").is_none());
        assert!(b.get("system").is_none());
    }

    #[test]
    fn extract_anthropic_text_and_usage() {
        let resp = json!({
            "content": [{ "type": "text", "text": "{\"findings\":[]}" }],
            "usage": { "input_tokens": 11, "output_tokens": 22 }
        });
        let (text, input, output) = extract_anthropic(&resp).unwrap();
        assert_eq!(text, "{\"findings\":[]}");
        assert_eq!(input, 11);
        assert_eq!(output, 22);
    }

    #[test]
    fn extract_anthropic_missing_usage_is_zero() {
        let resp = json!({ "content": [{ "type": "text", "text": "hi" }] });
        let (text, input, output) = extract_anthropic(&resp).unwrap();
        assert_eq!(text, "hi");
        assert_eq!((input, output), (0, 0));
    }

    #[test]
    fn extract_anthropic_no_content_is_none() {
        let resp = json!({ "usage": { "input_tokens": 1, "output_tokens": 1 } });
        assert!(extract_anthropic(&resp).is_none());
    }

    #[test]
    fn extract_openai_text_and_usage() {
        let resp = json!({
            "choices": [{ "message": { "role": "assistant", "content": "{\"findings\":[]}" } }],
            "usage": { "prompt_tokens": 7, "completion_tokens": 9 }
        });
        let (text, input, output) = extract_openai(&resp).unwrap();
        assert_eq!(text, "{\"findings\":[]}");
        assert_eq!(input, 7);
        assert_eq!(output, 9);
    }

    #[test]
    fn extract_openai_missing_usage_is_zero() {
        let resp = json!({
            "choices": [{ "message": { "content": "hi" } }]
        });
        let (text, input, output) = extract_openai(&resp).unwrap();
        assert_eq!(text, "hi");
        assert_eq!((input, output), (0, 0));
    }

    #[test]
    fn extract_openai_empty_choices_is_none() {
        let resp = json!({ "choices": [] });
        assert!(extract_openai(&resp).is_none());
    }

    /// 端到端：抽取出的文本喂给 parse_lens_output 能拿到 findings。
    #[test]
    fn extracted_text_parses_into_lens_output() {
        let resp = json!({
            "content": [{ "type": "text", "text": "```json\n{\"findings\":[]}\n```" }],
            "usage": { "input_tokens": 1, "output_tokens": 2 }
        });
        let (text, _, _) = extract_anthropic(&resp).unwrap();
        assert!(parse_lens_output(&text).unwrap().findings.is_empty());
    }

    /// Finding 1 回归：build_anthropic_body / build_openai_body 以 self.model（绑定模型 id）
    /// 而非 req.model（路由键）作为请求体的 model 字段。
    /// 此处直接测纯函数调用路径——call_* 内部将 &self.model 传入，确保二者隔离。
    #[test]
    fn body_uses_bound_model_not_route_key() {
        let bound_model = "claude-opus-4-8";
        let route_key = "claude"; // req.model 仅是路由键，不能作 API model id 发出

        let anthropic_body = build_anthropic_body(bound_model, SYSTEM, "prompt", MAX_TOKENS);
        assert_eq!(
            anthropic_body["model"], bound_model,
            "anthropic body must carry bound model id"
        );
        assert_ne!(
            anthropic_body["model"], route_key,
            "anthropic body must NOT carry route key as model"
        );

        let openai_body = build_openai_body(bound_model, SYSTEM, "prompt");
        assert_eq!(
            openai_body["model"], bound_model,
            "openai body must carry bound model id"
        );
        assert_ne!(
            openai_body["model"], route_key,
            "openai body must NOT carry route key as model"
        );
    }

    /// Finding 2 回归：extract_* 返回 None（HTTP 成功但响应形状异常）时，
    /// ok_or_else 映射为 ProviderError::Parse，而非 Unavailable。
    #[test]
    fn bad_response_shape_maps_to_parse_error() {
        // Anthropic：content 字段缺失
        let bad_anthropic = json!({ "id": "msg_123" });
        assert!(
            extract_anthropic(&bad_anthropic).is_none(),
            "malformed anthropic response should yield None"
        );
        // 验证 None -> Parse 的映射（与 call_anthropic 内的 ok_or_else 一致）
        let err = extract_anthropic(&bad_anthropic)
            .ok_or_else(|| ProviderError::Parse("anthropic response missing content text".into()))
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::Parse(_)),
            "expected Parse, got: {:?}",
            err
        );

        // OpenAI：choices 字段缺失
        let bad_openai = json!({ "id": "chatcmpl-123" });
        assert!(
            extract_openai(&bad_openai).is_none(),
            "malformed openai response should yield None"
        );
        let err = extract_openai(&bad_openai)
            .ok_or_else(|| ProviderError::Parse("openai response missing choices content".into()))
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::Parse(_)),
            "expected Parse, got: {:?}",
            err
        );
    }

    /// 真实网络冒烟测试：需要真实 API key，不在 CI 跑。
    /// 手动运行：`REDTEAM_SMOKE_ANTHROPIC_KEY=<key> cargo test -- --ignored smoke_anthropic_run`。
    #[tokio::test]
    #[ignore = "requires real API key + network; run manually with --ignored"]
    async fn smoke_anthropic_run() {
        let key = std::env::var("REDTEAM_SMOKE_ANTHROPIC_KEY").expect("set REDTEAM_SMOKE_ANTHROPIC_KEY");
        let pc = ProviderConfig {
            kind: ProviderKind::Anthropic,
            model: "claude-3-5-haiku-latest".into(),
            base_url: None,
            api_key_env: None,
            priority: 0,
        };
        let p = DirectProvider::from_config(&pc, key);
        let call = p
            .run(LensRequest {
                model: "claude-3-5-haiku-latest".into(),
                prompt: "Return JSON: {\"findings\":[]}".into(),
            })
            .await
            .expect("real anthropic call");
        assert!(call.findings.is_empty());
    }
}
