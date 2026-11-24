# M15-S2-b（SCIP substrate + 契约三桶）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 第一个把 SCIP 语义索引接进 `redteam` 二进制——`REDTEAM_PACK=scip` 引擎用 SCIP references 替 regex 引用、廉价契约分类器(A/B)驱动方向性 radius(caller 厚度 + callee 签名注入)，单语言 Rust、单树、无缓存。

**Architecture:** review 前置对工作树跑一次 `rust-analyzer scip` → tempfile → `scip` crate 解析成内存查询表(`ScipIndex`) → 即删。pack 引擎按位置对把改动符号连到 SCIP symbol，用 references 填 caller 侧(A 桶给完整外围体/B 桶给片段)、用 def 区间内 callee 引用注入被调签名；SCIP 查无该符号则单符号退回 regex-A `reference_points`。分类与 SCIP 解耦——分桶纯从新树 diff 捕获路径导出。

**Tech Stack:** Rust 2021 · `scip` 0.9 + `protobuf` 3(解析) · `rust-analyzer scip`(内建发射器，运行时借) · 复用现有 `src/dossier`(符号/引用/块扫描引擎) + `src/pack`(三段预算填充)。

## Global Constraints

- 全程简体中文：代码注释、commit message、文档。
- **权威 spec** = `.doc/design/2026-07-09-redteam-context-management-s2b-design.md`；差异账 = `.doc/roadmap/mvp-delta-ledger.md`(S2-b 段)。**只做 MVP 边界(design §0)内的件**，缓项(双树/缓存/剪枝/传递深度+多样性排序/穷尽性/build-gate/per-model 分叉/C 装饰桶)**不做**。
- **`REDTEAM_PACK` 默认不翻**：默认仍是当前 regex-pack；`scip`/`scip-strict` 是 opt-in，验收过前不改默认。regex/dossier/null 三臂**行为零变化**，其现有测试须保持绿。
- **rust-analyzer 缺 = 降级非报错**：`scip` 引擎发现 RA 不在或索引失败 → 回退 regex-pack + 一行 stderr 提示，绝不 panic、绝不阻塞 review。
- **测试不得依赖 rust-analyzer**：查询/分类/pack 逻辑测试用**手搓 `scip::types::Index`**；真跑 RA 的 runner 测试标 `#[ignore]`(手动/有 RA 时跑)。
- **索引每 review 建一次**(非 per-bundle)：`build_packs` 前置建 `ScipIndex`、传引用给各 bundle。
- CI 门：`cargo test` 全绿 + `cargo clippy` 净新增警告 0 + `make verify` 绿(commands↔codex 镜像不漂移；本计划不改 `plugins/*/commands/`，verify 应自然绿)。
- 每 Task 独立 commit，message 简体中文，结尾 `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`。**不 push**(用户本地开发阶段，push 后续自解)。
- 行号约定：SCIP occurrence `range[0]` = 0-based 起始行；`enclosing_def_start` 返回 0-based def 行——两者对齐，全模块统一 0-based。

---

## 文件结构

| 文件 | 责任 | Task |
|---|---|---|
| `Cargo.toml` | 加 `scip`/`protobuf` 依赖、`tempfile` 升为正式依赖 | 1 |
| `src/lib.rs` | 注册 `pub mod scip;` | 1 |
| `src/scip/mod.rs`（新） | SCIP 索引：解析 + 查询 API（`symbol_at`/`callers_of`/`callees_in`/`signature_of`）+ runner（`build`） | 1、6 |
| `src/dossier/mod.rs` | `ChangedSym` 加 `bucket`、`symbols_in_file` 两条捕获路径打 A/B 标（additive） | 2 |
| `src/pack/mod.rs` | `build_pack_scip`：SCIP caller 侧（A 体/B 片段）+ callee 签名段 + 符号级 regex 兜底 | 3、4、5 |
| `src/cli.rs` | `REDTEAM_PACK` 枚举化 + 引擎 dispatch + RA 缺降级 + 索引前置建一次 | 7 |

---

### Task 1: SCIP 索引解析 + 查询 API

**Files:**
- Modify: `Cargo.toml`（`[dependencies]` 加 `scip`/`protobuf`；`tempfile` 从 dev 移入正式依赖）
- Modify: `src/lib.rs`（加 `pub mod scip;`）
- Create: `src/scip/mod.rs`
- Test: `src/scip/mod.rs` `#[cfg(test)]`

**Interfaces:**
- Produces:
  - `pub struct ScipIndex`（内存查询表，行号 0-based）
  - `pub fn ScipIndex::from_index(idx: scip::types::Index) -> ScipIndex`
  - `pub fn ScipIndex::symbol_at(&self, path: &str, def_line0: usize) -> Option<&str>`
  - `pub fn ScipIndex::callers_of(&self, symbol: &str) -> Vec<(String, usize)>`（已排序去 Definition）
  - `pub fn ScipIndex::callees_in(&self, path: &str, start0: usize, end0: usize, own: &str) -> Vec<String>`（区间内 distinct 引用 target，去自身/去 Definition，已排序）
  - `pub fn ScipIndex::signature_of(&self, symbol: &str) -> Option<&str>`

- [ ] **Step 1: 加依赖**

`Cargo.toml`：`[dependencies]` 末尾加（探针已验 `scip` 0.9 免 protoc、`protobuf` 3 是其解析所需直接依赖）：
```toml
# SCIP 语义索引解析（M15-S2-b）。scip 0.9 bundled protobuf 绑定、免 protoc；protobuf 是 Index::parse_from_bytes 所需直接依赖。
scip = "0.9"
protobuf = "3"
# 运行期 SCIP 索引临时文件（即弃、零仓库足迹）——从 dev-dependencies 升为正式依赖。
tempfile = "3"
```
并从 `[dev-dependencies]` 删除 `tempfile = "3"`（避免重复；正式依赖对 dev 亦可见）。

- [ ] **Step 2: 注册模块**

`src/lib.rs` 在既有 `pub mod pack;` 一行旁加：
```rust
pub mod scip;
```

- [ ] **Step 3: 写失败测试**

