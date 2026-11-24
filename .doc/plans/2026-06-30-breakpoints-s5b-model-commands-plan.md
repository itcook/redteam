# breakpoints S5-b — model 命令（setup / list / check） Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development（推荐）。Steps 用 checkbox（`- [ ]`）跟踪。

**Goal:** 补齐 S5 运营面的 model 命令半——`brk model setup`（写 provider 配置，主配置不含明文 key）、`brk model list`（列已配置 + 可用性 + 配置位置，`--remote` 查 /v1/models）、`brk model check`（轻量可用性预检，退出码全可用 0 / 有不可用非 0）。

**Architecture:** 探测抽可注入 `Prober` trait（async）——`HttpProber`(reqwest 真探测)/`FakeProber`(测试)；可用性判定（key 就位 × 端点可达真值表）+ check 退出码逻辑进程内全测，真 HTTP 探测作薄缝。key 默认走 env（主配置仅记 `api_key_env` + 打印 `export VAR=...`；`secrets.toml` chmod 600 作可选）。setup 纯 flag 驱动（交互延后）。

**Tech Stack:** Rust 2021、serde、toml 0.8（写配置/secrets）、reqwest（rustls，探测 GET）、async-trait（Prober）、std::os::unix::fs（chmod 600）。**不新增依赖。**

## Global Constraints

- **引擎层冻结**：src/tools/、src/provider/tool_loop.rs、src/provider/backends.rs 不得改。默认 base 常量（`api.anthropic.com` / `api.openai.com/v1`）在 model 模块**重定义**（backends.rs 的同名 const 私有且冻结）。
- **TLS rustls 无 OpenSSL**（`cargo tree -i openssl-sys` 须空）。复用既有 reqwest（rustls），不引新 crate。
- **密钥卫生（硬约束）**：主配置 `config.toml` **永不含明文 key**——`model setup` 只把 `kind/base_url/model/priority/api_key_env` 写主配置；key 默认走 env（打印 export 指引），可选写 `secrets.toml`（**chmod 600**）。读取优先级 env > secrets（既有 `Config::resolve_key`）。
- **model check = key 就位 + 端点可达**（非 key 鉴权验证；不烧 API 调用）。
- **测试无真实端点/key**：探测逻辑用 `FakeProber` + tempdir（`XDG_CONFIG_HOME` 覆盖）进程内测；真 HTTP 探测（HttpProber）作薄缝 reviewed-not-unit-tested。env 测试用 `ENV_LOCK` 串行化。
- `.doc/`、`.superpowers/` 为 gitignored scratch。

## 文件结构（创建/修改）

- 修改 `src/config/secrets.rs`：新增 `write_secret`（chmod 600）。
- 修改 `src/config/mod.rs`：新增 `write_provider`（toml::Value 插 `[providers.<name>]`）。
- 创建 `src/model/mod.rs`：`Prober` trait + `Availability` + `check_all` + `check_exit_code` + `effective_base` + `FakeProber`（test）+ setup helper（`derive_kind`/`derive_api_key_env`）+ `render_model_list`。
- 创建 `src/model/probe.rs`：`HttpProber`（reqwest 真探测，薄缝）。
- 修改 `src/lib.rs`：`pub mod model;`。
- 修改 `src/main.rs`：`Cmd::Model{action}` + `ModelCmd{Setup/List/Check}` + 三个 handler。

---

### Task 1: secrets 写入（chmod 600）

**Files:**
- Modify: `src/config/secrets.rs`（新增 `write_secret`）
- Test: `src/config/secrets.rs`（内联）

**Interfaces:**
- Produces: `pub fn write_secret(provider: &str, key: &str) -> anyhow::Result<()>`——读现有 secrets → 插/更 provider key → 写回 `secrets.toml` + chmod 600。

