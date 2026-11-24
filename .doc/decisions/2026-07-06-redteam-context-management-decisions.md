# redteam 上下文管理·决策记录（ADR 日志）

> 状态: 锁定 · 架构设计 `../design/2026-07-06-redteam-context-management-design.md` 的**决策过程/权衡/被否/证据**。
> 单份日志、分节记（非一决策一文件）。design 各节以 〔Dn〕 反引本表。里程碑 roadmap M15。
> 来源: 2026-07-06/07 grilling（compact 数轮）。格式: 决策 / 备选·权衡 / 被否。

## D1 · 本质重定为「上下文管理」，KPI=残余 tool_use→0
- **决策**：把「慢且贵」重定为症状，根因=每 cell 上下文不对（多=噪音稀释、少=补读多轮）；北极星取可证伪的**残余 tool_use（模型主动 read）→0**。
- **权衡**：直接优化「慢/贵/质量」各自为战、无统一杠杆；「上下文对不对」是三者共同上游，且 read 调用数可仪器化（384fda6 已量）、falsifiable。
- **被否**：以「成本」为北极星——已 park（D9 卷宗成本不可测、两跑并发污染）；成本是上下文管理的下游。

## D2 · 意图（commit/PR 描述）排除，不注入 radius/裁决
- **决策**：变更类型纯从 diff+索引机械导出，作者的话一概不用。
- **理由**：① 正确性无法保证；② 锚定作者声称的 X→确认偏误、漏「顺手破了 Y」；③ operating_stance 已写「give no credit for good intent」。
- **被否**：从人类协作评审抄「意图优先」——那是**信任姿态**，和对抗内核打架。元教训：抄人类评审的**取上下文机制**、不抄**信任姿态**。（此为一次明确纠偏。）

## D3 · radius 按变更类型展开、非均匀按距离；只在可证不可能处剪
- **决策**：签名/类型/契约变→狠展 caller；删除→展旧 caller；同签名 body 改→默认行为可能变、展；新增→caller 查接线/callee 查用对；纯注释·空白/真 private 无 caller→剪。
- **权衡**：均匀按距离展=浪费远壳、漏近壳高风险；按类型=精力∝风险（比例分诊）。
- **dead code**：不 skip——「无 in-repo caller」可能是未接线/孤儿/breaking public、或反射·宏索引盲区 → 待裁信号交模型，非确定性 dead。

## D4 · 契约变更 = 签名面 delta，三桶机械分类
- **决策**：A 签名面变（参数/返回/泛型/可见性/限定符/struct 字段/enum 变体/trait 方法）传递展；B 仅 body 变展直接 caller 一跳；C 装饰 radius≈0。三桶=深度调速档非门。
- **边界**：签名面（机械、索引白给）可判；**行为安全性不可机械、且不该让它机械**——让分类器猜「只是重构」=把善意走私进对抗内核。
- **被否**：让分类器判 body 改是否安全——范畴错误（那是评审产出、非分类输入）。

## D5 · 旧签名 + 删除符号 = 双树索引（HEAD 树 + 工作树）
- **决策**：索引 HEAD 树与工作树各一份，diff 符号表得旧签名；旧树天然产出调用图 → 解「删除符号 radius=旧 callers」（删的符号不在新树）。旧签名与删除符号=同一基础设施、一次立项。
- **备选（被否为默认，留作降级）**：hunk 就地解析（从 enclosing def 抠签名）——省一次索引但多行签名跨 hunk 会漏（~90%）；降为无索引档。
- **理由**：双树顺带解 §8.4 删除符号，旧树能力早晚要建，一次到位。

## D6 · caller 集 = 剪枝 + 多样性排序（否「按 breakage 可能性排序」）
- **决策**：先剪枝（可证安全 caller 踢：返回类型变踢丢弃返回值者、可见性拓宽踢现有 caller）、再排序（仅溢出时、按覆盖多样性）。
- **被否**：「按 breakage 可能性排序」——高 fan-in 溢出时众 caller 崩法相同、给不出有用全序；真杠杆是**多样性覆盖**（每 distinct 调用形态取代表）。剪枝喂「可证」、多样性喂「排序」。
- **ponytail**：排序器只在预算溢出才建（低 fan-in 全装下）；剪枝永远跑。

## D7 · 枚举/trait 穷尽性 flag，靶 = 编译器被 fallback 击穿的站点
- **决策**：enum 加变体→flag 有 catch-all `_` 臂的 match（新变体静默落兜底）；trait 加方法带 default→flag 所有 impl（静默继承）。硬报错站点跳过。产物=待裁信号（带标注站点清单）。
- **理由**：编译器本身就是穷尽性检查器，硬报错站点 dev 漏不了、flag=噪音；差异化价值只在**编译器沉默处**。
- **定性**：非独立子系统——match/impl 站点 caller-radius 已拉进，④ 只是其上一个 AST 判定。

## D8 · 索引构建 = 事实 build gate；穷尽性 skip 由索引路径门控
- **背景/隐忧**（用户提）：「skip 硬报错站点」暗含「编译器跑过了」，但 review 可能在 build 前。
- **决策**：scip-* 生成索引要跑真类型分析 → 工作树编译不过⟹索引失败=确定性「diff 不 build」信号，不另挂 `cargo build`。故 skip 门控：**索引路径**（索引绿⟹编译过）skip 硬报错站点；**降级/regex 路径**（无编译器）不 skip、连硬报错一起 flag。
- **裂缝**：sound 性依赖索引对工作树新鲜 → 挂 D13 staleness。

## D9 · 方向性 = 结构不对称，不设标量权重
- **决策**：caller 要 body/传递/贵、callee 要契约签名/1 跳/廉（且多半已在包）。预算天然流向 caller、溢出排序只在 caller 侧。方向决定深度(body vs 签名)+传递性，成本随之。
- **被否**：标量「方向权重」旋钮——把结构性差异压成假旋钮。守线：callee 契约不足→是 finding（依赖未指定行为），非展进 callee body。