`src/scip/mod.rs` 末尾：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ::scip::types::{Document, Index, Occurrence, SymbolInformation};

    // 构造一个 occurrence：range=[line0,0,0]、给定 symbol/roles
    fn occ(line0: i32, symbol: &str, roles: i32) -> Occurrence {
        Occurrence { range: vec![line0, 0, 0], symbol: symbol.into(), symbol_roles: roles, ..Default::default() }
    }
    // 构造带签名的 SymbolInformation
    fn sym_info(symbol: &str, sig: &str) -> SymbolInformation {
        let mut si = SymbolInformation { symbol: symbol.into(), ..Default::default() };
        si.signature_documentation = protobuf::MessageField::some(Document { text: sig.into(), ..Default::default() });
        si
    }

    fn mini_index() -> Index {
        // a.rs: 第2行(0-based=1) def `foo()`；bar 在第0行调 foo（reference）
        // b.rs: 第5行调 foo（reference）
        let a = Document {
            relative_path: "src/a.rs".into(),
            occurrences: vec![
                occ(0, "S bar().", ROLE_DEFINITION),
                occ(0, "S foo().", 0),            // bar 里调 foo = 引用
                occ(1, "S foo().", ROLE_DEFINITION),
            ],
            symbols: vec![sym_info("S foo().", "pub fn foo(x: i32) -> i32"), sym_info("S bar().", "fn bar()")],
            ..Default::default()
        };
        let b = Document {
            relative_path: "src/b.rs".into(),
            occurrences: vec![occ(5, "S foo().", 0)],
            ..Default::default()
        };
        Index { documents: vec![a, b], ..Default::default() }
    }

    #[test]
    fn queries_join_def_refs_callees_and_signatures() {
        let idx = ScipIndex::from_index(mini_index());
        // 位置对：a.rs 第1行是 foo 的 def
        assert_eq!(idx.symbol_at("src/a.rs", 1), Some("S foo()."));
        assert_eq!(idx.symbol_at("src/a.rs", 99), None);
        // caller：foo 被 a.rs:0 与 b.rs:5 引用（去 Definition、排序）
        assert_eq!(idx.callers_of("S foo()."), vec![("src/a.rs".to_string(), 0), ("src/b.rs".to_string(), 5)]);
        // callee：bar 的 def 区间[0,0]内引用了 foo（去自身 bar、去 Definition）
        assert_eq!(idx.callees_in("src/a.rs", 0, 0, "S bar()."), vec!["S foo().".to_string()]);
        // 签名
        assert_eq!(idx.signature_of("S foo()."), Some("pub fn foo(x: i32) -> i32"));
    }
}
```

- [ ] **Step 4: 跑测试确认失败**

Run: `cargo test -p redteam scip::tests 2>&1 | tail -5`
Expected: 编译失败（`ScipIndex`/`ROLE_DEFINITION` 未定义）。

- [ ] **Step 5: 实现**

`src/scip/mod.rs` 顶部：
```rust
//! SCIP 语义索引（M15-S2-b）：跑 rust-analyzer scip → 解析成内存查询表 → 即弃。
//! 行号全 0-based，与 dossier::enclosing_def_start 对齐。
use std::collections::{HashMap, HashSet};
use scip::types::Index;

/// SCIP symbol_roles 位掩码：Definition=1（其余 Import=2/WriteAccess=4/ReadAccess=8…本 MVP 只分 def/非 def）。
const ROLE_DEFINITION: i32 = 1;

/// 解析后的 SCIP 索引 + 查询表。构造一次、只读查询。
pub struct ScipIndex {
    def_at: HashMap<(String, usize), String>,          // (path, def_line0) -> symbol（位置对连接键）
    refs_of: HashMap<String, Vec<(String, usize)>>,    // symbol -> 引用点(path,line0)，去 Definition、已排序
    by_file: HashMap<String, Vec<(usize, String, bool)>>, // path -> [(line0, symbol, is_def)]，供 callee 区间查
    sig_of: HashMap<String, String>,                   // symbol -> signature_documentation.text
}

impl ScipIndex {
    pub fn from_index(idx: Index) -> Self {
        let mut def_at = HashMap::new();
        let mut refs_of: HashMap<String, Vec<(String, usize)>> = HashMap::new();
        let mut by_file: HashMap<String, Vec<(usize, String, bool)>> = HashMap::new();
        let mut sig_of = HashMap::new();
        for doc in &idx.documents {
            let path = doc.relative_path.clone();
            for si in &doc.symbols {
                if let Some(d) = si.signature_documentation.as_ref() {
                    if !d.text.is_empty() {
                        sig_of.insert(si.symbol.clone(), d.text.clone());
                    }
                }
            }
            for occ in &doc.occurrences {
                let line0 = match occ.range.first() {
                    Some(&l) if l >= 0 => l as usize,
                    _ => continue,
                };
                let is_def = occ.symbol_roles & ROLE_DEFINITION != 0;
                by_file.entry(path.clone()).or_default().push((line0, occ.symbol.clone(), is_def));
                if is_def {
                    def_at.insert((path.clone(), line0), occ.symbol.clone());
                } else {
                    refs_of.entry(occ.symbol.clone()).or_default().push((path.clone(), line0));
                }
            }
        }
        for v in refs_of.values_mut() {
            v.sort();
            v.dedup();
        }
        Self { def_at, refs_of, by_file, sig_of }
    }

    /// 位置对：某文件某 0-based 行的 Definition symbol（改动符号 ↔ SCIP 连接键）。
    pub fn symbol_at(&self, path: &str, def_line0: usize) -> Option<&str> {
        self.def_at.get(&(path.to_string(), def_line0)).map(String::as_str)
    }

    /// caller 侧：symbol 的引用点（去 Definition），已排序去重。查无 → 空。
    pub fn callers_of(&self, symbol: &str) -> Vec<(String, usize)> {
        self.refs_of.get(symbol).cloned().unwrap_or_default()
    }

    /// callee 侧：def 区间[start0,end0]内引用到的 distinct symbol（去自身 own、去 Definition），已排序。
    pub fn callees_in(&self, path: &str, start0: usize, end0: usize, own: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        if let Some(occs) = self.by_file.get(path) {
            for (line0, sym, is_def) in occs {
                if *is_def || *line0 < start0 || *line0 > end0 || sym == own {
                    continue;
                }
                if seen.insert(sym.clone()) {
                    out.push(sym.clone());
                }
            }
        }
        out.sort();
        out
    }

    pub fn signature_of(&self, symbol: &str) -> Option<&str> {
        self.sig_of.get(symbol).map(String::as_str)
    }
}
```

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p redteam scip::tests 2>&1 | tail -5`
Expected: `test result: ok. 1 passed`。

- [ ] **Step 7: clippy + commit**

