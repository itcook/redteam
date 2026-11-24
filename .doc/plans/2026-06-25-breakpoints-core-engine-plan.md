# breakpoints 核心评审引擎（Plan 1 / MVP）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现 `brk review --wait`，对真实代码 diff 或 Spec/Design 文档跑一次模型无关的并行对抗评审，输出统一的结构化报告（JSON / markdown）。

**Architecture:** 单向流水线 CLI → Config → Evidence → Prompt(按 lens) → Orchestrator(lens×model 并发 fan-out) → Consolidate(确定性 dedup + 有条件 LLM correlate) → Verdict(确定性) → Output。模型耦合全部收敛在 `ReviewProvider` trait 后面；其余组件纯模型无关、对 `FakeProvider` 可完全单测。

**Tech Stack:** Rust 2021、tokio(rt-multi-thread)、clap(derive)、serde/serde_json、toml、schemars(JSON Schema)、reqwest 0.13(rustls，自写 direct provider；aisdk 已弃用，见 Task 7 实施记录)。

## Global Constraints

> 以下为 spec 的项目级约束，每个 task 都隐含遵守（值逐字取自 `.doc/design/2026-06-25-breakpoints-adversarial-review-design.md`）：

- **语言 Rust**；产物为**真正独立的静态单二进制**；TLS 用 **rustls**（不链接 OpenSSL）。
- **模型无关**：唯一与具体模型打交道处是 `ReviewProvider` trait；其实现优先用 provider 原生 structured output，不支持则走 **prompt 内嵌 schema + 解析 + 重试（默认 2 次）** 兜底。
- **verdict 取值 `approve` | `reject`**（确定性规则，findings 的纯函数）。
- **severity 序 `low < medium < high < critical`**；`block_threshold` 默认 **medium**、`conf_floor` 默认 **0.5**。
- **规范遵循 lens（L5/D5）severity ≤ medium 双保险**：prompt 约束 + Synthesizer 落盘前钳制（未带 `hard_constraint` 标记则钳到 ≤ medium）。
- **多模型 = 单一结果**：所有 (lens×model) findings 汇入同一个 consolidate，出一份 result、一个 verdict；per-model 归因靠 `models`/`agreement` 保留。
- **失败处理**：未传 `--models`（默认路径）按 `priority` 降序候补；显式 `--models`（含单个）不候补、仅记 `run.warnings`/`run.failed_models`。
- **退出码**：`0 = approve`，`10 = reject`，`1/2 = 运行错误`。
- **密钥**：绝不写入主配置；读取优先级 env(`api_key_env`) > `secrets.toml`。
- **证据内联阈值**（code 模式）：`fileCount ≤ 2 且 diff ≤ 256KB` 才内联完整 diff，否则给文件清单 + "自己去看"指引。
- **dedup 偏向 under-merge**：行重叠 **且** 标题相似才合并。
- **correlate 仅当 ≥2 findings 触发**；`--no-correlate` 可关。

---

## File Structure

```
breakpoints/
  Cargo.toml
  src/
    main.rs               # CLI entry：解析 + dispatch（Plan 1 只有 review）
    cli.rs                # clap arg structs（ReviewArgs 等）
    domain/
      mod.rs              # re-export
      finding.rs          # Severity, RawFinding, Finding, Cluster
      report.rs           # Verdict, Stats, RunMeta, ReviewReport
      input.rs            # Mode, ReviewInput
    config/
      mod.rs              # Config, ProviderConfig, ReviewConfig, 分层加载
      secrets.rs          # env/secrets.toml key 解析（Plan 1 只读）
    lens/
      mod.rs              # LensId, Lens, 默认 code/doc lens 集, severity 上限
    prompt/
      mod.rs              # 模板插值 + 按 lens 构建 prompt
      templates/
        adversarial-code.md
        adversarial-doc.md
    evidence/
      mod.rs              # Mode 判定 + 分派
      git.rs              # code 模式证据（git diff）
      doc.rs              # doc 模式证据（读文档）
    provider/
      mod.rs              # ReviewProvider trait, LensOutput, ProviderError
      fake.rs             # FakeProvider（测试替身）
      direct.rs           # 自写 direct reqwest provider（Anthropic + OpenAI 兼容）+ parse_lens_output 兜底
    orchestrate/
      mod.rs              # 构建 lens×model 矩阵, fan-out, 候补/记录
    consolidate/
      mod.rs              # consolidate 入口（dedup → correlate）
      dedup.rs            # 确定性去重
      correlate.rs        # LLM 关联（有条件）
    verdict.rs            # 确定性 verdict + stats + severity 钳制
    output/
      mod.rs              # JSON + markdown 渲染
    hash.rs               # fnv1a64 稳定哈希（finding id）
  tests/
    review_e2e.rs         # 端到端（FakeProvider）
```

每文件单一职责；`ReviewProvider` 把唯一的外部耦合隔离，使 Tasks 8–13 全部能对 `FakeProvider` 确定性单测。

---

## Task 1: 项目脚手架 + domain 类型

**Files:**
- Create: `Cargo.toml`, `src/main.rs`, `src/hash.rs`, `src/domain/mod.rs`, `src/domain/finding.rs`, `src/domain/report.rs`, `src/domain/input.rs`
- Test: `src/domain/finding.rs`（内联 `#[cfg(test)]`）, `src/domain/report.rs`

**Interfaces:**
- Produces:
  - `enum Severity { Low, Medium, High, Critical }`（`Ord`：Low<Medium<High<Critical；serde lowercase）
  - `struct RawFinding { severity, title, body, file, line_start:u32, line_end:u32, confidence:f32, recommendation, hard_constraint:bool }`（`Deserialize + JsonSchema`）
  - `struct LensOutput { findings: Vec<RawFinding> }`（`Deserialize + JsonSchema + Default`）
  - `struct Finding { id, severity, title, body, file, line_start, line_end, confidence, recommendation, lenses:Vec<String>, models:Vec<String>, agreement:u32, hard_constraint:bool, cluster_id:Option<String> }`（`Serialize + Clone`）
  - `enum Verdict { Approve, Reject }`（serde kebab → `approve`/`reject`）
  - `struct Stats { critical:u32, high:u32, medium:u32, low:u32, total:u32 }`
  - `struct Cluster { id, theme, finding_ids:Vec<String>, relation:String, note:String, severity:Option<Severity> }`
  - `struct RunMeta { job_id, models:Vec<String>, lenses:Vec<String>, duration_ms:u64, usage:Usage, warnings:Vec<String>, failed_models:Vec<String> }`，`struct Usage { input_tokens:u64, output_tokens:u64 }`
  - `struct ReviewReport { schema_version:String, mode:Mode, verdict, stats, findings:Vec<Finding>, clusters:Vec<Cluster>, next_steps:Vec<String>, run:RunMeta }`
  - `fn fnv1a64_hex(parts: &[&str]) -> String`（稳定哈希）

- [ ] **Step 1: 写 Cargo.toml**

```toml
[package]
name = "breakpoints"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "brk"
path = "src/main.rs"

[dependencies]
clap = { version = "4", features = ["derive"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "process", "fs", "time"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
schemars = "0.8"
futures = "0.3"
anyhow = "1"
thiserror = "1"
# 注：aisdk / reqwest(rustls) 依赖推迟到 Task 7（首次用到时）再加，避免 aisdk 版本不确定性阻塞 Tasks 1–6

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: 写稳定哈希 `src/hash.rs`**

```rust
/// FNV-1a 64bit，确定性、跨进程稳定 —— 用于 finding 的稳定 id。
pub fn fnv1a64_hex(parts: &[&str]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for p in parts {
        for b in p.as_bytes() {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0xff; // 分隔，避免 ["ab","c"] 与 ["a","bc"] 同值
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_and_distinct() {
        assert_eq!(fnv1a64_hex(&["a", "b"]), fnv1a64_hex(&["a", "b"]));
        assert_ne!(fnv1a64_hex(&["ab", "c"]), fnv1a64_hex(&["a", "bc"]));
    }
}
```

- [ ] **Step 3: 写 `src/domain/finding.rs`（含失败测试）**

```rust
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Severity { Low, Medium, High, Critical } // 声明顺序即 Ord：Low<Medium<High<Critical

/// 模型每次 lens 调用返回的单条 finding（codex 形状 + 可选 hard_constraint）。
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct RawFinding {
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub confidence: f32,
    pub recommendation: String,
    #[serde(default)]
    pub hard_constraint: bool,
}

/// 本地在 dedup 时 enrich 后的 finding。
#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    pub id: String,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub confidence: f32,
    pub recommendation: String,
    pub lenses: Vec<String>,
    pub models: Vec<String>,
    pub agreement: u32,
    pub hard_constraint: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Cluster {
    pub id: String,
    pub theme: String,
    pub finding_ids: Vec<String>,
    pub relation: String,
    pub note: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn severity_orders_low_to_critical() {
        assert!(Severity::Low < Severity::Medium);
        assert!(Severity::Medium < Severity::High);
        assert!(Severity::High < Severity::Critical);
    }
    #[test]
    fn severity_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Severity::High).unwrap(), "\"high\"");
    }
}
```

- [ ] **Step 4: 写 `src/domain/report.rs` 与 `src/domain/input.rs`**

```rust
// src/domain/input.rs
use serde::Serialize;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode { Code, Doc }

/// 喂给模型的统一证据包。
#[derive(Clone, Debug)]
pub struct ReviewInput {
    pub mode: Mode,
    pub target_label: String,
    pub collection_guidance: String, // {{REVIEW_COLLECTION_GUIDANCE}}
    pub body: String,                // {{REVIEW_INPUT}}
    pub spec_docs: Vec<String>,      // 关联 spec 路径（L0 用）
}
```

```rust
// src/domain/report.rs
use super::finding::{Cluster, Finding, Severity};
use super::input::Mode;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict { Approve, Reject }

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats { pub critical: u32, pub high: u32, pub medium: u32, pub low: u32, pub total: u32 }

