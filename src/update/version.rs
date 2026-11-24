//! 版本归一、比较与 `latest` 解析（纯函数）。
//!
//! 语义化版本交给 `semver`：`v` 前缀只在输入边界剥离一次，比较遵循 semver 优先级
//! （build metadata 不参与）。

use std::cmp::Ordering;

use anyhow::{bail, Result};

pub use semver::Version;

/// 解析 `[v]MAJOR.MINOR.PATCH[-PRE][+BUILD]`；非法返回 None。
pub fn parse_version(input: &str) -> Option<Version> {
    Version::parse(strip_v(input.trim())).ok()
}

/// 归一为 `vX.Y.Z[-pre][+build]`；接受 `0.1.2` 与 `v0.1.2`。
pub fn normalize_version(input: &str) -> Result<String> {
    let trimmed = input.trim();
    let without_v = strip_v(trimmed);
    if Version::parse(without_v).is_err() {
        bail!("非法版本号: {input}（应形如 v0.1.0 或 0.1.0）");
    }
    Ok(format!("v{without_v}"))
}

/// 解析 `{base}/latest` 的响应体。
pub fn resolve_latest(body: &str) -> Result<String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        bail!("latest 内容为空");
    }
    normalize_version(trimmed).map_err(|_| anyhow::anyhow!("latest 内容不是合法版本号: {trimmed}"))
}

/// `latest` 是否严格新于 `current`。任一版本非法时返回 false（由调用方另行报错）。
pub fn needs_update(current: &str, latest: &str) -> bool {
    let (Some(current), Some(latest)) = (parse_version(current), parse_version(latest)) else {
        return false;
    };
    latest.cmp_precedence(&current) == Ordering::Greater
}

/// 两个版本是否等价（semver 优先级相同；build metadata 不计）。
pub fn same_version(left: &str, right: &str) -> bool {
    matches!(
        (parse_version(left), parse_version(right)),
        (Some(left), Some(right)) if left.cmp_precedence(&right) == Ordering::Equal
    )
}

/// 只剥离一次可选的 `v`/`V` 前缀：`vv1.2.3` 会留下 `v1.2.3` 并被 semver 拒绝。
fn strip_v(input: &str) -> &str {
    input
        .strip_prefix('v')
        .or_else(|| input.strip_prefix('V'))
        .unwrap_or(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_accepts_both_prefixes() {
        assert_eq!(normalize_version("0.1.2").unwrap(), "v0.1.2");
        assert_eq!(normalize_version("v0.1.2").unwrap(), "v0.1.2");
        assert_eq!(normalize_version("  V0.2.0 \n").unwrap(), "v0.2.0");
        assert_eq!(normalize_version("v0.2.0-rc.1").unwrap(), "v0.2.0-rc.1");
    }

    #[test]
    fn normalize_rejects_garbage() {
        assert!(normalize_version("latest").is_err());
        assert!(normalize_version("vv1.0.0").is_err());
        assert!(normalize_version("1.0").is_err());
    }

    #[test]
    fn update_rules_follow_semver_precedence() {
        assert!(needs_update("v0.1.0", "v0.2.0"));
        assert!(needs_update("v0.1.0", "v0.1.1"));
        assert!(!needs_update("v0.2.0", "v0.1.0"), "不自动降级");
        assert!(!needs_update("v0.1.0", "v0.1.0"));
        assert!(!needs_update("v0.1.0", "v0.1.0+build.7"), "build metadata 不参与比较");
        assert!(needs_update("v0.1.0-rc.1", "v0.1.0"), "正式版新于预发布");
        assert!(!needs_update("v0.1.0", "v0.1.0-rc.1"), "预发布不覆盖正式版");
        assert!(same_version("0.1.0", "v0.1.0"));
    }

    #[test]
    fn latest_body_is_trimmed() {
        assert_eq!(resolve_latest(" v0.3.0\n").unwrap(), "v0.3.0");
        assert!(resolve_latest("   ").is_err());
        assert!(resolve_latest("nope").is_err());
    }
}