- [ ] **Step 1: 写失败测试**（`src/config/secrets.rs` 末尾加 `#[cfg(test)] mod tests`）

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn write_secret_persists_and_chmods_600() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: ENV_LOCK 串行化 env 变更
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()); }
        write_secret("claude", "sk-test-123").unwrap();
        // 读回
        assert_eq!(load_secrets().get("claude").map(|s| s.as_str()), Some("sk-test-123"));
        // 再写一个，保留旧的
        write_secret("gpt", "sk-test-456").unwrap();
        assert_eq!(load_secrets().get("claude").map(|s| s.as_str()), Some("sk-test-123"));
        assert_eq!(load_secrets().get("gpt").map(|s| s.as_str()), Some("sk-test-456"));
        // chmod 600
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(secrets_path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "secrets.toml must be chmod 600");
        }
        unsafe { std::env::remove_var("XDG_CONFIG_HOME"); }
    }
}
```

Run: `cargo test --lib config::secrets::tests::write_secret_persists_and_chmods_600`
Expected: FAIL（`write_secret` 未定义）

- [ ] **Step 2: 实现**（`src/config/secrets.rs`，加在 `dirs_config` 之后）

```rust
/// 写入/更新 secrets.toml 的 provider key，并 chmod 600（密钥物理隔离主配置）。
pub fn write_secret(provider: &str, key: &str) -> anyhow::Result<()> {
    let path = secrets_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut map = load_secrets();
    map.insert(provider.to_string(), key.to_string());
    let text = toml::to_string(&map)?;
    std::fs::write(&path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
```

注：`secrets.rs` 顶部需确保可用 `anyhow`（已是项目依赖；若该文件无 `use`，直接用 `anyhow::Result` 全路径即可，无需额外 import）。

- [ ] **Step 3: 跑测试** Run: `cargo test --lib config::secrets` → PASS（含既有，如有）
- [ ] **Step 4: Commit**
```bash
git add src/config/secrets.rs
git commit -m "feat(s5b): write_secret persists provider key to secrets.toml chmod 600"
```

---

### Task 2: 主配置 provider 写入

**Files:**
- Modify: `src/config/mod.rs`（新增 `write_provider`）
- Test: `src/config/mod.rs`（内联）

**Interfaces:**
- Produces: `pub fn write_provider(name: &str, kind: &str, base_url: Option<&str>, model: &str, priority: i32, api_key_env: Option<&str>) -> anyhow::Result<()>`——把 `[providers.<name>]` 写进用户配置 `config_paths()[0]`（toml::Value 合并，保留其他内容；**绝不写 key**）。

- [ ] **Step 1: 写失败测试**（追加到 `src/config/mod.rs` 的 `mod tests`）

```rust
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn write_provider_persists_block_without_key() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: ENV_LOCK 串行化
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()); }
        // 先放一段已有 [review]，验证保留
        let cfg_path = config_paths()[0].clone();
        std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
        std::fs::write(&cfg_path, "[review]\nblock_threshold=\"high\"\n").unwrap();

        write_provider("local", "openai-compatible", Some("http://localhost:11434/v1"), "qwen", 10, Some("LOCAL_KEY")).unwrap();

        let cfg = Config::load().unwrap();
        let p = cfg.providers.get("local").expect("provider written");
        assert_eq!(p.model, "qwen");
        assert_eq!(p.priority, 10);
        assert_eq!(p.base_url.as_deref(), Some("http://localhost:11434/v1"));
        assert_eq!(p.api_key_env.as_deref(), Some("LOCAL_KEY"));
        // 既有 [review] 保留
        assert_eq!(cfg.review.block_threshold, crate::domain::Severity::High);
        // 配置文本不含明文 key（只有 api_key_env 引用）
        let raw = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(!raw.contains("sk-"), "no plaintext key in config");
        unsafe { std::env::remove_var("XDG_CONFIG_HOME"); }
    }
