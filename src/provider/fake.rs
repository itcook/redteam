use super::*;
use crate::domain::RawFinding;
use std::collections::BTreeMap;

pub struct FakeProvider { pub scripted: BTreeMap<String, Vec<RawFinding>> }

impl FakeProvider {
    pub fn with(model: &str, findings: Vec<RawFinding>) -> Self {
        let mut m = BTreeMap::new();
        m.insert(model.to_string(), findings);
        FakeProvider { scripted: m }
    }
}

#[async_trait::async_trait]
impl ReviewProvider for FakeProvider {
    async fn run(&self, req: LensRequest) -> Result<LensCall, ProviderError> {
        match self.scripted.get(&req.model) {
            Some(fs) => Ok(LensCall { findings: fs.clone(), input_tokens: 0, output_tokens: 0, cached_input_tokens: 0 }),
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
