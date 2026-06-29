pub mod backends;
pub mod direct;
pub mod fake;
pub mod registry;
pub mod tool_loop;

use crate::domain::RawFinding;
use async_trait::async_trait;

#[derive(Clone, Debug)]
pub struct LensRequest { pub model: String, pub prompt: String }

#[derive(Clone, Debug)]
pub struct LensCall { pub findings: Vec<RawFinding>, pub input_tokens: u64, pub output_tokens: u64 }

#[derive(thiserror::Error, Debug)]
pub enum ProviderError {
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    #[error("structured-output parse failed: {0}")]
    Parse(String),
}

#[async_trait]
pub trait ReviewProvider: Send + Sync {
    async fn run(&self, req: LensRequest) -> Result<LensCall, ProviderError>;
}