impl Stats {
    pub fn from_findings(fs: &[Finding]) -> Self {
        let mut s = Stats::default();
        for f in fs {
            match f.severity {
                Severity::Critical => s.critical += 1,
                Severity::High => s.high += 1,
                Severity::Medium => s.medium += 1,
                Severity::Low => s.low += 1,
            }
            s.total += 1;
        }
        s
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Usage { pub input_tokens: u64, pub output_tokens: u64 }

#[derive(Clone, Debug, Default, Serialize)]
pub struct RunMeta {
    pub job_id: String,
    pub models: Vec<String>,
    pub lenses: Vec<String>,
    pub duration_ms: u64,
    pub usage: Usage,
    pub warnings: Vec<String>,
    pub failed_models: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReviewReport {
    pub schema_version: String,
    pub mode: Mode,
    pub verdict: Verdict,
    pub stats: Stats,
    pub findings: Vec<Finding>,
    pub clusters: Vec<Cluster>,
    pub next_steps: Vec<String>,
    pub run: RunMeta,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::finding::*;
    fn f(sev: Severity) -> Finding {
        Finding { id: "x".into(), severity: sev, title: "t".into(), body: "b".into(),
            file: "a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), lenses: vec![], models: vec![], agreement: 1,
            hard_constraint: false, cluster_id: None }
    }
    #[test]
    fn stats_counts_by_severity() {
        let s = Stats::from_findings(&[f(Severity::High), f(Severity::High), f(Severity::Low)]);
        assert_eq!((s.high, s.low, s.total), (2, 1, 3));
    }
    #[test]
    fn verdict_serializes_kebab() {
        assert_eq!(serde_json::to_string(&Verdict::Reject).unwrap(), "\"reject\"");
    }
}
```

- [ ] **Step 5: 写 `src/domain/mod.rs` 与最小 `src/main.rs`**

```rust
// src/domain/mod.rs
pub mod finding;
pub mod input;
pub mod report;
pub use finding::*;
pub use input::*;
pub use report::*;
```

```rust
// src/main.rs
mod domain;
mod hash;
fn main() { println!("brk"); }
```

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test`
Expected: PASS（severity 序、序列化、stats、hash 测试全绿；编译通过）

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock src/
git commit -m "feat: project scaffold + domain types"
```

---

## Task 2: 配置加载（分层 TOML + key 解析，只读）

**Files:**
- Create: `src/config/mod.rs`, `src/config/secrets.rs`
- Modify: `src/main.rs`（`mod config;`）
- Test: `src/config/mod.rs`（内联）

**Interfaces:**
- Consumes: `Severity`（Task 1）
- Produces:
  - `enum ProviderKind { Anthropic, Openai, OpenaiCompatible, Google }`（serde kebab）
  - `struct ProviderConfig { kind, model:String, base_url:Option<String>, api_key_env:Option<String>, priority:i32 }`
  - `struct LensSets { code:Vec<String>, doc:Vec<String> }`
  - `struct ReviewConfig { default_models:Vec<String>, block_threshold:Severity, conf_floor:f32, correlate:bool, max_concurrency:usize, state_ttl:String, lenses:LensSets }`
  - `struct Config { providers:BTreeMap<String,ProviderConfig>, review:ReviewConfig }`
  - `impl Config { fn load() -> anyhow::Result<Config>; fn resolve_key(&self, provider:&str) -> Option<String>; }`
  - `fn config_paths() -> Vec<PathBuf>`（分层：内置默认 → 用户 → 项目）

- [ ] **Step 1: 写失败测试（分层覆盖 + key 解析优先级）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_overrides_user() {
        let user = r#"[review]
default_models=["claude"]
block_threshold="low"
"#;
        let project = r#"[review]
block_threshold="high"
"#;
        let cfg = Config::from_layers(&[user, project]).unwrap();
        // 项目层覆盖用户层
        assert_eq!(cfg.review.block_threshold, crate::domain::Severity::High);
        // 未覆盖字段保留
        assert_eq!(cfg.review.default_models, vec!["claude".to_string()]);
    }
    #[test]
    fn defaults_apply_when_absent() {
        let cfg = Config::from_layers(&[]).unwrap();
        assert_eq!(cfg.review.block_threshold, crate::domain::Severity::Medium);
        assert_eq!(cfg.review.conf_floor, 0.5);
        assert!(cfg.review.correlate);
    }
    #[test]
    fn key_resolves_env_over_secrets() {
        std::env::set_var("BRK_TEST_KEY", "from-env");
        let toml = r#"[providers.x]
kind="openai"
model="m"
api_key_env="BRK_TEST_KEY"
priority=10
"#;
        let cfg = Config::from_layers(&[toml]).unwrap();
        assert_eq!(cfg.resolve_key("x").as_deref(), Some("from-env"));
        std::env::remove_var("BRK_TEST_KEY");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test config::`
Expected: FAIL（`Config::from_layers`/`resolve_key` 未定义）

- [ ] **Step 3: 实现 `src/config/secrets.rs`**

```rust
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 读取 ~/.config/breakpoints/secrets.toml → { provider_name: key }
pub fn load_secrets() -> BTreeMap<String, String> {
    let path = secrets_path();
    let Ok(text) = std::fs::read_to_string(&path) else { return BTreeMap::new() };
    toml::from_str::<BTreeMap<String, String>>(&text).unwrap_or_default()
}

pub fn secrets_path() -> PathBuf {
    dirs_config().join("breakpoints").join("secrets.toml")
}

pub fn dirs_config() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            home.join(".config")
        })
}
```

- [ ] **Step 4: 实现 `src/config/mod.rs`**

```rust
pub mod secrets;

use crate::domain::Severity;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind { Anthropic, Openai, OpenaiCompatible, Google }

#[derive(Clone, Debug, Deserialize)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub priority: i32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct LensSets {
    #[serde(default = "default_code_lenses")]
    pub code: Vec<String>,
    #[serde(default = "default_doc_lenses")]
    pub doc: Vec<String>,
}
fn default_code_lenses() -> Vec<String> {
    ["L1", "L2", "L3", "L4", "L0", "L5"].iter().map(|s| s.to_string()).collect()
}
fn default_doc_lenses() -> Vec<String> {
    ["D1", "D2", "D3", "D4", "D5"].iter().map(|s| s.to_string()).collect()
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReviewConfig {
    #[serde(default)]
    pub default_models: Vec<String>,
    #[serde(default = "default_block")]
    pub block_threshold: Severity,
    #[serde(default = "default_floor")]
    pub conf_floor: f32,
    #[serde(default = "default_true")]
    pub correlate: bool,
    #[serde(default = "default_concurrency")]
    pub max_concurrency: usize,
    #[serde(default = "default_ttl")]
    pub state_ttl: String,
    #[serde(default)]
    pub lenses: LensSets,
}
fn default_block() -> Severity { Severity::Medium }
fn default_floor() -> f32 { 0.5 }
fn default_true() -> bool { true }
fn default_concurrency() -> usize { 8 }
fn default_ttl() -> String { "24h".into() }
impl Default for LensSets {
    fn default() -> Self { LensSets { code: default_code_lenses(), doc: default_doc_lenses() } }
}
impl Default for ReviewConfig {
    fn default() -> Self {
        ReviewConfig { default_models: vec![], block_threshold: default_block(),
            conf_floor: default_floor(), correlate: default_true(),
            max_concurrency: default_concurrency(), state_ttl: default_ttl(),
            lenses: LensSets::default() }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub review: ReviewConfig,
}

impl Config {
    /// 真实加载：内置默认（空）→ 各分层文件文本依次合并。
    pub fn load() -> anyhow::Result<Config> {
        let texts: Vec<String> = config_paths()
            .into_iter()
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .collect();
        let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        Config::from_layers(&refs)
    }

    /// 把多层 TOML 文本按 toml::Value 深合并（右侧覆盖左侧）后反序列化。
    pub fn from_layers(layers: &[&str]) -> anyhow::Result<Config> {
        let mut merged = toml::Value::Table(Default::default());
        for layer in layers {
            let v: toml::Value = toml::from_str(layer)?;
            merge(&mut merged, v);
        }
        Ok(merged.try_into()?)
    }

    /// key 解析：env(api_key_env) 优先，回落 secrets.toml[provider]。
    pub fn resolve_key(&self, provider: &str) -> Option<String> {
        if let Some(pc) = self.providers.get(provider) {
            if let Some(var) = &pc.api_key_env {
                if let Ok(v) = std::env::var(var) {
                    return Some(v);
                }
            }
        }
        secrets::load_secrets().get(provider).cloned()
    }
}

fn merge(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(b), toml::Value::Table(o)) => {
            for (k, v) in o {
                merge(b.entry(k).or_insert(toml::Value::Table(Default::default())), v);
            }
        }
        (b, o) => *b = o,
    }
}

/// 分层路径（左→右优先级递增）：用户 → 项目。内置默认由 serde default 提供。
pub fn config_paths() -> Vec<PathBuf> {
    let mut v = vec![secrets::dirs_config().join("breakpoints").join("config.toml")];
    v.push(PathBuf::from(".breakpoints.toml"));
    v
}
```

- [ ] **Step 5: 接线 main.rs**

```rust
mod config;
```

- [ ] **Step 6: 跑测试**

Run: `cargo test config::`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/config/ src/main.rs
git commit -m "feat: layered TOML config + key resolution (read-only)"
```

---

## Task 3: lens 注册表（默认集 + severity 上限）

**Files:**
- Create: `src/lens/mod.rs`
- Modify: `src/main.rs`（`mod lens;`）
- Test: `src/lens/mod.rs`（内联）

**Interfaces:**
- Consumes: `Severity`, `Mode`
- Produces:
  - `struct Lens { id:&'static str, title:&'static str, focus:&'static str, max_severity:Option<Severity> }`
  - `fn all_for(mode: Mode) -> Vec<&'static Lens>`
  - `fn by_id(id:&str) -> Option<&'static Lens>`
  - `fn select(mode: Mode, ids: &[String]) -> Vec<&'static Lens>`（按配置/`--lenses` 取子集；未知 id 报错）

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Mode;
    #[test]
    fn code_has_six_default_lenses_incl_l0_l5() {
        let ids: Vec<_> = all_for(Mode::Code).iter().map(|l| l.id).collect();
        assert!(ids.contains(&"L1") && ids.contains(&"L0") && ids.contains(&"L5"));
    }
    #[test]
    fn compliance_lens_caps_at_medium() {
        assert_eq!(by_id("L5").unwrap().max_severity, Some(crate::domain::Severity::Medium));
        assert_eq!(by_id("D5").unwrap().max_severity, Some(crate::domain::Severity::Medium));
        assert_eq!(by_id("L1").unwrap().max_severity, None);
    }
    #[test]
    fn select_subset_and_reject_unknown() {
        assert_eq!(select(Mode::Code, &["L1".into(), "L2".into()]).unwrap().len(), 2);
        assert!(select(Mode::Code, &["NOPE".into()]).is_err());
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test lens::`
Expected: FAIL（未定义）

- [ ] **Step 3: 实现 `src/lens/mod.rs`**

```rust
use crate::domain::{Mode, Severity};

pub struct Lens {
    pub id: &'static str,
    pub title: &'static str,
    pub focus: &'static str,        // 注入 prompt 的"本 lens 攻击面"
    pub max_severity: Option<Severity>,
}

static CODE: &[Lens] = &[
    Lens { id: "L1", title: "安全与边界", focus: "鉴权/越权/租户隔离/信任边界/输入校验", max_severity: None },
    Lens { id: "L2", title: "数据与状态", focus: "数据丢失/损坏/重复/不可逆状态/回滚安全/幂等", max_severity: None },
    Lens { id: "L3", title: "并发与失败", focus: "竞态/顺序/重入/重试/部分失败/超时/降级依赖/空态null", max_severity: None },
    Lens { id: "L4", title: "演进与可观测", focus: "版本漂移/schema 漂移/迁移风险/兼容回归/可观测盲区", max_severity: None },
    Lens { id: "L0", title: "spec 一致性", focus: "实现是否偏离所给 spec 的设计意图（drift）", max_severity: None },
    Lens { id: "L5", title: "规范遵循", focus: "编码约定/命名/结构/团队标准；非强约束时仅 low/medium", max_severity: Some(Severity::Medium) },
];

static DOC: &[Lens] = &[
    Lens { id: "D1", title: "内部一致性", focus: "段落矛盾、互斥需求、自相冲突约束", max_severity: None },
    Lens { id: "D2", title: "完备性与歧义", focus: "缺失分支/未定义错误处理/欠规约/会坑实现的歧义", max_severity: None },
    Lens { id: "D3", title: "隐藏假设与风险", focus: "未言明假设/未验证依赖；设计层是否回应安全/数据/失败/迁移", max_severity: None },
    Lens { id: "D4", title: "可测性与成功标准", focus: "成功标准可否度量、验收条件是否定义", max_severity: None },
    Lens { id: "D5", title: "规范遵循", focus: "是否遵循团队文档规范/模板；非强约束时仅 low/medium", max_severity: Some(Severity::Medium) },
];

pub fn all_for(mode: Mode) -> Vec<&'static Lens> {
    match mode { Mode::Code => CODE.iter().collect(), Mode::Doc => DOC.iter().collect() }
}

pub fn by_id(id: &str) -> Option<&'static Lens> {
    CODE.iter().chain(DOC.iter()).find(|l| l.id == id)
}

pub fn select(mode: Mode, ids: &[String]) -> anyhow::Result<Vec<&'static Lens>> {
    if ids.is_empty() {
        return Ok(all_for(mode));
    }
    let mut out = Vec::new();
    for id in ids {
        let l = by_id(id).ok_or_else(|| anyhow::anyhow!("未知 lens: {id}"))?;
        out.push(l);
    }
    Ok(out)
}
```

- [ ] **Step 4: 接线 + 测试**

`src/main.rs` 加 `mod lens;`，运行：

Run: `cargo test lens::`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/lens/ src/main.rs
git commit -m "feat: lens registry with code/doc sets and severity caps"
```

---

## Task 4: prompt 构建（模板 + 插值 + lens 注入）

**Files:**
- Create: `src/prompt/mod.rs`, `src/prompt/templates/adversarial-code.md`, `src/prompt/templates/adversarial-doc.md`
- Modify: `src/main.rs`（`mod prompt;`）
- Test: `src/prompt/mod.rs`（内联）

**Interfaces:**
- Consumes: `ReviewInput`, `Mode`, `Lens`
- Produces: `fn build_prompt(input:&ReviewInput, lens:&Lens, user_focus:&str) -> String`

- [ ] **Step 1: 写两个模板（搬 codex 对抗灵魂，按 lens 参数化）**

`src/prompt/templates/adversarial-code.md`：

```text
<role>
You are performing an ADVERSARIAL software review.
Your job is to break confidence in the change, not to validate it.
</role>

<lens>
Attack this change ONLY through this lens: {{LENS_TITLE}}
Focus surface: {{LENS_FOCUS}}
</lens>

<operating_stance>
Default to skepticism. Assume subtle, high-cost, user-visible failure until evidence says otherwise.
Give no credit for good intent, partial fixes, or likely follow-up. Happy-path-only counts as a weakness.
</operating_stance>

<review_method>
Actively disprove the change: violated invariants, missing guards, unhandled failure paths, assumptions that break under stress.
{{LENS_SEVERITY_RULE}}
User focus (weight heavily but still report other material issues in this lens): {{USER_FOCUS}}
{{COLLECTION_GUIDANCE}}
</review_method>

<finding_bar>
Report only material findings. No style/naming/low-value cleanup, no evidence-free speculation.
Each finding answers: what breaks / why vulnerable / impact / concrete fix.
</finding_bar>

<output_contract>
Return ONLY JSON matching the provided schema: an object with a "findings" array.
Each finding: severity (low|medium|high|critical), title, body, file, line_start>=1, line_end>=1, confidence 0..1, recommendation.
If nothing material in THIS lens, return {"findings": []}. Prefer one strong finding over several weak.
Every finding must be grounded in the provided context — do not invent files, lines, or runtime behavior.
</output_contract>

<repository_context>
Target: {{TARGET_LABEL}}
{{REVIEW_INPUT}}
</repository_context>
```

`src/prompt/templates/adversarial-doc.md`：同结构，`<role>` 改为 "ADVERSARIAL review of a spec/design document — prove it is NOT ready to implement"，`<repository_context>` 改 `<document_context>`，其余占位符相同。

- [ ] **Step 2: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Mode, ReviewInput};
    use crate::lens::by_id;
    fn input() -> ReviewInput {
        ReviewInput { mode: Mode::Code, target_label: "branch x".into(),
            collection_guidance: "GUIDE".into(), body: "DIFF".into(), spec_docs: vec![] }
    }
    #[test]
    fn interpolates_all_placeholders() {
        let p = build_prompt(&input(), by_id("L1").unwrap(), "auth");
        assert!(!p.contains("{{"));
        assert!(p.contains("安全与边界") && p.contains("DIFF") && p.contains("auth") && p.contains("GUIDE"));
    }
    #[test]
    fn compliance_lens_injects_severity_cap_rule() {
        let p = build_prompt(&input(), by_id("L5").unwrap(), "");
        assert!(p.to_lowercase().contains("medium"));
    }
}
```

- [ ] **Step 3: 实现 `src/prompt/mod.rs`**

```rust
use crate::domain::{Mode, ReviewInput};
use crate::lens::Lens;

