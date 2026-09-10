//! 风险评分器（M21b 批 A）：成区 → 四类信号 → H0 / R / 预算三档。
//! 规则即 spec：P0 §15.4 v4 定案常量化，零 LLM、零配置旋钮。

mod regions;

pub use regions::{regions, Region, RegionKind};