Run: `cargo clippy --all-targets 2>&1 | grep -c "warning:" ` （确认无净新增）
```bash
git add Cargo.toml Cargo.lock src/lib.rs src/scip/mod.rs
git commit -m "feat(scip): SCIP 索引解析 + 查询 API（位置对/callers/callees/签名）

scip 0.9 + protobuf 3 依赖；ScipIndex::from_index 建四张查询表
（def_at 位置对连接键 / refs_of caller / by_file callee 区间 / sig_of 签名）。
tempfile 升正式依赖备 runner 用（Task 6）。手搓 Index 单测、不依赖 rust-analyzer。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: 契约分类器（A/B 桶）

**Files:**
- Modify: `src/dossier/mod.rs`（`ChangedSym` 加 `bucket`；`symbols_in_file` 两条捕获路径打标）
- Test: `src/dossier/mod.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: 无（纯新树 diff/regex，Task 1 无关）。
- Produces:
  - `pub enum Bucket { A, B }`（A=签名面变、B=仅体变；C 装饰桶缓，见 ledger）
  - `ChangedSym` 新增 `pub bucket: Bucket` + `pub def_line0: usize`（0-based 新树 def 行——**位置对连接键**，下游 `symbol_at` 直接用，杜绝同名重扫歧义）
  - 语义：`+` 行本身匹配 def 正则(签名行进 diff) → `A`（def_line0=该 `+` 行）；kind ∈ {struct/enum/trait/type/const/static/macro} 且经该路径 → `A`；仅经外围 def 路径捕获 → `B`（def_line0=`enclosing_def_start` 结果）。

> 说明：现 `symbols_in_file` 两条 push 路径已天然对应 A/B——`match_def(&raw[1..], defs)` 命中(签名行) = A 候选；`innermost_enclosing_def`(hunk 外围) = B。本 Task 只把这信号记进 `bucket`。regex/dossier 引擎不读 `bucket`，故此改 **additive、对现有臂零行为变化**。

- [ ] **Step 1: 写失败测试**

`src/dossier/mod.rs` tests 模块加：
```rust
#[test]
fn symbols_in_file_tags_bucket_and_def_line() {
    let lines: Vec<&str> = "pub fn foo(x: i32) {\n    helper();\n}\n".lines().collect();
    let defs = compile_defs(&RUST);
    // 改签名行（+ 行匹配 fn 正则）→ A，def_line0=0
    let a = symbols_in_file("@@ -1,0 +1,1 @@\n+pub fn foo(x: i32) {\n", &lines, "src/a.rs", &defs);
    let fa = a.iter().find(|s| s.name == "foo").unwrap();
    assert_eq!(fa.bucket, Bucket::A, "签名行进 diff = A");
    assert_eq!(fa.def_line0, 0, "A 桶 def_line0=签名行");
    // 只改体（+ 行是 helper 调用、外围 def=foo 在第0行）→ B，def_line0=0
    let b = symbols_in_file("@@ -2,0 +2,1 @@\n+    helper();\n", &lines, "src/a.rs", &defs);
    let fb = b.iter().find(|s| s.name == "foo").unwrap();
    assert_eq!(fb.bucket, Bucket::B, "仅体变 = B");
    assert_eq!(fb.def_line0, 0, "B 桶 def_line0=外围 def 行");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p redteam symbols_in_file_tags_bucket 2>&1 | tail -5`
Expected: 编译失败（`Bucket` 未定义、`ChangedSym` 无 `bucket`）。

- [ ] **Step 3: 实现**

`src/dossier/mod.rs`：`ChangedSym` 定义处（约 L124）改为：
```rust
/// 契约桶（M15-S2-b）：A=签名面变（传递展/caller 给体）、B=仅体变（caller 给片段）。
/// C 装饰桶（注释/空白 skip）缓，见 mvp-delta-ledger #12。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Bucket {
    A,
    B,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChangedSym {
    pub name: String,
    pub kind: &'static str,
    pub file: String,
    pub bucket: Bucket,
    pub def_line0: usize, // 0-based 新树 def 行 = 位置对连接键（symbol_at 直接用）
}
```
`symbols_in_file` 的 `push` 闭包加 `bucket` + `def_line0` 参数（约 L142）：
```rust
    let push = |name: String, kind: &'static str, bucket: Bucket, def_line0: usize, out: &mut Vec<ChangedSym>, seen: &mut std::collections::HashSet<String>| {
        if seen.insert(name.clone()) {
            out.push(ChangedSym { name, kind, file: file.to_string(), bucket, def_line0 });
        }
    };
```
三处调用点分别传桶 + 精确 def 行：
- `+` 行 `match_def` 命中（约 L174，在 `Some(b'+')` 臂内、`anchor0 = new_line.saturating_sub(1)` 已算）→ `push(name, kind, Bucket::A, anchor0, ...)`（签名行进 diff=A、def_line0=该 `+` 行 0-based）。
- 两处 hunk 收尾的外围 def（约 L155、L189）：把 `innermost_enclosing_def` 换成取行版——
  ```rust
      if let Some(st) = enclosing_def_start(file_lines, a, defs) {
          if let Some((kind, n)) = match_def(file_lines[st], defs) {
              push(n, kind, Bucket::B, st, &mut out, &mut seen);
          }
      }
  ```
  （外围路径=体变=B、def_line0=`enclosing_def_start` 返回的 0-based def 行）。

> kind 无需在 push 里再分流：{struct/enum/trait/type/const/static/macro} 的定义行被 `+` 触到即走 `match_def` 路径=A，与 fn 同理天然归 A；粗判 A 由「类型定义行进 diff」自然达成。
> `innermost_enclosing_def` 私有辅助若无其它调用点、换掉后 dead → 一并删（clippy 会报 unused）。

- [ ] **Step 4: 跑测试确认通过 + 修既有测试**

Run: `cargo test -p redteam dossier 2>&1 | tail -15`
Expected: 新测试 PASS。既有构造 `ChangedSym { ... }` 的测试若报「缺 bucket 字段」→ 补 `bucket: Bucket::B`（或按语境 A）。`pack/mod.rs` 的 `sr()` 测试助手构造的是 `SymRefs` 非 `ChangedSym`，不受影响。

- [ ] **Step 5: 全量测试 + clippy + commit**

