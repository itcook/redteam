//! `capabilities/redteam.json` 的钉子（计划 §5 出口 / hibot 契约 §4）。
//!
//! 契约原话：`kinds` 是**声明，不是愿望**——hibot 只会派已声明的类型。
//! Phase 4 完成后三 kind 齐：`review` + `triage` + `verify`。

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
        serde_json::json!(["review", "triage", "verify"]),
        "三 kind 已全部实现（Phase 4）——声明多于实现即违约（hibot 只派已声明类型）"
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
    // invoke 是给人看的调用形状示例：三 kind 同一入口，不得只写一个 kind（Phase 6 / 9.1）。
    let invoke = c["invoke"].as_str().expect("invoke must be a string");
    for kind in ["triage", "review", "verify"] {
        assert!(invoke.contains(kind), "invoke 必须覆盖 {kind}：{invoke}");
    }
    assert!(invoke.contains("--job-dir"), "invoke 必须写明 job 模式入口：{invoke}");
}

/// `requires` 是对**所有声明 kind** 的要求（hibot B4）：形状固定，多一个键就是多一条隐性派活门槛。
#[test]
fn requires_is_exactly_the_contract_set() {
    let c = capabilities();
    assert_eq!(
        c["requires"],
        serde_json::json!({"repo_checkout": true, "pr_context": false, "network": "provider-only"}),
        "requires 必须恰为 {{repo_checkout:true, pr_context:false, network:provider-only}}"
    );
}

#[test]
fn limits_declare_a_positive_wall_clock() {
    let c = capabilities();
    let secs = c["limits"]["max_wall_clock_s"].as_u64().expect("limits.max_wall_clock_s 必须是整数");
    assert!(secs >= 1, "limits.max_wall_clock_s 必须 ≥ 1（hibot 契约 minimum:1），实际 {secs}");
}

#[test]
fn declares_the_config_schema_command() {
    let c = capabilities();
    let cs = &c["extensions"]["config_schema"];
    assert_eq!(cs["command"], "redteam config schema");
    assert_eq!(cs["schemaVersion"], 2);
}

/// schema 产物与 capabilities 的版本必须同步（hibot 渲染 CI 的拒发比对点）。
#[test]
fn config_schema_version_matches_capabilities() {
    let c = capabilities();
    let cap = &c["extensions"]["config_schema"]["schemaVersion"];
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("dist/config.schema.json");
    let schema: Value = serde_json::from_slice(&std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap();
    assert_eq!(schema["schemaVersion"], *cap, "dist/config.schema.json 与 capabilities 必须同版本");
    assert_eq!(schema["schemaVersion"], 2, "v2 硬切");
    assert_eq!(schema["$id"], "urn:redteam:config/2");
    // executor 契约不含明文密钥槽；未知键在 schema 层也被拒绝（与 job 模式一致）。
    for def in ["ProviderConfig", "ModelConfig"] {
        assert!(schema["definitions"][def]["properties"].get("apiKey").is_none(), "{def} 不得暴露 apiKey");
        assert_eq!(schema["definitions"][def]["additionalProperties"], false, "{def}");
    }
    assert_eq!(schema["additionalProperties"], false);
}
