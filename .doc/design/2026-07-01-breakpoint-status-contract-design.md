# Breakpoint status 契约设计（P1-D：统一 status schema + phase + 删 --wait）

> 来源：`.doc/research/2026-06-30-live-review-operability-lessons.md` 主题 D（R3.2/R3.3）。
> 前置：P0 已交付 `progress`/`last_active`/`deadline_at`；P1-C 已交付弹性/重试。
> 本 spec 是同一可靠性 roadmap 的续篇，续在 `feat/reliability-core` 分支。

## 1. 动机

上层 supervisor（loop SKILL）要无人值守驱动 `brk`，靠的是 `status` 输出。两个契约缺口：

1. **双 schema**：`status <id>` 打 JobState 的 JSON 对象（main.rs:396），而 `status`（全部）走
   `render_job_list` 打**自定义文本行** `{id} {status:?} {mode} {done}/{total}`（main.rs:392,152）。
   解析单 job 与解析列表是两套逻辑——codex 的「单 job 与 --all JSON 结构不一致」正是此坑。
2. **`--wait` 说谎**：`--wait` 字段存在但**被忽略**（`wait: _wait`，main.rs:244）——前台默认路径
   本就同步等待，`--wait` 是空操作、死 flag。留着它迟早长成 codex 的「exit-124 但 job 仍
   detached」隐患。

外加 P0 dogfood 观察：后台 job 在 `running 2/2` 卡约 3min，其实是 verify 尾段（progress 与
last_active 在尾段都不动）——上层无法区分「尾段慢」与「真卡死」。

## 2. 范围

**做**：
- R3.2 `status`（全部）改为**JobState 的 JSON 数组**（与 `status <id>` 的对象**同 schema**）；
  把「`result` 仅终态可取」写进命令 help 文本；§3.1 顺手统一 `deadline_at` 的 None 省略。
- **phase 字段**：JobState 增 `phase: Option<Phase>`（Collecting/Fanout/Finalizing），worker 在
  阶段转换处刷新，让上层区分「尾段慢 vs 真卡死」。
- **workspace 字段 + 跨 workspace 查询**（§3.2）：JobState 增 `workspace: Option<String>`（canonical
  根路径，自描述属哪个 repo/dir）；单个 `status --workspaces [substr]` flag——无值聚合所有 workspace、
  带值按路径子串筛——支持跨 workspace 并行评审的查询与筛选。
- R3.3 **删 `--wait`**：删掉这个空操作 flag（cancel 命令已能查 + 取消 `--background` 作业，
  `--wait` 无实需）。

**不做（YAGNI / defer）**：
- 实现「持久后台 + 阻塞轮询」的诚实 `--wait`（B 方案）——用户裁定删而非实现（§7①）。
- `elapsed_ms` 字段——可由 `started_at` + now 派生，caller 自算，不入 schema。
- phase 细分到 lens/verify 子步——Collecting/Fanout/Finalizing 三态足够定位卡死段。

## 3. R3.2 统一 status schema

`src/main.rs` `Cmd::Status`：
- `status <id>`（已是）：`serde_json::to_string_pretty(&st)` → 一个 JobState 对象。**不变**。
- `status`（无 id，当前 `render_job_list`）：改为 `serde_json::to_string_pretty(&jobs)` →
  **JobState 数组**。空 workspace → `[]`。
- 删除 `render_job_list`（main.rs:152-158）及其测试。

上层解析：`status <id>` → 一个对象，`status` → 对象数组，**每元素同 JobState schema**，
`.status`/`.progress`/`.last_active`/`.deadline_at`/`.phase` 一套解析通吃。

**result 仅终态**：`result <id>` 已在非 `Done` 时报错退 2（main.rs:424）。把这条规则写进
`Cmd::Result` 的 clap doc-comment（现为「取回已完成作业的评审结果（读后标记已消费）」→ 补
「仅作业处于终态 `done` 时可取；未终态返回退出码 2」）。

### 3.1 schema 一致性收尾：deadline_at