Run: `cargo test -p redteam 2>&1 | tail -5 && cargo clippy --all-targets 2>&1 | grep -c "warning:"`
Expected: 全绿、clippy 无净新增。
```bash
git add src/dossier/mod.rs
git commit -m "feat(dossier): ChangedSym 加契约桶 A/B（捕获路径导出，additive）

+ 行匹配 def 正则(签名行进 diff)=A；仅经外围 def 捕获=B。纯新树信号、
regex/dossier 引擎不读 bucket → 现有臂零行为变化。C 装饰桶缓（ledger #12）。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: SCIP caller 侧引用 + 符号级 regex 兜底

**Files:**
- Modify: `src/pack/mod.rs`（新增 `build_pack_scip` + SCIP 版 refs 构建）
- Test: `src/pack/mod.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `crate::scip::ScipIndex`（Task 1）、`crate::dossier::{ChangedSym, Bucket, SymRefs, RefHit, reference_points, symbols_in_file, enclosing_def_start, ...}`。
- Produces:
  - `pub fn build_pack_scip(bundle: &Bundle, root: &Path, budget: usize, index: &ScipIndex, strict: bool) -> String`
  - 内部 `fn scip_refs(syms, root, index, strict) -> Vec<SymRefs>`：每改动符号 → `symbol_at`(位置对) → `callers_of` → `RefHit`；`symbol_at` 查无 → strict 则跳过、否则退回单符号 `reference_points`（宁多勿漏）；打降级计数。
  - 复用 Task 3 只做 **caller=片段**（B 表示）；A 桶厚度在 Task 4。

- [ ] **Step 1: 写失败测试**（手搓 index + 真文件，验位置对→caller + 兜底）

```rust
#[test]
fn build_pack_scip_uses_index_callers_and_falls_back_per_symbol() {
    use crate::scip::ScipIndex;
    use ::scip::types::{Document, Index, Occurrence};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    // target def 在第0行；caller u1 在第1行调 target
    std::fs::write(root.join("src/a.rs"),
        "pub fn target(x: i32) {}\nfn u1() { target(1); }\n").unwrap();
    let occ = |l: i32, s: &str, r: i32| Occurrence { range: vec![l,0,0], symbol: s.into(), symbol_roles: r, ..Default::default() };
    let idx = ScipIndex::from_index(Index { documents: vec![Document {
        relative_path: "src/a.rs".into(),
        occurrences: vec![occ(0, "S target().", 1), occ(1, "S target().", 0)],
        ..Default::default()
    }], ..Default::default() });
    let b = bundle_one("src/a.rs", "@@ -1,0 +1,1 @@\n+pub fn target(x: i32) {}\n");
    let pack = build_pack_scip(&b, root, 1_000_000, &idx, false);
    assert!(pack.contains("## 引用点（1 壳）"), "有 1 壳: {pack}");
    assert!(pack.contains("u1()") || pack.contains("target(1)"), "SCIP caller 命中 u1: {pack}");
}
```

- [ ] **Step 2: 跑确认失败**

Run: `cargo test -p redteam build_pack_scip_uses_index 2>&1 | tail -5`
Expected: 失败（`build_pack_scip` 未定义）。

- [ ] **Step 3: 实现**

`src/pack/mod.rs` 加（复用现有 `body_section`/`refs_section`/`OMIT_MAX_ENTRIES`，只把 1 壳的 refs 来源换成 SCIP）：
```rust
use crate::scip::ScipIndex;
use crate::dossier::{Bucket, enclosing_def_start, RefHit, SymRefs};

/// 单符号截断长 context（照 dossier 惯例，防 minified 行撑爆）。
const CONTEXT_MAX: usize = 200;

/// SCIP 版 1 壳：每改动符号按位置对连 SCIP symbol → callers_of 引用点；
/// 位置对查无 → strict 跳过、否则退回单符号 regex reference_points。返回同构 Vec<SymRefs>（复用 refs_section 渲染）。
fn scip_refs(syms: &[crate::dossier::ChangedSym], root: &Path, index: &ScipIndex, strict: bool, fallback_count: &mut usize) -> Vec<SymRefs> {
    let mut out = Vec::new();
    for s in syms {
        // 位置对：改动符号 def 行（symbols_in_file 已带 s.def_line0）→ SCIP symbol，精确无同名歧义
        let sym = index.symbol_at(&s.file, s.def_line0);
        match sym {
            Some(symbol) => {
                let hits: Vec<RefHit> = index.callers_of(symbol).into_iter().map(|(path, line0)| {
                    let c = std::fs::read_to_string(root.join(&path)).unwrap_or_default();
                    let ctx: String = c.lines().nth(line0).unwrap_or("").chars().take(CONTEXT_MAX).collect();
                    RefHit { path, line: line0 + 1, context: ctx }
                }).collect();
                out.push(SymRefs { name: s.name.clone(), kind: s.kind, defs_same_name: 1, hits });
            }
            None if strict => { /* 诊断臂：纯 SCIP、不兜底 */ }
            None => {
                *fallback_count += 1;
                // 符号级 regex 兜底：只对本符号跑 reference_points
                let defs = crate::dossier::compile_defs(crate::dossier::lang_for(&s.file).unwrap_or(&crate::dossier::RUST));
                let mut one = reference_points(std::slice::from_ref(s), root, &defs);
                out.append(&mut one);
            }
        }
    }
    out
}

/// SCIP 引擎入口：0 壳复用（body_section 不变）+ SCIP 1 壳 + 省略清单。
/// strict=true 为 scip-strict 诊断臂（关兜底）。callee 段在 Task 5 追加。
pub fn build_pack_scip(bundle: &Bundle, root: &Path, budget: usize, index: &ScipIndex, strict: bool) -> String {
    let diff_bytes: usize = bundle.files.iter().map(|fc| fc.diff.len()).sum();
    let remaining = budget.saturating_sub(diff_bytes);
    let mut omit: Vec<String> = Vec::new();

    let bodies = body_section(bundle, root, remaining, &mut omit);
    let after_bodies = remaining.saturating_sub(bodies.len());

    // 改动符号（复用 symbols_in_file，带 bucket）
    let mut all_syms: Vec<crate::dossier::ChangedSym> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for fc in &bundle.files {
        let lang = match crate::dossier::lang_for(&fc.path) { Some(l) => l, None => continue };
        let defs = crate::dossier::compile_defs(lang);
        let content = std::fs::read_to_string(root.join(&fc.path)).unwrap_or_default();
        let file_lines: Vec<&str> = content.lines().collect();
        for s in crate::dossier::symbols_in_file(&fc.diff, &file_lines, &fc.path, &defs) {
            if seen.insert(s.name.clone()) { all_syms.push(s); }
        }
    }
    let mut fallback_count = 0usize;
    let refs = scip_refs(&all_syms, root, index, strict, &mut fallback_count);
    let refs = refs_section(&refs, after_bodies, &mut omit);
    if fallback_count > 0 {
        eprintln!("redteam: scip 引擎 {fallback_count} 个符号退回 regex 兜底（位置对无 Definition occurrence）");
    }

    let mut sections: Vec<String> = Vec::new();
    if !bodies.is_empty() { sections.push(bodies); }
    if !refs.is_empty() { sections.push(refs); }
    if !omit.is_empty() {
        let total = omit.len();
        omit.truncate(OMIT_MAX_ENTRIES);
        let mut sec = String::from("## 已省略（可 read_files 定点读）\nread_files({paths:[{path,start,end}]})\n");
        for line in &omit { sec.push_str(line); sec.push('\n'); }
        if total > OMIT_MAX_ENTRIES { sec.push_str(&format!("… +{} more omitted\n", total - OMIT_MAX_ENTRIES)); }
        sections.push(sec);
    }
    sections.join("\n")
}
```
在 `src/dossier/mod.rs` 暴露内部件供 pack 用：
- `pub(crate) const RUST: Lang`（现为私有 `const RUST`）→ 改 `pub(crate)`（兜底 `lang_for` 返 None 时的默认 defs 用）。
- 确认 `compile_defs`/`lang_for`/`enclosing_def_start`/`reference_points`/`RefHit`/`SymRefs`/`ChangedSym`/`Bucket` 已 `pub(crate)` 或 `pub`（`reference_points`/`SymRefs`/`RefHit`/`ChangedSym` 已 pub；`compile_defs`/`lang_for`/`enclosing_def_start` 已 `pub(crate)`）。
- 位置对**免重扫**：`scip_refs` 直接用 `s.def_line0`（Task 2 已带），不需 `match_def_name`。