## D10 · symbol-reference radius 超集掉 call-graph；不做 taint
- **决策**：SCIP occurrence 是通用符号引用（const 读/type 用/field 访问），非只 call 边。故非函数变更零新引擎：const/type/field→reference radius、import swap→签名 delta+in-file 使用点、config/string→regex-B、注释→近 skip。
- **被否**：为非函数变更建独立「数据流 radius」引擎；污点/taint 分析（YAGNI，只给直接读者、传播交模型）。盲区（宏内 const/反射 type）= 索引盲区、走 regex-B/降级。

## D11 · test 处置 = 改了没改分层（非二元入/排）
- **决策**：改动 diff 里的 test=一等材料、**与被测主体同 bundle 共审**（显式亲和，不靠目录碰巧）；未改引用 test=reference-radius、caller-rank 最低、预算自动剪。
- **理由**：最毒对抗模式=代码与测试同 diff 一起改（削断言迁就坏改动），reviewer 须同屏看两者；「契约 vs 噪音」两者都是、靠 rank 分层。

## D12 · 跨 cell 重叠 = 接受打包冗余，不去重不合并
- **决策**：打包字节不去重（独立 LLM context 要自足、去了就瞎）；引用计算已被索引缓存（非 per-bundle 走仓、消 T01① 债）；不合并 bundle（合并→巨型稀释爆预算）；finding 去重下游正交（quote/file 锚定）。
- **量级论证（用户要求先估）**：拆跨模型复制(M×，设计本身非浪费) vs 模型内跨 bundle 重叠（真靶）；后者四刀夹死——① 只在大变更 K≥2、② 共享 helper 多是 callee 打签名不打 body、③ 高 fan-in⟹小体积、④ 大共享正文被省略清单接。**悲观现实上限≈十几个 %、以签名为主、常见为 0**。

## D13 · 索引 staleness = 双树拆解，工作树增量现推、永不 stale
- **决策**：HEAD 树索引 SHA 键不可变→永久缓存；工作树索引=取缓存 HEAD base+脏文件增量、审起始现推、**绝不跨 run 缓存**→总从当前快照派生。消费一致性边界=审起始快照（审中编辑用户重跑）。
- **理由**：凭「只补温 base 的小 diff」**不重蹈 live-LSP 冷启动**（从不冷）；焊死 D8 build-gate（反映真被审树）+ 对齐（索引端点=diff 端点）。

## D14 · 存储布局 index/+cache/ + 自隐身 gitignore
- **决策**：`.redteam/index/`(温热工作态恒 ignore) + `.redteam/cache/`(SHA 归档、LRU 驱逐旧 SHA=唯一清理、非每 review 擦)。自生成 `.redteam/.gitignore` 用**锚定 `/*`** + 注释 `#!/cache/` opt-in。
- **footgun（用户半记、我修正）**：裸 `*` 每层匹配、`!cache/` 拉不进 cache 内容（git 官方点名）；锚定 `/*` 只忽略顶层项 → `!/cache/` 自动含全内容、无需 `!cache/**`。
- **opt-in**：`!cache/` 让团队共享 SHA cache；提醒代价（32M blob 提交进 git 通常不划算、CI artifact 更好）→ 默认关。

## D15 · provisioning = 无强制 init（review 前置自省 + 降级 + 可选升级）
- **决策**（用户主张「大胆点」）：YAGNI 掉独立 init 步骤。按**副作用分层**：纯/本地索引构建静默折进 review 前置；改系统安装绝不阻塞——indexer 缺→regex-A 降级跑完+一行提示，安装降成可选 consented `setup-index`。
- **连带**：regex-A 从异国兜底升为「未升级前载重默认」；T26（model 可用性）同哲学 at-use 检查。
- **被否**：强制 init 仪式（目标就是评审）；review 静默装工具链（两坑：首审延迟尖峰 + 未经同意改系统）。

## D16 · 获取 = 工具链永远借、发射器借或侧载；借=批量调 binary
- **决策**：拆**工具链/类型检查器**（永远借、repo-local 优先、绝不侧载——没法给任意仓装构建环境）vs **SCIP 发射器**（在则借、不在按需侧载、不 bundle）。「借」=探测+批量调已装 binary（`rust-analyzer scip`），**非 attach 活 LSP**、不需宿主 API（保持宿主无关）。
- **被否**：侧载工具链（荒谬）；bundle 发射器进二进制（bloat）。

## D17 · 发射器选择 = 策展权威表（机械准则，非个人喜好）
- **决策**：准则序 0 能离线产静态 SCIP（排除活 LSP server 如 gopls）/1 全类型检查保真/2 权威在维护（Sourcegraph scip-* + rust-analyzer 内建）；策展表烤进二进制 + config 覆盖（喜好合法出口）+ regex-A fallback；可用性仅 tie-break。
- **Q1（跨语言 indexer？）**：语义层**不可约按语言**（物理：一引擎没法类型检查多语言；scip-java 多语言仅限 JVM 内）→ per-language 表被本质逼出、非任选。多语言单工具只在**语法层**（scip-ctags/tree-sitter，定义为主、弱于 caller-radius）=地板升级选项、建地板时再议。
- **Q2（有 SCIP 还借 LSP？）**：不。静态路径活 LSP server 零角色；只有一样要获取=每语言 SCIP 发射器（个别碰巧是 LSP 如 rust-analyzer，只调其批量 scip 子命令）。「借 LSP」是用词混淆、作废。
- **被否**：个人喜好选（如 Go 硬选 gopls）——gopls 被准则 0 排除（是 server 非批量发射器），要它走 config override 自负。

## D18 · 静态索引 > live LSP
- **决策**：用 scip-* 产**索引文件**离线查。
- **理由**：确定性、可缓存、无「服务器启动即全仓索引」的冷启动延迟；oracle=rust-analyzer LSIF 已知量。放弃早先「tree-sitter 不立项」旧决策。

## D19 · 侧载非内置（否 bundle / 否硬要索引）
- **决策**：发射器侧载获取、二进制不塞 grammar/indexer。
- **被否**：打包 indexer=bloat；硬要索引才能 review=破裸终端可用。

