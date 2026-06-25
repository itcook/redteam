use super::{LensCall, LensRequest, ProviderError, ReviewProvider};
use crate::config::{ProviderConfig, ProviderKind};
use crate::domain::LensOutput;
use async_trait::async_trait;

pub struct AisdkProvider {
    pub kind: ProviderKind,
    pub model: String,
    pub base_url: Option<String>,
    pub api_key: String,
}

impl AisdkProvider {
    pub fn from_config(pc: &ProviderConfig, api_key: String) -> Self {
        AisdkProvider {
            kind: pc.kind,
            model: pc.model.clone(),
            base_url: pc.base_url.clone(),
            api_key,
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

#[async_trait]
impl ReviewProvider for AisdkProvider {
    async fn run(&self, req: LensRequest) -> Result<LensCall, ProviderError> {
        // 用 aisdk 发请求；原生 schema 优先，失败回落兜底解析。
        // 文档 API：LanguageModelRequest::builder().model(..).system(..).prompt(..).schema::<LensOutput>().generate_text()
        // 这里以"取回文本 → parse_lens_output"的统一兜底路径实现，保证模型无关。
        let text = call_model_text(self, &req.prompt)
            .await
            .map_err(|e| ProviderError::Unavailable(e.to_string()))?;
        let out = parse_lens_output(&text)?;
        Ok(LensCall {
            findings: out.findings,
            input_tokens: 0,
            output_tokens: 0,
        })
    }
}

/// 用 aisdk 取回模型文本。实现期按 kind 走原生 schema/自定义 base_url/key 注入。
async fn call_model_text(p: &AisdkProvider, prompt: &str) -> anyhow::Result<String> {
    // 伪实现骨架（实现期用 aisdk 真实 builder 填充，按 p.kind 选 provider、p.base_url/p.api_key 注入 header）：
    // let req = aisdk::core::LanguageModelRequest::builder()
    //     .model(select_aisdk_model(p))
    //     .system("Return ONLY JSON matching the schema.")
    //     .prompt(prompt)
    //     .schema::<crate::domain::LensOutput>()
    //     .build();
    // Ok(req.generate_text().await?.text().unwrap_or_default())
    let _ = (p, prompt);
    anyhow::bail!("aisdk call not wired in MVP test path");
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
}