- [ ] **Step 4: 跑确认通过**

Run: `cargo test -p redteam build_pack_scip_uses_index 2>&1 | tail -5`
Expected: PASS。

- [ ] **Step 5: 兜底测试**（位置对查无 → regex 兜底命中）

```rust
#[test]
fn build_pack_scip_falls_back_when_symbol_absent_from_index() {
    use crate::scip::ScipIndex;
    use ::scip::types::Index;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"),
        "pub fn ghost() {}\nfn u1() { ghost(); }\n").unwrap();
    let idx = ScipIndex::from_index(Index::default()); // 空索引 → 位置对必查无
    let b = bundle_one("src/a.rs", "@@ -1,0 +1,1 @@\n+pub fn ghost() {}\n");
    let pack = build_pack_scip(&b, root, 1_000_000, &idx, false);
    assert!(pack.contains("ghost"), "空索引 → regex 兜底仍抓到 caller: {pack}");
    // strict 臂：关兜底 → 无引用点
    let strict = build_pack_scip(&b, root, 1_000_000, &idx, true);
    assert!(!strict.contains("## 引用点"), "strict 关兜底 → 空: {strict}");
}
```
Run: `cargo test -p redteam build_pack_scip 2>&1 | tail -5`
Expected: 两测试 PASS。

- [ ] **Step 6: clippy + commit**

```bash
git add src/pack/mod.rs src/dossier/mod.rs
git commit -m "feat(pack): SCIP caller 侧引用 + 符号级 regex 兜底（build_pack_scip）

位置对（def 行→SCIP Definition）→ callers_of 填 1 壳；查无退单符号
reference_points（宁多勿漏、打降级计数）；strict 臂关兜底供诊断。
同构 SymRefs 复用 refs_section 渲染。0 壳 body_section 不变。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: A 桶 caller 厚度（完整外围体）

**Files:**
- Modify: `src/pack/mod.rs`（A 桶 caller 提升为外围体）
- Test: `src/pack/mod.rs`

**Interfaces:**
- Consumes: Task 3 的 `scip_refs` + `Bucket`。
- Produces: A 桶符号的 caller `RefHit.context` = 该 caller 的**完整外围 def 体**（`enclosing_def_start`+`block_end` 抠范围、读文件切片）；B 桶保持单行片段。方向性 D9：高危桶付 body 钱。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn build_pack_scip_a_bucket_gives_caller_full_body_b_gives_snippet() {
    use crate::scip::ScipIndex;
    use ::scip::types::{Document, Index, Occurrence};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    // target def 第0行；caller u1 跨 2-4 行（多行体）
    std::fs::write(root.join("src/a.rs"),
        "pub fn target(x: i32) {}\nfn u1() {\n    let z = 1;\n    target(z);\n}\n").unwrap();
    let occ = |l: i32, s: &str, r: i32| Occurrence { range: vec![l,0,0], symbol: s.into(), symbol_roles: r, ..Default::default() };
    let idx = ScipIndex::from_index(Index { documents: vec![Document {
        relative_path: "src/a.rs".into(),
        occurrences: vec![occ(0,"S target().",1), occ(3,"S target().",0)],
        ..Default::default()
    }], ..Default::default() });
    // A：改签名行
    let a = build_pack_scip(&bundle_one("src/a.rs", "@@ -1,0 +1,1 @@\n+pub fn target(x: i32) {}\n"), root, 1_000_000, &idx, false);
    assert!(a.contains("fn u1() {") && a.contains("let z = 1;"), "A 桶 caller 给完整外围体: {a}");
    // B：只改体
    let b = build_pack_scip(&bundle_one("src/a.rs", "@@ -1,0 +1,1 @@\n+    // touched body\n"), root, 1_000_000, &idx, false);
    assert!(!b.contains("let z = 1;"), "B 桶 caller 只给片段、不含 caller 体其余行: {b}");
}
```

- [ ] **Step 2: 跑确认失败**

Run: `cargo test -p redteam a_bucket_gives_caller_full_body 2>&1 | tail -5`
Expected: 失败（当前 A/B 都给单行片段）。

- [ ] **Step 3: 实现**

