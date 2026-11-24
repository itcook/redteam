# redteam Roadmap · 总览

`redteam` = 模型无关的对抗式代码评审工具(单静态二进制 + 薄插件壳 + 循环对抗 SKILL `ship`)。
本文件夹是**项目级活索引**:读 `summary.md` 即知全貌与下一步;已交付明细见 `done.md`,开放/进行中见
`followup.md`;零散小项见 `../TODO/`。

状态图例:✅ 已交付 · 🟢 机制完成待验收 · 🔨 进行中 · ⏳ 开放未启/后置 · 🧊 backlog · 🚫 搁置/作废
更新于 2026-07-22(**重规划**:橡皮鸭走读四路评审驱动,评审=`../reviews/2026-07-22-rubber-duck-walkthrough-review.md`;
用户三裁决=先重构后发布 / 五支柱关账 / 现有三宿主先发)。

---

## 路线主轴(2026-07-22 重规划)

未发布阶段,一切可推翻。走读抓出**四大结构性问题**(尾段双实现 / provider 抽象颠倒 / state 历史夹层 /
ship 散文状态机)——全部源自同一件事:**M15 把评审范式翻成了无工具单发,但代码骨架还是 tool-loop 时代的**。
主轴=先把骨架翻到与范式一致(M17),再做发布硬化(M18),然后发布(M19)。

## 旧账关账(2026-07-22)

- **五支柱账目(成本重构主线)整体关账**:支柱 1(轮次增量)✅ M13、支柱 2(卷宗)已重定位并入 M15、
  支柱 5(可观测)近全(仅剩 $ 估算,归 TODO);**支柱 3(驱逐/分段读)与支柱 4(预算分层)作废**——
  两者针对 tool-loop 二次方尾巴与多轮成本结构,无工具默认(input −97.5%/恒 1-2 轮)后病灶已不存在。
  历史账目见 git 与 `../research/2026-07-03-cost-redesign-five-pillars.md`。
- **M6-b(提示词 harness)、M8(backlog)、M14 余项**:并入 M20 发布后 backlog,不再单列活跃里程碑。

## 里程碑总览

| 编号 | 一句话描述 | 状态 | 详见 |
|---|---|---|---|
| **M1-M5** | 核心引擎 / 运维面 / 可靠性内核 / 打包分发 / 交互优化+更名 redteam | ✅ | done |
| **M6** | 提示词 & lens 质量 batch1(69003b2);M6-b harness → 并入 M20 | ✅(收口) | done |
| **M7** | Loop SKILL + 循环对抗层 ship(c268b1f) | ✅ | done |
| **M9** | 宿主集成(host-model 委派)——技术不可行+立场冲突 | 🚫 | followup |
| **M11** | doc 评审守卫化(711edcc;doc 模式后随 E10 移除,守卫经验沉淀进 code 线) | ✅ | done |
| **M12** | dogfood 自反馈仪器(已随 M16 拆除) | ✅(已拆) | done |
| **M13** | 成本重构·支柱 1 轮次增量协议(fc1e583) | ✅ | done |
| **M14** | 成本重构·支柱 2 卷宗(57c0011;D9 后重定位并入 M15) | ✅(收口) | done |
| **M15** | **上下文管理架构 → 无工具评审默认化**:input −97.5%/恒 1-2 轮/挂钟 −94%;E8 九裁决;四批 SDD;SCIP/1 壳安乐死 | ✅ | done |
| **M16** | **发布基线 P0 三批**:清理批(E10 doc 移除+E8-E SCIP/仪器拆除)+ 安全组(威胁模型 B 四守卫)+ 并发组(三向死结+panic 兜底四件套;merge 290236e + dogfood hotfix 814e010) | ✅ | done |
| **M17** | **结构重构组(先重构后发布,2026-07-22 立项)**:走读四大结构问题各成一批——**a✅(2026-07-24 合 dev 8b9eef2,明细 done.md)** 尾段统一· **b✅(2026-07-27 合 dev 16f118b,明细 done.md)** provider 翻转· **c✅(2026-07-27 合 dev 06d589e,明细 done.md)** state 夹层合并· **d✅(2026-07-27 合 dev 175af20)** ship 协议硬化(重定义) | ✅ 全组收官(a/b/c/d,2026-07-27,明细 done.md) | done |
| **M18** | **发布硬化(grill 2026-07-27 八裁决锁定,四批)**:批1 安全(D2/denylist 合一/T46/注入回归,合 dev eb67922) · 批2 契约(exit 五档/D3/conf_floor 门/T48/T25/T26,批4 polish 搭车) · 批3 e2e(T47③ 真 panic 破局/退出码链/台账注入,合 00e23fc) · 收官 dogfood✅(DeepSeek+gpt 三轮内循环 reject→reject→approve,F3/F5/F6+追打修复合 dev,F1/F4+低危残余 dismiss 入台账) | ✅(2026-07-28,明细 done.md) | done |
| **M19** | **发布 1.0(现有三宿主)**:Claude Code 插件 + Codex 镜像 + 裸终端二进制(macOS arm64);release 文档(README/安装/快速上手,以 --help 为纲)+ 版本 tag + 分发面定稿(bin 入 git 现状 vs T11 GitHub Release 届时定) | ⏳ | followup |
| **M21** | **评审引擎重构(风险路由 DAG)**:scanner 疑点扫描 + 确定性风险路由 + 深挖优先级队列 + 疑点台账 + verifier 流式;P0 验证门 ✅(2026-09-07,D11,design v8 锁定);M21a-e 见分支 `feat/m21-review-engine` 工作树 `.next/roadmap/`(README/P0/M21a-e/followup),M19 后置于 M21e | 🔨 | 分支 `.next/roadmap/README.md` |
| **M20** | **发布后 backlog**:M10 OpenCode(需 brainstorm,软前置=linux 交叉构建)· linux/win 交叉构建 · M6-b 提示词 harness · provider 拓展 · R6 执行判(远期) | 🧊 | followup |

**下一步**:**M21 评审引擎重构进行中**(2026-09-05 起,P0 ✅ 2026-09-07,M21a ✅ 2026-09-09 出口验收交付,见 done.md)——下一步 M21b 风险深挖队列(plan 待派生到分支 `.next/plans/`);
M19 发布 1.0 后置于 M21e(重构改 CLI 面),范围见 followup M19 节。

> 历史注记:M8/M10 原编号内容已并入 M20;五支柱表原文见 git 历史(2026-07-22 前版本)。
