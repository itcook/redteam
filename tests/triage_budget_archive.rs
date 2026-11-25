//! triage 整轮预算：归档输入的端到端对拍（2026-11-25）。
//!
//! 读 `.trellis/tasks/11-25-hibot-triage-budget/evidence/` 的历史 `diff_patch` / `pr_context`，
//! 走 `projection::from_patch` → `input::parse` → `triage::total_estimate`，
//! 断言与 `evidence/budget_estimate.py` 的 B2 列一致。
//!
//! 这些输入与 `/tmp/hibot-triage-eval/jobs` 原件逐字节一致（归档时已核对 SHA256）。
//! 期望值是**向上取整**后的秒数，不是四舍五入。

use redteam::job::projection;
use redteam::triage::{self, input};

/// (repo, PR, B2 期望秒数)
const CASES: &[(&str, &str, u64)] = &[
    ("backend", "333", 1313),
    ("cli", "18", 1027),
    ("sites", "235", 1800), // 原始 2595s，被 manifest 截断
    ("sites", "277", 1470),
    ("sites", "328", 1133),
];

#[test]
fn archived_prs_match_the_b2_estimate() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".trellis/tasks/11-25-hibot-triage-budget/evidence");
    for (repo, pr, expected) in CASES {
        let patch = std::fs::read_to_string(root.join(format!("diff.hiapi-{repo}-pr-{pr}.patch")))
            .unwrap_or_else(|e| panic!("read patch {repo}#{pr}: {e}"));
        let context = std::fs::read_to_string(root.join(format!("pr-context.hiapi-{repo}-pr-{pr}.json")))
            .unwrap_or_else(|e| panic!("read pr-context {repo}#{pr}: {e}"));
        let changes = projection::from_patch(&patch).unwrap_or_else(|e| panic!("parse patch {repo}#{pr}: {e:?}"));
        let parsed = input::parse(&context).unwrap_or_else(|e| panic!("parse pr-context {repo}#{pr}: {e:?}"));
        assert_eq!(triage::total_estimate(&parsed, &changes, 1800), *expected, "{repo}#{pr}");
    }
}