## D20 · fallback YAGNI
- **决策**：每模型独立审自己分区、模型挂了记 failed_models、用户自保可用性。
- **理由**：候补链与 per-model bundling 不兼容、顺带松绑 T05；跨模型 agreement 靠 quote/file 锚定不受影响。

## D21 · 索引单元 = 编译项目（tsconfig / go module），非「仓」
- **决策**（多语言仓实测暴露）：polyglot monorepo=N 编译单元；「索引工作树」=索引 diff 受影响的项目（按项目 scoping 增量）；未装依赖项目（如 electron/ 无 node_modules）→regex-A。

## D22 · 缝隙泛化：跨语言 → 跨索引单元
- **决策**：两独立 tsconfig 项目无共享 SCIP 符号图（不同编译单元），与 Go↔TS 同构。regex-B 触发从「跨语言」推广成「跨索引单元」——字符串耦合跨任何索引边界（同语言 web↔workers 或跨语言）都是它的活。

## D23 · fan-out 融合：blast-radius bundling = 规划半
- **决策**：`(changes, models, budgets, 索引)→cells`；老「bundle 层 + orchestrate×模型」两阶段塌成一个 per-model 规划器，执行层（并发/重试/收集）不变。bundling 算法=dir/import 关联种子 + 预算贪心；单文件溢出独占 bundle。

## D24 · pack 粒度 + 精确省略清单（砍 ±3 片段层）
- **决策**：符号/enclosing 粒度（非整文件）；被省 radius 的 file:line 纯坐标附 pack 尾 → 模型定点 read_files。
- **被否**：卷宗旧的 ±3 行片段层——被省略清单 + read_files 定点读取代，梯度收敛三层（packed 全函数→省略清单→定点读）。

## D25 · 独立性协同门（D9 红旗回灌）· 门锁 / 解开放
- **背景（D9 实测，2026-07-06 两跑）**：给所有模型灌同一份卷宗事实 → ON 臂可复现冒 agreement=3（三模型共识）finding、OFF 臂全 agreement=1 → 判断趋同、侵蚀「独立多模型对抗」立身之本。**适用整个 blast-radius 引擎**——per-model bundling 只分预算、同 bundle 仍同事实，没自动解掉。
- **决策（门，锁）**：独立性作 **co-equal 退出门**——每个打包共享上下文的 stage（S1 起）判据 = 残余 tool_use↓ **且** agreement 分布不向假共识收窄；S0 建**双基线**（tool_use + agreement 分布）。
- **决策（解，开放）**：保独立性的机制（每模型事实扰动 / 来源标注让模型仍独立判 / 部分 radius per model）**不现在设计**——D9 信号小样本、S1 粗 regex radius 未必像精炼卷宗那样趋同；**S1 装门即度量实验**，门跳了才 grill 解。零 baking-in 风险（保全机制叠加在 packer 上、不返工）。
- **关联**：卷宗成本线闭（D9 成本门 ❌，见 E-D9）；本红旗是卷宗从「成本优化」重定位到 M15 的核心遗产。

---

## E-D9 · 卷宗 D9 多模型 A/B（2026-07-06，两跑均并发污染、部分完成）
- **成本门 ❌ 不成立/测不准**：聚合符号跨两跑翻转（跑1 +23.7% / 跑2 −13.8%）；run-to-run 方差 ±50%/cell、连 turn 数相同 cell 都反号；强 model-dependent（DeepSeek −10~15% / Kimi +179% / VibeCoding 噪声）。旧「DeepSeek −38~50%」= 乐观+model-specific。**成本线查到头、不再测**。
- **独立性 ⚠️ 可复现弱回退** → D25。**四判据 ➖ 不定论**（部分完成）。
- **方法论教训**：双臂须串行（并发触发 VibeCodingFast 403 / Kimi timeout）；完成数不等时 run 级求和无意义、须匹配 cell；n=1-2 测不出卷宗成本（方差淹没信号）。

---

## 实测证据（2026-07-07，支撑 D13/D16/D17/D21）

**E1 · 本仓 redteam（Rust 40 文件 ~12K LOC）**：借用探测全绿（cargo/rustc 1.96.0 全局、rust-analyzer 1.96.0 内建 `scip` 子命令 + rust-src component，**零侧载零安装**）。`rust-analyzer scip .` = **11.96s / 3.2M / 15985 符号**、exit 0；已知符号及其**引用+test 引用**入索引（`dossier/truncate_line().` def+ref occurrence 齐全、旁挂 `dossier/tests/…()` 兑现 test-as-caller）。12s=全量冷索引、每 SHA 一次；review 时只付亚秒增量。

**E2 · hiapi/backend 借用探测（Go 1021 + TS 75/~8 tsconfig）**：Go `go` 1.26.4 ✅ 但 scip-go/gopls 缺；TS **全局 tsc 缺、repo-local tsc 有**、scip-typescript 全缺、electron/ 无 node_modules。验证：① **repo-local 工具链优先序载重**（只探全局会误判）；② **「发射器缺」是常态**（scip-* 不随工具链分发）→ D15 侧载按需 + regex-A 载重是真仓正常路径。

**E3 · 侧载安装 + 三仓计时**：`scip-typescript` npm ✅ 0.4.0；`scip-go` `@latest` **旧路径失败**——上游从 `sourcegraph/scip-go` 迁 `scip-code/scip-go`（**侧载配方会漂移**，佐证 config 覆盖+版本探测+fallback），换新路径装成 0.2.7。计时：Rust redteam **11.96s/3.2M**；Go hiapi module **9.20s/32M**（1021 文件·137 包·231K 符号、CPU 41s 重并行）；TS workers/producer **0.66s/166K**。五结论：冷全量全快·per-review 增量亚秒（D13 延迟赌注成立）/ 索引体积随 dep 涨→存储 scaling 因子（D14 LRU 载重）/ per-project 极快（D21）/ 侧载配方漂移（D17）/ **外部 dep 契约在索引里**（stdlib bufio、@cloudflare/workers-types）→ callee 契约查（D9）从索引免费拿。