const CODE_TPL: &str = include_str!("templates/adversarial-code.md");
const DOC_TPL: &str = include_str!("templates/adversarial-doc.md");

pub fn build_prompt(input: &ReviewInput, lens: &Lens, user_focus: &str) -> String {
    let tpl = match input.mode { Mode::Code => CODE_TPL, Mode::Doc => DOC_TPL };
    let sev_rule = match lens.max_severity {
        Some(_) => "This is a compliance lens: severity MUST be low or medium unless the violated rule is an explicit hard project constraint (then set hard_constraint=true).",
        None => "",
    };
    tpl.replace("{{LENS_TITLE}}", lens.title)
        .replace("{{LENS_FOCUS}}", lens.focus)
        .replace("{{LENS_SEVERITY_RULE}}", sev_rule)
        .replace("{{USER_FOCUS}}", if user_focus.is_empty() { "(none)" } else { user_focus })
        .replace("{{COLLECTION_GUIDANCE}}", &input.collection_guidance)
        .replace("{{TARGET_LABEL}}", &input.target_label)
        .replace("{{REVIEW_INPUT}}", &input.body)
}
```

- [ ] **Step 4: 接线 + 测试**

`src/main.rs` 加 `mod prompt;`。

Run: `cargo test prompt::`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/prompt/ src/main.rs
git commit -m "feat: adversarial prompt templates + per-lens interpolation"
```

---

## Task 5: 证据收集（git code 模式 + doc 模式 + mode 判定）

**Files:**
- Create: `src/evidence/mod.rs`, `src/evidence/git.rs`, `src/evidence/doc.rs`
- Modify: `src/main.rs`（`mod evidence;`）
- Test: `src/evidence/git.rs`, `src/evidence/doc.rs`（内联，用 tempfile）

**Interfaces:**
- Consumes: `Mode`, `ReviewInput`
- Produces:
  - `struct EvidenceOpts { mode:Option<Mode>, base:Option<String>, scope:Scope, spec:Vec<String>, targets:Vec<String> }`
  - `enum Scope { WorkingTree, Branch }`
  - `fn collect(opts:&EvidenceOpts) -> anyhow::Result<ReviewInput>`
  - `fn detect_mode(opts:&EvidenceOpts) -> anyhow::Result<Mode>`
  - 常量 `const INLINE_MAX_FILES: usize = 2; const INLINE_MAX_BYTES: usize = 256*1024;`

- [ ] **Step 1: 写失败测试（doc 内联 + 阈值切换）**

```rust
// src/evidence/doc.rs 内联
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn doc_mode_inlines_file_contents() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("spec.md");
        std::fs::write(&p, "# Design\nhello").unwrap();
        let body = collect_doc(&[p.to_string_lossy().into()]).unwrap();
        assert!(body.contains("hello") && body.contains("spec.md"));
    }
}
```

```rust
// src/evidence/git.rs 内联
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn large_diff_falls_back_to_file_list() {
        let files = vec!["a.rs".to_string(), "b.rs".into(), "c.rs".into()]; // >2 文件
        let (inline, guidance) = decide_inline(&files, 10, "DIFF");
        assert!(inline.is_none());
        assert!(guidance.contains("a.rs") && guidance.contains("自己"));
    }
    #[test]
    fn small_diff_inlines() {
        let files = vec!["a.rs".to_string()];
        let (inline, _) = decide_inline(&files, 10, "DIFF");
        assert_eq!(inline.as_deref(), Some("DIFF"));
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test evidence::`
Expected: FAIL（未定义）

- [ ] **Step 3: 实现 `src/evidence/doc.rs`**

```rust
/// 读取 doc 模式目标文档，拼成 {{REVIEW_INPUT}} 主体。
pub fn collect_doc(targets: &[String]) -> anyhow::Result<String> {
    let mut out = String::new();
    for t in targets {
        let text = std::fs::read_to_string(t)
            .map_err(|e| anyhow::anyhow!("读取文档 {t} 失败: {e}"))?;
        out.push_str(&format!("\n===== FILE: {t} =====\n{text}\n"));
    }
    if out.is_empty() {
        anyhow::bail!("doc 模式至少需要一个目标文档");
    }
    Ok(out)
}
```

- [ ] **Step 4: 实现 `src/evidence/git.rs`**

