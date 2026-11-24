# M18 发布硬化 · 设计

> 状态: 已交付（2026-07-28 收官，dogfood 三轮收敛 approve；明细 roadmap done.md M18 节）· 2026-07-27 grill 八裁决；WHY 见 `../decisions/2026-07-27-redteam-m18-release-hardening-decisions.md`；plan=`../plans/2026-07-27-redteam-m18-release-hardening-plan.md`（单份四批分节）；里程碑=roadmap M18。

## 1. 目标与纪律

发布（M19）前的安全收尾 + 正确性契约定稿 + 覆盖补网 + polish。纪律：**零优化**——token
轮廓类全部逐出 M20；每项要么修 bug、要么补披露、要么锚测试。

## 2. 批 1 · 安全组（威胁模型 B 收尾）

| # | 项 | 规格 |
|---|---|---|
| 1-1 | **D2 增量注入面中和** | `render_incremental_section`（prompt/mod.rs）：two_layer_manifest 与 claimed 卡 id/file/title/quote 全部过 `neutralize_structure` + 单行化（oneline，对齐 render_precedents 的 E11-Q5 手法）。修复前后行为差异仅限恶意输入。 |
| 1-2 | **denylist 合一** | spec.rs `is_sensitive_spec_path` 与 second_pass.rs `is_needs_denylisted` 同族分叉 → 抽单一真相源（落点新 `cli/deny.rs` 或并入一侧，实施定）；语义=两清单并集（收紧安全侧）；T46④ 补 `.netrc`/`.p12`；顺手清 `.env` 死判（`base==".env"` 被 `ends_with(".env")` 蕴含）。 |
| 1-3 | **T46① 安全 warning 入账** | spec/needs 拒读事件（现仅 eprintln）→ 同时进 `run.warnings`（CI/bot 可见；复用 M17-c 披露通道手法）。 |
| 1-4 | **T46② needs 拒绝披露 DRY** | second_pass 两处拒读 eprintln 重复段收敛单函数。 |
| 1-5 | **git `--` 分隔** | evidence/git.rs 凡 base/range 作位置参数处补 `--`（:49 等已有的核对、branch_files 等缺的补）。低危顺手。 |
| 1-6 | **注入回归 e2e（本批自带）** | 两向量红测法：manifest 路径含 XML 闭标签 → 中和后无越权段；claimed 卡 title/file 含换行 → 单行化无伪造条目。修复前红、修复后绿。 |

## 3. 批 2 · 正确性/契约组

| # | 项 | 规格 |
|---|---|---|
| 2-1 | **exit code 五档**（唯一契约变化） | 0=干净 approve / 1=命令错误（main.rs 全部错误路径 exit(2)→exit(1) 机械替换）/ 2=降级 approve / 3=incomplete（`exit_code` 加臂：verdict==Incomplete→3）/ 10=reject。ship.md 停机与 `--help` 语义表同步随本批 commit。lib 级五档全谱测试锚。 |
| 2-2 | **D3 result --format md** | md 缺失时不再静默回落 JSON：改为即时渲染（result_json 在手时 `render_md` 现算）——两全（ship rN.md 依赖不断、无静默谎报格式）；render 不可行（result 也缺）→ 报错退 1。 |
| 2-3 | **refute conf_floor 门** | `refute_targets(findings, precedents, block, floor)`：孤证 blocking 分支加 `f.confidence >= floor`；判例预筛分支不门（decisions Q4）。三测锚（低置信孤证不进/高置信进/判例命中低置信仍进）。 |
| 2-4 | **T48 source_snippet 行契约** | verify.rs 证据段：用 line_start/end 切行窗（±上下文若干行，实施定）替代 ≤64KB 整文件；行号越界（谎报）→ clamp 到文件实际范围并注记。 |
| 2-5 | **--conf-floor 值域** | main.rs clap 校验 0.0..=1.0，越界报错退 1。 |
| 2-6 | **T25 整链失败 tokens** | orchestrate 候补链全灭路径：失败 CellRecord 回填累计 usage（真实烧掉的 input 入账）。 |
| 2-7 | **T26 setup 自动 probe** | model setup 收尾跑该 provider probe：失败警告但仍落盘 + 提示 `model check`。 |
| 2-8 | **resume 披露** | run_resume 差集补跑 `let _= append_partial` → `if let Err` eprintln（前台 stderr 可见，最后一处 let _= 清零）。 |

## 4. 批 3 · e2e 扩充（后置批 2）

| # | 项 | 规格 |
|---|---|---|
| 3-1 | md 渲染 e2e | result --format md 五段模板 + D3 修后行为（md 缺→现算渲染）。 |
| 3-2 | 台账注入 e2e | dismiss add → 同 id 再评审 → previously_dismissed 标记 + 判例进 refute。 |
| 3-3 | 退出码链 e2e | 1（坏配置）/ 2（fake registry 插必败 provider）/ 3（partial 场景，构造手法批内自裁）；0/10 既有覆盖核对。 |
| 3-4 | **T47③ 真 panic 全链路** | worker 路径 env 钩子 `REDTEAM_TEST_PANIC`（设即 panic!，fan-out 前触发，无需模型；e2e 回归资产永久保留，不进 --help/prompt 面）。真 spawn 二进制 → 断言 Failed + error 含 "panic" + 封盘不变式。 |
| 3-5 | is_same_bug 同义非全等补例 | lib 级（M17-a 记债）。 |

## 5. 批 4 · polish（随批 2 搭车或独立半天）

Duration 秒级显示（<1min 显秒）· TwoStage 语义注释（main.rs --tools 文档串与 config 注释对齐现实）· tools read_file 面收口（read_file/read_files 仪器字段语义注释）· `<ID>` help 参数描述（dismiss/resolve show 两处对称补）· validate_setup 错误文案与 serde alias 双份收敛 · main.rs dispatch 抽取（**可选**，评审看着办）。

## 6. 契约冻结面

- 变化白名单：exit code 五档（2-1）、D3 行为（2-2）、恶意输入下的注入中和（1-1）、失败 usage 入账（2-6）——全部修 bug/补披露。
- 零变化：report schema v3、proj.db DDL、review 主 prompt 轮廓（正常输入；D2 中和只动恶意输入）、review 默认行为。
  注：T48 改的是 refute 子阶段证据窗口（≤64KB 整文件→±60 行窗），非 review 主 prompt——属 §2 表 2-4 授权的正确性修复，不在「主 prompt 轮廓」冻结范围（批2 终审 M2 消歧）。

## 7. 验收

每批：SDD 门（全绿+clippy 零新增+make verify）+ 隔离评审；批 1/2 opus 终审、批 3/4 sonnet 合一。批 1 红测法门；批 2 契约同步不留窗口期。**收官门：多模型 live dogfood**（审 M17+M18 累计 diff）。

## 8. 范围闭集

§2-§5 即全部。排除（decisions Q8）：轮廓四件+连接计数仪器+T49/T50/T27-余→M20；T35 等 Kimi；T06/T11/README→M19。