```

Run: `cargo test --lib config::tests::write_provider_persists_block_without_key`
Expected: FAIL（`write_provider` 未定义）

- [ ] **Step 2: 实现**（`src/config/mod.rs`，加在 `config_paths` 之后）

```rust
/// 把 [providers.<name>] 写进用户配置（config_paths()[0]），toml 深合并保留其他内容。
/// 只写 kind/base_url/model/priority/api_key_env——**绝不写明文 key**。
pub fn write_provider(
    name: &str,
    kind: &str,
    base_url: Option<&str>,
    model: &str,
    priority: i32,
    api_key_env: Option<&str>,
) -> anyhow::Result<()> {
    let path = config_paths()
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no config path"))?;
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut root: toml::Value =
        toml::from_str(&existing).unwrap_or_else(|_| toml::Value::Table(Default::default()));
    let root_t = root
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("config root is not a table"))?;
    let providers = root_t
        .entry("providers".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let providers_t = providers
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[providers] is not a table"))?;

    let mut t = toml::value::Table::new();
    t.insert("kind".into(), toml::Value::String(kind.to_string()));
    t.insert("model".into(), toml::Value::String(model.to_string()));
    if let Some(b) = base_url {
        t.insert("base_url".into(), toml::Value::String(b.to_string()));
    }
    if let Some(e) = api_key_env {
        t.insert("api_key_env".into(), toml::Value::String(e.to_string()));
    }
    t.insert("priority".into(), toml::Value::Integer(priority as i64));
    providers_t.insert(name.to_string(), toml::Value::Table(t));

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, toml::to_string_pretty(&root)?)?;
    Ok(())
}
```

- [ ] **Step 3: 跑测试** Run: `cargo test --lib config` → PASS（既有 config 测试仍绿）
- [ ] **Step 4: Commit**
```bash
git add src/config/mod.rs
git commit -m "feat(s5b): write_provider persists [providers.*] block (no plaintext key)"
```

---

### Task 3: model 模块——Prober trait + 可用性判定 + check 退出码

**Files:**
- Create: `src/model/mod.rs`
- Modify: `src/lib.rs`（加 `pub mod model;`）
- Test: `src/model/mod.rs`（内联，FakeProber）

**Interfaces:**
- Produces:
  - `pub const ANTHROPIC_DEFAULT_BASE` / `OPENAI_DEFAULT_BASE`；`pub fn effective_base(pc: &ProviderConfig) -> String`
  - `#[async_trait] pub trait Prober: Send+Sync { async fn reachable(&self, base) -> bool; async fn list_models(&self, base, key: Option<&str>) -> anyhow::Result<Vec<String>>; }`
  - `pub struct Availability { provider, key_present, reachable }`（+ `available()` + `status_str()`）
  - `pub async fn check_all(cfg: &Config, prober: &dyn Prober) -> Vec<Availability>`
  - `pub fn check_exit_code(avs: &[Availability]) -> i32`（非空且全可用→0，否则→1）
  - `pub struct FakeProber { reachable: bool, models: Vec<String> }`（test 替身，置 pub 供后续 task 测试复用）

- [ ] **Step 1: lib.rs 注册 + 建 mod**

`src/lib.rs` 加 `pub mod model;`（字母序，`pub mod lens;` 附近）。

- [ ] **Step 2: 写失败测试** 创建 `src/model/mod.rs`，先放 use + 测试：

```rust
use crate::config::{Config, ProviderConfig, ProviderKind};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn cfg_with(providers: &str) -> Config {
        Config::from_layers(&[providers]).unwrap()
    }

    #[tokio::test]
    async fn check_all_truth_table_and_exit_code() {
        let _g = ENV_LOCK.lock().unwrap();
        // alpha 有 env key、beta 无 key
        unsafe { std::env::set_var("ALPHA_KEY", "k"); std::env::remove_var("BETA_KEY"); }
        let cfg = cfg_with(
            "[providers.alpha]\nkind=\"openai\"\nmodel=\"a\"\napi_key_env=\"ALPHA_KEY\"\npriority=10\n\
             [providers.beta]\nkind=\"openai\"\nmodel=\"b\"\napi_key_env=\"BETA_KEY\"\npriority=5\n",
        );
        // 端点可达
        let reach = FakeProber { reachable: true, models: vec![] };
        let avs = check_all(&cfg, &reach).await;
        let alpha = avs.iter().find(|a| a.provider == "alpha").unwrap();
        let beta = avs.iter().find(|a| a.provider == "beta").unwrap();
        assert!(alpha.available(), "alpha: key+reachable");
        assert!(!beta.available() && !beta.key_present, "beta: missing key");
        assert_eq!(check_exit_code(&avs), 1, "有不可用→非 0");

        // 端点不可达 → 即便有 key 也不可用
        let unreach = FakeProber { reachable: false, models: vec![] };
        let avs2 = check_all(&cfg, &unreach).await;
        assert!(avs2.iter().all(|a| !a.available()));
        assert_eq!(check_exit_code(&avs2), 1);
        unsafe { std::env::remove_var("ALPHA_KEY"); }
    }

    #[test]
    fn exit_code_empty_is_nonzero() {
        assert_eq!(check_exit_code(&[]), 1, "无 provider→非 0（预检无可用 model）");
    }

    #[test]
    fn effective_base_defaults_by_kind() {
        let pc: ProviderConfig = toml::from_str("kind=\"anthropic\"\nmodel=\"m\"").unwrap();
        assert_eq!(effective_base(&pc), ANTHROPIC_DEFAULT_BASE);
        let pc2: ProviderConfig = toml::from_str("kind=\"openai\"\nmodel=\"m\"").unwrap();
        assert_eq!(effective_base(&pc2), OPENAI_DEFAULT_BASE);
        let pc3: ProviderConfig = toml::from_str("kind=\"openai-compatible\"\nmodel=\"m\"\nbase_url=\"http://x/v1\"").unwrap();
        assert_eq!(effective_base(&pc3), "http://x/v1");
    }
}
```