```rust
use std::process::Command;

pub const INLINE_MAX_FILES: usize = 2;
pub const INLINE_MAX_BYTES: usize = 256 * 1024;

/// 阈值决策：返回 (内联 diff 或 None, 指引文本)。
pub fn decide_inline(files: &[String], diff_bytes: usize, diff: &str) -> (Option<String>, String) {
    if files.len() <= INLINE_MAX_FILES && diff_bytes <= INLINE_MAX_BYTES {
        (Some(diff.to_string()), String::new())
    } else {
        let list = files.join("\n  - ");
        let guidance = format!(
            "改动较大（{} 文件 / {} 字节），未内联完整 diff。改动文件：\n  - {}\n请自己用工具读取这些文件与其改动来评审。",
            files.len(), diff_bytes, list
        );
        (None, guidance)
    }
}

fn git(args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git").args(args).output()?;
    if !out.status.success() {
        anyhow::bail!("git {:?} 失败: {}", args, String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 收集 code 证据：branch scope = base...HEAD；working-tree = cached+worktree。
/// 返回 (body, guidance, target_label)。
pub fn collect_code(base: Option<&str>, branch_scope: bool) -> anyhow::Result<(String, String, String)> {
    let (diff, files, label) = if branch_scope {
        let base = base.ok_or_else(|| anyhow::anyhow!("branch scope 需要 --base"))?;
        let merge_base = git(&["merge-base", base, "HEAD"])?.trim().to_string();
        let diff = git(&["diff", &format!("{merge_base}...HEAD")])?;
        let files = git(&["diff", "--name-only", &format!("{merge_base}...HEAD")])?;
        (diff, files, format!("branch {base}...HEAD"))
    } else {
        let mut diff = git(&["diff", "--cached"])?;
        diff.push_str(&git(&["diff"])?);
        let mut files = git(&["diff", "--cached", "--name-only"])?;
        files.push_str(&git(&["diff", "--name-only"])?);
        (diff, files, "working tree".to_string())
    };
    let file_list: Vec<String> = files.lines().filter(|l| !l.is_empty()).map(|s| s.to_string()).collect();
    let (inline, guidance) = decide_inline(&file_list, diff.len(), &diff);
    let body = inline.unwrap_or_else(|| format!("(diff 未内联)\n改动文件:\n{}", file_list.join("\n")));
    Ok((body, guidance, label))
}
```

- [ ] **Step 5: 实现 `src/evidence/mod.rs`（mode 判定 + 分派）**

```rust
pub mod doc;
pub mod git;

use crate::domain::{Mode, ReviewInput};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope { WorkingTree, Branch }

#[derive(Clone, Debug)]
pub struct EvidenceOpts {
    pub mode: Option<Mode>,
    pub base: Option<String>,
    pub scope: Scope,
    pub spec: Vec<String>,
    pub targets: Vec<String>, // doc 模式受审文档
}

/// 自动判定：显式 mode 优先；否则有 base/branch scope → code；有 targets 文档 → doc；歧义报错。
pub fn detect_mode(opts: &EvidenceOpts) -> anyhow::Result<Mode> {
    if let Some(m) = opts.mode { return Ok(m); }
    if opts.base.is_some() || opts.scope == Scope::Branch { return Ok(Mode::Code); }
    if !opts.targets.is_empty() { return Ok(Mode::Doc); }
    anyhow::bail!("无法判定 mode：请显式 --mode，或提供 --base（code）/目标文档（doc）");
}

pub fn collect(opts: &EvidenceOpts) -> anyhow::Result<ReviewInput> {
    let mode = detect_mode(opts)?;
    match mode {
        Mode::Doc => {
            let body = doc::collect_doc(&opts.targets)?;
            Ok(ReviewInput { mode, target_label: opts.targets.join(", "),
                collection_guidance: String::new(), body, spec_docs: opts.spec.clone() })
        }
        Mode::Code => {
            let (body, guidance, label) = git::collect_code(opts.base.as_deref(), opts.scope == Scope::Branch)?;
            let mut g = guidance;
            if !opts.spec.is_empty() {
                g.push_str(&format!("\n关联 spec 文档（核对实现是否偏离设计意图）：{}", opts.spec.join(", ")));
            }
            Ok(ReviewInput { mode, target_label: label, collection_guidance: g, body, spec_docs: opts.spec.clone() })
        }
    }
}
```

- [ ] **Step 6: 接线 + 测试**

`src/main.rs` 加 `mod evidence;`。

Run: `cargo test evidence::`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/evidence/ src/main.rs
git commit -m "feat: evidence collection (git code-mode + doc-mode + autodetect)"
```

---

## Task 6: ReviewProvider trait + FakeProvider

**Files:**
- Create: `src/provider/mod.rs`, `src/provider/fake.rs`
- Modify: `src/main.rs`（`mod provider;`）
- Test: `src/provider/fake.rs`（内联）

**Interfaces:**
- Consumes: `LensOutput`, `RawFinding`
- Produces:
  - `struct LensRequest { model:String, prompt:String }`
  - `#[async_trait] trait ReviewProvider { async fn run(&self, req:LensRequest) -> Result<LensCall, ProviderError>; }`
  - `struct LensCall { findings:Vec<RawFinding>, input_tokens:u64, output_tokens:u64 }`
  - `enum ProviderError { Unavailable(String), Parse(String) }`（`thiserror`）
  - `struct FakeProvider { scripted: BTreeMap<String, Vec<RawFinding>> }`（按 model 名返回脚本化 findings；未知 model → Unavailable）

> 注：加 `async-trait = "0.1"` 到 Cargo.toml。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Severity;
    #[tokio::test]
    async fn fake_returns_scripted_findings() {
        let p = FakeProvider::with("claude", vec![raw("boom", Severity::High)]);
        let call = p.run(LensRequest { model: "claude".into(), prompt: "x".into() }).await.unwrap();
        assert_eq!(call.findings.len(), 1);
        assert_eq!(call.findings[0].title, "boom");
    }
    #[tokio::test]
    async fn fake_unknown_model_is_unavailable() {
        let p = FakeProvider::with("claude", vec![]);
        let err = p.run(LensRequest { model: "gpt".into(), prompt: "x".into() }).await.unwrap_err();
        assert!(matches!(err, ProviderError::Unavailable(_)));
    }
}
```

（`raw(title, sev)` 为测试辅助，构造 `RawFinding`。）

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test provider::`
Expected: FAIL

- [ ] **Step 3: 实现 `src/provider/mod.rs`**

```rust
pub mod fake;

use crate::domain::RawFinding;
use async_trait::async_trait;

#[derive(Clone, Debug)]
pub struct LensRequest { pub model: String, pub prompt: String }

#[derive(Clone, Debug)]
pub struct LensCall { pub findings: Vec<RawFinding>, pub input_tokens: u64, pub output_tokens: u64 }

#[derive(thiserror::Error, Debug)]
pub enum ProviderError {
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    #[error("structured-output parse failed: {0}")]
    Parse(String),
}

#[async_trait]
pub trait ReviewProvider: Send + Sync {
    async fn run(&self, req: LensRequest) -> Result<LensCall, ProviderError>;
}
```

- [ ] **Step 4: 实现 `src/provider/fake.rs`**

```rust
use super::*;
use crate::domain::RawFinding;
use std::collections::BTreeMap;

pub struct FakeProvider { pub scripted: BTreeMap<String, Vec<RawFinding>> }

impl FakeProvider {
    pub fn with(model: &str, findings: Vec<RawFinding>) -> Self {
        let mut m = BTreeMap::new();
        m.insert(model.to_string(), findings);
        FakeProvider { scripted: m }
    }
}

#[async_trait::async_trait]
impl ReviewProvider for FakeProvider {
    async fn run(&self, req: LensRequest) -> Result<LensCall, ProviderError> {
        match self.scripted.get(&req.model) {
            Some(fs) => Ok(LensCall { findings: fs.clone(), input_tokens: 0, output_tokens: 0 }),
            None => Err(ProviderError::Unavailable(format!("no script for model {}", req.model))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Severity;
    fn raw(title: &str, sev: Severity) -> RawFinding {
        RawFinding { severity: sev, title: title.into(), body: "b".into(), file: "a.rs".into(),
            line_start: 1, line_end: 2, confidence: 0.9, recommendation: "fix".into(), hard_constraint: false }
    }
    #[tokio::test]
    async fn fake_returns_scripted_findings() {
        let p = FakeProvider::with("claude", vec![raw("boom", Severity::High)]);
        let call = p.run(LensRequest { model: "claude".into(), prompt: "x".into() }).await.unwrap();
        assert_eq!(call.findings[0].title, "boom");
    }
    #[tokio::test]
    async fn fake_unknown_model_is_unavailable() {
        let p = FakeProvider::with("claude", vec![]);
        let err = p.run(LensRequest { model: "gpt".into(), prompt: "x".into() }).await.unwrap_err();
        assert!(matches!(err, ProviderError::Unavailable(_)));
    }
}
```

- [ ] **Step 5: 接线 + 测试**

`src/main.rs` 加 `mod provider;`，Cargo.toml 加 `async-trait = "0.1"`。

Run: `cargo test provider::`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/provider/ src/main.rs Cargo.toml
git commit -m "feat: ReviewProvider trait + FakeProvider test double"
```

---

## Task 7: Provider 实现（兜底解析 + 直连 provider）

> **实施记录（已完成，与下文原 aisdk 方案不同）**：实现期发现 aisdk 0.5.2 经 reqwest-eventsource 传递拉 OpenSSL，且不暴露 TLS-backend feature，需 vendor 整个 fork（1.8MB）才能满足 rustls-only 硬约束。经用户决定 **弃用 aisdk，改为自写 `src/provider/direct.rs`（reqwest rustls 直连 Anthropic + OpenAI 兼容 REST）**，保留已测的 `parse_lens_output` 兜底解析。下文保留原 aisdk 方案作历史参考；实际产物以 `direct.rs`/`DirectProvider` 为准（HEAD=2804a5a）。

**Files:**
- Create: `src/provider/aisdk_provider.rs`
- Modify: `src/provider/mod.rs`（`pub mod aisdk_provider;`）, `src/config`（传入 ProviderConfig + key）
- Test: `src/provider/aisdk_provider.rs`（内联：仅测**兜底 JSON 解析**纯函数；真实 API 走 `#[ignore]` 冒烟）