**E4 · rust-analyzer scip 容错性实测（2026-07-08，S2-a grill 中）→ 反证 D8 build-gate premise**：重跑 `rust-analyzer scip .`（热态 **4.6s**，对 E1 冷 12s → 增量赌注侧证，D13 成立）；stderr 报 `Duplicate symbol: …crate/`（main.rs+lib.rs 双 crate root），**却仍 exit 0 出完整 3.4M 索引**。含义：rust-analyzer 设计即**容错**——编译/解析问题照出（部分）索引、不 hard-fail。故 D8「工作树编译不过⟹索引失败=确定性『diff 不 build』信号」premise 对 rust-analyzer **不成立**（不能白嫖 scip 退出码当 build-gate）。**后果**：build-gate 语义须 `cargo check` 另做（非 RA scip 退出码），留分支 E / S2-later 专判；**S2-a 明确不实现 build-gate**（索引失败纯触发 regex 降级、不产 finding，见 s2a-design §3）。D8 其余（穷尽性 skip 由索引路径门控）待随 build-gate 重判一并复核。

**E5 · S2-a 假阳率探针（2026-07-08，零 LLM 成本）→ 撤 S2-a 独立里程碑、改交付廉价过滤 B**：静态比 redteam 仓 regex `\b名\b` 引用 vs SCIP references（`scip` crate dump，**Task 0 顺验✅ 免 protoc**），量精度切法价值。**结果=双峰极化**：fn/type 改动符号加权假阳率 38.9%，但**中位 0%、57% 符号零假阳、仅 10% >50%**；假阳集中「与 stdlib 同名（`.collect()`/`.write()`）+ 域名词作字段/变量（finding/bundle/provider）」，**真杠杆=1 壳 cap 10 名额纯度、与 tool_use 正交**（印证 D25-邻 Q5 指标错配）。**决策（撤+替代）**：① **撤 S2-a**（SCIP 引用精度独立里程碑）——精度切法单独 ROI 不足撑一个 SCIP 里程碑（57% 零增益）；② **交付廉价过滤 B**：给 regex 地板加**注释/字符串词法剥离**（`dossier::strip_rust_noncode`），**−37% 假阳、零成本零风险**、永久受益兜底路径（=T37 首兑现；顺带删死代码 is_doc/[doc]，注释命中改「剔除」非「标注」）；③ **语义引用精度并入 S2-b**（跟契约分类一次装 SCIP 白捡剩余 63% 假阳）。**被否**：(A) S2-a 照做（57% 零增益不值 5s+复杂度）；(C) 直上 S2-b 跳 B（B 零成本即时价值不该弃）——选 B 兼得即时价值 + SCIP 省给其不可替代的契约分类。**纠偏**：B 砍的非 stdlib-method（那需剥 `.method`、误伤本仓真方法），是注释/字符串同名（redteam 评审工具、注释/prompt 串满域名词，意外大）。多语言词法→T38。**S2-a 设计文档标作废、作过程存档**（记探针如何写 plan 前推翻 Q1）。

---

## E6 · S2-b grill（2026-07-09，`grill-with-docs`）→ 单树 MVP + 廉价契约分类器 + callee 注入

S2-a 撤销后 SCIP 一行没进二进制 → S2-b 是**第一个真接 SCIP 的里程碑**（substrate + 分类器 + 双树 + 方向性全压这档）。grill 6 决策（design `2026-07-09-…-s2b-design.md` 为 spec，本节记 WHY）。

- **substrate 探针（grill 前置，重塑领域模型）**：扩 `scip` crate 探针查 `SymbolInformation` → **rust-analyzer 给每个 symbol 填 `signature_documentation.text` = 归一化完整签名串**（fn=`pub fn reference_points(syms: &[ChangedSym], …) -> Vec<SymRefs>`；struct 字段/enum 变体是**各自独立 symbol**，`SymRefs#name.`→`pub name: String`）。`documentation`（doc 注释）本仓为空、不可靠；锚 `signature_documentation.text`。含义：契约分类器**不必自解析 Rust 签名**、签名面天然 per-symbol（D4 的字段/变体/方法不是特例）。

- **Q1 MVP 边界**：核心闭环 = 分类器→深度分档→SCIP 填，单语言 Rust、**单树**。缓双树/缓存/剪枝/传递+排序/穷尽性/build-gate/per-model 分叉 → S2-c/S4/分支E（差异账进 `mvp-delta-ledger.md`）。**争议=单树缓双树**（把锁定决策 D5 的**实现**缓到下一档、非推翻）；用户认。

- **Q2 分类器信号源**：**sig-串-diff 出局**——单树下新签名出自 SCIP 归一化器（`Vec<FileChange, Global>` 补了 `Global` 分配器参数），文本抽取的旧签名**永远匹配不上**；两签名须同归一化器 = 须双树，已缓。改用**捕获路径**（`symbols_in_file` 已算好的信号）：`+`行匹配 def 正则=签名行进 diff=A、只经外围 def=B、剥注释/空白后无实体=C。**副产**：分类=纯新树 regex，与 SCIP 解耦（SCIP 只在 radius 展开登场）。天花板=多行签名只改参数行误判 B（→T40，B 仍展 1 跳=优雅降级非漏）。

- **Q3 radius 模型（核心权衡）**：A/B 区别落**caller 厚度**（A=完整外围体、B=片段）**非传递深度**。**被否「传递深度 A=2跳」**：传递闭包=高 fan-in 反向图爆炸源、且与多样性排序耦合（传递才溢出、排序才有意义）→ 两者一起推 S2-c。厚度切法既让分类器有牙、honor D9「caller 要 body」、又躲爆炸，且 KPI 上「A caller 给全体」比「2 跳片段」更直接砍 read_files。**callee 签名注入判为 MVP 核心**（新件但廉价、是 D9「callee 要契约」那半、直接打「search 被调函数签名」的 tool_use；纯 caller 侧≈warmed-over S2-a）。用户要求两条备选**入差异账供 A/B**（Q3-a 传递深度、Q3-b callee on/off）。

