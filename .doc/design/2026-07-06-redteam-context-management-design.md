# redteam 上下文管理·架构设计（2026-07-06 起 / 2026-07-07 收口）

> 状态: 已交付（经 E7-E9 大幅重定向）· 2026-07-14 M15 收尾归档：原框架（blast-radius pack 粒度/双树 SCIP/per-model 规划器/S2-c/S3/S4）大半被解剖与 A/B 证伪或安乐死（E7 三错配、E8-E SCIP、E9 收缩）；**最终交付形态=`2026-07-13-redteam-no-tools-default-design.md`**。本文保留为架构考古与复活项索引（L4 拆缝/外围注入=痛点驱动复活点）。
> 决策过程/权衡/被否选项/实测证据 → `../decisions/2026-07-06-redteam-context-management-decisions.md`（各节标 〔Dn〕）。
> 里程碑: roadmap **M15**。派生 plan: 待 staging 产出（`../plans/2026-*-redteam-context-management-*-plan.md`）。
> 多里程碑级、非一个 PR。

## 0. 本质与北极星

- 本质 = **上下文管理**：慢/贵/质量差是同一病的症状——每个 cell 拿到的上下文不对（多=噪音稀释，少=补读多轮）。〔D1〕
- 北极星（可证伪）= **残余 tool_use（模型主动 read 调用）→ 0**。仪器 384fda6 per-cell 量化；读工具使用率 = bundling 质量反馈。

## 1. 统一模型：pack = 变更的 blast radius

- **radius = symbol-reference 半径**（call-graph 是子集）：0=变更符号 / 1=直接引用者·被引用者 / 2+=传递闭包·类型契约用户。SCIP occurrence 覆盖 call / const 读 / type 用 / field 访问。〔D10〕
- **预算 = radius 调速器**：从近壳到远壳填满 per-model 预算，未填入的半径交读工具；半径由预算封顶、天然有界。
- **pack = diff（审什么）+ blast-radius 正文（完整上下文）**，叠加非替换。
- **非函数变更**塌进同一 reference radius、无新引擎：const/type/field→reference radius；import swap（同名换源）→签名 delta（§2）+ in-file 使用点；config/string 字面量→regex-B（§5）；注释/空白→近 skip。**不做污点/数据流分析**（只给直接读者，传播交模型）。〔D10〕

## 2. Radius 展开规则（对抗·不信任式）

- **意图排除**：commit/PR 描述不进 radius、不进裁决；变更类型纯从 diff+索引机械导出。〔D2〕
- **按变更类型展开、非均匀距离**：breakage 可能处就展，只在可证不可能处剪。〔D3〕
- **契约变更三桶分类（纯签名面 diff）**〔D4〕：
  - **A 签名面变**（参数增删改序/改类型、返回类型、泛型约束、可见性 private→pub、async·const·unsafe、struct 字段、enum 变体、trait 方法）→ 传递展。
  - **B 仅 body 变**（签名逐字节同）→ 展直接 caller 一跳（不假设「只是重构」，涟漪不穿签名）。
  - **C 装饰变**（注释/空白）→ radius≈0 近 skip。
  - 三桶是 radius **深度+排序档、非门**（A/B 都展）；边界=签名面（机械）止，行为安全性交模型。
- **caller 集剪枝 + 排序**〔D6〕：
  - **剪枝（可证安全才踢）**：返回类型变→踢丢弃返回值的 call site、留用返回值的；可见性拓宽→踢现有 caller、另立「新公开面暴露安全吗」finding（另一轴）；参数签名变→不剪、全留高。
  - **排序（仅预算溢出/高 fan-in 时）**：按**覆盖多样性**（distinct 模块/文件/enclosing-fn/调用形态、近重复去重），**非 breakage 可能性**。剪枝永远跑、排序器只在溢出建。
