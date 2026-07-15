use super::*;
use crate::domain::RawFinding;
use std::collections::BTreeMap;

#[derive(Default)]
pub struct FakeProvider {
    pub scripted: BTreeMap<String, Vec<RawFinding>>,
    /// (input, output, cached) 令每次 run() 都回报的固定 usage；测试 token 入账用，默认 (0,0,0)。
    pub tokens: (u64, u64, u64),
}

impl FakeProvider {
    pub fn with(model: &str, findings: Vec<RawFinding>) -> Self {
        let mut m = BTreeMap::new();
        m.insert(model.to_string(), findings);
        FakeProvider { scripted: m, tokens: (0, 0, 0) }
    }

    /// 同 `with`，但 run() 回报指定 usage（T03：refute usage 入账测试用）。
    pub fn with_tokens(model: &str, findings: Vec<RawFinding>, tokens: (u64, u64, u64)) -> Self {
        let mut p = Self::with(model, findings);
        p.tokens = tokens;
        p
    }
}

#[async_trait::async_trait]
impl ReviewProvider for FakeProvider {
    async fn run(&self, req: LensRequest) -> Result<LensCall, ProviderError> {
        match self.scripted.get(&req.model) {
            Some(fs) => Ok(LensCall {
                findings: fs.clone(),
                input_tokens: self.tokens.0,
                output_tokens: self.tokens.1,
                cached_input_tokens: self.tokens.2,
                iterations: 1,
                tool_use: Default::default(),
                needs: vec![],
            }),
            None => Err(ProviderError::Unavailable(format!("no script for model {}", req.model))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Severity;
    fn raw(title: &str, sev: Severity) -> RawFinding {
        RawFinding { severity: sev, title: title.into(), body: "b".into(), file: "a.rs".into(),
            line_start: 1, line_end: 2, confidence: 0.9, recommendation: "fix".into(),
            hard_constraint: false, quote: "let x = 1;".into(), lens: None }
    }
    #[tokio::test]
    async fn fake_returns_scripted_findings() {
        let p = FakeProvider::with("claude", vec![raw("boom", Severity::High)]);
        let call = p.run(LensRequest { model: "claude".into(), prompt: "x".into() }).await.unwrap();
        assert_eq!(call.findings.len(), 1);
        assert_eq!(call.findings[0].title, "boom");
    }
    #[tokio::test]
    async fn fake_unknown_model_is_unavailable() {
        let p = FakeProvider::with("claude", vec![]);
        let err = p.run(LensRequest { model: "gpt".into(), prompt: "x".into() }).await.unwrap_err();
        assert!(matches!(err, ProviderError::Unavailable(_)));
    }
}