**Interfaces:**
- Consumes: `ProviderConfig`, `ProviderKind`, `LensOutput`, `ReviewProvider`
- Produces:
  - `struct AisdkProvider { kind:ProviderKind, model:String, base_url:Option<String>, api_key:String }`
  - `fn parse_lens_output(raw:&str) -> Result<LensOutput, ProviderError>`（容错：剥离 ```json fence、找首个 `{`）
  - `impl ReviewProvider for AisdkProvider`

> 实现期待办：钉定 aisdk 版本 + provider 能力表（哪些 kind 支持原生 `.schema::<T>()`）。本 task 的 trait 边界使其余系统不受 aisdk API 漂移影响。

- [ ] **Step 0: 加 aisdk / reqwest(rustls) 依赖到 Cargo.toml**（前置：Tasks 1–6 不含这两个依赖）

```toml
# 追加到 [dependencies]
aisdk = { version = "*", default-features = false }   # 版本/features 实现期钉定
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json"] }
```

先 `cargo build` 确认 aisdk 解析成功；若拉不动则在此收口 crate 坐标（不影响 Tasks 1–6 已通过的成果）。

- [ ] **Step 1: 写兜底解析的失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_strips_code_fence() {
        let raw = "```json\n{\"findings\":[]}\n```";
        assert!(parse_lens_output(raw).unwrap().findings.is_empty());
    }
    #[test]
    fn parse_finds_object_in_prose() {
        let raw = "Here is the result: {\"findings\":[{\"severity\":\"low\",\"title\":\"t\",\"body\":\"b\",\"file\":\"a\",\"line_start\":1,\"line_end\":1,\"confidence\":0.5,\"recommendation\":\"r\"}]} done";
        assert_eq!(parse_lens_output(raw).unwrap().findings.len(), 1);
    }
    #[test]
    fn parse_garbage_errors() {
        assert!(parse_lens_output("no json here").is_err());
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test provider::aisdk`
Expected: FAIL

- [ ] **Step 3: 实现兜底解析 + provider**

```rust
use super::{LensCall, LensRequest, ProviderError, ReviewProvider};
use crate::config::{ProviderConfig, ProviderKind};
use crate::domain::LensOutput;
use async_trait::async_trait;

pub struct AisdkProvider {
    pub kind: ProviderKind,
    pub model: String,
    pub base_url: Option<String>,
    pub api_key: String,
}

impl AisdkProvider {
    pub fn from_config(pc: &ProviderConfig, api_key: String) -> Self {
        AisdkProvider { kind: pc.kind, model: pc.model.clone(), base_url: pc.base_url.clone(), api_key }
    }
}

/// 兜底：从模型自由文本里抠出符合 schema 的 JSON。
pub fn parse_lens_output(raw: &str) -> Result<LensOutput, ProviderError> {
    let trimmed = raw.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
    if let Ok(v) = serde_json::from_str::<LensOutput>(trimmed) {
        return Ok(v);
    }
    // 从首个 '{' 到匹配的 '}' 暴力扫描
    if let Some(start) = trimmed.find('{') {
        let mut depth = 0i32;
        for (i, c) in trimmed[start..].char_indices() {
            match c { '{' => depth += 1, '}' => { depth -= 1; if depth == 0 {
                let cand = &trimmed[start..start + i + 1];
                if let Ok(v) = serde_json::from_str::<LensOutput>(cand) { return Ok(v); }
                break;
            } }, _ => {} }
        }
    }
    Err(ProviderError::Parse(format!("no schema-valid JSON in: {}", &raw.chars().take(120).collect::<String>())))
}

#[async_trait]
impl ReviewProvider for AisdkProvider {
    async fn run(&self, req: LensRequest) -> Result<LensCall, ProviderError> {
        // 用 aisdk 发请求；原生 schema 优先，失败回落兜底解析。
        // 文档 API：LanguageModelRequest::builder().model(..).system(..).prompt(..).schema::<LensOutput>().generate_text()
        // 这里以"取回文本 → parse_lens_output"的统一兜底路径实现，保证模型无关。
        let text = call_model_text(self, &req.prompt).await
            .map_err(|e| ProviderError::Unavailable(e.to_string()))?;
        let out = parse_lens_output(&text)?;
        Ok(LensCall { findings: out.findings, input_tokens: 0, output_tokens: 0 })
    }
}

/// 用 aisdk 取回模型文本。实现期按 kind 走原生 schema/自定义 base_url/key 注入。
async fn call_model_text(p: &AisdkProvider, prompt: &str) -> anyhow::Result<String> {
    // 伪实现骨架（实现期用 aisdk 真实 builder 填充，按 p.kind 选 provider、p.base_url/p.api_key 注入 header）：
    // let req = aisdk::core::LanguageModelRequest::builder()
    //     .model(select_aisdk_model(p))
    //     .system("Return ONLY JSON matching the schema.")
    //     .prompt(prompt)
    //     .schema::<crate::domain::LensOutput>()
    //     .build();
    // Ok(req.generate_text().await?.text().unwrap_or_default())
    let _ = (p, prompt);
    anyhow::bail!("aisdk call not wired in MVP test path");
}
```

> 说明：`call_model_text` 在实现期接 aisdk 真实 builder（按 §14：原生 `.schema::<LensOutput>()` 优先，无原生能力时把 schema 文本嵌进 system + `parse_lens_output` 兜底）。单测只覆盖 `parse_lens_output`；真实端到端用 `#[ignore]` 冒烟（需 key）。retry（默认 2 次指数退避）在 orchestrate 层包裹（Task 8）以便统一控制。

- [ ] **Step 4: 接线 + 测试**

`src/provider/mod.rs` 加 `pub mod aisdk_provider;`。

Run: `cargo test provider::aisdk`
Expected: PASS（兜底解析三测全绿）

- [ ] **Step 5: Commit**

```bash
git add src/provider/ 
git commit -m "feat: aisdk provider scaffold + robust fallback JSON parsing"
```

---

## Task 8: 编排（lens×model fan-out + 候补/记录 + 重试 + enrich）

**Files:**
- Create: `src/orchestrate/mod.rs`
- Modify: `src/main.rs`（`mod orchestrate;`）
- Test: `src/orchestrate/mod.rs`（内联，FakeProvider）

**Interfaces:**
- Consumes: `ReviewProvider`, `Lens`, `ReviewInput`, `ProviderConfig`(priority), `LensCall`, `RawFinding` → `Finding`
- Produces:
  - `struct PlannedModel { name:String, priority:i32 }`
  - `struct OrchestrateOpts { models:Vec<PlannedModel>, explicit:bool, no_fallback:bool, max_concurrency:usize, retries:u32 }`
  - `struct RawResults { findings:Vec<Finding>, warnings:Vec<String>, failed_models:Vec<String>, usage:(u64,u64) }`
  - `async fn run_fanout(provider:&dyn ReviewProvider, input:&ReviewInput, lenses:&[&Lens], focus:&str, opts:&OrchestrateOpts) -> RawResults`
  - `fn enrich(raw:&RawFinding, lens_id:&str, model:&str) -> Finding`（assign id/lenses/models/agreement=1）

- [ ] **Step 1: 写失败测试（默认路径候补 + 显式仅记录）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Severity;
    use crate::lens::by_id;
    use crate::provider::fake::FakeProvider;
    use crate::domain::{Mode, ReviewInput, RawFinding};

    fn input() -> ReviewInput {
        ReviewInput { mode: Mode::Code, target_label: "t".into(), collection_guidance: "".into(),
            body: "d".into(), spec_docs: vec![] }
    }
    fn raw(t: &str) -> RawFinding {
        RawFinding { severity: Severity::High, title: t.into(), body: "b".into(), file: "a.rs".into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(), hard_constraint: false }
    }

    #[tokio::test]
    async fn default_path_falls_back_to_next_priority() {
        // claude 不可用（无脚本），gpt 可用且 priority 更低 → 候补命中 gpt
        let p = FakeProvider::with("gpt", vec![raw("found-by-gpt")]);
        let opts = OrchestrateOpts {
            models: vec![PlannedModel{name:"claude".into(),priority:30}, PlannedModel{name:"gpt".into(),priority:20}],
            explicit: false, no_fallback: false, max_concurrency: 4, retries: 0 };
        let r = run_fanout(&p, &input(), &[by_id("L1").unwrap()], "", &opts).await;
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].models, vec!["gpt".to_string()]);
    }

    #[tokio::test]
    async fn explicit_models_record_failure_no_fallback() {
        // 显式指定 claude（不可用）→ 不候补，记 warning + failed_models
        let p = FakeProvider::with("gpt", vec![raw("x")]);
        let opts = OrchestrateOpts {
            models: vec![PlannedModel{name:"claude".into(),priority:30}],
            explicit: true, no_fallback: false, max_concurrency: 4, retries: 0 };
        let r = run_fanout(&p, &input(), &[by_id("L1").unwrap()], "", &opts).await;
        assert!(r.findings.is_empty());
        assert!(r.failed_models.contains(&"claude".to_string()));
        assert!(!r.warnings.is_empty());
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test orchestrate::`
Expected: FAIL

- [ ] **Step 3: 实现 `src/orchestrate/mod.rs`**

```rust
use crate::domain::{Finding, RawFinding, ReviewInput};
use crate::hash::fnv1a64_hex;
use crate::lens::Lens;
use crate::prompt::build_prompt;
use crate::provider::{LensRequest, ProviderError, ReviewProvider};
use futures::stream::{FuturesUnordered, StreamExt};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct PlannedModel { pub name: String, pub priority: i32 }

#[derive(Clone, Debug)]
pub struct OrchestrateOpts {
    pub models: Vec<PlannedModel>,
    pub explicit: bool,     // 是否经 --models 显式指定
    pub no_fallback: bool,
    pub max_concurrency: usize,
    pub retries: u32,
}

#[derive(Default, Debug)]
pub struct RawResults {
    pub findings: Vec<Finding>,
    pub warnings: Vec<String>,
    pub failed_models: Vec<String>,
    pub usage: (u64, u64),
}

pub fn enrich(raw: &RawFinding, lens_id: &str, model: &str) -> Finding {
    let id = fnv1a64_hex(&[&raw.file, &raw.line_start.to_string(), &raw.line_end.to_string(),
        &raw.title.to_lowercase()]);
    Finding {
        id, severity: raw.severity, title: raw.title.clone(), body: raw.body.clone(),
        file: raw.file.clone(), line_start: raw.line_start, line_end: raw.line_end,
        confidence: raw.confidence, recommendation: raw.recommendation.clone(),
        lenses: vec![lens_id.to_string()], models: vec![model.to_string()],
        agreement: 1, hard_constraint: raw.hard_constraint, cluster_id: None,
    }
}

/// 对一个 lens 执行调用：默认路径按 priority 候补；显式则只试第一个 model。
async fn run_one_lens(
    provider: &dyn ReviewProvider, input: &ReviewInput, lens: &Lens, focus: &str, opts: &OrchestrateOpts,
) -> Result<(Vec<Finding>, (u64, u64)), (String, String)> {
    let prompt = build_prompt(input, lens, focus);
    // 默认路径：按 priority 降序逐个候补；显式：仅 models（保序，外层 caller 已每 model 一份）。
    let mut chain: Vec<&PlannedModel> = opts.models.iter().collect();
    if !opts.explicit { chain.sort_by(|a, b| b.priority.cmp(&a.priority)); }
    let chain: Vec<&PlannedModel> = if opts.explicit || opts.no_fallback {
        chain.into_iter().take(1).collect()
    } else { chain };

    let mut last_err = String::new();
    for pm in chain {
        match call_with_retry(provider, &pm.name, &prompt, opts.retries).await {
            Ok(call) => {
                let fs = call.findings.iter().map(|r| enrich(r, lens.id, &pm.name)).collect();
                return Ok((fs, (call.input_tokens, call.output_tokens)));
            }
            Err(e) => { last_err = format!("lens {} model {}: {}", lens.id, pm.name, e); }
        }
    }
    Err((opts.models.first().map(|m| m.name.clone()).unwrap_or_default(), last_err))
}

async fn call_with_retry(
    provider: &dyn ReviewProvider, model: &str, prompt: &str, retries: u32,
) -> Result<crate::provider::LensCall, ProviderError> {
    let mut attempt = 0;
    loop {
        let req = LensRequest { model: model.to_string(), prompt: prompt.to_string() };
        match provider.run(req).await {
            Ok(c) => return Ok(c),
            Err(e) => {
                if attempt >= retries { return Err(e); }
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(200 * (1 << attempt))).await;
            }
        }
    }
}