Run: `cargo test --lib model::tests::effective_base_defaults_by_kind` → FAIL

- [ ] **Step 3: 实现**（`src/model/mod.rs`，`use` 之后、`#[cfg(test)]` 之前）

```rust
pub const ANTHROPIC_DEFAULT_BASE: &str = "https://api.anthropic.com";
pub const OPENAI_DEFAULT_BASE: &str = "https://api.openai.com/v1";

/// provider 有效 base URL：显式 base_url 优先，否则按 kind 取默认。
pub fn effective_base(pc: &ProviderConfig) -> String {
    if let Some(b) = &pc.base_url {
        return b.clone();
    }
    match pc.kind {
        ProviderKind::Anthropic => ANTHROPIC_DEFAULT_BASE.to_string(),
        _ => OPENAI_DEFAULT_BASE.to_string(),
    }
}

/// 端点探测（可注入）：reachable=端点可达；list_models=查 /v1/models。
#[async_trait::async_trait]
pub trait Prober: Send + Sync {
    async fn reachable(&self, base_url: &str) -> bool;
    async fn list_models(&self, base_url: &str, key: Option<&str>) -> anyhow::Result<Vec<String>>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct Availability {
    pub provider: String,
    pub key_present: bool,
    pub reachable: bool,
}
impl Availability {
    pub fn available(&self) -> bool {
        self.key_present && self.reachable
    }
    pub fn status_str(&self) -> &'static str {
        if self.available() {
            "✓ available"
        } else if !self.key_present {
            "✗ missing key"
        } else {
            "✗ unreachable"
        }
    }
}

/// 对所有已配置 provider 做可用性判定（key 就位 × 端点可达）。
pub async fn check_all(cfg: &Config, prober: &dyn Prober) -> Vec<Availability> {
    let mut out = Vec::new();
    for (name, pc) in &cfg.providers {
        let key_present = cfg.resolve_key(name).is_some();
        let reachable = prober.reachable(&effective_base(pc)).await;
        out.push(Availability { provider: name.clone(), key_present, reachable });
    }
    out
}

/// 退出码：非空且全部可用→0；否则→1（供脚本/循环 SKILL 评审前预检）。
pub fn check_exit_code(avs: &[Availability]) -> i32 {
    if !avs.is_empty() && avs.iter().all(|a| a.available()) {
        0
    } else {
        1
    }
}

/// 测试探测替身（pub 供后续 task 复用）。
pub struct FakeProber {
    pub reachable: bool,
    pub models: Vec<String>,
}
#[async_trait::async_trait]
impl Prober for FakeProber {
    async fn reachable(&self, _base_url: &str) -> bool {
        self.reachable
    }
    async fn list_models(&self, _base_url: &str, _key: Option<&str>) -> anyhow::Result<Vec<String>> {
        Ok(self.models.clone())
    }
}
```

- [ ] **Step 4: 跑测试** Run: `cargo test --lib model` → PASS
- [ ] **Step 5: Commit**
```bash
git add src/lib.rs src/model/mod.rs
git commit -m "feat(s5b): model Prober trait + availability check + exit code (FakeProber-tested)"
```

