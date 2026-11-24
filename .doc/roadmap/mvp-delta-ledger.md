# redteam · MVP delta ledger（跨里程碑滚动）

> 账本区第三联（与 `summary.md` 里程碑账、`../TODO/` 零散债并列）。

> **M15 收尾总注记（2026-07-14）**：S2-b 段全部条目随 **SCIP/1 壳安乐死（E8-E）** 失去承接对象——引擎拆除归 M16，各条目视为关账（复活时随代码从 git 捞回账目）。S1 段条目中与 1 壳/引用点相关者同此；预算类（#1/#3）已被批 1 `pack_budget_bytes` 常数查表版承接（余项=T27）。M15 四批自身的 MVP 缓项已在 design §5/§6 与 followup M16 段记录（L4 拆缝后置/骨架语义更正/needs 双条目有界重复/evidence clamp 一行修等），不在本账重复。
> **一句话**：记录「**完整设计 → MVP 实施路径所缓的一切**」，逐项留痕，防复盘/A-B 断线。

## 这本账是什么（2026-07-09 独立化）

用户按 **MVP** 规划实施——**「最小实现」只是 S1 那一档的 MVP 路径/方式**，不是账本本体。
泛化后本体 = **MVP delta**：每个里程碑的 MVP 都从完整设计里**缓掉广度/精度/维护层**，
每一处缓都**必须上账**（记 完整本会含什么 / 实取 / 差异 / 缓去 / 为何），否则规划实施阶段
丢失的信息后续无从查找。**正确性门（独立性/KPI/对抗内核）不缓、不上本账**。

**纪律**（每 Stage 的 design 放一节「§差异账」指回本账；本账是单项追踪的权威）：

| 列 | 含义 |
|---|---|
| 完整 | 权威架构设计里该件的完整形态 |
| 实取 | MVP 实际做的窄路径 |
| 缓去 | 去向：`Tn`（TODO 单项）/ `S2-c`·`S3`·`S4`（后续 Stage）/ `分支E`（专判）/ **A-B**（可控实验候选，非债） |
| 为何 | 砍它的判据（ROI/耦合/爆炸风险/对抗安全方向） |

---

## M15-S1（regex 地板 pack，已交付 ✅ 合 dev b6aabed）

权威差异全貌见 `../design/2026-07-07-redteam-context-management-s1-design.md` §6；单项：

| # | 件 | 完整 | 实取 | 缓去 | 为何 |
|---|---|---|---|---|---|
| 1 | per-model 预算 | models.dev 策展表 + 真 tokenizer 校准（bytes/token D7） | flat 常量 64KB、model-invariant | **T27 / S2-c** | 接缝是 budget 参数、S2-c per-model 各调；S1 先证机制 |
| 2 | pack 语义包裹/分槽 | 独立 `<pack>` 标签 + diff 物理并入 | 复用 `<dossier>` 标签 | **T28** | 实测 `<dossier>` 误导才做，可能永不需要 |
| 3 | 0 壳小文件全文 | 小文件给全文（含 imports/type） | 只给改动符号外围体 | **T29** | 打小文件残余 tool_use，痛点驱动 |
| 4 | 省略清单 cap | 分组/提 cap | 封顶 ~200 + 计数兜底 | **T30** | hiapi 规模频触时 |
| 5 | 省略清单 prompt 指令 | 全局 prompt 层指令 | 只内联提示 | **T31** | 实测模型无视内联才加 |
| 6 | 纯新增零 caller 符号 | body==diff 时跳 0 壳（免冗余） | 照填 0 壳 | **T32** | 撤密度门副作用；小优化 |
| 7 | 小抛光 | build_pack 混合语言注释 + 端到端断言 + null 臂意图注释 | — | **T33** | 终审 Minor |

---

## M15-S2-b（双树+契约三桶，grill 2026-07-09 锁 → 待落 plan）

MVP 边界见 `../design/2026-07-09-redteam-context-management-s2b-design.md` §0；单项：