现状不一致：`deadline_at` 只有 `#[serde(default)]`、无 `skip_serializing_if`，故 None 时序列化成
`"deadline_at": null`；而 `mode`/`pid`/`exit_code`/`error`/`result_path` None 时**省略键**。契约
主题里一并收干净——给 `deadline_at` 加 `skip_serializing_if = "Option::is_none"`，与其余可选字段
统一为「None → 省略键」。`#[serde(default)]` 保留（读旧文件缺键回 None）。上层判「有无 deadline」
统一用「键是否存在」，不再区分 `null` 与省略。

### 3.2 workspace 字段 + 跨 workspace 查询

**背景**：job 按 workspace 分桶存 `state_root()/<fnv1a64(canonical_root)>/jobs/`；`status` 只读
cwd 那一桶。跨 workspace 并行评审时看不全、job JSON 里也只有哈希隐含 workspace、无显式字段。

**字段**：JobState 增

```rust
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub workspace: Option<String>,
```

= canonical 根路径（如 `/Users/weilai/hiapi/sites`），后台建 job 时写入（`std::fs::canonicalize(cwd)`
失败则回落原始 cwd 字符串）。让每条 job 自描述属哪个 repo/dir，也是哈希桶的可读反向。旧 job 缺键
→ None，与其余可选字段一致。前台无 JobState、不涉及。

**参数**（`Cmd::Status`）——单个带可选值的 `--workspaces` flag：

```rust
#[arg(long, num_args = 0..=1, default_missing_value = "")]
workspaces: Option<String>,
```

| 调用 | `workspaces` 值 | 行为 |
|---|---|---|
| `status` | `None` | 当前 workspace 全部 job（JSON 数组）。默认，不变。 |
| `status <job_id>` | — | 单 job（对象），当前 workspace。不变。 |
| `status --workspaces` | `Some("")` | **所有** workspace 聚合（每元素带 `workspace`）。空子串不筛。 |
| `status --workspaces <substr>` | `Some(substr)` | 扫全部 + 按 `workspace` 路径子串筛。 |

实现——一条代码路径，`Some("")`（全部）即 `Some(substr)`（筛）的空筛特例：

```rust
let jobs = match workspaces {
    None => list_states(&cwd_ws),                       // 默认：当前 workspace
    Some(filter) => list_all_states(&state_root())      // --workspaces [substr]
        .into_iter()
        .filter(|j| j.workspace.as_deref().unwrap_or("").contains(&filter))
        .collect(),
};
```

`filter=""`（`--workspaces`）→ `.contains("")` 恒真 → 含所有 job（含 `workspace=None` 的旧 job）；
`filter="sites"` → `workspace=None` 的旧 job 不匹配、被排除。positional `job_id` 优先（给了就走单
job 路径，忽略 `--workspaces`）。

新增 `state/store.rs`（或 `state/job.rs`）helper：`list_all_states(state_root: &Path) -> Vec<JobState>`
——`read_dir(state_root)` 遍历每个桶目录 → `jobs_dir(bucket)` → `list_states` 聚合。

**只读聚合、不跨桶 prune**：`--workspaces` 只读，机会式 prune 仍每 workspace 自理（`status`
默认路径的 `prune_here(cwd_ws)` 不变），避免一条 status 命令去动别的 workspace 的文件（§7⑥）。

## 4. phase 字段

### 4.1 类型