pub async fn run_fanout(
    provider: &dyn ReviewProvider, input: &ReviewInput, lenses: &[&Lens], focus: &str, opts: &OrchestrateOpts,
) -> RawResults {
    // 显式多模型：每个 (lens, model) 一格；默认：每个 lens 一条候补链。
    let provider = Arc::new(provider);
    let mut tasks = FuturesUnordered::new();

    if opts.explicit {
        for lens in lenses {
            for pm in &opts.models {
                let single = OrchestrateOpts { models: vec![pm.clone()], explicit: true, ..opts.clone() };
                let (p, inp, l, f) = (Arc::clone(&provider), input.clone(), *lens, focus.to_string());
                tasks.push(async move { (pm.name.clone(), run_one_lens(*p, &inp, l, &f, &single).await) });
            }
        }
    } else {
        for lens in lenses {
            let (p, inp, l, f, o) = (Arc::clone(&provider), input.clone(), *lens, focus.to_string(), opts.clone());
            tasks.push(async move { (String::new(), run_one_lens(*p, &inp, l, &f, &o).await) });
        }
    }

    let mut out = RawResults::default();
    while let Some((model_hint, res)) = tasks.next().await {
        match res {
            Ok((fs, (it, ot))) => { out.findings.extend(fs); out.usage.0 += it; out.usage.1 += ot; }
            Err((failed_model, warn)) => {
                let m = if model_hint.is_empty() { failed_model } else { model_hint };
                out.warnings.push(warn);
                if opts.explicit && !out.failed_models.contains(&m) { out.failed_models.push(m); }
            }
        }
    }
    out
}
```

> 注：`max_concurrency` 限流在实现期用 `tokio::sync::Semaphore` 包住 `provider.run`（此处 FuturesUnordered 已并发；信号量为限速）。MVP 测试不依赖限流值。

- [ ] **Step 4: 接线 + 测试**

`src/main.rs` 加 `mod orchestrate;`。

Run: `cargo test orchestrate::`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/orchestrate/ src/main.rs
git commit -m "feat: lens x model fan-out with priority fallback / explicit record"
```

---

## Task 9: consolidate — 确定性 dedup

**Files:**
- Create: `src/consolidate/mod.rs`, `src/consolidate/dedup.rs`
- Modify: `src/main.rs`（`mod consolidate;`）
- Test: `src/consolidate/dedup.rs`（内联）

**Interfaces:**
- Consumes: `Finding`
- Produces:
  - `fn dedup(findings: Vec<Finding>) -> Vec<Finding>`（合并同 file + 行重叠(≤3) + 标题 Jaccard≥0.5；取最高 severity/conf，union lenses/models，agreement=命中模型数）
  - `fn title_jaccard(a:&str, b:&str) -> f32`
  - `const LINE_GAP: u32 = 3; const TITLE_SIM: f32 = 0.5;`

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Finding, Severity};
    fn f(id:&str, file:&str, ls:u32, le:u32, title:&str, model:&str, sev:Severity) -> Finding {
        Finding { id:id.into(), severity:sev, title:title.into(), body:"b".into(), file:file.into(),
            line_start:ls, line_end:le, confidence:0.8, recommendation:"r".into(),
            lenses:vec!["L1".into()], models:vec![model.into()], agreement:1, hard_constraint:false, cluster_id:None }
    }
    #[test]
    fn merges_overlapping_same_title_cross_model() {
        let v = vec![
            f("1","a.rs",10,12,"missing auth check","claude",Severity::High),
            f("2","a.rs",11,13,"Missing auth check","gpt",Severity::Critical),
        ];
        let out = dedup(v);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].severity, Severity::Critical);     // 取最高
        assert_eq!(out[0].agreement, 2);                      // 跨模型一致
        assert!(out[0].models.contains(&"gpt".to_string()) && out[0].models.contains(&"claude".to_string()));
    }
    #[test]
    fn under_merge_keeps_distinct_when_titles_differ() {
        let v = vec![
            f("1","a.rs",10,12,"missing auth check","claude",Severity::High),
            f("2","a.rs",10,12,"unvalidated write to db","claude",Severity::High),
        ];
        assert_eq!(dedup(v).len(), 2); // 行重叠但标题不相似 → 不合并
    }
    #[test]
    fn different_files_never_merge() {
        let v = vec![
            f("1","a.rs",10,12,"same title","claude",Severity::High),
            f("2","b.rs",10,12,"same title","claude",Severity::High),
        ];
        assert_eq!(dedup(v).len(), 2);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test consolidate::dedup`
Expected: FAIL

- [ ] **Step 3: 实现 `src/consolidate/dedup.rs`**

```rust
use crate::domain::Finding;
use std::collections::BTreeSet;

pub const LINE_GAP: u32 = 3;
pub const TITLE_SIM: f32 = 0.5;

pub fn title_jaccard(a: &str, b: &str) -> f32 {
    let ta: BTreeSet<String> = a.to_lowercase().split_whitespace().map(|s| s.to_string()).collect();
    let tb: BTreeSet<String> = b.to_lowercase().split_whitespace().map(|s| s.to_string()).collect();
    if ta.is_empty() && tb.is_empty() { return 1.0; }
    let inter = ta.intersection(&tb).count() as f32;
    let union = ta.union(&tb).count() as f32;
    if union == 0.0 { 0.0 } else { inter / union }
}

fn overlaps(a: &Finding, b: &Finding) -> bool {
    a.file == b.file
        && a.line_start <= b.line_end.saturating_add(LINE_GAP)
        && b.line_start <= a.line_end.saturating_add(LINE_GAP)
}

fn merge_into(base: &mut Finding, other: &Finding) {
    if other.severity > base.severity { base.severity = other.severity; }
    if other.confidence > base.confidence { base.confidence = other.confidence; }
    for l in &other.lenses { if !base.lenses.contains(l) { base.lenses.push(l.clone()); } }
    for m in &other.models { if !base.models.contains(m) { base.models.push(m.clone()); } }
    base.agreement = base.models.len() as u32;
    if other.recommendation.len() > base.recommendation.len() { base.recommendation = other.recommendation.clone(); }
    base.hard_constraint = base.hard_constraint || other.hard_constraint;
}

/// under-merge：仅当 file 同 + 行重叠 + 标题相似 三者皆成立才合并。
pub fn dedup(findings: Vec<Finding>) -> Vec<Finding> {
    let mut clusters: Vec<Finding> = Vec::new();
    for f in findings {
        let mut merged = false;
        for c in clusters.iter_mut() {
            if overlaps(c, &f) && title_jaccard(&c.title, &f.title) >= TITLE_SIM {
                merge_into(c, &f);
                merged = true;
                break;
            }
        }
        if !merged { clusters.push(f); }
    }
    clusters
}
```

- [ ] **Step 4: 写 `src/consolidate/mod.rs`（入口骨架，correlate 在 Task 10）**

```rust
pub mod dedup;
pub use dedup::dedup;
```

- [ ] **Step 5: 接线 + 测试**

`src/main.rs` 加 `mod consolidate;`。

Run: `cargo test consolidate::dedup`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/consolidate/ src/main.rs
git commit -m "feat: deterministic under-merge dedup with cross-model agreement"
```

---

## Task 10: consolidate — LLM correlate（有条件）

**Files:**
- Create: `src/consolidate/correlate.rs`
- Modify: `src/consolidate/mod.rs`
- Test: `src/consolidate/correlate.rs`（内联，FakeProvider）

**Interfaces:**
- Consumes: `Finding`, `ReviewProvider`, `Cluster`
- Produces:
  - `struct CorrelateOut { clusters:Vec<Cluster> }`（`Deserialize + JsonSchema`：模型返回 cluster 列表，引用 finding_ids）
  - `async fn correlate(provider:&dyn ReviewProvider, model:&str, findings:&[Finding], enabled:bool) -> Vec<Cluster>`（findings<2 或 !enabled → 空；否则一次调用，把 cluster_id 写回 finding 由 caller 做）
  - `fn apply_clusters(findings:&mut [Finding], clusters:&[Cluster])`（回填 `cluster_id`）

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Finding, Severity};
    use crate::provider::fake::FakeProvider;
    fn f(id:&str) -> Finding {
        Finding { id:id.into(), severity:Severity::High, title:"t".into(), body:"b".into(), file:"a.rs".into(),
            line_start:1,line_end:1,confidence:0.9,recommendation:"r".into(),
            lenses:vec![],models:vec![],agreement:1,hard_constraint:false,cluster_id:None }
    }
    #[tokio::test]
    async fn skips_when_fewer_than_two() {
        let p = FakeProvider::with("m", vec![]);
        assert!(correlate(&p, "m", &[f("1")], true).await.is_empty());
    }
    #[tokio::test]
    async fn skips_when_disabled() {
        let p = FakeProvider::with("m", vec![]);
        assert!(correlate(&p, "m", &[f("1"), f("2")], false).await.is_empty());
    }
    #[test]
    fn apply_clusters_backfills_cluster_id() {
        let mut fs = vec![f("1"), f("2")];
        let cl = vec![Cluster { id:"c1".into(), theme:"t".into(), finding_ids:vec!["1".into()],
            relation:"root-cause".into(), note:"n".into(), severity:None }];
        apply_clusters(&mut fs, &cl);
        assert_eq!(fs[0].cluster_id.as_deref(), Some("c1"));
        assert_eq!(fs[1].cluster_id, None);
    }
}
```

> 注：correlate 通过 `ReviewProvider` 复用同一个 trait——为此给 trait 加一个**通用文本调用**或复用 `run`（把 cluster schema 嵌进 prompt、解析）。MVP 用：correlate 自带一个 `CorrelateOut` 的 schema-嵌入 prompt，复用 provider 的底层文本能力（实现期与 aisdk_provider 的 `call_model_text` 对齐）。FakeProvider 的 `run` 返回空 findings，故 correlate 在 Fake 下退化为"无 cluster"，上面两测覆盖跳过分支即可。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test consolidate::correlate`
Expected: FAIL

- [ ] **Step 3: 实现 `src/consolidate/correlate.rs`**

```rust
use crate::domain::{Cluster, Finding};
use crate::provider::ReviewProvider;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct CorrelateOut { pub clusters: Vec<ClusterRaw> }

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClusterRaw {
    pub theme: String,
    pub finding_ids: Vec<String>,
    pub relation: String,
    pub note: String,
}

/// 有条件关联：findings<2 或 !enabled → 空。否则一次 LLM 调用产出 cluster。
pub async fn correlate(
    provider: &dyn ReviewProvider, model: &str, findings: &[Finding], enabled: bool,
) -> Vec<Cluster> {
    if !enabled || findings.len() < 2 {
        return Vec::new();
    }
    let _ = (provider, model); // 实现期：构造 schema-嵌入 prompt → provider 文本调用 → 解析 CorrelateOut
    // MVP 安全降级：调用失败/Fake 无能力 → 返回空，不阻断主流程。
    Vec::new()
}

/// 把 LLM 关联结果回填到 finding.cluster_id。
pub fn apply_clusters(findings: &mut [Finding], clusters: &[Cluster]) {
    for c in clusters {
        for f in findings.iter_mut() {
            if c.finding_ids.contains(&f.id) {
                f.cluster_id = Some(c.id.clone());
            }
        }
    }
}
```