---

### Task 4: HttpProber（真探测，薄缝）+ `brk model check` 命令

**Files:**
- Create: `src/model/probe.rs`（`HttpProber`）
- Modify: `src/model/mod.rs`（`pub mod probe;`）
- Modify: `src/main.rs`（`Cmd::Model` + `ModelCmd::Check` handler）
- Test: 无单测（HttpProber 真 HTTP 为薄缝；check_all/exit_code 已 T3 测）

**Interfaces:**
- Produces: `pub struct HttpProber`（`Default`/`new`）impl `Prober`（reqwest GET，5s 超时，any 响应=reachable；GET `{base}/models` 解析 `data[].id`）；`brk model check` → `check_all`(HttpProber) → 打印每条 status + `std::process::exit(check_exit_code)`。

- [ ] **Step 1: HttpProber 实现**（创建 `src/model/probe.rs`）

```rust
use crate::model::Prober;
use std::time::Duration;

/// 真 HTTP 探测（reqwest，rustls）。薄缝——不单测（无真实端点）。
pub struct HttpProber {
    client: reqwest::Client,
}
impl Default for HttpProber {
    fn default() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        HttpProber { client }
    }
}
impl HttpProber {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl Prober for HttpProber {
    async fn reachable(&self, base_url: &str) -> bool {
        // 任意 HTTP 响应（含 4xx）即视为端点可达；connect/timeout/DNS 失败=不可达。
        self.client.get(base_url).send().await.is_ok()
    }
    async fn list_models(&self, base_url: &str, key: Option<&str>) -> anyhow::Result<Vec<String>> {
        let url = format!("{}/models", base_url.trim_end_matches('/'));
        let mut req = self.client.get(&url);
        if let Some(k) = key {
            req = req.bearer_auth(k);
        }
        let resp = req.send().await?.error_for_status()?;
        let v: serde_json::Value = resp.json().await?;
        let ids = v
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Ok(ids)
    }
}
```

`src/model/mod.rs` 顶部加 `pub mod probe;`。

- [ ] **Step 2: main.rs — `Cmd::Model` + `ModelCmd` + Check handler**

`enum Cmd` 加：
```rust
    /// 配置/查询 provider 与 model。
    Model {
        #[command(subcommand)]
        action: ModelCmd,
    },
```
新增 `ModelCmd`（放在 `Cmd` 定义之后）：
```rust
#[derive(Subcommand)]
enum ModelCmd {
    /// 配置一个 provider/model（flag 驱动）。
    Setup {
        #[arg(long)]
        provider: String,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        priority: Option<i32>,
        /// key 去处：env(默认，打印 export) | secrets(写 secrets.toml chmod 600)
        #[arg(long, default_value = "env")]
        key_store: String,
    },
    /// 列已配置 provider + 可用性 + 配置位置。
    List {
        #[arg(long)]
        remote: Option<String>,
    },
    /// 轻量可用性预检（退出码全可用 0 / 有不可用非 0）。
    Check,
}
```
在 `match cli.cmd` 加（Check 部分；Setup/List 在 Task 5/6 填，此 task 可先放 `todo!()` 占位**否则编译不过**——实际本 task 直接连同空壳一起加，Setup/List 分支先 `eprintln!("not yet")+2` 占位，Task 5/6 替换）：
```rust
        Cmd::Model { action } => match action {
            ModelCmd::Check => {
                let cfg = Config::load().unwrap_or_default();
                let prober = breakpoints::model::probe::HttpProber::new();
                let avs = breakpoints::model::check_all(&cfg, &prober).await;
                for a in &avs {
                    println!("{}  {}", a.provider, a.status_str());
                }
                breakpoints::model::check_exit_code(&avs)
            }
            ModelCmd::Setup { .. } => { eprintln!("model setup: implemented in next task"); 2 }
            ModelCmd::List { .. } => { eprintln!("model list: implemented in next task"); 2 }
        },
```
注：`ModelCmd::Setup { .. }` / `List { .. }` 用 `..` 忽略字段占位。Task 5/6 替换为真实 handler。