- **Q4 连接键**：按位置对（承 S2-a）+ 符号级 regex 兜底（查无 Definition occurrence 即退，打无声降级计数）。新边角：**callee 注入外部 dep 签名**（索引免费给，E3）——注入但打 `[dep]` 标、不占近壳优先级、溢出先降级。

- **Q5 生命周期**：单树、tempfile 即弃、**无缓存**（每跑重索引，热 4.6s 可忽略；缓存基建缓 S2-c/S4）、RA 缺则降级、三值枚举（regex 默认/scip/scip-strict）。**默认不翻**（scip opt-in 到验收过，照 S1 纪律）。

- **Q6 验收**：尺子翻面——S2-a 量精度（假阳 `regex\SCIP`），S2-b 量 **recall**（`SCIP\regex` = 补集）+ 分桶分布 + callee 产量，**静态为主零 LLM**；显著才端到端（`scip-pack` vs S1 `regex-pack` 对**附四**做差、附三 OFF 不重跑、成本纪律）。**特有盯点**：A-caller 全体 + callee 注入**加 input** → input 升 / tool_use 降是预期（赌注本身），端到端须**同时量两者**；D25 独立性门在此更吃紧。

**账本独立化（用户 2026-07-09 定）**：`mvp-vs-minimal-delta-ledger` 从 memory 毕业为 `.doc/roadmap/mvp-delta-ledger.md`——「最小实现」只是 S1 那档的 MVP 路径，泛化后本体 = 跨里程碑 **MVP delta**（完整→MVP 缓项滚动账）。

---

## E7 · 残余读解剖 → S2-b' 立项 + S2-c 死刑 + S3/S4 重定位（2026-07-09）

**背景**：S2-b 交付后 §5 验收两连平（静态 recall≈regex、小端到端 tool_use −2% 淹噪声），用户问「设计层还是实施层」。**方法转向**：不数 tool_use、解剖其内容——仪器 `REDTEAM_TOOL_LOG`（4fe9b9b）记每次调用参数 + `max_concurrency=1` 顺序归属 + 行级 overlap 比对 pack 全文（scratch classify.py/packs_dump）。前置排除：64K vs 1M pack 渲染逐字节同（预算非瓶颈）、两臂真分化（scip 多 251 行独有+callee 段）、5 格中 2 格两臂 pack 全空=天然对照组（TU 波动纯噪声）。

**发现（定案）**：「重读已 pack 内容」≈0——模型理性用 pack、只补缺。残余读四类：①整读改动文件（每格第一动作，pack 0 壳仅占所需 ~20%）②跨 bundle 拼 feature（D12 隔离逼的）③类型完整定义（callee 签名行对 struct 是 `pub struct X` 空壳、无字段；search 18 中 16 词不在 pack）④refute pass 零 pack（14 调用/趟，整个消费者在机制外）。**根因=设计层三错配：粒度（符号级 pack vs 文件级消费）/范围（per-bundle vs per-changeset）/覆盖（refute 无 pack）**。回溯解释全线数据：S1 给 def 体 −8~10%、S2-b 给更准引用行+签名行 ≈0——**模型缺成块原文非行级精度，SCIP 语义精度在回答模型没问的问题**。

- **决策（S2-b' 立项，第一批）**：①0 壳→改动文件全文（预算内、超则回落 def 体；顺带吞 T42 锚定 bug；非支持语言也给全文）②refute prompt 附 finding.file 相关段（±60 行、~24KB 帽）③预算帽 64K→256K（①的前提：cli.rs 单文件 ~90KB>64K；=T27 MVP 半兑现）。**验证=解剖法**（看「整读改动文件」类调用消失与否；不 A/B 计数——N=1 计数噪声 ±40% 已实证，行为类别消失是一跑可见的强信号）。
- **决策（缓，ledger）**：类型定义块注入（①解剖后「search struct」类仍在再做）；变更集全景（与 D12 独立性张力、需 grill；①②后跨 bundle 读仍大头再启）。
- **决策（S2-c 死刑）**：全部件（双树/剪枝/传递+多样性排序/per-model 预算/Cell 规划器分叉）都在**错配的符号级精度轴**上加码——binding constraint 已证不在该轴。个别件若他线需要可单独复活（D7 穷尽性=质量件、D23 规划器=架构件、per-model 预算若 flat 帽不够再正名）。
- **决策（S3/S4 重定位）**：S3 的 **SCIP 语义多语言部分随 scip park 降级**；仍活的=多语言 regex 地板（Go/TS 词法 T38）+ regex-B 跨索引单元缝隙（差异化价值不依赖语义精度）——痛点（hiapi 规模）驱动再启。S4 provisioning 的 setup-index/发射器线随 scip park 降级；降级可观测部分仍有效。
- **被否**：继续在精度轴投资（S2-c 照做）；用 gpt-5.5 紧尺 N≥5 重测 scip（边际信号不值成本）；「给更多同形状信息」路线整体证伪。

## E8 · 无工具默认化 grill 九裁决（2026-07-13，design=2026-07-13-redteam-no-tools-default-design.md）

前情：S2-b''（无工具质量未降+轮次 39→5）+ 粒度 A/B（符号 pack 质量未守住、转正降级档）→ 方案 v0 → grill。

