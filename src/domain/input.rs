use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode { Code, Doc }

/// 喂给模型的统一证据包。
#[derive(Clone, Debug)]
pub struct ReviewInput {
    pub mode: Mode,
    pub target_label: String,
    pub collection_guidance: String, // {{REVIEW_COLLECTION_GUIDANCE}}
    pub body: String,                // {{REVIEW_INPUT}}
    pub spec_docs: Vec<String>,      // 关联 spec 路径（L0 用）
}