- [ ] **Step 3: 验收** Run: `cargo build && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: 编译通过、clippy 干净、既有测试全绿。`cargo tree -i openssl-sys` 空。
（HttpProber 无单测属薄缝设计；check 逻辑 T3 已测。）

- [ ] **Step 4: Commit**
```bash
git add src/model/mod.rs src/model/probe.rs src/main.rs
git commit -m "feat(s5b): HttpProber + brk model check (exit 0 all-available / nonzero otherwise)"
```

---

### Task 5: `brk model setup` 命令（flag 驱动，env 默认）

**Files:**
- Modify: `src/model/mod.rs`（`derive_kind` / `derive_api_key_env` 纯函数）
- Modify: `src/main.rs`（`ModelCmd::Setup` handler 替换占位）
- Test: `src/model/mod.rs`（内联，纯函数）

**Interfaces:**
- Produces:
  - `pub fn derive_kind(provider: &str, explicit_kind: Option<&str>, base_url: Option<&str>) -> String`（显式优先；有 base_url→openai-compatible；否则按名推断 claude→anthropic / gpt→openai / 其他→openai-compatible）
  - `pub fn derive_api_key_env(provider: &str, kind: &str) -> String`（anthropic→ANTHROPIC_API_KEY / openai→OPENAI_API_KEY / 否则 `<PROVIDER_UPPER>_API_KEY`）
- Setup handler：写 provider 配置（`write_provider`）；key 默认 env（打印 `export <VAR>=...` + 主配置记 api_key_env）；`--key-store secrets` 且有 `--key`→`write_secret`；完成后自动跑 check。

- [ ] **Step 1: 写失败测试**（`src/model/mod.rs` 的 `mod tests`）

```rust
    #[test]
    fn derive_kind_rules() {
        assert_eq!(derive_kind("anything", Some("anthropic"), None), "anthropic"); // 显式优先
        assert_eq!(derive_kind("x", None, Some("http://h/v1")), "openai-compatible"); // base_url
        assert_eq!(derive_kind("claude", None, None), "anthropic"); // 名推断
        assert_eq!(derive_kind("gpt", None, None), "openai");
        assert_eq!(derive_kind("mystery", None, None), "openai-compatible"); // 兜底
    }

    #[test]
    fn derive_api_key_env_rules() {
        assert_eq!(derive_api_key_env("claude", "anthropic"), "ANTHROPIC_API_KEY");
        assert_eq!(derive_api_key_env("gpt", "openai"), "OPENAI_API_KEY");
        assert_eq!(derive_api_key_env("local", "openai-compatible"), "LOCAL_API_KEY");
    }
```

Run: `cargo test --lib model::tests::derive_kind_rules` → FAIL

- [ ] **Step 2: 实现纯函数**（`src/model/mod.rs`，加在 `check_exit_code` 之后）

```rust
/// 推断 provider kind：显式 > 有 base_url(openai-compatible) > 按名推断 > 兜底 openai-compatible。
pub fn derive_kind(provider: &str, explicit_kind: Option<&str>, base_url: Option<&str>) -> String {
    if let Some(k) = explicit_kind {
        return k.to_string();
    }
    if base_url.is_some() {
        return "openai-compatible".to_string();
    }
    let p = provider.to_lowercase();
    if p.contains("claude") || p.contains("anthropic") {
        "anthropic".to_string()
    } else if p.contains("gpt") || p.contains("openai") {
        "openai".to_string()
    } else {
        "openai-compatible".to_string()
    }
}