- **A 翻默认顺序**：批 1 直接翻 `two-stage`（弃「验证门前置」）。Why：未发布零外部用户、dogfood 即验证；无工具证据已是全线最厚（2 组 A/B 四跑）；`tools=on` 一键回退风险有界。异语料 smoke 最低配前置进批 1 验收，多模型复验降级为回归监控。
- **B fan-out**：分格交付，单格全审 A/B 臂提前进批 1 验收（~56K/跑，免费级）。Why：一次只动一个变量（S2-b'' 结论硬的方法论根源）；单格臂免费没有理由跳过实测直接赌。判决后才进批 4 收缩。
- **C needs 契约**：非空即重跑（不做 conf 加权聪明判断）/ cap 5 超出截断+warning / 第二趟整体替换第一趟（草稿以「确认或修订」语义随包）/ 路径安全复用 ToolExecutor 既有不变量（safe_join+canonicalize+读帽），不发明第二套。Why：触发率先实测再调；替换语义避免 finding 级合并复杂度；安全面=read_files 现状、M16 一并审计不新增。
- **D 梯子 L1 修正**：changeset scope 兄弟传真 diff（现状空 diff → def 降级不可达+静默消失 bug，cli.rs:385 实证）；L1=兄弟改动 def 体（E7：兄弟改动=feature 其他块、价值密度最高）→ L2=骨架（签名地图防幻觉）；降级空产出必入 omit（禁真空可观测）。
- **E SCIP 生死两层判决树**：S2-b'' B 臂是 scip 跑的——「质量未降」属于「全景+1壳+callee」整包，纯 0 壳从未测过 → 减法实验（REDTEAM_PACK_SHELLS=0 类仪器）进批 1 验收；判据 = Q-B 同套 + **needs 触发率差**（two-stage 世界里 1 壳=needs 预防针）。无差→全线安乐死（降 opt-in，物理删归 M16）；有差→二层定位 regex 1 壳够否（SCIP≈regex recall 已证）vs callee 独有增益。
- **F refute 证据升档**：走文件内梯子（锚点文件全文→骨架→±60 快照兜底=现状不倒退），批 2；验收人工抽查 refute note（样本小、A/B 判据难自动化）。Why：refute=无工具世界唯一质量闸门、finding 数少成本可控。
- **G T43 三层修**：前缀嗅探（strip 包装后头部 REFUTED/UPHELD，复用 classify_refute 前缀语义；不做全文正则防「would be REFUTED if」误判）→ 嗅探不明才重试一次 → 兜底保原 conf（宁假 reject）。Why：错误成本不对称——假 approve 吞真问题=r1 20260703 老事故方向，对抗工具宁严勿松。
- **H doc 模式同批翻转**：doc 已是形态样板（单 cell+全文入 prompt），差距只剩对话工具；needs 机制零新增复用；批 2 带 doc smoke。Why：保留 doc 特例反而分叉 tool_loop/配置面两套行为。
- **I lens 去实体化**（用户历史建议，此次正式落痕；定义经用户 2026-07-13 二次确认为**代码零结构**）：lens 不作为参数/常量/enum 存在于代码——src/lens 模块、rules 参数链、--lenses flag、state/报告字段全退场，攻击面清单内联为 code/doc prompt 模板固有文本段；不参与 pack/fan-out/bundle（现状本就不参与，确认锁死）；注意力引导由 --focus 独家承担；报告 lens 归属丢失代价趋零（用户只关心 bug 真伪非视角归属）。批 4 杂交备选随之简化为「焦点 prompt 变体」。报告 schema 字段删除批次待消费面查证（数据契约，plan 层）。

## E9 · fan-out 收缩判决 + lens 去实体化执行裁定（2026-07-13，批 4 开工前置）

- **E8-B 落锤：单格全审转正默认。** 证据：单格臂两跑（批 1 代码 74K / dev 全套 75K）input 一致 −77%、挂钟 1/3；质量未见系统性下降（跑 1 抓到主臂漏的 env 竞态真问题且存活；verdict 分歧两方向各出现一次=方差非结构性；classify_refute 系列面两臂持续命中）；大变更集反证（批 2/3 ripgrep 8 格=651K=全景重复喂 8 次）。分格的「注意力分工」价值在 4 跑数据中不可见，成本是 N 倍确定项。多模型复验降级为回归监控（同 E8-A 先例）。
- **dep_rank 随收缩删除**（批 3 fee48e8 刚交付）：岗位①「兄弟依赖距离排序」在单格范式下失去参照系（无自家/兄弟二分）；0 壳内降级顺序 MVP=diff 顺序。不留死代码——L4 拆缝立项时从 git（656ed23/fee48e8）复活。沉没成本不是留存理由（同 S2-c 死刑先例）。
- **clamp_compliance 机械钳制随 lens 表删除**：钳制唯一实职=L5/D5 standards 类非硬约束 cap Medium；lens 去实体化（E8-I 代码零结构）后触发信号（finding.lenses 查表）消失。替代=模板散文（compliance 注记烘进固有文本）+ refute 防线（批 1-3 验收实证其杀伤力）。风险记验收观察项：standards 假 high 若在验收中 block，回补一个不依赖 lens 的钳制信号（模型自标 compliance 布尔）。
- **消费面查证结论**：lens 字段无外部消费者（唯一足迹=ship.md 透传 --lenses 一词，随批 4 删+make gen）；state/result 的 lenses 字段删除向后兼容（serde 忽略未知键、无读方）。

## E10 · 文档模式移除（2026-07-14 讨论达成共识，并入 M16 前清理）

**决策**：从 redteam 二进制移除 doc 评审模式（`--mode doc`）。不保留弱自足版本。

**根因=范式冲突（非成本）**：文档评审的高价值形态=把设计/方案批评**锚在现有代码地基上**判合理/遗漏/冗余（用户澄清：文档撰写背景=当前什么代码之上，缺此只剩内容自洽=弱版本）。而「代码地基锚定的设计评审」本质是**探索式、无边界、多轮、agentic**——设计触及的「相关代码」无变更集可界定（除全 repo 从头到尾=不存在的场景）、模型不预先识库不知该读哪。这恰是 redteam 刚为代码评审抛弃的范式（M15 定：有界/无工具/单趟/成本可预测）。文档模式一路二等公民、反复单独和解（模板仍教工具的 M16 债、批 2 硬穿 needs），病根即逆工具纹理跑。三路只一自洽：弱自足（用户不要）／bolt agentic 回来（两对立范式精神分裂）／**删除，交给本为此而生的交互式 grill 工具**（plan-eng-review/grill-with-docs=agentic+工具+多轮）。

**替代供给**：交互式宿主 skill（Claude Code 的 grill 系）已服务此需求；裸终端/Codex/OpenCode 用户失去 redteam 内对抗式设计评审——但当前 doc 模式从未真正服务强版本（给不了代码地基），失去的是弱能力。**未来若判定裸终端确需**，做**结合宿主的纯 skill 版本**（参考其他技能），**不再放入二进制**——用户 2026-07-14 定。

