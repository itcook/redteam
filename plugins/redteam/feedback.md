# feedback — dogfood 自反馈 subagent 执行规范

> ⚠️ **开发期临时仪器 · Claude Code only · 发布前移除**。本文件被 `ship` 收工时派出的 subagent 读取执行。
> 反馈对象是 **redteam 工具自身(redteam + ship)**——它好不好用、准不准——**不是被审代码**。
> (codex 镜像会带一条指向本文件的悬挂引用,codex 不跑此仪器,无害;发布前连同 ship 段一并删。)

你是一个反馈 subagent,在一次 ship 会话收工时被派出。写权限边界:**对被审工作树只读、不碰;
唯一写入目标是 home 中枢**。**不阻塞收工判定**(本步失败仅告警)。产出两样,都写进中枢:
1. 一行**确定性量化记分卡**(跑 jq 精确算,**别用脑子数**)。
2. 一段**定性缺陷/改进项**(redteam/ship 工具本身哪里糙、怎么改)。

## 输入
派发方(ship)给你:会话目录 `<SESSION>`(= `.redteam/rounds/<session>/`)、dismissed 台账目录路径。其中:
- `<SESSION>/r*.result.json` — 每轮 `redteam result` 存档(ReviewReport,schema 见下)。
- `<SESSION>/r*.md` — 每轮简报(含修复/驳回/存疑清单)。
- `<SESSION>/observations.md` — 可选;主线在撞见工具异常时留的一行流水。

**若没有任何 `r*.result.json` → 整步跳过,无可记录。**

### ReviewReport schema(jq 依据)
```
{ schema_version, mode:"code"|"doc", verdict:"approve"|"reject"|"incomplete",
  stats:{critical,high,medium,low,total}, findings:[Finding], clusters,
  run:{ job_id, models:[], lenses:[], duration_ms, warnings:[], failed_models:[],
        usage:{ input_tokens, output_tokens, cached_input_tokens },  // 实付全价 ≈ input−cached；
        // cached=0 且 input 大 = 未命中或该 provider 缓存字段未知（L1 缓存观测）
        dismissed_reappeared:[],  // 空时该键被省略
        head_sha?, incremental?, delta_base?, resolved?, resolution_disputed?, carried_dropped? } }
Finding = { id, severity:"low"|"medium"|"high"|"critical", title, body, file,
  line_start, line_end, confidence, recommendation, lenses:[], models:[], agreement,
  hard_constraint, quote, previously_dismissed?(默认省), cluster_id?, verify?,
  origin?("carried"|"new"，v3;缺省视 new) }
verify = { verdict:"upheld"|"refuted", model, note }   // 省略表示未验证
```

## 步骤

### 1. Tier-1 机械指标(**跑这段 jq**,取精确数)
```bash
jq -s '
  ([.[].findings[]] | group_by(.id) | map(.[0])) as $f
  | { raised:        ([$f[] | select((.origin // "new") == "new")] | length),
      carried:       ([$f[] | select(.origin == "carried")] | length),
      refuted:       ([$f[] | select(.verify.verdict == "refuted")] | length),
      agreement_ge2: ([$f[] | select(.agreement >= 2)] | length),
      anchor_fail:   ([$f[] | select(.line_start == 0)] | length),
      dismissed_reappeared: ([.[] | .run.dismissed_reappeared // [] | .[]] | unique | length),
      rounds:        length,
      mode:          .[0].mode,
      models:        ([.[] | .run.models // [] | .[]] | unique),
      failed_models: ([.[] | .run.failed_models // [] | .[]] | unique),
      partial:       any(.[]; .verdict == "incomplete")
    }' <SESSION>/r*.result.json
```
- `raised` = 跨轮按 `id` 去重后的 **new-only** finding 数（M12 记分卡口径改增量语义：携带项
  不重复计 raise）；`carried` = 携带项数；`refuted`/`agreement_ge2`/`anchor_fail` 同基于去重集。
- 存下这段 JSON 备用(记 `$TIER1`)。

### 2. Tier-2 判定指标(从简报**数整数**)
读 `<SESSION>/r*.md` 的修复/驳回/存疑清单,按去重 finding 归类计数:
- `real` = 判属实并修复的;`false` = 判误报驳回的;`uncertain` = 存疑/上交人裁未决的。
- 简报缺失 → 三者置 `null` 并在 digest 注记"判定数据缺失"。

### 3. Stamping(Bash)
```bash
TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)
REPO=$(basename "$(git rev-parse --show-toplevel 2>/dev/null || pwd)")
BIN=$(ls "${CLAUDE_PLUGIN_ROOT}"/bin/*/*/redteam 2>/dev/null | head -1)
BUILD=$({ shasum -a 256 "$BIN" 2>/dev/null || sha256sum "$BIN" 2>/dev/null; } | cut -c1-12)
```
`BUILD` 取不到留空。`build` = 平台二进制 sha256 前 12 位:每次 rebuild 变,用来把分数趋势关联到"换了哪版 redteam"。

### 4. 组装并追加记分卡行(中枢 = `${REDTEAM_FEEDBACK_DIR:-$HOME/.redteam/feedback}`)
```bash
DEST="${REDTEAM_FEEDBACK_DIR:-$HOME/.redteam/feedback}"; mkdir -p "$DEST"
# $TIER1=步骤1输出; $R/$F/$U=步骤2整数(null 用 null); <session>=会话名
jq -cn --argjson t1 "$TIER1" \
       --argjson adj "{\"real\":$R,\"false\":$F,\"uncertain\":$U}" \
       --arg ts "$TS" --arg repo "$REPO" --arg build "$BUILD" --arg session "<session>" \
  '$t1 + { ts:$ts, repo:$repo, session:$session, build:$build, adjudication:$adj,
           headline_precision_proxy:
             (if ($adj.real != null and $adj["false"] != null and ($adj.real + $adj["false"]) > 0)
              then ($adj.real / ($adj.real + $adj["false"])) else null end) }' \
  >> "$DEST/scorecard.jsonl"
```
一行一 run,**跨所有 repo** 汇总进同一 `scorecard.jsonl`(趋势用 `jq` 看)。
<!-- ponytail: 单行追加假设 ship run 非并发(单用户顺序);真并发再上锁,YAGNI。 -->

### 5. 写 `<session>.md`(人读:记分卡渲染 + 定性 digest + 效度警告)
`"$DEST/<session>.md"`,含三块:
- **记分卡**:把本行渲染成小表(raised/refuted/agreement_ge2/anchor_fail/dismissed_reappeared/rounds/
  provider health/adjudication/headline)。
- **定性缺陷 & 改进项**:合成 `observations.md` 活体一行 + 产物异常(`run.warnings`、`failed_models` 非空、
  `anchor_fail>0`、`dismissed_reappeared>0`、`partial`)→ 讲**工具本身**哪里糙、怎么改。无则写"本 run 无工具级异常"。
- **效度警告(逐字放入,防误读数字)**:
  1. 地面真值是**宿主裁决**(运动员),非客观准确;`false` 被宿主误驳污染 → **方向性/相对**可用,绝对值不可信。
  2. **只测 precision,测不了 recall(漏报)**;别据此让 redteam 变胆小。
  3. `uncertain` 归错或 escalation 未决会轻微污染 `real`。

## 返回宿主
只回 3 行:记分卡是否写入 + `<session>.md` 路径 + 一条最高信号的工具缺陷(若有)。**不回长文**。

## 毕业
中枢是**原始观测池**。值得动手的缺陷/趋势由人**手工毕业**进 redteam 的 `.doc/TODO/`（summary+followup）/`.doc/roadmap/`——不自动写。