- **方向性 = 结构不对称、不设标量权重**〔D9〕：caller 要 body、契约变更传递、散全仓要正文=贵；callee 只要契约/签名、1 跳不递归、多半已随改动函数在包=廉。预算天然流向 caller；溢出/多样性排序只在 caller 侧。callee 契约不足以判「用对没」→本身是 finding，非展进 callee body。
- **枚举/trait 穷尽性 flag（radius 上 AST 标注、非独立引擎）**〔D7〕：靶=**编译器穷尽性被 fallback 击穿的站点**——enum 加变体→有 catch-all `_` 臂的 match；trait 加方法带 default→所有 impl。硬报错站点（match 无 catch-all / trait 方法无 default）跳过（编译器管）。产物=**待裁信号**（注入 pack 的带标注站点清单）。**skip 由索引路径门控（§5）**。
- **信任边界**：radius 在「非被审改动」处停（外部 dep/stdlib 的 **body** 不展）；「用对没」看其**契约**不看 body。
- **比例分诊**：精力∝风险；变更类型分类=确定性风险信号（空 radius 轻档、签名/删除深挖）。
- **dead code 非 skip**〔D3〕：「无 in-repo caller」=**待裁信号**（可能未接线/孤儿/breaking public，或反射·宏索引盲区）；机械 flag 交裁、连比例分诊轻档。

## 3. 预算

- **per-model 预算**：每模型按自己 context 定 → per-model bundling（每模型一套 cell）。
- **context 数据源**：models.dev 策展表烤进二进制 + config 覆盖 + 兜底；解析序 `config > /v1/models 自报 > 表 > 兜底`。（实测 DeepSeek 1M / gpt-5.5 1M / Kimi 256K）
- **上限语义**（非目标）：填到相关内容完即停、不 pad；`预算 ≈ context×0.5`（留半给 tool-loop 重发+output）；bytes↔tokens ~3.5。
- 单一 per-cell 天花板（卷宗 28K 帽并入）；优先级 = radius 半径壳层（近壳优先）。

## 4. pack 粒度 + 溢出契约

- **符号/enclosing 粒度**（非整文件）：小文件全文、大文件回落 enclosing 函数（复用 `innermost_enclosing_def`）；caller 提升排序按引用次数降序、同数按 path。
- **精确省略清单**〔D24〕：被省 radius 的 file:line 纯坐标附 pack 尾 → 模型定点 `read_files({path,start,end})`、绝不误判「不存在」。梯度三层：**packed 近半径全函数 → 省略清单 → read_files 定点读**。

## 5. 静态索引引擎（SCIP，侧载非内置）

- **静态 SCIP 索引、非 live LSP**〔D18〕：产索引文件、离线查——确定性/可缓存/无冷启动全仓延迟。
- **双树索引**（HEAD 树 + 工作树各一份）〔D5〕：diff 两份符号表 = 契约分类的旧签名 + **旧树调用图**（解删除符号 radius=旧 callers）。同一基础设施。无索引降级=hunk 就地解析（enclosing def 抠签名，~90%）。
- **索引构建 = 事实 build gate**〔D8〕：生成索引跑真类型分析 → 编译不过⟹索引失败=确定性「diff 不 build」信号。穷尽性 skip 由此门控：**索引路径 skip 硬报错站点、降级/regex 路径不 skip**。
- **staleness/生命周期**〔D13〕：HEAD 树索引 SHA 键、不可变→永久缓存、恒无 stale；工作树索引=**HEAD base + 脏文件增量、审起始现推、不跨 run 缓存**→永不 stale。对齐=构造保证（索引端点=diff 端点；range 评审 SHA..SHA 全确定）。
- **存储布局 + 自隐身 .gitignore**〔D14〕：`<repo>/.redteam/index/`（增量器温热工作态、恒 ignore）+ `.redteam/cache/`（SHA 不可变归档、**LRU/容量帽驱逐旧 SHA**=唯一「清理」）。redteam 首次 run 自生成 `.redteam/.gitignore`：
  ```
  # redteam 本地运行态，默认全 git 忽略
  /*
  # 可选：与队友共享 SHA 索引 cache（可再生且大，通常 CI 缓存 artifact 更划算）
  # 取消下一行注释即跟踪 cache/：
  #!/cache/
  ```
  必须锚定 `/*`（裸 `*` 每层匹配、`!cache/` 拉不进内容）；opt-in=删 `#`。可选全局 `~/.cache/redteam/<repo-key>/<sha>` 去重 worktree。