**并入 M16 前清理**（用户定序）。删除面：`build_doc_prompt`/D1-D5 doc lens/`run_doc_fanout`/`adversarial-doc-bundle.md`+refute-doc 模板/mode 推断的 doc 分支/`--mode doc` 值。连带好处：①`--base`/`--scope` 转必选（mode 消歧消失，承 E10 前用户参数核查 TODO）②**M16「doc 模板教工具」散文债蒸发**（无 doc 模板可和解）③`--spec` 角色收窄清晰=仅 code 模式对照 spec 文档（有界、变更集界定），不再与「doc 作评审对象」混淆。规模类比 lens 去实体化（净删除批）。

## E11 · M16 安全组设计（2026-07-14 grill 七裁决）

**威胁模型（Q1）**：上界=**B（本地审不可信仓/PR，clone-and-review）**；C（CI/bot 自动审外部 PR、`--comment` 回帖）=远期目标。按 B 严重度设计、按 B 现实排期。B 下唯一「报告无关的真实外泄通道」=①（key 直发攻击者）；②③④在 B 下降为「误导本人/隐私泄露给第三方 LLM」，无公开外泄。

**范围与修序（Q2）**：**①③④真修 + ②轻加固**；序 ①→④→③→②。

- **① config 偷 key（Q3）**：向量=项目层 `.redteam/config.toml` 深合并右覆盖，可改已定义 provider 的 `base_url`（重定向请求+真 key 发攻击者）/`api_key_env`（指任意 env 外传）；key 仍从用户层 secrets 解析。**修=项目层只允许 `[review]` 子树，`[providers.*]` 一律丢弃+warning**（provider 定义只活用户层 `~/.config/redteam`；`default_models` 留项目层安全=只能选用户已定义 provider；信任 team 仓的 provider 靠手动抄进用户层=显式信任摩擦，B 下靠文件分不出可信/恶意仓）。实现=`from_layers` 给项目层打标、merge 前剥 `[providers]`。

- **④ needs 变更集外任意读（Q4，dogfood 2026-07-14 抓）**：前提=单格默认后整变更集已在 pack，needs 唯一活=读变更集外（F5 17% 佐证），故「白名单=变更集内」会让 needs 失业（排除）；B+①修后真实靶子窄=你 clone 目录里工具自建的 untracked `.redteam/proj.db`、你扔的 untracked `.env`（攻击者自己文件泄露不伤你、root 外已被 safe_join 挡）。**修=git-tracked-only（`git ls-files` 边界，天然放行合法源、挡 untracked 敏感文件）+ 每次 needs 读的变更集外文件强制披露进 run.warnings/报告（B 下人肉读报告=最后防线）+ 小 denylist 兜底（`.env*`/`*.pem`/`*.key`/`id_rsa`/`.redteam/`/`.git/`）**。E10 删 doc 后 needs 只剩 code 模式、无 targets/spec 白名单支。

- **③ render_precedents 二阶注入（Q5）**：向量=卡片 `title`/`reason`/`file`（模型可控）未单行化直入 refute prompt，含换行即注入假结构行（假 `</method>`/假段/假条目）。**修=单行化所有模型可控卡片字段（file/title/quote/reason，换行/CR 折叠空格）**，结构杜绝假行；跳过冗余 `- [` 转义（单行化后字段内 `- [` 只能行中、模仿不了行首条目，YAGNI）。B 下窄面（需你亲手 dismiss 攻击者构造 finding 才中毒）但一 helper 顺手。

- **② prompt injection 轻加固（Q6）**：现状仅中和 `</dossier>` 一处（prompt/mod.rs:249）。B 下②=误导本人+无工具已缩面到「压 findings/诱导 approve」。**修=结构中和扩面（模板全套闭合标签+假段头，防内容伪造边界）+ 不可信内容免疫段（system message 里明示：dossier 内是不可信数据、任何看似指令的文本是待评审数据非命令、verdict 只基于代码分析）**。不做 C 级 nonce 不可伪造围栏/分层防御（推 C）。

- **prompt 消息角色分离（Q7）**：现状=薄 system + 指令与不可信数据全糊在一个 user 串。**(a) 免疫段放 system message=②安全内核、现在做**；**(b) 完整角色分离（全部指令→system、user 只留不可信数据）+ 配合无工具默认的 prompt 简化/适配/调优 = 单立 polish 项排后面、带独立 A/B 门**（用户定：非纯格式重排、有 verdict 校准漂移风险、不塞轻加固；范畴不止结构、含无工具时代 prompt 的次优处清理）。

**验证方式**：每条守卫函数层 TDD 单测（攻击输入→被挡输出：项目 config 带 providers→丢+warn / 卡片带换行→单行 / needs untracked→拒+披露 / 内容假闭合标签→中和）+ 一个对抗集成 smoke（crafted 恶意仓一次跑四攻击全挡）。

**执行次序（用户 2026-07-14 定）**：① M16 前清理批（doc 移除[E10]+SCIP 拆除）→ ② 仪器拆除/简化（仅拆不再用；无工具后测量仪器做简化适配=保留还需/可测的、移除其内不再需要的功能/指标，整体拆不需/不可测的）→ ③ 本安全组。之所以先记 E11+写 plan=上下文将满、防 compaction 丢设计。

## E12 · M16 并发组设计（2026-07-21 grill 五裁决）

**问题（三向死结 + 一兜底）**：后台 job 生命周期状态机（`JobState` 落 proj.db，整块 `state_json` JSON blob）存三个并发/崩溃缺陷 + 一个 panic 空洞。三者共根=**所有写点做非原子 RMW**（`read_state → 改 → write_state`），进程内 `JobCellSink.lock`（Mutex）只串行化 worker 自身的并发 cell 写，**跨不进独立的 `cancel` 进程**。