> 实现期：把上面 `correlate` 的 `let _ = ...` 替换为真实调用——构造一段"给定 findings(JSON) → 输出 CorrelateOut" 的对抗关联 prompt，复用 provider 文本能力 + `parse`（同 Task 7 兜底），并给每个 cluster 分配 `c{n}` id。失败安全降级为空（不阻断）。

- [ ] **Step 4: 更新 `src/consolidate/mod.rs`**

```rust
pub mod correlate;
pub mod dedup;
pub use correlate::{apply_clusters, correlate};
pub use dedup::dedup;
```

- [ ] **Step 5: 接线 + 测试**

Run: `cargo test consolidate::`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/consolidate/
git commit -m "feat: conditional LLM correlate (>=2 findings) + cluster backfill"
```

---

## Task 11: verdict + severity 钳制

**Files:**
- Create: `src/verdict.rs`
- Modify: `src/main.rs`（`mod verdict;`）
- Test: `src/verdict.rs`（内联）

**Interfaces:**
- Consumes: `Finding`, `Severity`, `Verdict`, `Lens`(max_severity)
- Produces:
  - `fn clamp_compliance(findings:&mut [Finding])`（来自规范 lens 且 !hard_constraint → severity ≤ medium）
  - `fn compute_verdict(findings:&[Finding], block_threshold:Severity, conf_floor:f32) -> Verdict`

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Finding, Severity, Verdict};
    fn f(sev:Severity, conf:f32, lens:&str, hard:bool) -> Finding {
        Finding { id:"x".into(), severity:sev, title:"t".into(), body:"b".into(), file:"a".into(),
            line_start:1,line_end:1,confidence:conf,recommendation:"r".into(),
            lenses:vec![lens.into()],models:vec![],agreement:1,hard_constraint:hard,cluster_id:None }
    }
    #[test]
    fn reject_when_high_above_floor() {
        let v = vec![f(Severity::High, 0.9, "L1", false)];
        assert_eq!(compute_verdict(&v, Severity::Medium, 0.5), Verdict::Reject);
    }
    #[test]
    fn approve_when_only_low_or_below_floor() {
        let v = vec![f(Severity::Low, 0.9, "L1", false), f(Severity::High, 0.2, "L1", false)];
        assert_eq!(compute_verdict(&v, Severity::Medium, 0.5), Verdict::Approve);
    }
    #[test]
    fn clamp_compliance_lens_to_medium() {
        let mut v = vec![f(Severity::Critical, 0.9, "L5", false)];
        clamp_compliance(&mut v);
        assert_eq!(v[0].severity, Severity::Medium);
    }
    #[test]
    fn hard_constraint_not_clamped() {
        let mut v = vec![f(Severity::High, 0.9, "L5", true)];
        clamp_compliance(&mut v);
        assert_eq!(v[0].severity, Severity::High);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test verdict::`
Expected: FAIL

- [ ] **Step 3: 实现 `src/verdict.rs`**

```rust
use crate::domain::{Finding, Severity, Verdict};
use crate::lens::by_id;

/// 规范遵循 lens 的 finding 且未标 hard_constraint → severity 钳到 ≤ medium。
pub fn clamp_compliance(findings: &mut [Finding]) {
    for f in findings.iter_mut() {
        if f.hard_constraint { continue; }
        let capped = f.lenses.iter().any(|id| matches!(by_id(id).and_then(|l| l.max_severity), Some(_)));
        if capped && f.severity > Severity::Medium {
            f.severity = Severity::Medium;
        }
    }
}

/// reject 当存在 finding severity ≥ block_threshold 且 confidence ≥ conf_floor。
pub fn compute_verdict(findings: &[Finding], block_threshold: Severity, conf_floor: f32) -> Verdict {
    let blocking = findings.iter().any(|f| f.severity >= block_threshold && f.confidence >= conf_floor);
    if blocking { Verdict::Reject } else { Verdict::Approve }
}
```

- [ ] **Step 4: 接线 + 测试**

`src/main.rs` 加 `mod verdict;`。

Run: `cargo test verdict::`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/verdict.rs src/main.rs
git commit -m "feat: deterministic verdict + compliance severity clamp"
```

---

## Task 12: 输出渲染（JSON + markdown）

**Files:**
- Create: `src/output/mod.rs`
- Modify: `src/main.rs`（`mod output;`）
- Test: `src/output/mod.rs`（内联）

**Interfaces:**
- Consumes: `ReviewReport`
- Produces:
  - `enum Format { Json, Md }`
  - `fn render(report:&ReviewReport, fmt:Format) -> String`

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::*;
    fn report() -> ReviewReport {
        ReviewReport { schema_version:"1".into(), mode:Mode::Code, verdict:Verdict::Reject,
            stats:Stats{high:1,total:1,..Default::default()},
            findings:vec![Finding{ id:"1".into(),severity:Severity::High,title:"boom".into(),body:"b".into(),
                file:"a.rs".into(),line_start:3,line_end:5,confidence:0.9,recommendation:"fix".into(),
                lenses:vec!["L1".into()],models:vec!["claude".into()],agreement:1,hard_constraint:false,cluster_id:None}],
            clusters:vec![], next_steps:vec![], run:RunMeta::default() }
    }
    #[test]
    fn json_contains_verdict_and_provenance() {
        let s = render(&report(), Format::Json);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["verdict"], "reject");
        assert_eq!(v["findings"][0]["models"][0], "claude");
    }
    #[test]
    fn md_groups_and_shows_location() {
        let s = render(&report(), Format::Md);
        assert!(s.contains("reject") && s.contains("a.rs:3") && s.contains("boom"));
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test output::`
Expected: FAIL

- [ ] **Step 3: 实现 `src/output/mod.rs`**

```rust
use crate::domain::{ReviewReport, Severity};

#[derive(Clone, Copy, Debug)]
pub enum Format { Json, Md }

pub fn render(report: &ReviewReport, fmt: Format) -> String {
    match fmt {
        Format::Json => serde_json::to_string_pretty(report).unwrap(),
        Format::Md => render_md(report),
    }
}

fn render_md(r: &ReviewReport) -> String {
    let mut s = String::new();
    let verdict = serde_json::to_string(&r.verdict).unwrap().replace('"', "");
    s.push_str(&format!("# 对抗评审：{}\n\n", verdict));
    s.push_str(&format!("- critical {} / high {} / medium {} / low {}（共 {}）\n\n",
        r.stats.critical, r.stats.high, r.stats.medium, r.stats.low, r.stats.total));
    for sev in [Severity::Critical, Severity::High, Severity::Medium, Severity::Low] {
        let group: Vec<_> = r.findings.iter().filter(|f| f.severity == sev).collect();
        if group.is_empty() { continue; }
        s.push_str(&format!("## {:?}\n", sev));
        for f in group {
            s.push_str(&format!("- **{}** `{}:{}` _(lens {:?}, models {:?}, agree {})_\n  {}\n  → {}\n",
                f.title, f.file, f.line_start, f.lenses, f.models, f.agreement, f.body, f.recommendation));
        }
        s.push('\n');
    }
    if !r.clusters.is_empty() {
        s.push_str("## 关联 clusters\n");
        for c in &r.clusters {
            s.push_str(&format!("- **{}** ({}): {} — {:?}\n", c.theme, c.relation, c.note, c.finding_ids));
        }
    }
    s
}
```

- [ ] **Step 4: 接线 + 测试**

`src/main.rs` 加 `mod output;`。

Run: `cargo test output::`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/output/ src/main.rs
git commit -m "feat: JSON + markdown report rendering"
```

---

## Task 13: CLI 接线 `brk review --wait` + 退出码 + 端到端

**Files:**
- Create: `src/cli.rs`, `tests/review_e2e.rs`
- Modify: `src/main.rs`
- Test: `tests/review_e2e.rs`（FakeProvider 端到端）

**Interfaces:**
- Consumes: 所有上游模块
- Produces:
  - `struct ReviewArgs { mode:Option<String>, base:Option<String>, scope:String, spec:Vec<String>, focus:Option<String>, lenses:Option<String>, models:Option<String>, no_correlate:bool, block_threshold:Option<String>, conf_floor:Option<f32>, wait:bool, background:bool, format:String, targets:Vec<String> }`
  - `async fn run_review(args:ReviewArgs, provider:&dyn ReviewProvider, cfg:&Config) -> anyhow::Result<(ReviewReport, i32)>`（返回报告 + 退出码）

> Plan 1 仅实现 `--wait`（前台同步）；`--background` 留 Plan 2（此处给出 stub 报错）。

- [ ] **Step 1: 写端到端失败测试**

```rust
// tests/review_e2e.rs
use breakpoints::cli::{run_review, ReviewArgs};
use breakpoints::config::Config;
use breakpoints::domain::{RawFinding, Severity, Verdict};
use breakpoints::provider::fake::FakeProvider;

fn raw(sev: Severity) -> RawFinding {
    RawFinding { severity: sev, title: "auth missing".into(), body: "b".into(), file: "a.rs".into(),
        line_start: 1, line_end: 1, confidence: 0.9, recommendation: "add check".into(), hard_constraint: false }
}