- **provisioning：无强制 init**〔D15〕：纯/本地索引构建静默折进 review 前置；改系统安装**绝不阻塞**——indexer 缺→**regex-A 降级跑完 + 一行提示**（`redteam setup-index` 可选 consented 升级）。regex-A = 未升级前**载重默认**。
- **获取**〔D16〕：**工具链永远借**（repo-local 优先→全局→无则 regex-A、**绝不侧载**）；**SCIP 发射器在则借 / 不在按需侧载**（借=批量调 binary、**非 attach 活 LSP**）。
- **发射器选择 = 策展权威表**〔D17〕：准则序 0 离线产静态 SCIP（排除 gopls）/ 1 全类型检查保真 / 2 权威在维护；表烤进二进制 + config 覆盖（喜好合法出口）+ regex-A fallback；可用性仅 tie-break。语义层不可约按语言。
- **索引单元 = 编译项目（tsconfig / go module）、非仓**〔D21〕：多语言仓 N 单元；「索引工作树」=**索引 diff 受影响的项目**（按项目 scoping）；未装依赖的项目→regex-A。
- **regex 两角色**〔D22〕：
  - **A 同语言地板**（降级兜底）：无索引时 grep 符号名做同语言引用 radius。
  - **B 跨索引单元缝隙**（条件自门控）：触发 = **跨索引单元 && (改动串在别单元有回声 ‖ schema 文件)**；抓字符串/约定耦合（跨语言路由 / config key / env / 事件名 / schema…）——单语言索引全瞎的集成 bug。
  - 对抗下 regex 不精确是优点（**宁多展不漏缝**）。

## 6. fan-out 融合

- **blast-radius bundling = fan-out 的规划半**〔D23〕：`(changes, models, budgets, 索引) → cells`；老「bundle 层 + orchestrate×模型」塌成一个 per-model 规划器，执行层（并发/重试/收集）不变。
- **bundling 算法**：dir/import 关联种子 + 预算贪心；单文件溢出=独占 bundle、填满、余下读工具。
- **test 处置**〔D11〕：**改动 diff 里的 test = 一等材料、与被测主体同 bundle 共审**；未改、引用被审符号的 test = reference-radius、caller-rank 最低、预算自动剪。
- **跨 cell 重叠**〔D12〕：**接受打包字节冗余**（不去重、独立 context 自足）；引用计算已被索引缓存；**不合并 bundle**。
- **fallback YAGNI**〔D20〕：每模型独立审自己分区、挂了记 failed_models、用户自保可用性；跨模型 agreement 靠 quote/file 锚定不受影响。
- **独立性协同门（开放张力）**〔D25〕：给多模型灌同一份 radius 事实有「判断趋同、侵蚀多模型独立对抗」风险（D9 两跑可复现弱回退）。**约束（锁）**：每个打包共享上下文的 stage 退出判据含「agreement 分布不向假共识收窄」（与残余 tool_use 同为门）。**解**（每模型扰动/来源标注/部分 radius）为**开放问题**，仅 staging 独立性门跳时触发设计。

> **开放问题**（design 里唯一未锁项）：独立性保全机制——见 D25。其余 §0-§6 均为锁定定论。

## 7. 关联

- 里程碑：roadmap **M15**；决策记录：`../decisions/2026-07-06-redteam-context-management-decisions.md`。
- 派生 TODO：T26（model setup check-before-persist）。
- 前置线：延迟主线（1a / read_files / read_files 行段 / ToolUse 仪器已合 dev，[[redteam-latency-rounds-thread]]）= 本架构局部先声。
- 成本线 park；质量/延迟统一收进「上下文管理」本质。
