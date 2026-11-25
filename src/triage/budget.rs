//! triage 预算模型 v1（2026-11-24 用户"全默认"）。
//!
//! 规模只决定**预算包线**与分片数，不预测 LLM 实际耗时：跑得快就提前结束，预算只是上限。
//! 常量锚点 = GLM-5.4 high 的实测（censored：34KB slice >172s、64KB slice >321s）。
//! 全部标 `[经验值 v1]`，后续用 `axes[].elapsed_ms` 的真实数据校准。
//!
//! v2（2026-11-25）：新增**整轮**预算包线 [`total_estimate_s`]（B2 经验系数），
//! 只用于把 deadline 收到 `min(manifest, started + estimate)`；仍不预测模型耗时。

use std::time::Duration;

pub const BASE_CELL_S: f64 = 60.0;
pub const PER_KB_S: f64 = 6.0;
pub const SAFETY: f64 = 1.15;
pub const MIN_CELL_S: f64 = 120.0;
pub const MAX_CELL_S: f64 = 600.0;

pub const I0_BASE_S: f64 = 120.0;
pub const I0_MAX_S: f64 = 240.0;
pub const I0_PER_KB_S: f64 = 2.0;

/// 目标单切片大小；分片数 = min(4, ceil(total_kb/45), ceil(files/6))。
pub const TARGET_SLICE_KB: f64 = 45.0;
pub const MAX_SHARDS: usize = 4;

/// intent slice 除 diff 外的固定开销（claims 列表 + 输出合并）。
pub const INTENT_EXTRA_S: f64 = 20.0;
pub const INTENT_PER_CLAIM_S: f64 = 0.5;
/// drift 除 diff 外的 standards pack 处理成本。
pub const DRIFT_PER_PACK_KB_S: f64 = 1.0;

/// ── 整轮预算包线 v2（2026-11-25 用户批准 B2）──
///
/// `base + per_diff_kb×diff_kb + per_file×files + per_text_kb×text_kb`。
/// `per_diff_kb` 在 B1 档复用单 cell 的 [`PER_KB_S`]（6.0），B2 档上调到 7.5；其余为经验系数。
/// 这是**包线**，不是耗时预测：跑得快照常提前结束，manifest 仍是硬上限。
pub const TOTAL_BASE_S: f64 = 220.0;
pub const TOTAL_PER_KB_S: f64 = 7.5;
pub const TOTAL_PER_FILE_S: f64 = 7.5;
pub const TOTAL_PER_TEXT_KB_S: f64 = 2.5;
/// 整轮包线下限；manifest 更小时以 manifest 为准。
pub const TOTAL_MIN_S: f64 = 900.0;

/// 总预算放大系数：1200s 为基准（1.0），最多 1.5×（1800s）。
/// 只放大 per-cell 包线；实际完成时间仍由模型决定。
pub fn time_factor(manifest_cap_s: u64) -> f64 {
    (manifest_cap_s as f64 / 1200.0).clamp(1.0, 1.5)
}

/// I0 随总预算放大；上限 360s。
pub fn scaled_i0_timeout(text_kb: f64, factor: f64) -> Duration {
    i0_timeout(text_kb)
        .mul_f64(factor.clamp(1.0, 1.5))
        .min(Duration::from_secs(360))
}

/// 应用总预算系数后的单 cell 预算；上限随系数放宽到 900s。
pub fn scaled_cell_timeout(diff_kb: f64, extra_s: f64, factor: f64) -> Duration {
    let d = cell_timeout(diff_kb, extra_s).mul_f64(factor.clamp(1.0, 1.5));
    d.min(Duration::from_secs(900))
}

/// 字节 → KB（f64，避免整除丢精度）。
pub fn kb(bytes: usize) -> f64 {
    bytes as f64 / 1024.0
}

/// 单 cell 预算：`clamp((BASE + PER_KB * (diff_kb + extra_kb)) * SAFETY, MIN, MAX)`。
pub fn cell_timeout(diff_kb: f64, extra_s: f64) -> Duration {
    let raw = (BASE_CELL_S + PER_KB_S * diff_kb + extra_s) * SAFETY;
    Duration::from_secs_f64(raw.clamp(MIN_CELL_S, MAX_CELL_S))
}

/// I0（claims + overlap）预算：文本规模线性，60–180s。
pub fn i0_timeout(text_kb: f64) -> Duration {
    Duration::from_secs_f64((I0_BASE_S + I0_PER_KB_S * text_kb).clamp(I0_BASE_S, I0_MAX_S))
}

/// 分片数：规模与文件数双重上限，1–4。
pub fn shard_count(total_diff_kb: f64, files: usize) -> usize {
    if files == 0 {
        return 1;
    }
    let by_size = (total_diff_kb / TARGET_SLICE_KB).ceil().max(1.0) as usize;
    let by_files = files.div_ceil(6);
    by_size.min(by_files).clamp(1, MAX_SHARDS)
}

/// 一个 cell 的 diff 规模（KB）。
pub fn slice_kb(files: &[(String, String)]) -> f64 {
    kb(files.iter().map(|(_, d)| d.len()).sum())
}