`scip_refs` 里构建 `RefHit` 处按符号 `bucket` 分流 context：
```rust
                let hits: Vec<RefHit> = index.callers_of(symbol).into_iter().map(|(path, line0)| {
                    let c = std::fs::read_to_string(root.join(&path)).unwrap_or_default();
                    let clines: Vec<&str> = c.lines().collect();
                    let context = if s.bucket == Bucket::A {
                        // A 桶：caller 完整外围体（enclosing def 范围切片）
                        let cdefs = crate::dossier::compile_defs(crate::dossier::lang_for(&path).unwrap_or(&crate::dossier::RUST));
                        match crate::dossier::enclosing_def_start(&clines, line0, &cdefs) {
                            Some(st) => {
                                let en = crate::dossier::block_end(&clines, st).min(clines.len().saturating_sub(1));
                                clines.get(st..=en).unwrap_or(&[]).join("\n")
                            }
                            None => clines.get(line0).unwrap_or(&"").chars().take(CONTEXT_MAX).collect(),
                        }
                    } else {
                        clines.get(line0).unwrap_or(&"").chars().take(CONTEXT_MAX).collect()
                    };
                    RefHit { path, line: line0 + 1, context }
                }).collect();
```
`src/dossier/mod.rs`：`block_end` 现为私有 `fn`（约 L84）→ 改 `pub(crate) fn block_end`。

- [ ] **Step 4: 跑确认通过**

Run: `cargo test -p redteam a_bucket_gives_caller_full_body 2>&1 | tail -5`
Expected: PASS。

- [ ] **Step 5: 全量 + clippy + commit**

Run: `cargo test -p redteam 2>&1 | tail -5 && cargo clippy --all-targets 2>&1 | grep -c "warning:"`
```bash
git add src/pack/mod.rs src/dossier/mod.rs
git commit -m "feat(pack): A 桶 caller 给完整外围体、B 桶给片段（方向性 D9）

签名面变(A)=高危桶付 body 钱、caller 全体杀「读 caller 查用法」；
仅体变(B)=片段。均 1 跳；传递深度缓 S2-c（ledger #6）。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: callee 签名注入段

**Files:**
- Modify: `src/pack/mod.rs`（新 callee 段）
- Test: `src/pack/mod.rs`

**Interfaces:**
- Consumes: `ScipIndex::callees_in` + `signature_of`、`enclosing_def_start`/`block_end`（定改动符号 def 区间）。
- Produces: `fn callee_section(syms, root, index, budget, omit) -> String`——每改动符号取其 def 区间内 callee → `signature_of` → `## 被调契约（callee）` 段，每行 `symbol — signature`；外部 dep（`signature_of` 有但本仓 def 无）打 `[dep]` 标；预算封顶、溢出进 omit。空 → ""。

> 判「外部 dep」：`index.symbol_at` 在本仓任一文件查得到该 symbol 的 def = 本仓；否则 = dep。MVP 用简化判据：symbol 串含 `" cargo redteam "`（本 crate 前缀，探针实测 `rust-analyzer cargo redteam 0.1.0 …`）= 本仓，否则 `[dep]`。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn build_pack_scip_injects_callee_signatures() {
    use crate::scip::ScipIndex;
    // 注：signature_documentation 内层类型是 Signature（非 Document）——Task 1 已核实 scip 0.9.0 生成源，Signature 有同名 text: String。
    use ::scip::types::{Document, Index, Occurrence, Signature, SymbolInformation};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    // caller def 第0-2行，内部调 helper
    std::fs::write(root.join("src/a.rs"),
        "pub fn caller() {\n    helper(1);\n}\n").unwrap();
    let occ = |l: i32, s: &str, r: i32| Occurrence { range: vec![l,0,0], symbol: s.into(), symbol_roles: r, ..Default::default() };
    let mut helper_si = SymbolInformation { symbol: "rust-analyzer cargo redteam 0.1.0 x/helper().".into(), ..Default::default() };
    helper_si.signature_documentation = protobuf::MessageField::some(Signature { text: "fn helper(n: i32) -> i32".into(), ..Default::default() });
    let idx = ScipIndex::from_index(Index { documents: vec![Document {
        relative_path: "src/a.rs".into(),
        occurrences: vec![
            occ(0, "rust-analyzer cargo redteam 0.1.0 x/caller().", 1),
            occ(1, "rust-analyzer cargo redteam 0.1.0 x/helper().", 0), // caller 体内调 helper
        ],
        symbols: vec![helper_si],
        ..Default::default()
    }], ..Default::default() });
    let b = bundle_one("src/a.rs", "@@ -1,0 +1,3 @@\n+pub fn caller() {\n+    helper(1);\n+}\n");
    let pack = build_pack_scip(&b, root, 1_000_000, &idx, false);
    assert!(pack.contains("## 被调契约（callee）"), "有 callee 段: {pack}");
    assert!(pack.contains("fn helper(n: i32) -> i32"), "注入 helper 签名: {pack}");
}
```

- [ ] **Step 2: 跑确认失败**

Run: `cargo test -p redteam injects_callee_signatures 2>&1 | tail -5`
Expected: 失败（无 callee 段）。

- [ ] **Step 3: 实现**

`src/pack/mod.rs` 加 `callee_section`，并在 `build_pack_scip` 组装处（`refs` 段之后、`omit` 之前）插入：
```rust
/// callee 段：改动符号 def 区间内 callee symbol → 注入其签名（1 跳、不递归）。外部 dep 打 [dep]。
fn callee_section(syms: &[crate::dossier::ChangedSym], root: &Path, index: &ScipIndex, budget: usize, omit: &mut Vec<String>) -> String {
    let mut body = String::new();
    let mut seen = std::collections::HashSet::new();
    for s in syms {
        let content = std::fs::read_to_string(root.join(&s.file)).unwrap_or_default();
        let lines: Vec<&str> = content.lines().collect();
        // 位置对精确 def 行（Task 2 带的 s.def_line0）→ 区间 [def_line0, block_end]
        let end0 = crate::dossier::block_end(&lines, s.def_line0).min(lines.len().saturating_sub(1));
        let own = index.symbol_at(&s.file, s.def_line0).unwrap_or("");
        for callee in index.callees_in(&s.file, s.def_line0, end0, own) {
            let sig = match index.signature_of(&callee) { Some(sig) => sig, None => continue };
            if !seen.insert(callee.clone()) { continue; }
            let dep = if callee.contains(" cargo redteam ") { "" } else { " [dep]" };
            // 展示名取 signature 本身（已含名+签名），dep 标后缀
            let line = format!("- {sig}{dep}\n");
            if body.len() + line.len() > budget {
                omit.push(format!("callee 签名 {callee}"));
            } else {
                body.push_str(&line);
            }
        }
    }
    if body.is_empty() { return String::new(); }
    format!("## 被调契约（callee）\n{body}")
}
```
`build_pack_scip` 组装：
```rust
    let callees = callee_section(&all_syms, root, index, after_bodies.saturating_sub(refs.len()), &mut omit);
    // sections 顺序：bodies → refs → callees → omit
    if !bodies.is_empty() { sections.push(bodies); }
    if !refs.is_empty() { sections.push(refs); }
    if !callees.is_empty() { sections.push(callees); }
    // …omit 段不变