| # | 件 | 完整（架构 design/decisions） | 实取（S2-b MVP） | 缓去 | 为何 |
|---|---|---|---|---|---|
| 1 | 双树索引 | HEAD 树 SHA 缓存 + 工作树各一份，diff 得旧签名 + 旧调用图（删除符号旧 callers）（D5/D13） | **单树**（只索工作树）；旧签名靠捕获路径近似、删除符号退 regex-A | **S2-c** | 删除符号 regex-A 兜底已覆盖（S2-a 单树已认）；第二棵树纯 scope 膨胀 |
| 2 | 精确契约分类 | 两树同 symbol 的 `signature_documentation.text` 比字节（A/B 精确） | 「捕获路径」启发（`+`行匹配 def 正则=A；只经外围 def=B） | **S2-c**（随双树） | 单树下 SCIP 归一化（`Vec<…, Global>`）匹配不上文本旧签名 → sig-串-diff 出局 |
| 3 | 多行签名精度 | 块扫描签名跨度 `[def行..{]`，任一改动行落内即 A | fn 名行匹配 def 正则才判 A（多行签名只改参数行 → 误判 B） | **T40** | under-expansion 天花板；D4 下 B 仍展 1 跳 caller=优雅降级非漏 |
| 4 | SCIP 缓存基建 | `.redteam/index`（温热）+`cache`（SHA 归档 LRU）+ 自隐身 gitignore（D14） | tempfile 即弃、**每跑重索引**（热 4.6s） | **S2-c / S4** | review 分钟级、4.6s 可忽略；缓存基建独立大件 |
| 5 | caller 剪枝 | 可证安全 caller 踢（返回类型变踢丢弃返回值者、可见性拓宽踢现有 caller）（D6） | 不剪、全留（cap 10 + 预算封顶兜底） | **S2-c** | 只省特定契约变的几个 caller、非核心 |
| 6 | 传递深度 + 多样性排序 | A 桶传递闭包（2+跳）+ 溢出按覆盖多样性排序（D6/D9） | **1 跳**；A/B 区别落 **caller 厚度**（A 给完整体、B 给片段） | **S2-c** / **A-B** | 传递=高 fan-in 爆炸源 + 与排序耦合（传递才溢出）；A/B 候选=若 tool_use 仍高试 A 开 2 跳 |
| 7 | callee 签名注入 | —（D9「callee 要契约」那半） | **入 MVP**：改动 def 内 callee 引用 → 只注入签名（1 跳不递归） | **A-B** | 打 callee-search tool_use；A/B 候选=on/off 隔离其边际（vs caller-recall 杠杆） |
| 8 | callee 外部 dep | 索引里免费拿外部 dep 契约（E3） | 注入但打 `[dep]` 标、溢出先降级进省略清单 | — | 本仓改动优先；简化非缓 |
| 9 | 穷尽性 flag | enum 加变体→标有 catch-all 的 match；trait 加方法→标所有 impl（D7） | 不做 | **S2-c** | radius 上一个 AST 标注、独立小件 |
| 10 | build-gate | 索引构建=事实 build gate（编译不过⟹索引失败）（D8） | 不做（索引失败纯触发 regex 降级、不产 finding） | **分支E** | E4 实测 RA 容错（重复符号仍 exit 0 出全索引）→ 退出码当 gate 的 premise 反证；须 `cargo check` 另判 |
| 11 | per-model 预算分叉 + 候补退役 | per-model bundling（每模型一套 cell）+ D20 候补退役 | flat 预算（承 S1）、候补留 | **S2-c** | S1 已 flat 跑通、分叉是 S2-c 的活 |
| 12 | C 装饰桶 skip | 注释/空白 hunk（剥 `strip_rust_noncode` 后无实体）→ radius≈0 skip（D4/design §3） | 分桶只 {A,B}；注释改动 → 归 B（1 跳 caller、安全过展） | **TODO/S2-c** | 精确 C-skip 要 per-hunk 内容判、撞符号/hunk 粒度；B 过展无害=纯优化非正确性 |

> **2026-07-09 E7 修订**：残余读解剖后 **S2-c 死刑**——上表 S2-b 段所有「缓去 S2-c」项（#1 双树/#2 精确分类/#5 剪枝/#6 传递+排序/#9 穷尽性/#11 per-model 分叉）**不再有承接 Stage**，降级为「个别件他线需要才单独复活」（D7 穷尽性=质量件、D23 规划器=架构件）。#4 缓存基建随 scip park 同降。

---

## M15-S2-b'（三错配修正·第一批，立项 2026-07-09 E7 → 实施中）

设计 `../design/2026-07-09-redteam-context-management-s2b-prime-design.md` §1；单项：

| # | 件 | 完整（解剖指出的全量修正） | 实取（第一批） | 缓去 | 为何 |
|---|---|---|---|---|---|
| 1 | 类型完整定义块注入 | callee 对 struct/enum 给整块定义（fields/variants，`enclosing_def_start`+`block_end` 抠） | 不做（callee 仍签名行） | **解剖门控** | ①落地后重解剖，「search struct」类残余仍在再做 |
| 2 | 变更集全景 | cell 附其他 bundle 的 diff 一览（修范围错配） | 不做 | **grill 门控** | 与 D12「cell 独立不合并」张力；①②后跨 bundle 读仍是大头才值得 |
| 3 | per-model 真预算 | models.dev 表 + per-model 各算（T27 完整版） | flat 帽 64K→256K → **批 1（07-13）常数查表 `pack_budget_bytes`**（config 声明窗口/128K 保守缺省/多模型 min/3B-token 固定估算） | **T27 余项** | 余项收窄为策展表+tokenizer 校准；固定估算不准再正名 |
| 4 | refute 上下文完整度 | refute 附整 bundle pack | finding.file ±60 行段（~24KB 帽） | — | 段级先行（refute 靶点明确、全 pack 稀释）；解剖验证不足再扩 |

**图例**：`Tn`=TODO 单项 · `S2-c/S3/S4`=后续 Stage · `分支E`=build-gate 专判 · **A-B**=可控实验候选（非债，有 tool_use 假设待测）。
