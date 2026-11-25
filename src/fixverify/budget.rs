//! verify 预算模型 v1（2026-11-25 用户裁"先拍基准值，用 #400 实测校准"）。
//!
//! 规模只决定**预算包线**，不预测 LLM 实际耗时：跑得快就提前结束，预算只是上限。
//! 常量全部 `[经验值 v1]`：一条 finding 的输入 ≈ 该文件 hunk 块 + 相关 hunk + ±60 行快照 + finding 正文
//! （典型 5–15KB、上限 ~56KB），输出只有一个 JSON 对象。
//!
//! 整体上限**不在这里**：唯一的总帽是 manifest 的 `limits.wall_clock_s`（hibot 给），再压一层只会提前
//! 产生 `inconclusive`（hibot 侧一次 inconclusive = 阻塞一整轮）。

use std::time::Duration;

pub const BASE_ITEM_S: f64 = 60.0;
pub const PER_KB_S: f64 = 4.0;
pub const SAFETY: f64 = 1.15;
pub const MIN_ITEM_S: f64 = 60.0;
pub const MAX_ITEM_S: f64 = 240.0;
/// 乘总预算系数后的硬帽（模型 `perTurnTimeoutSecs` 再大也压到这）。
pub const SCALED_CAP_S: u64 = 300;

/// 总预算放大系数：1200s 为基准（1.0），最多 1.5×；只放大不缩小（与 triage 同口径）。
pub fn time_factor(manifest_cap_s: u64) -> f64 {
    (manifest_cap_s as f64 / 1200.0).clamp(1.0, 1.5)
}

/// 字节 → KB（f64，避免整除丢精度）。
pub fn kb(bytes: usize) -> f64 {
    bytes as f64 / 1024.0
}

/// 单条预算：`clamp((BASE + PER_KB × kb) × SAFETY, MIN, MAX)`。
pub fn item_timeout(text_kb: f64) -> Duration {
    let raw = (BASE_ITEM_S + PER_KB_S * text_kb) * SAFETY;
    Duration::from_secs_f64(raw.clamp(MIN_ITEM_S, MAX_ITEM_S))
}

/// 应用总预算系数后的单条预算；硬帽 [`SCALED_CAP_S`]。
pub fn scaled_item_timeout(text_kb: f64, factor: f64) -> Duration {
    item_timeout(text_kb)
        .mul_f64(factor.clamp(1.0, 1.5))
        .min(Duration::from_secs(SCALED_CAP_S))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_timeout_scales_with_evidence_size() {
        // 5KB → (60+20)×1.15 = 92s；20KB → (60+80)×1.15 = 161s。
        assert_eq!(item_timeout(5.0).as_secs(), 92);
        assert_eq!(item_timeout(20.0).as_secs(), 161);
        assert_eq!(item_timeout(0.0).as_secs(), 69, "下限之上（60×1.15）");
    }

    #[test]
    fn item_timeout_is_clamped() {
        assert_eq!(item_timeout(10_000.0).as_secs(), 240, "上限");
    }

    #[test]
    fn scaled_timeout_amplifies_but_never_shrinks() {
        assert_eq!(scaled_item_timeout(0.0, 1.0).as_secs(), 69);
        assert_eq!(scaled_item_timeout(0.0, 1.5).as_secs(), 103);
        assert_eq!(scaled_item_timeout(10_000.0, 1.5).as_secs(), 300, "乘后硬帽");
    }

    #[test]
    fn time_factor_follows_the_manifest_cap() {
        assert!((time_factor(1200) - 1.0).abs() < f64::EPSILON);
        assert!((time_factor(600) - 1.0).abs() < f64::EPSILON, "只放大不缩小");
        assert!((time_factor(1800) - 1.5).abs() < f64::EPSILON);
        assert!((time_factor(99_999) - 1.5).abs() < f64::EPSILON);
    }
}