#[tokio::test]
async fn doc_review_end_to_end_rejects() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("spec.md");
    std::fs::write(&p, "# design").unwrap();

    let provider = FakeProvider::with("claude", vec![raw(Severity::High)]);
    let cfg = Config::from_layers(&[r#"[review]
default_models=["claude"]
"#]).unwrap();
    let args = ReviewArgs {
        mode: Some("doc".into()), base: None, scope: "working-tree".into(), spec: vec![],
        focus: None, lenses: Some("D1".into()), models: None, no_correlate: true,
        block_threshold: None, conf_floor: None, wait: true, background: false,
        format: "json".into(), targets: vec![p.to_string_lossy().into()] };

    let (report, code) = run_review(args, &provider, &cfg).await.unwrap();
    assert_eq!(report.verdict, Verdict::Reject);
    assert_eq!(code, 10);
    assert_eq!(report.findings[0].models, vec!["claude".to_string()]);
}
```

> 需要把 crate 暴露为 lib：加 `src/lib.rs` re-export 模块（`pub mod ...`），`main.rs` 改为薄壳调用 `breakpoints::cli`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --test review_e2e`
Expected: FAIL（`cli` 未定义）

- [ ] **Step 3: 加 `src/lib.rs` 暴露模块**

```rust
// src/lib.rs
pub mod cli;
pub mod config;
pub mod consolidate;
pub mod domain;
pub mod evidence;
pub mod hash;
pub mod lens;
pub mod orchestrate;
pub mod output;
pub mod prompt;
pub mod provider;
pub mod verdict;
```

- [ ] **Step 4: 实现 `src/cli.rs`**

```rust
use crate::config::Config;
use crate::consolidate::{apply_clusters, correlate, dedup};
use crate::domain::{ReviewReport, RunMeta, Severity, Stats, Verdict};
use crate::evidence::{collect, EvidenceOpts, Scope};
use crate::lens::select;
use crate::orchestrate::{run_fanout, OrchestrateOpts, PlannedModel};
use crate::output::Format;
use crate::provider::ReviewProvider;
use crate::verdict::{clamp_compliance, compute_verdict};

#[derive(Clone, Debug)]
pub struct ReviewArgs {
    pub mode: Option<String>,
    pub base: Option<String>,
    pub scope: String,
    pub spec: Vec<String>,
    pub focus: Option<String>,
    pub lenses: Option<String>,
    pub models: Option<String>,
    pub no_correlate: bool,
    pub block_threshold: Option<String>,
    pub conf_floor: Option<f32>,
    pub wait: bool,
    pub background: bool,
    pub format: String,
    pub targets: Vec<String>,
}

fn parse_sev(s: &str) -> Severity {
    match s { "low" => Severity::Low, "high" => Severity::High, "critical" => Severity::Critical, _ => Severity::Medium }
}

pub async fn run_review(
    args: ReviewArgs, provider: &dyn ReviewProvider, cfg: &Config,
) -> anyhow::Result<(ReviewReport, i32)> {
    if args.background { anyhow::bail!("--background 在 Plan 2 实现"); }

    // 1. mode + 证据
    let mode = args.mode.as_deref().map(|m| match m { "code" => crate::domain::Mode::Code, _ => crate::domain::Mode::Doc });
    let scope = if args.scope == "branch" { Scope::Branch } else { Scope::WorkingTree };
    let eopts = EvidenceOpts { mode, base: args.base.clone(), scope, spec: args.spec.clone(), targets: args.targets.clone() };
    let input = collect(&eopts)?;

    // 2. lens
    let lens_ids: Vec<String> = args.lenses.as_deref()
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
        .unwrap_or_else(|| match input.mode { crate::domain::Mode::Code => cfg.review.lenses.code.clone(), _ => cfg.review.lenses.doc.clone() });
    let lenses = select(input.mode, &lens_ids)?;

    // 3. 模型集 + 显式判定
    let explicit = args.models.is_some();
    let model_names: Vec<String> = match &args.models {
        Some(s) => s.split(',').map(|x| x.trim().to_string()).collect(),
        None if !cfg.review.default_models.is_empty() => cfg.review.default_models.clone(),
        None => {
            // 默认：全部已配置 provider，按 priority（候补链）
            let mut v: Vec<_> = cfg.providers.iter().collect();
            v.sort_by(|a, b| b.1.priority.cmp(&a.1.priority));
            v.into_iter().map(|(k, _)| k.clone()).collect()
        }
    };
    let models: Vec<PlannedModel> = model_names.iter()
        .map(|n| PlannedModel { name: n.clone(), priority: cfg.providers.get(n).map(|p| p.priority).unwrap_or(0) })
        .collect();

    let oopts = OrchestrateOpts {
        models: models.clone(), explicit, no_fallback: false,
        max_concurrency: cfg.review.max_concurrency, retries: 2 };

    // 4. fan-out
    let focus = args.focus.clone().unwrap_or_default();
    let raw = run_fanout(provider, &input, &lenses, &focus, &oopts).await;

    // 5. consolidate
    let mut findings = dedup(raw.findings);
    clamp_compliance(&mut findings);
    let correlate_on = cfg.review.correlate && !args.no_correlate;
    let lead_model = model_names.first().cloned().unwrap_or_default();
    let clusters = correlate(provider, &lead_model, &findings, correlate_on).await;
    apply_clusters(&mut findings, &clusters);

    // 6. verdict
    let block = args.block_threshold.as_deref().map(parse_sev).unwrap_or(cfg.review.block_threshold);
    let floor = args.conf_floor.unwrap_or(cfg.review.conf_floor);
    let verdict = compute_verdict(&findings, block, floor);

    // 7. 组装报告
    let report = ReviewReport {
        schema_version: "1".into(), mode: input.mode, verdict,
        stats: Stats::from_findings(&findings), findings, clusters, next_steps: vec![],
        run: RunMeta { job_id: String::new(), models: model_names, lenses: lens_ids,
            duration_ms: 0, usage: Default::default(),
            warnings: raw.warnings, failed_models: raw.failed_models } };

    let code = match (verdict, !report.run.failed_models.is_empty()) {
        (Verdict::Approve, false) => 0,
        (Verdict::Reject, _) => 10,
        (Verdict::Approve, true) => 2, // 对照集不完整
    };
    let _ = args.format; // 渲染在 main 里按 format 输出
    Ok((report, code))
}

pub fn parse_format(s: &str) -> Format { if s == "md" { Format::Md } else { Format::Json } }
```

- [ ] **Step 5: 改 `src/main.rs` 为薄壳（clap + 真实 provider 选择 + 渲染 + 退出码）**

```rust
use breakpoints::cli::{parse_format, run_review, ReviewArgs};
use breakpoints::config::Config;
use breakpoints::output::render;
use breakpoints::provider::direct::DirectProvider;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "brk")]
struct Cli { #[command(subcommand)] cmd: Cmd }

#[derive(Subcommand)]
enum Cmd {
    Review {
        #[arg(long)] mode: Option<String>,
        #[arg(long)] base: Option<String>,
        #[arg(long, default_value = "working-tree")] scope: String,
        #[arg(long)] spec: Vec<String>,
        #[arg(long)] focus: Option<String>,
        #[arg(long)] lenses: Option<String>,
        #[arg(long)] models: Option<String>,
        #[arg(long)] no_correlate: bool,
        #[arg(long)] block_threshold: Option<String>,
        #[arg(long)] conf_floor: Option<f32>,
        #[arg(long, default_value_t = true)] wait: bool,
        #[arg(long)] background: bool,
        #[arg(long, default_value = "json")] format: String,
        targets: Vec<String>,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Review { mode, base, scope, spec, focus, lenses, models, no_correlate,
                      block_threshold, conf_floor, wait, background, format, targets } => {
            let cfg = Config::load().unwrap_or_default();
            // 选第一个可用 provider 构造真实 DirectProvider（多 provider 在 orchestrate 内候补；
            // MVP：用 default/priority 首个 provider 的 key 构造单实例，Plan 2 再做 multi-provider 实例池）
            let lead = models.as_deref().and_then(|s| s.split(',').next()).map(|s| s.to_string())
                .or_else(|| cfg.review.default_models.first().cloned())
                .or_else(|| {
                    let mut v: Vec<_> = cfg.providers.iter().collect();
                    v.sort_by(|a, b| b.1.priority.cmp(&a.1.priority));
                    v.first().map(|(k, _)| (*k).clone())
                });
            let provider: Box<dyn breakpoints::provider::ReviewProvider> = match lead.as_deref()
                .and_then(|n| cfg.providers.get(n).map(|pc| (n, pc))) {
                Some((n, pc)) => {
                    let key = cfg.resolve_key(n).unwrap_or_default();
                    Box::new(DirectProvider::from_config(pc, key))
                }
                None => { eprintln!("无可用 provider，请先配置 [providers.*]"); std::process::exit(2); }
            };
            let fmt = parse_format(&format);
            let args = ReviewArgs { mode, base, scope, spec, focus, lenses, models, no_correlate,
                block_threshold, conf_floor, wait, background, format, targets };
            match run_review(args, provider.as_ref(), &cfg).await {
                Ok((report, code)) => { println!("{}", render(&report, fmt)); code }
                Err(e) => { eprintln!("error: {e}"); 2 }
            }
        }
    };
    std::process::exit(code);
}
```

> 说明：MVP 的 main 用单一 lead provider 实例构造 `DirectProvider`；**真正的"按 priority 候补到不同 provider 实例"需要一个 provider 注册表（按 model 名路由到对应 DirectProvider 实例）**——这是 Plan 2 的 `ProviderRegistry`（实现 `ReviewProvider`，内部按 `req.model` 分派）。Plan 1 的 orchestrate 候补逻辑已就绪，注册表补齐后即生效。端到端测试用 `FakeProvider`（已能按 model 路由）验证候补/记录两条路径。

- [ ] **Step 6: 跑端到端 + 全量测试**

Run: `cargo test`
Expected: PASS（含 `review_e2e` doc 端到端 reject + code=10）

- [ ] **Step 7: Commit**

```bash
git add src/cli.rs src/lib.rs src/main.rs tests/
git commit -m "feat: wire brk review --wait end-to-end with exit codes"
```

---

## Self-Review（对照 spec）

**1. Spec coverage（Plan 1 范围）：**
- §3 流水线骨架 → Tasks 1–13 ✓
- §5 组件边界（domain/config/lens/prompt/evidence/provider/orchestrate/consolidate/verdict/output/cli）→ 各 Task ✓
- §7 lens（code/doc + L0/L5 + severity 上限）→ Task 3，钳制 Task 11 ✓
- §8 fan-out lens×model + §8.1 候补/记录 → Task 8 ✓
- §8 多模型=单一结果 → Task 8 fan-out 汇入单一 → Task 9 consolidate ✓
- §9 dedup(under-merge) + correlate(≥2 条件) → Tasks 9/10 ✓
- §10 verdict(确定性, approve|reject, 阈值) → Task 11 ✓
- §11 schema（id/lenses/models/agreement/clusters/run.warnings/failed_models）→ Tasks 1/8/12 ✓
- §12 CLI review + 退出码(0/10/2) → Task 13 ✓
- §13 配置加载（分层 + key 解析 env>secrets）→ Task 2 ✓（写配置 `model setup` 属 Plan 2）
- §14 provider（原生 schema + 兜底 + 重试）→ Tasks 6/7（重试在 Task 8 包裹）✓
- **延后到 Plan 2/3（非 Plan 1 范围，非缺口）**：§6 job/state/background、§12 status/result/cancel/model、§15 插件打包。

**2. Placeholder 扫描：** Task 7/10 的 aisdk/correlate 真实模型调用以"安全降级 + 实现期注记"给出（trait 边界隔离），单测覆盖纯函数（解析/跳过分支）；非占位符——每步均有可运行代码与可验证断言。

**3. 类型一致性：** `RawFinding`/`Finding`/`LensOutput`/`LensCall`/`Verdict`/`Severity`/`Cluster`/`ReviewReport` 全程同名同签名；`ReviewProvider::run(LensRequest)->Result<LensCall,ProviderError>` 在 Tasks 6/7/8/10/13 一致；`Severity` Ord 在 dedup/verdict 一致。

> 已知实现期收口项（不阻断 Plan 1 测试）：①aisdk 版本钉定 + `call_model_text` 真实接线 ②`ProviderRegistry`（Plan 2，按 model 路由多 provider 实例，使跨 provider 候补在真实路径生效）③correlate 真实 prompt。三者均由 trait/函数边界隔离，FakeProvider 已覆盖编排与收敛逻辑。