`src/state/job.rs`：

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Collecting,  // worker 起手，收集证据（git diff / 读文件），fanout 前
    Fanout,      // cell 并发执行中（progress.done 推进）
    Finalizing,  // 尾段：dedup/correlate/verify/verdict（progress 静止、last_active 静止）
}
```

JobState 增字段（与既有可选字段同风格，旧文件缺键 → None）：

```rust
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub phase: Option<Phase>,
```

`JobState::new` 里 `phase: None`。phase 是**后台作业专属**概念——前台路径无 JobState、不写 phase。
终态（Done/Failed/TimedOut/Cancelled）由 `status` 表达，phase 只在 running 期间有意义（终态时
保留最后值即可，上层看 `status` 而非 phase 判终结）。

### 4.2 转换埋点（仅 worker 侧写）

三处，全部 sink-gated 或 worker-scoped，前台（sink=None）天然不触发：

1. **Collecting**：`run_job`（worker 独有）起手、调 dispatch 前，直接写 `st.phase = Some(Collecting)`
   （run_job 已在设 `deadline_at`，同处一并设）。
2. **Fanout**：`JobCellSink::manifest()`（已是 fanout-start 钩子，现设 `progress.total`）额外写
   `st.phase = Some(Fanout)`。
3. **Finalizing**：`CellSink` trait 增一个**带默认 no-op 实现**的方法
   `fn on_phase(&self, _phase: Phase) {}`；`JobCellSink` override 为写 `st.phase`。dispatch 在
   fanout 返回后、进尾段（`assemble_report`）前调 `sink.on_phase(Phase::Finalizing)`。
   默认 no-op 保证非 JobCellSink 的 sink 与前台 sink=None 零影响。

> 埋点插入点（plan 定精确写法）：`on_phase(Finalizing)` 置于 `assemble_report` 入口（把 sink
> 塞进 `TailCtx` 一次性调用）或 `run_review`/`run_code_review`/`run_resume` 三处 fanout 后各调
> 一次——plan 择最短 diff。

## 5. R3.3 删 --wait

`src/main.rs` `Cmd::Review`：删 `wait: bool` clap 字段 + `wait: _wait` 解构行（main.rs:244）。
`src/cli.rs:28` 提及 `--wait` 的注释一并清理。`ReviewArgs`（cli.rs）本就不含 `wait` 字段，无需动。

**替代路径已足**：前台默认路径同步阻塞等待；`--background` 即发即查（`status`）即取消（`cancel`）。
删 `--wait` 后 `brk review --wait ...` → clap 报未知参数错，明确而非静默空操作。

## 6. 消费者核查

改 `status`（全部）从文本行 → JSON 数组，须核查并更新任何解析旧文本格式的消费者：
- `plugins/breakpoint/commands/*.md`（review/status/result 命令说明）
- codex skills（若有 `brk status` 文本解析）

若这些仅调用 `brk status <id>`（单 job，格式未变）或原样透传输出，则无需改；解析 `status`
全列表文本的地方须改为解析 JSON 数组。plan 内先 grep 定位。

## 7. 关键决策

① **删 `--wait` 而非实现（用户 2026-07-01 定）**：`--wait` 当前空操作；前台已同步等待、
`--background` 有 `status`（查）+ `cancel`（取消），第三个「持久后台+阻塞」模式无实需。删掉最小
最诚实，直接消灭「会超时的 --wait 孤儿 job」隐患。B 方案（诚实 `--wait --timeout`）仅在确有
「关终端也续跑但当前又想阻塞等」交互需求时才值——defer。

② **加 phase（用户 2026-07-01 定）**：三态 Collecting/Fanout/Finalizing 精确定位 P0 dogfood 观察
到的「running 2/2 卡 3min = verify 尾段」——让上层区分尾段慢与真卡死。轻量：一个 enum + 三处
埋点，全 sink-gated/worker-scoped，前台零影响。

③ **status 全列表用 JSON 数组而非 NDJSON/带包裹对象**：数组元素即 JobState，与 `status <id>`
对象同 schema，`jq '.[]'` 或 `serde_json::from_str::<Vec<JobState>>` 直接吃。不加 `{jobs:[...]}`
包裹（多一层无收益）。

④ **phase 为 `Option`、旧文件缺键回 None**：与 P0 的 `last_active`/`deadline_at` 同风格，
状态文件向后兼容（旧 job 文件读得回、phase=None）。

⑤ **跨 workspace 查询用单个 `--workspaces [substr]`（用户 2026-07-01 定）**：一个带可选值的 flag——
无值聚合全部、带值按路径子串筛（`Some("")` 即空筛特例，一条代码路径，免双 flag 判断）。`workspace`
字段令每条 job 自描述。默认 `status` 仍只当前 workspace（向后兼容）。选内置过滤而非「仅字段+jq」是
为跨 repo 并行时的直接体验。

⑥ **`--workspaces` 只读、不跨桶 prune**：一条 status 命令不去动别的 workspace 的状态文件；
prune 仍每 workspace 自理。避免「查一下把别人的作业清了」的意外。

## 8. 测试策略

hermetic，纯序列化 + 状态读写：
- **R3.2 数组 schema**：写 3 个 JobState → `status`（全部）路径产出的 JSON `serde_json::from_str::<Vec<JobState>>` 解析成功、含 3 元素；空 workspace → `[]`（`Vec` 空）。
- **phase 序列化**：JobState 带 `phase=Some(Fanout)` round-trip；`rename_all=lowercase` 断言序列化为 `"fanout"`；旧文件（无 phase 键）→ `phase=None`。
- **phase 埋点**：扩展 `run_job` 既有测试或加一条——后台 job 跑完后读回 state，验其间 phase 曾经过 Fanout（可在 fake provider 下断言 manifest 触发后 state.phase=Some(Fanout)）；或单测 `JobCellSink::manifest` 写入 `phase=Fanout`、`on_phase(Finalizing)` 写入 `phase=Finalizing`。
- **on_phase 默认 no-op**：非 JobCellSink 的 sink（如测试替身/None）调 `on_phase` 不 panic、不写盘。
- **deadline_at 一致性（§3.1）**：`deadline_at=None` 的 JobState 序列化后**不含** `deadline_at` 键（与 `pid` 等一致）；`deadline_at=Some(x)` 含键；旧文件缺键 → None（既有 `jobstate_new_fields_default_and_roundtrip` 测试已覆盖缺键→None，补一条 None→省略键的断言）。
- **workspace 字段（§3.2）**：`workspace=Some(path)` round-trip；None→省略键；旧文件缺键→None。
- **跨 workspace 聚合 + 筛选（§3.2）**：`list_all_states` 在含 2 个桶（各写 1 job、workspace 各异）的 `state_root` 下返回 2 条；空子串 `""`（`--workspaces` 无值）保留全部（含 `workspace=None` 的 job）；非空子串只留 `workspace` 含该串的那条、排除 `workspace=None`；桶目录里的非法/无关文件不 panic（沿用 `list_states` 的容错）。
- **R3.3**：删 `--wait` 为编译期（flag 消失）；补一句文档/手测 `brk review --wait` → clap 未知参数错。不写单测。

## 9. 触及文件

| 文件 | 改动 |
|---|---|
| `src/state/job.rs` | `Phase` enum；JobState 增 `phase: Option<Phase>` + `workspace: Option<String>`（§3.2）+ `JobState::new` 置 None；`deadline_at` 加 `skip_serializing_if = "Option::is_none"`（§3.1）；round-trip/默认/workspace 测试 |
| `src/state/store.rs` | 新增 `list_all_states(state_root: &Path) -> Vec<JobState>`（遍历所有桶聚合）+ 测试 |
| `src/orchestrate/mod.rs` | `CellSink` trait 增 `fn on_phase(&self, _: Phase) {}`（默认 no-op） |
| `src/cli.rs` | `JobCellSink::manifest` 加写 `phase=Fanout`；`impl CellSink for JobCellSink` 加 `on_phase` 写 `phase`；dispatch 尾段前调 `on_phase(Finalizing)`；`run_job` 起手写 `phase=Collecting` |
| `src/main.rs` | `Cmd::Status` 增 `--workspaces [substr]` 参数（`Option<String>`, `num_args=0..=1`, `default_missing_value=""`）+ 全列表改 JSON 数组、删 `render_job_list` + 其测试；后台建 job 时写 `st.workspace = canonical(cwd)`；`Cmd::Result` doc 补终态规则；`Cmd::Review` 删 `wait` clap 字段 + `wait: _wait` 解构 |
| `plugins/breakpoint/commands/*.md`、codex skills | 若解析 `status` 全列表文本 → 改 JSON 数组（plan 内 grep 定位，无解析则不动） |

## 10. 非目标

- 诚实 `--wait --timeout`（B 方案，§7① defer）。
- `elapsed_ms`（caller 由 started_at 派生）。
- phase 细分子步 / 每 lens 进度。
- `status` 的 `--format text` 人类友好视图（YAGNI；JSON 数组够机读，人可 `jq`）。
