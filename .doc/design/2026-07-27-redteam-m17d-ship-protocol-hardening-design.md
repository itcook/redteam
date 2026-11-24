# M17-d ship 协议硬化 · 设计

> 状态: 锁定 · 2026-07-27 grill（中途问题重定义，原「ship-step 下沉」作废）；WHY 与重定义过程见 `../decisions/2026-07-24-redteam-m17-structural-refactor-decisions.md` §D；实施计划 `../plans/2026-07-27-redteam-m17d-ship-protocol-hardening-plan.md`；里程碑=roadmap M17-d。

## 1. 目标（重定义后）

修 ship 循环的一条实证缺陷（T05 半失败 approve 误收工）+ 消除跨轮记账对 LLM 会话记忆的
依赖（compaction 必漂）。**不做**判定下沉/ship 命名空间/新表——ship.md 保持 prompt 编排
skill 形态，二进制只补一个对称读口。

## 2. ship.md 协议修订（六处）

1. **T05 交人出口**（停机 §1）：verdict=approve 且 `run.failed_models` 非空 → **不收工**，
   AskUserQuestion：「接受降级 approve（N 模型未参战，对抗火力不全）」/「重跑本轮」。
   failed_models 空的 approve 照旧收工。
2. **新增「跨轮状态重建」节**（起手之后）：铁律=跨轮判定一律从盘上重建、不信会话记忆
   （compaction 后记忆不可靠）。三条重建口：
   - 轮次链/轮数：`ls rounds/<session>/` 的 rN.result.json 序列，job id 读文件内 `run.job_id`；
   - 复裁资格（每 id 至多一次）：`dismiss show <id>` 的 status+history（现状已有，明示）；
   - 修复历史：`resolve show <id>` 的 history 条数。
3. **销案争议刹车判定查表化**（停机 §2）：「同一 id 第二次出现在 resolution_disputed」
   （记忆比对）→「id ∈ 本轮 `run.resolution_disputed` **且** `resolve show <id>` history
   已有 ≥2 条」（纯查表；语义等价：修过两次仍 disputed = 死锁交人）。内循环 §3 的
   「第二次出现在 resolution_disputed」措辞同步。
4. **max-rounds 计数口径**（停机 §6）：改「rounds/<session>/ 目录 rN 计数」，不靠自数。
5. **rN.result.json 存档义务化**（收轮）：措辞从取用便利升格为「链持久化协议义务——
   跨轮状态重建依赖此文件序列」。
6. **T07 目录两层化**（起手 §0.1）：`rounds/<YYYYMMDD>/<HHmmss>/`。

## 3. T08 session.md 模板骨架（md 薄版）

收工步（停机 §1）给固定模板（宿主填值，禁自由发挥）：

```markdown
# ship session <date>/<time>
- 轮数：N（r1..rN，job id 清单）
- 判定：approve | 降级 approve（用户接受）| 停机（<守卫名>）
- 统计：修复 X · 驳回 Y（新judgment卡 Z）· 销案 W · 复裁 V
- 半失败注记：<有则列轮次与 failed_models>
- carried 遗留：<id+一句话 | 无>
- 遗留观察：<非 finding 级备忘 | 无>
```

## 4. Rust：resolve 子命令空间化（唯一二进制改动）

`Cmd::Resolve` 平铺形态 → `resolve add|show`（对称 dismiss）：
- `resolve add <id> --from … --round … --reason …`：语义与现行平铺形态逐字等价（纯挪壳）；
- `resolve show <id>`（新）：输出卡四件套（title/file/quote/severity）+ reason/round +
  history 全条目（`resolve_history` db 读口已存在，只补 CLI 面）；查无此卡退非零。
- ship.md 全部 `redteam resolve <id> …` 调用点同步改 `resolve add`；`--help` 同步。

## 5. 契约面

- report schema v3 / proj.db DDL / review 命令面：零变化。
- CLI 破坏面仅 `resolve` 裸形态→`resolve add`（消费方=ship.md 本批同步；未发布零外部用户）。
- codex 镜像：`make gen` 再生成，`make verify` 零 diff 门。

## 6. 验收

- Rust：add 等价兼容测试 + show 输出断言（卡+history）+ 全量绿 + clippy 零新增。
- md：隔离评审按 §2/§3 九项逐条核对（一致性/无自相矛盾/瘦身不丢模型本职规则——裁决
  规则、fuzz 配方、refuted 例外目检等一字不少）。
- **live ship 循环验收不在本批**：并入 M19 发布前全仓 dogfood（届时 T05 出口/状态重建/
  两层目录全要素实测）。

## 7. 范围闭集

§2-§4 即全部。作废/排除（decisions §D-QD）：ship 命名空间四命令与 ship_rounds 表
（设计推理留痕于 decisions，实证再启可复用）· T09 watch（等痛点）· 停机未涉条款措辞
重写 · feedback 类仪器。