/// 推断默认 api_key_env：anthropic→ANTHROPIC_API_KEY / openai→OPENAI_API_KEY / 否则 <PROVIDER_UPPER>_API_KEY。
pub fn derive_api_key_env(provider: &str, kind: &str) -> String {
    match kind {
        "anthropic" => "ANTHROPIC_API_KEY".to_string(),
        "openai" => "OPENAI_API_KEY".to_string(),
        _ => format!("{}_API_KEY", provider.to_uppercase().replace('-', "_")),
    }
}
```

- [ ] **Step 3: Setup handler 替换占位**（`src/main.rs`）

```rust
            ModelCmd::Setup { provider, kind, base_url, model, key, priority, key_store } => {
                let k = breakpoints::model::derive_kind(&provider, kind.as_deref(), base_url.as_deref());
                let model = match model {
                    Some(m) => m,
                    None => { eprintln!("error: --model is required"); std::process::exit(2); }
                };
                let api_key_env = breakpoints::model::derive_api_key_env(&provider, &k);
                let prio = priority.unwrap_or(0);
                if let Err(e) = breakpoints::config::write_provider(
                    &provider, &k, base_url.as_deref(), &model, prio, Some(&api_key_env),
                ) {
                    eprintln!("error: failed to write provider config: {e}");
                    std::process::exit(2);
                }
                // key 去处：env(默认，打印 export) / secrets(写 secrets.toml)
                match key_store.as_str() {
                    "secrets" => match key {
                        Some(k) => {
                            if let Err(e) = breakpoints::config::secrets::write_secret(&provider, &k) {
                                eprintln!("error: failed to write secret: {e}");
                                std::process::exit(2);
                            }
                            println!("wrote key to {} (chmod 600)", breakpoints::config::secrets::secrets_path().display());
                        }
                        None => { eprintln!("error: --key-store secrets requires --key"); std::process::exit(2); }
                    },
                    _ => {
                        // env 默认：主配置只记 api_key_env，提示用户设 env。
                        match key {
                            Some(k) => println!("add to your shell profile:\n  export {api_key_env}={k}"),
                            None => println!("set the API key in your environment:\n  export {api_key_env}=<your-key>"),
                        }
                    }
                }
                println!("configured provider '{provider}' (kind={k}, model={model}, api_key_env={api_key_env})");
                // 完成后自动跑一次 check
                let cfg = Config::load().unwrap_or_default();
                let prober = breakpoints::model::probe::HttpProber::new();
                let avs = breakpoints::model::check_all(&cfg, &prober).await;
                for a in &avs {
                    println!("{}  {}", a.provider, a.status_str());
                }
                0
            }
```

- [ ] **Step 4: 验收** Run: `cargo test --lib model && cargo build && cargo clippy --all-targets -- -D warnings`
Expected: 纯函数测试绿、编译通过、clippy 干净。

- [ ] **Step 5: Commit**
```bash
git add src/model/mod.rs src/main.rs
git commit -m "feat(s5b): brk model setup (flag-driven, env-default key; secrets opt-in)"
```

---

### Task 6: `brk model list` 命令（含 `--remote`）

**Files:**
- Modify: `src/model/mod.rs`（`render_model_list` 纯函数）
- Modify: `src/main.rs`（`ModelCmd::List` handler 替换占位）
- Test: `src/model/mod.rs`（内联）

**Interfaces:**
- Produces:
  - `pub fn render_model_list(cfg: &Config, avs: &[Availability], config_paths: &[String]) -> String`——每行一条 provider（name/kind/model + status），尾附配置文件位置。纯函数可测。
- List handler：无 `--remote`→`check_all`(HttpProber) + `render_model_list` + 打印配置路径；有 `--remote <provider>`→对该 provider 的 base 查 `list_models` 打印实际模型。

- [ ] **Step 1: 写失败测试**（`src/model/mod.rs` 的 `mod tests`）

```rust
    #[test]
    fn render_model_list_one_line_per_provider_with_status() {
        let cfg = cfg_with(
            "[providers.alpha]\nkind=\"openai\"\nmodel=\"a\"\npriority=10\n\
             [providers.beta]\nkind=\"anthropic\"\nmodel=\"b\"\npriority=5\n",
        );
        let avs = vec![
            Availability { provider: "alpha".into(), key_present: true, reachable: true },
            Availability { provider: "beta".into(), key_present: false, reachable: true },
        ];
        let out = render_model_list(&cfg, &avs, &["/u/config.toml".into()]);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.iter().any(|l| l.contains("alpha") && l.contains("a") && l.contains("available")));
        assert!(lines.iter().any(|l| l.contains("beta") && l.contains("missing key")));
        assert!(out.contains("/u/config.toml"), "config 位置打印");
    }
