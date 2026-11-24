//! 外派（job）模式：按 hibot `hibot.audit.executor/1` 契约读 job 目录、写 `out/report.json`。
//!
//! 分工（计划 §4 Phase 1）：
//! - [`manifest`]：`job.json` 解析与 kind / schema 校验（不可解析 → 无报告，见 1.8）。
//! - [`artifact`]：`inputs.*` 的路径约束、sha256、schema/format 常量校验。
//! - [`io`]：job 目录解析、越界判定、`*.tmp` → `rename` 原子写。
//! - [`fingerprint`]：hibot 指纹的唯一实现（逐字按契约 §3.2）。
//! - [`status`]：`ok/partial/error` + `status_reason` / `status_code` 对照表（随 `language`）。
//!
//! Phase 2（review 端到端）：
//! - [`projection`]：`inputs.diff_patch` → 变更集（输入真相源）+ 与 git 的一致性披露（2.1）。
//! - [`repo`]：检出完整性（非 shallow / 对象可得 / merge-base 可算，2.6）。
//! - [`prior`]：`prior_report` 只读 `summary.routing`（focus / specs / 预算，2.3）。
//! - [`dismissed`]：`dismissed.csv` 严格 RFC4180 解析与指纹匹配（2.4）。
//! - [`quote`]：quote 逐字重锚与丢弃计数（2.5）。
//!
//! **硬规则**：job 模式只写 `paths.out`；不读 `$REDTEAM_HOME` / `secrets.json`；不依赖 cwd / stdin。

pub mod artifact;
pub mod dismissed;
pub mod fingerprint;
pub mod executor_config;
pub mod io;
pub mod lifecycle;
pub mod manifest;
pub mod prior;
pub mod projection;
pub mod quote;
pub mod report;
pub mod repo;
pub mod run;
pub mod status;