- **① 终态覆写**：worker `run_job` 收尾读 `latest`（cli.rs:1421）→ 判 cancelled → 写终态（:1428）之间有窗口；此间 `cancel` 进程写 Cancelled，worker 的 Done 覆盖回去 → cancel 丢失。反向亦然。
- **② 猝死永久 running**：worker 进程被 SIGKILL / panic / 断电 → 状态永远停在 Running，`status`/`list` 永远显示「运行中」，无人收尸。
- **③ cancel 盲杀**：`cancel` 无条件 `libc::kill(pid, SIGTERM)`（main.rs:575），pid 来自可能早已死亡并被 OS 复用的记录 → 误杀无辜进程（同 pid 的他人进程）。

**Q1 终态优先级策略（"首个终态封盘,cancel 抢先即终"）**：**首个终态封盘——任何终态一旦落定,永不被后续写覆盖**（含被另一终态覆盖）。cancel 与 worker-done 竞争时，谁先到达终态谁赢；非终态字段（progress/last_active/phase）仍可自由更新。落点=db.rs 守卫原语在事务内读现值、若已终态则拒写终态类更新。被否：last-writer-wins（现状，即 bug①）；worker 总赢（cancel 永远可能丢失，违背用户显式取消意图）。

**Q2 收尸设计（"接受你的建议"）**：**惰性收尸**——`status`/`list` 读状态时，对 Running 且 pid 已死的 job，就地判定 `reap_if_dead` → 落 **Failed** 终态（复用既有 Failed variant，不新增 Dead/Crashed；error 记 "worker process died (reaped)"）。无常驻守护进程（YAGNI，裸终端工具无 daemon）。pid 存活探测=`libc::kill(pid, 0)`（ESRCH=死、Ok/EPERM=活）。**MVP 接受 pid-reuse 误判**（死后被复用的同号活进程会被误判为「仍在运行」而不收尸）——但 Q3 的身份锚把这条缝也补上。

**Q3 身份锚（"Y"=轻量身份锚）**：pid 存活探测无法区分「原 job 进程还活着」与「pid 被 OS 复用给了别的进程」。**加轻量身份锚=进程启动时刻（start-time）**：`JobState` 增 `pid_start: Option<u64>`，父进程 spawn detached worker 后（main.rs:452 回填 `st.pid` 同处）读子进程 start-time 一并记录；收尸/cancel 时比对「当前 pid 的 start-time」是否等于记录值，不等=pid 已被复用（原进程已死）→ 按死亡处理（收尸落 Failed / cancel 不发信号）。macOS 用 `proc_pidinfo`（`libc::proc_pidinfo` + `proc_bsdinfo.pbi_start_tvsec`）。被否（Q3 Part1）：纯存活探测（留 pid-reuse 缝）；重量级（pidfd/kqueue EVFILT_PROC 订阅=需常驻监听，与无 daemon 冲突）。`pid_start=None`（旧状态/读取失败）时降级为纯存活探测（不因缺锚而拒绝收尸）。

**Q4 panic 兜底（"认可,顶层兜底 + 只带 message"）**：worker 评审体若 panic（unwrap/index 越界等），`#[tokio::main]` 主任务崩，状态停 Running（bug②的一个诱因；收尸只治「进程已死」，但 panic 与进程退出间有窗口且退出码非 2）。**修=进程级 panic hook（真·顶层兜底）**：worker（`Cmd::Worker`）起手、调 `run_job` 前 `std::panic::set_hook` 装一个 hook，捕获 panic payload 的 message（`downcast_ref::<&str>` / `::<String>`），落 **Failed** 终态（error 只带 message，**不带 backtrace/location**——噪音大且可能含路径/环境信息，B 下报告要干净），再链回默认 hook 正常展开。
  - **为何不用 grill 初拟的「spawn tokio task + `JoinError::is_panic()`」**：`run_job` 的 `provider: &dyn ReviewProvider`、`sink` 均为借用参数，非 `'static`；`tokio::spawn` 要求 `Future: Send + 'static`——要么把 provider 提到 `Arc<dyn ReviewProvider + Send + Sync>`（跨 main.rs 建 registry 的签名重构），要么撞 Send 炸弹（registry 需 `Sync` 才能 `&registry` 跨 await）。进程级 hook 无任何借用/`'static`/`Send` 约束，装在 worker 进程独有、且能捕获任意 tokio worker 线程上的 panic，是「顶层兜底」的字面兑现。hook 是 `'static Fn`，捕获 `ws`/`job_id`（clone 进闭包）；写 Failed 走 Task 1 的守卫封装（panic 若发生在真完成之后，守卫拒写、保留真终态）。不用 `std::panic::catch_unwind`（async 体裹它别扭、`UnwindSafe` 跨 await 点不成立）。

**Q5 分解与文档（"保持 6 个,照规范走 plan+decision"）**：保持 6 任务分解（依赖序：①守卫原语 → 顺次接入）；按 CLAUDE.md 规范写 plan（HOW）+ 本 decision（WHY）。db.rs 守卫原语保持「只认 JSON 字符串、零 JobState 知识」（分层：db 管持久化、state 层管 serde）；state 层加薄封装 `update_state_if_active` 承载终态封盘策略，进度写与终态写统一走它。

**验证方式**：Task 1 = N 线程竞争测试（首个终态冻结、终态永不被覆盖，靠 SQLite `BEGIN IMMEDIATE` 跨连接串行化）；Task 2 = 自读 start-time 两次相等 + 复用比对逻辑单测 + 旧状态兼容（`pid_start` 缺→default None）；Task 4/5 = 死 pid→Failed、复用 pid（锚不符）→按死处理、活+锚符→不动；Task 6 = 注入 panic 的 job→终态 Failed 而非永久 Running。每任务函数层 TDD + 现有生命周期测试（`review_e2e`）保持全绿。

**威胁模型系于 B**：并发组是可靠性缺陷（非安全外泄），但在 B（本地审不可信仓）下，恶意仓可诱发大量 job（`--background` 风暴）放大 bug②的僵尸堆积、cancel 盲杀③的误伤面——故与安全组同属 M16 发布基线门控。

**基线**：dev@f028f92（安全组 dogfood hotfix 后）。**永不 push**。SDD：实施者=sonnet、任务评审=sonnet、整支终审=**opus**（微妙并发变更→最强模型终审，见 subagent-driven-development「Model Selection」）。