```

Run: `cargo test --lib model::tests::render_model_list_one_line_per_provider_with_status` → FAIL

- [ ] **Step 2: 实现 `render_model_list`**（`src/model/mod.rs`）

```rust
/// 渲染已配置 provider 列表（每行 name/kind/model/status）+ 配置文件位置。
pub fn render_model_list(cfg: &Config, avs: &[Availability], config_paths: &[String]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (name, pc) in &cfg.providers {
        let status = avs
            .iter()
            .find(|a| &a.provider == name)
            .map(|a| a.status_str())
            .unwrap_or("?");
        let kind = format!("{:?}", pc.kind).to_lowercase();
        lines.push(format!("{name}  [{kind}]  {}  {status}", pc.model));
    }
    lines.sort();
    let mut out = lines.join("\n");
    out.push_str("\n\nconfig files (low→high priority):");
    for p in config_paths {
        out.push_str(&format!("\n  {p}"));
    }
    out
}
```

- [ ] **Step 3: List handler 替换占位**（`src/main.rs`）

```rust
            ModelCmd::List { remote } => {
                let cfg = Config::load().unwrap_or_default();
                let prober = breakpoints::model::probe::HttpProber::new();
                match remote {
                    Some(prov) => {
                        let pc = match cfg.providers.get(&prov) {
                            Some(p) => p,
                            None => { eprintln!("no such provider: {prov}"); std::process::exit(2); }
                        };
                        let base = breakpoints::model::effective_base(pc);
                        let key = cfg.resolve_key(&prov);
                        match prober.list_models(&base, key.as_deref()).await {
                            Ok(models) => {
                                for m in models { println!("{m}"); }
                                0
                            }
                            Err(e) => { eprintln!("error querying {base}/models: {e}"); 2 }
                        }
                    }
                    None => {
                        let avs = breakpoints::model::check_all(&cfg, &prober).await;
                        let paths: Vec<String> = breakpoints::config::config_paths()
                            .iter().map(|p| p.display().to_string()).collect();
                        println!("{}", breakpoints::model::render_model_list(&cfg, &avs, &paths));
                        0
                    }
                }
            }
```

- [ ] **Step 4: 验收** Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo tree -i openssl-sys`
Expected: 全绿、clippy 干净、openssl-sys 空。

- [ ] **Step 5: Commit**
```bash
git add src/model/mod.rs src/main.rs
git commit -m "feat(s5b): brk model list (configured + availability + config paths; --remote /v1/models)"
```

---

## Self-Review

- **Spec 覆盖**（设计 §13 + S5-b 决策）：secrets 写 chmod 600=T1 ✓；主配置 provider 写（不含 key）=T2 ✓；Prober 可注入 + 可用性真值表 + check 退出码=T3 ✓；HttpProber 真探测 + `model check`=T4 ✓；`model setup` flag 驱动 env 默认 + 自动 check=T5 ✓；`model list` + `--remote`=T6 ✓。key 默认 env（打印 export）+ secrets 可选(chmod 600)=T5 ✓。
- **类型一致性**：`Prober`/`Availability`/`check_all`/`check_exit_code`/`effective_base`（T3）在 T4/T5/T6 调用一致；`write_provider`（T2）/`write_secret`（T1）/`derive_kind`/`derive_api_key_env`（T5）签名与 Setup handler 调用一致；`FakeProber`（T3）pub 供 T3 测试，`HttpProber`（T4）impl 同 trait。
- **占位符扫描**：T4 的 Setup/List 占位是**刻意分阶段**（Task 5/6 替换），已注明；其余步骤完整代码。
- **跨 task 编译依赖**：T3 注册 `pub mod model`；T4 加 `pub mod probe` + main.rs `Cmd::Model`（Setup/List 占位保编译，T5/T6 替换）；T1/T2 是独立的 config 写入，T5 依赖之。
- **冻结约束**：无 task 触碰 src/tools/、tool_loop.rs、backends.rs；默认 base 常量在 model 模块重定义（非 import 冻结私有 const）。
- **密钥卫生**：主配置写入（T2）只含 api_key_env 引用、测试断言无 `sk-` 明文；key 默认 env（T5 打印 export 不落盘）；secrets 模式 chmod 600（T1）。
- **测试无 key/端点**：check/list 逻辑（T3/T6）用 FakeProber + tempdir（XDG 覆盖）；HttpProber（T4）真 HTTP 为薄缝；env 测试 ENV_LOCK 串行化。
