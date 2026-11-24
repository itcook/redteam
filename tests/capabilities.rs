//! `capabilities/redteam.json` 的钉子（计划 §5 出口 / hibot 契约 §4）。
//!
//! 契约原话：`kinds` 是**声明，不是愿望**——hibot 只会派已声明的类型。
//! Phase 3 完成后声明 `review` + `triage`；`verify` 仍等 Phase 4。

use serde_json::Value;

fn capabilities() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities/redteam.json");
    serde_json::from_slice(&std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .expect("capabilities/redteam.json must be valid JSON")
}

#[test]
fn declares_only_implemented_kinds() {
    let c = capabilities();
    assert_eq!(c["schema"], "hibot.audit.executor/1");
    assert_eq!(c["name"], "redteam");
    assert_eq!(
        c["kinds"],
        serde_json::json!(["review", "triage"]),
        "triage 已实现（Phase 3）；verify 尚未实现（Phase 4）——提前声明即违约（hibot 只派已声明类型）"
    );
}

#[test]
fn version_matches_the_crate() {
    let c = capabilities();
    assert_eq!(
        c["version"], env!("CARGO_PKG_VERSION"),
        "capabilities.version 必须与 CARGO_PKG_VERSION 同源（升版时一起改）"
    );
}

#[test]
fn form_and_delivery_follow_the_contract() {
    let c = capabilities();
    assert_eq!(c["form"], "cli", "v1 只允许命令行程序");
    assert_eq!(c["config_delivery"], "job_native", "job 模式无配置 flag（Q16）");
    let req = &c["requires"];
    assert_eq!(req["repo_checkout"], true, "评审要读检出");
    assert_eq!(req["pr_context"], false, "review 不依赖 pr-context（Q4）");
    assert_eq!(req["network"], "provider-only", "只调模型 provider");
    assert!(c["invoke"].as_str().unwrap().contains("--job-dir"));
}

#[test]
fn declares_the_config_schema_command() {
    let c = capabilities();
    let cs = &c["extensions"]["config_schema"];
    assert_eq!(cs["command"], "redteam config schema");
    assert_eq!(cs["schemaVersion"], 1);
}