```
`src/dossier/mod.rs`：确认 `block_end` 已在 Task 4 转 `pub(crate)`。

- [ ] **Step 4: 跑确认通过 + 全量 + clippy**

Run: `cargo test -p redteam 2>&1 | tail -5 && cargo clippy --all-targets 2>&1 | grep -c "warning:"`
Expected: 全绿、clippy 无净新增。

- [ ] **Step 5: commit**

```bash
git add src/pack/mod.rs src/dossier/mod.rs
git commit -m "feat(pack): callee 签名注入段（方向性 D9 的 callee 半）

改动符号 def 区间内 callee → signature_of 注入（1 跳不递归无体），
杀「search 被调函数签名」；外部 dep 打 [dep]、预算溢出进省略。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: SCIP runner（跑 rust-analyzer + tempfile 即弃）

**Files:**
- Modify: `src/scip/mod.rs`（加 `ScipIndex::build`）
- Test: `src/scip/mod.rs`（`#[ignore]` 集成测试）

**Interfaces:**
- Produces:
  - `pub fn ScipIndex::build(root: &Path) -> Option<ScipIndex>`：探测 `rust-analyzer` 在 PATH → 跑 `rust-analyzer scip <root> --output <tmp>` → 解析 → 删 tmp → `Some`；RA 缺 / 非零退出 / 解析失败 → `None`（降级信号，调用方转 regex）。

- [ ] **Step 1: 写 `#[ignore]` 集成测试**

```rust
#[test]
#[ignore] // 需本机装 rust-analyzer；手动 `cargo test -p redteam scip_build -- --ignored`
fn scip_build_indexes_this_repo() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let idx = ScipIndex::build(root).expect("本仓应能被 rust-analyzer 索引");
    // 本仓已知符号：dossier::reference_points 应有 def 位置对
    // （精确行号随源码漂移，故只断言签名表非空 + 某已知函数签名可查）
    assert!(idx.signature_of("rust-analyzer cargo redteam 0.1.0 dossier/reference_points().").is_some(),
        "reference_points 签名应在索引里");
}
```

- [ ] **Step 2: 跑确认失败**

Run: `cargo test -p redteam scip_build -- --ignored 2>&1 | tail -5`
Expected: 失败（`ScipIndex::build` 未定义）。

- [ ] **Step 3: 实现**

`src/scip/mod.rs` 加（用 `std::process::Command` 同步跑；`tempfile` 建即弃临时文件）：
```rust
use std::path::Path;
use std::process::Command;
use protobuf::Message;

impl ScipIndex {
    /// review 前置：跑 rust-analyzer scip 对工作树建索引 → 解析 → 删临时文件。
    /// RA 缺 / 失败 / 解析错 → None（调用方降级 regex-pack，绝不阻塞）。
    pub fn build(root: &Path) -> Option<ScipIndex> {
        // 借工具链：探 rust-analyzer 在不在（D16）
        if Command::new("rust-analyzer").arg("--version").output().ok()?.status.success() == false {
            return None;
        }
        let tmp = tempfile::Builder::new().prefix("redteam-scip-").suffix(".scip").tempfile().ok()?;
        let out_path = tmp.path().to_path_buf();
        let status = Command::new("rust-analyzer")
            .arg("scip").arg(root)
            .arg("--output").arg(&out_path)
            .current_dir(root)
            .status().ok()?;
        if !status.success() {
            return None;
        }
        let bytes = std::fs::read(&out_path).ok()?;
        // tmp（NamedTempFile）drop 时自动删；解析后即弃
        let idx = Index::parse_from_bytes(&bytes).ok()?;
        Some(ScipIndex::from_index(idx))
    }
}
```
> 注：`rust-analyzer scip` 容错（E4：重复符号仍 exit 0 出全索引），故 `status.success()` 基本恒真；真降级触发点=RA 没装（第一道 `--version` 探测）。`tmp` 是 `NamedTempFile`、离开作用域自动删=零仓库足迹。

- [ ] **Step 4: 跑确认通过（有 RA 时）**

Run: `cargo test -p redteam scip_build -- --ignored 2>&1 | tail -5`
Expected: PASS（本机装了 rust-analyzer；~5s）。若本机无 RA：跳过此验证、Task 7 的降级路径覆盖无-RA 场景。

- [ ] **Step 5: clippy + commit**

Run: `cargo clippy --all-targets 2>&1 | grep -c "warning:"`（`== false` 若被 clippy 挑 → 改 `!...success()`）
```bash
git add src/scip/mod.rs
git commit -m "feat(scip): runner——跑 rust-analyzer scip → tempfile 即弃 → 解析

探 rust-analyzer 在不在（借，D16）；跑 scip 输出临时文件、NamedTempFile
drop 自动删=零仓库足迹；RA 缺/失败/解析错 → None（调用方降级）。集成测试标 #[ignore]。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: cli `REDTEAM_PACK` 枚举化 + 引擎 dispatch + 降级

**Files:**
- Modify: `src/cli.rs`（`build_packs` 引擎枚举 + 索引前置建一次 + RA 缺降级）
- Test: `src/cli.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `crate::scip::ScipIndex`、`crate::pack::{build_pack, build_pack_scip}`。
- Produces: `REDTEAM_PACK` 语义扩展——unset/其他=regex-pack（默认不变）；`0`=dossier 基线；`scip`=SCIP 引擎（RA 缺则降级 regex + 一行提示）；`scip-strict`=SCIP 诊断臂（关兜底）。`REDTEAM_NO_DOSSIER` null 臂不变。

- [ ] **Step 1: 写失败测试**（默认/dossier/null 三臂行为不变 + scip 无 RA 降级不 panic）

```rust
#[test]
fn build_packs_scip_arm_degrades_without_ra_and_keeps_other_arms() {
    use crate::bundle::Bundle;
    use crate::evidence::git::FileChange;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"),
        "pub fn target() {}\nfn u1() { target(); }\n").unwrap();
    let bundles = vec![Bundle { files: vec![FileChange {
        path: "src/a.rs".into(), diff: "@@ -1,0 +1,1 @@\n+pub fn target() {}\n".into(),
    }] }];
    // scip 臂：本测试环境可能无 RA → 必须降级 regex（不 panic、仍出引用点）
    unsafe { std::env::remove_var("REDTEAM_NO_DOSSIER"); std::env::set_var("REDTEAM_PACK", "scip"); }
    let (scip, _) = build_packs(&bundles, root);
    assert!(scip[0].contains("target"), "scip 臂（或其降级）仍产 pack: {}", scip[0]);
    // 默认臂不变
    unsafe { std::env::remove_var("REDTEAM_PACK"); }
    let (def, _) = build_packs(&bundles, root);
    assert!(def[0].contains("改动符号完整体"), "默认臂仍 regex-pack");
    unsafe { std::env::remove_var("REDTEAM_PACK"); }
}
```

