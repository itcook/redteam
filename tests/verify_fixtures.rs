//! `tests/fixtures/verify/pr400/` 的完整性（**不需要外部仓、不需要模型**，进 CI）。
//!
//! 真跑由 `scripts/verify-pr400.sh` 负责（需要本机 `~/hiapi/backend` + 一个可用模型）。本文件只钉住
//! 夹具自身的可信度：指纹按契约复算一致、`expected` 恰好覆盖输入集合、状态在四态内、场景覆盖齐、
//! 每组 ≤3 条（真跑边界）。

use redteam::job::fingerprint;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/verify/pr400")
}

fn groups() -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(fixture_root())
        .expect("fixture root")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("group-"))
        .collect();
    out.sort();
    assert!(out.len() >= 3, "至少三组（a 回归 / b 部分 / c 非代码修复）: {out:?}");
    out
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn is_sha40(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[test]
fn findings_carry_the_contract_fingerprint_and_expected_covers_them() {
    for group in groups() {
        let dir = fixture_root().join(&group);
        let findings = read_json(&dir.join("findings.json"));
        let expected = read_json(&dir.join("expected.json"));
        let items = findings.as_array().expect("findings 必须是数组");
        assert!(!items.is_empty(), "{group}: 空夹具没有意义");
        assert!(items.len() <= 3, "{group}: 真跑边界 ≤3 条/组（协调者裁）");
        let mut fps = std::collections::BTreeSet::new();
        for f in items {
            let file = f["file"].as_str().expect("file");
            let quote = f["quote"].as_str().expect("quote");
            let title = f["title"].as_str().expect("title");
            assert!(!quote.trim().is_empty(), "{group}/{file}: quote 不能为空");
            let got = f["fingerprint"].as_str().expect("fingerprint");
            assert_eq!(got, fingerprint::of(file, quote, title), "{group}/{file}: 指纹必须按契约复算一致");
            assert!(fps.insert(got.to_string()), "{group}: 指纹重复 {got}");
            // 真跑要能定位：行号是 1-based 且 start ≤ end。
            assert!(f["line_start"].as_u64().unwrap_or(0) >= 1, "{group}/{file}: line_start");
            assert!(f["line_end"].as_u64().unwrap() >= f["line_start"].as_u64().unwrap());
            assert!(!f["body"].as_str().unwrap_or("").is_empty(), "{group}/{file}: body（判决依据）不能空");
        }
        let map = expected.as_object().expect("expected 必须是对象");
        let keys: std::collections::BTreeSet<String> = map.keys().cloned().collect();
        assert_eq!(keys, fps, "{group}: expected 必须恰好覆盖 findings 的指纹集合");
        for (fp, e) in map {
            let status = e["status"].as_str().expect("status");
            assert!(
                matches!(status, "resolved" | "unresolved" | "regressed" | "inconclusive"),
                "{group}/{fp}: 非法状态 {status}"
            );
            assert!(!e["note"].as_str().unwrap_or("").is_empty(), "{group}/{fp}: 必须写人工判决依据");
        }
    }
}

#[test]
fn scenario_coverage_covers_the_hard_states() {
    let mut seen = std::collections::BTreeSet::new();
    for group in groups() {
        let dir = fixture_root().join(&group);
        let expected = read_json(&dir.join("expected.json"));
        for (_, e) in expected.as_object().unwrap() {
            seen.insert(e["status"].as_str().unwrap().to_string());
        }
    }
    for want in ["regressed", "unresolved", "resolved"] {
        assert!(seen.contains(want), "三组必须覆盖 {want}（真实夹具的价值就在这里）: {seen:?}");
    }
}

#[test]
fn meta_pins_the_commits_and_matches_the_group_dir() {
    for group in groups() {
        let dir = fixture_root().join(&group);
        let meta = read_json(&dir.join("meta.json"));
        assert_eq!(meta["group"].as_str(), Some(group.trim_start_matches("group-")), "{group}: meta.group");
        for key in ["h1", "h2"] {
            let sha = meta[key].as_str().unwrap_or_else(|| panic!("{group}: 缺 {key}"));
            assert!(is_sha40(sha), "{group}/{key}: 必须是 40 位小写 sha（实际 {sha}）");
        }
        assert_ne!(meta["h1"], meta["h2"], "{group}: H1/H2 不能相同");
        assert!(!meta["expected_source"].as_str().unwrap_or("").is_empty(), "{group}: 必须写明 expected 的人工来源");
        assert!(!meta["scenario"].as_str().unwrap_or("").is_empty(), "{group}: 必须写明场景");
    }
}

#[test]
fn the_script_exists_and_never_lands_in_ci_gates() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/verify-pr400.sh");
    assert!(script.is_file(), "真跑脚本必须在：{}", script.display());
    // 脚本不进任何 cargo/CI 门禁：这里只断言它存在且不是测试入口。
    let text = std::fs::read_to_string(&script).unwrap();
    assert!(text.contains("redteam verify"), "脚本必须真的跑 verify");
    assert!(text.contains("PR400_VERIFY_KEY"), "密钥只经临时 env 注入");
    assert!(!text.contains("echo \"$KEY\""), "绝不能把密钥打印出来");
}
