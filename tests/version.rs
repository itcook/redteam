//! 版本单一来源的钉子（计划 §10 Phase 7 / 7.1）。
//!
//! 契约：`Cargo.toml` 是唯一真相，以下四处必须一致，二进制 `--version` 也必须报它：
//!   - `plugins/redteam/.claude-plugin/plugin.json`
//!   - `plugins/redteam/.codex-plugin/plugin.json`
//!   - `capabilities/redteam.json`（hibot 契约；tests/capabilities.rs 另有同义断言）
//!   - `redteam --version`
//!
//! 另外守 `CHANGELOG.md` 顶层必须是 `## [Unreleased]`（release notes 提取的锚点）。

use std::process::Command;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn read(relative: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn json(relative: &str) -> serde_json::Value {
    serde_json::from_str(&read(relative)).unwrap_or_else(|e| panic!("parse {relative}: {e}"))
}

#[test]
fn plugin_manifests_share_the_crate_version() {
    for relative in [
        "plugins/redteam/.claude-plugin/plugin.json",
        "plugins/redteam/.codex-plugin/plugin.json",
    ] {
        assert_eq!(
            json(relative)["version"], VERSION,
            "{relative}: 版本必须由 `make gen`（scripts/sync-version）从 Cargo.toml 派生，禁止手改"
        );
    }
}

#[test]
fn capabilities_share_the_crate_version() {
    assert_eq!(
        json("capabilities/redteam.json")["version"], VERSION,
        "capabilities/redteam.json: 版本必须与 Cargo.toml 同源（`make gen` 同步）"
    );
}

#[test]
fn binary_reports_the_crate_version() {
    let exe = env!("CARGO_BIN_EXE_redteam");
    let output = Command::new(exe).arg("--version").output().unwrap_or_else(|e| panic!("run {exe}: {e}"));
    assert!(output.status.success(), "--version 退出码 {:?}", output.status.code());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        format!("redteam {VERSION}"),
        "二进制 --version 必须与 Cargo.toml 同源（cli 的 #[command(version)]）"
    );
}

#[test]
fn changelog_starts_with_unreleased() {
    let changelog = read("CHANGELOG.md");
    let first_section = changelog
        .lines()
        .find(|line| line.starts_with("## "))
        .unwrap_or_else(|| panic!("CHANGELOG.md 缺少 `## [..]` 段"));
    assert_eq!(
        first_section.trim(),
        "## [Unreleased]",
        "CHANGELOG.md 顶层必须是 [Unreleased]（release.sh 从对应版本段提取 notes）"
    );
}
