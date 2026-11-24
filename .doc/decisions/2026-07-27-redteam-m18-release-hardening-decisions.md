# M18 发布硬化 · 决策日志

> 单份决策日志、分节记（grill 2026-07-27 八题全锁 + 后续批内裁决追加）。design 引本文件。

## Grill 八裁决（2026-07-27，用户全数拍板）

**账目清洗底座**：M17 四批已顺手清掉 M18 原清单近半（退避注释/probe/derive_kind/常量/
route_key/write_state 降权/检查点吞错主体/两条 e2e——grill 起手逐项亲查存活状态，防清扫
批修已死之病）。

### Q1 边界 → token 轮廓类四件逐出 M20

- 裁决：pack diff 双注入裁决(+T32)/pack 利用率落账/prompt 三重复段合一/lens L0 段序 →
  M20 优化群（与 T10/T49/T50/T27-余 同桶）。
- 理由：发布硬化判据=「不做就有发布风险」；这四件是优化且引入轮廓变化需独立 A/B，与
  硬化批「零行为/修 bug」纪律相冲。利用率落账虽是观测加法，但它是 A/B 前提而非发布
  依赖，随优化群走（群内顺序先落账后 A/B）。

### Q2 批次切分 → 四批（安全/契约/e2e/polish）

- 批 1 安全组=威胁模型 B 收尾（D2/denylist 合一+T46④/T46①②/git `--`/注入回归 e2e 自带）；
  批 2 正确性契约组（D3/exit code/conf_floor 门/T48/值域/T25/T26/resume 披露）；
  批 3 e2e 扩充（后置——退出码链依赖批 2 契约定稿）；批 4 polish 随批 2 搭车或半天批。
- M17 记债归属：resume 披露盲区→批 2；连接计数仪器→M20（防回归仪器非发布依赖）。

### Q3 exit code → 五档一次性重整

- 裁决：0=干净 approve / **1=命令错误**（Unix 惯例，原全部 exit(2) 机械换）/ 2=降级
  approve / **3=incomplete**（新）/ 10=reject。
- 探底发现撞车：命令级错误与降级 approve 共用 2——CI/bot 分不清「工具挂/降级过/没跑完」。
- 被否（只加 3 不动错误码）：留着 1/2 语义脏；未发布=改契约最后窗口（M17-d C1 同款
  理由），发布后 exit code 即永久 API。

### Q4 refute conf_floor 门 → 仅孤证 blocking 分支

- 裁决：refute_targets 加 floor 参，孤证 blocking 分支加 `confidence >= floor`（低于
  floor 者 verdict 层本就永不 blocking，refute 无裁决增益纯烧 LLM）。**判例预筛分支
  有意不门**：变体压制语义（跨轮一致驳回）+ refuted 标记披露价值 + 命中率低成本可控。
- conf_floor=0 退化配置行为与现状全等（零默认行为变化）。

### Q5 e2e 五项 → 含 REDTEAM_TEST_PANIC 钩子

- T47③ 破局：c-T6 判死「真 spawn 需真模型」的绕法=panic 注入在 fan-out **之前**
  （worker 路径读 env REDTEAM_TEST_PANIC 即 panic!），不需要模型。真 spawn 二进制 →
  断言 Failed(error 含 panic)+封盘不变式。补上 M16 以来 panic hook 端到端零覆盖的
  最后一块。
- 纪律注记：env 钩子是 dev-time 仪器的例外——随 e2e 永久存在（回归资产），不进
  prompt/文档面，feedback.md「发布前拆除」教训不适用。
- 退出码 3（incomplete）的构造手法批内自裁（函数级+dispatch 级组合可接受）。

### Q6 记债三项落法

- T25：orchestrate 整链失败路径把累计 usage 回填失败 CellRecord——失败场景 run.usage
  变诚实，属修 bug 非轮廓变化，不触发 A/B 纪律。
- T26：model setup 收尾自动 probe（复用 M17-b 按 kind 认证），失败**警告但仍落盘**
  +提示 model check 复核。被否（失败拒落盘）：离线场景配置合法，过强。
- resume 披露：前台命令 stderr 可见，eprintln 即可（不需要 warnings 计数通道——那是
  detached worker 黑洞的方案）。

### Q7 验收门

- 每批 SDD 常规门+隔离评审；批 1/2 opus 终审、批 3/4 可 sonnet 合一。批 1 特别门=
  注入回归红测法（修复前红）；批 2 契约门=lib 级 exit 五档锚+ship.md/--help 同步不留
  窗口期。**M18 收官门=多模型 live dogfood**（审 M17+M18 累计 diff；是 M19 发布前
  dogfood 的预演不替代）。

### Q8 闭集

- 排除：轮廓四件+连接计数仪器+T49/T50/T27-余（M20）/T35（等 Kimi）/T06+T11+README
  （M19）/main dispatch 抽取（批 4 可选不强制）。

**定论落点**：`../design/2026-07-27-redteam-m18-release-hardening-design.md`（锁定）；
plan=`../plans/2026-07-27-redteam-m18-release-hardening-plan.md`（单份四批分节）。