/// 整轮预算包线：`clamp(BASE + PER_KB×diff + PER_FILE×files + PER_TEXT_KB×text, MIN, manifest)`。
///
/// `TOTAL_MIN_S` 也受 manifest 约束（manifest 更小时取 manifest），因此结果恒 ≤ manifest。
/// 用 `ceil` 向上取整：包线只收紧，不因小数舍入少给时间。
pub fn total_estimate_s(diff_kb: f64, files: usize, text_kb: f64, manifest_cap_s: u64) -> u64 {
    let raw = TOTAL_BASE_S
        + TOTAL_PER_KB_S * diff_kb
        + TOTAL_PER_FILE_S * files as f64
        + TOTAL_PER_TEXT_KB_S * text_kb;
    // 先把浮点包线上取整成 u64，再与 manifest 取 min：不把 u64 manifest 转 f64，
    // 避免 >2^53 时舍入导致结果超过硬上限。
    let estimate = raw.max(TOTAL_MIN_S).ceil() as u64;
    estimate.min(manifest_cap_s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_timeout_matches_the_glm54_anchors() {
        // 44KB → (60+264)*1.15 ≈ 373s；34KB 的包线在 321s 之上。
        assert_eq!(cell_timeout(44.0, 0.0).as_secs(), 372);
        assert_eq!(cell_timeout(30.0, 0.0).as_secs(), 276);
        assert_eq!(cell_timeout(10.0, 0.0).as_secs(), 138);
    }

    #[test]
    fn cell_timeout_is_clamped() {
        assert_eq!(cell_timeout(0.0, 0.0).as_secs(), 120, "下限");
        assert_eq!(cell_timeout(10_000.0, 0.0).as_secs(), 600, "上限");
    }

    #[test]
    fn i0_timeout_scales_with_text() {
        // v1.1 校准：GLM-5.4 在 73.6s 内未返回 claims（in-flight），5.3 在 150s 内也未完成；
        // 基线抬到 120s、上限 240s。
        assert_eq!(i0_timeout(0.0).as_secs(), 120);
        assert_eq!(i0_timeout(5.0).as_secs(), 130);
        assert_eq!(i0_timeout(1_000.0).as_secs(), 240, "上限");
    }

    #[test]
    fn time_factor_scales_with_manifest_cap() {
        assert!((time_factor(1200) - 1.0).abs() < f64::EPSILON);
        assert!((time_factor(1800) - 1.5).abs() < f64::EPSILON);
        assert!((time_factor(600) - 1.0).abs() < f64::EPSILON, "只放大不缩小");
        assert_eq!(scaled_cell_timeout(44.0, 0.0, 1.5).as_secs(), 558);
        assert_eq!(scaled_cell_timeout(10_000.0, 0.0, 1.5).as_secs(), 900, "上限 900");
        assert_eq!(scaled_i0_timeout(5.0, 1.5).as_secs(), 195);
        assert_eq!(scaled_i0_timeout(5.0, 1.0).as_secs(), 130);
    }

    #[test]
    fn shard_count_uses_size_and_files() {
        assert_eq!(shard_count(130.0, 13), 3, "backend #333");
        assert_eq!(shard_count(91.0, 30), 3, "sites #328（旧公式是 4）");
        assert_eq!(shard_count(10.0, 2), 1);
        assert_eq!(shard_count(500.0, 100), 4, "上限 4");
        assert_eq!(shard_count(0.0, 0), 1);
    }

    #[test]
    fn total_estimate_matches_the_b2_calibration() {
        // B2 = 220 + 7.5×diff_kb + 7.5×files + 2.5×text_kb（2026-11-25 用户批准）。
        // 输入是归档的历史 diff_patch / pr_context 的真实字节数（evidence/budget-candidates.md）。
        // backend #333：1312.09… → 1313；GLM-5.4 唯一 ok 轮实测 1050.1s（1.25×）。
        assert_eq!(total_estimate_s(kb(133444), 13, kb(7054), 1800), 1313);
        // cli #18：1026.64… → 1027。
        assert_eq!(total_estimate_s(kb(80796), 28, kb(1996), 1800), 1027);
        // sites #235：2594.02… → 被 manifest 截断到 1800。
        assert_eq!(total_estimate_s(kb(259197), 62, kb(4345), 1800), 1800);
        // sites #277：1469.09… → 1470；sites #328：1132.57… → 1133。
        assert_eq!(total_estimate_s(kb(151212), 16, kb(8840), 1800), 1470);
        assert_eq!(total_estimate_s(kb(92751), 30, kb(3374), 1800), 1133);
    }

    #[test]
    fn total_estimate_floor_and_manifest_cap() {
        assert_eq!(total_estimate_s(0.0, 0, 0.0, 1800), 900, "小 PR 取下限");
        assert_eq!(total_estimate_s(1.0, 1, 0.3, 1800), 900, "小 PR 取下限");
        assert_eq!(total_estimate_s(1.0, 1, 0.3, 600), 600, "manifest 小于下限时取 manifest");
        assert_eq!(total_estimate_s(10_000.0, 500, 100.0, 1800), 1800, "大 PR 等于 manifest");
        assert_eq!(total_estimate_s(0.0, 0, 0.0, u64::MAX), 900, "u64::MAX 不做 f64 截断");
        assert_eq!(total_estimate_s(f64::MAX, 0, 0.0, u64::MAX), u64::MAX, "溢出饱和到 manifest");
        // >2^53 的 manifest 不能被 f64 舍入抬高：旧实现会返回 cap+1。
        let cap = (1_u64 << 53) + 3;
        assert_eq!(total_estimate_s(f64::MAX, 0, 0.0, cap), cap);
    }
}