- [ ] **Step 2: 跑确认失败/通过基线**

Run: `cargo test -p redteam build_packs_scip_arm 2>&1 | tail -5`
Expected: 失败（scip 臂未接、当前落进默认 regex 但无 scip 分支——先失败驱动实现）。

- [ ] **Step 3: 实现**

`src/cli.rs` `build_packs`（约 L334）改造为引擎枚举 + 索引前置建一次：
```rust
enum PackEngine { Regex, Dossier, Scip { strict: bool } }

fn pack_engine() -> PackEngine {
    match std::env::var("REDTEAM_PACK").ok().as_deref() {
        Some("0") => PackEngine::Dossier,
        Some("scip") => PackEngine::Scip { strict: false },
        Some("scip-strict") => PackEngine::Scip { strict: true },
        _ => PackEngine::Regex,
    }
}

pub(crate) fn build_packs(bundles: &[crate::bundle::Bundle], root: &std::path::Path) -> (Vec<String>, DossierMeta) {
    let suppressed = std::env::var_os("REDTEAM_NO_DOSSIER").is_some();
    let engine = pack_engine();
    // scip 引擎：前置建索引一次（非 per-bundle）；RA 缺 → None → 降级 regex
    let scip_index = match &engine {
        PackEngine::Scip { .. } => {
            let idx = crate::scip::ScipIndex::build(root);
            if idx.is_none() {
                eprintln!("redteam: 未探到 rust-analyzer（或索引失败），scip 引擎降级为 regex-pack");
            }
            idx
        }
        _ => None,
    };
    let strict = matches!(engine, PackEngine::Scip { strict: true });
    let entries: Vec<String> = bundles
        .iter()
        .map(|b| {
            if suppressed {
                String::new()
            } else {
                match (&engine, &scip_index) {
                    (PackEngine::Dossier, _) =>
                        crate::prompt::render_dossier_section(&crate::dossier::build_dossier(b, root)),
                    (PackEngine::Scip { .. }, Some(idx)) =>
                        crate::prompt::render_dossier_section(&crate::pack::build_pack_scip(b, root, PACK_BUDGET_BYTES, idx, strict)),
                    // Regex 默认，及 scip 降级（scip_index=None）都走 regex-pack
                    _ =>
                        crate::prompt::render_dossier_section(&crate::pack::build_pack(b, root, PACK_BUDGET_BYTES)),
                }
            }
        })
        .collect();
    // …meta 组装（injected/suppressed 计数）保持原逻辑不变
```
> 保留原 `DossierMeta` 组装尾巴（injected = 非空段计数等）；只替换臂分发逻辑。更新函数头 doc 注释：四臂（regex 默认 / dossier `0` / scip / scip-strict）+ null。

- [ ] **Step 4: 跑确认通过 + 既有三臂测试**

Run: `cargo test -p redteam build_packs 2>&1 | tail -10`
Expected: 新测试 + 既有 `build_packs_default_has_bodies_off_arm_dossier_only_null_arm_empty` 全 PASS（默认/dossier/null 行为未变）。

- [ ] **Step 5: 全量 + clippy + verify + commit**

Run: `cargo test -p redteam 2>&1 | tail -5 && cargo clippy --all-targets 2>&1 | grep -c "warning:" && make verify 2>&1 | tail -3`
Expected: 全绿、clippy 净新增 0、verify 绿。
```bash
git add src/cli.rs
git commit -m "feat(cli): REDTEAM_PACK 枚举化 + scip 引擎 dispatch + RA 缺降级

四臂 regex(默认不变)/dossier(0)/scip/scip-strict + null；scip 前置建索引
一次（非 per-bundle）、RA 缺则 None→降级 regex+一行提示、绝不阻塞。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## 验收（本计划交付后，design §5；单独动作、非本 plan 步骤）

- **静态、零 LLM**：`REDTEAM_PACK=scip-strict` vs 无索引 regex，对同一变更集量 `SCIP_refs \ regex_refs`（recall delta = tool_use 杠杆代理）+ 分桶 A/B 分布 + callee 注入产量。
- **端到端（显著才跑）**：`REDTEAM_PACK=scip` vs regex-pack，对**附四**做差、附三 OFF 不重跑；先单模型 DeepSeek 探方向、只下门决策 3 模型×N=3。**同时量 input 与 tool_use**（A-caller 全体+callee 注入加 input、赌注是补读轮次降）。
- **门（D25 双门）**：残余 tool_use robust 降 **且** agreement 不向假共识收窄。
- 过门 → 翻 `REDTEAM_PACK` 默认为 scip（另一 commit）；未过 → 差异记 research + ledger 复盘。

## Self-Review

- **Spec 覆盖**（design §0-§5）：§1 substrate=Task 1+6；§2 符号解析=Task 3(位置对/兜底)+Task 5(callee 外部 dep)；§3 分类器=Task 2；§4 radius=Task 3(caller)+4(A/B 厚度)+5(callee)；§5 验收=验收节。toggle 枚举=Task 7。**缓项确认不做**：双树/缓存/剪枝/传递+排序/穷尽性/build-gate/per-model 分叉/C 桶——无对应 Task（正确）。
- **占位符扫描**：无 TBD/"实现细节后补"；每步含真代码+真命令+期望输出。
- **类型一致**：`ScipIndex`/`Bucket`/`build_pack_scip`/`scip_refs`/`callee_section` 跨 Task 命名一致；`ChangedSym` 加 `bucket`+`def_line0`（位置对连接键，Task 2 带、Task 3/5 用）；`RefHit{path,line,context}`/`SymRefs{name,kind,defs_same_name,hits}` 沿用现结构；`ScipIndex::{from_index,symbol_at,callers_of,callees_in,signature_of,build}` 六方法签名 Task 1/3/4/5/6 一致。
- **已知天花板**（design §3 / ledger #3）：多行签名只改参数行误判 B → T40，非本计划修（B 仍展 1 跳=优雅降级）。
