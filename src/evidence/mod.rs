pub mod doc;
pub mod git;

use crate::domain::{Mode, ReviewInput};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope { WorkingTree, Branch }

#[derive(Clone, Debug)]
pub struct EvidenceOpts {
    pub mode: Option<Mode>,
    pub base: Option<String>,
    pub scope: Scope,
    pub spec: Vec<String>,
    pub targets: Vec<String>, // doc 模式受审文档
}

/// 自动判定：显式 mode 优先；否则有 base/branch scope → code；有 targets 文档 → doc；歧义报错。
pub fn detect_mode(opts: &EvidenceOpts) -> anyhow::Result<Mode> {
    if let Some(m) = opts.mode { return Ok(m); }
    if opts.base.is_some() || opts.scope == Scope::Branch { return Ok(Mode::Code); }
    if !opts.targets.is_empty() { return Ok(Mode::Doc); }
    anyhow::bail!("无法判定 mode：请显式 --mode，或提供 --base（code）/目标文档（doc）");
}

pub fn collect(opts: &EvidenceOpts) -> anyhow::Result<ReviewInput> {
    let mode = detect_mode(opts)?;
    match mode {
        Mode::Doc => {
            let body = doc::collect_doc(&opts.targets)?;
            Ok(ReviewInput { mode, target_label: opts.targets.join(", "),
                collection_guidance: String::new(), body, spec_docs: opts.spec.clone() })
        }
        Mode::Code => {
            let (body, guidance, label) = git::collect_code(opts.base.as_deref(), opts.scope == Scope::Branch)?;
            let mut g = guidance;
            if !opts.spec.is_empty() {
                g.push_str(&format!("\n关联 spec 文档（核对实现是否偏离设计意图）：{}", opts.spec.join(", ")));
            }
            Ok(ReviewInput { mode, target_label: label, collection_guidance: g, body, spec_docs: opts.spec.clone() })
        }
    }
}
