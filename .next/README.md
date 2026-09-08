# .next — 分支随行文档区（feat/m21-review-engine）

2026-09-07 自主 checkout 私有工作区 `.doc/`（gitignored，工作树不可见）迁入，让本分支 / 工作树实施时不必切回主 checkout。
子目录沿用 `.doc/` 七区命名（CLAUDE.md「文档规范」），相对链接原样有效。

| 目录 | 内容 | 入库 |
|---|---|---|
| `roadmap/` | M21 路线图（原 `.roadmap/`）：README · P0 · M21a-e · followup | ✅ |
| `design/` | `2026-09-04-redteam-review-engine-architecture-design.md`（v7，P0 出口锁定） | ✅ |
| `decisions/` | 同名 decisions（D1-D10） | ✅ |
| `research/` | 同名 research（§0-14；§15/16 为指针）· `2026-09-07-p0-raw/`（本仓 oracle B 与分布语料、v4 报告） | ✅ |
| `reviews/` | `2026-09-04-b-roll-verdict.md`（aisdk vs 自建收发层） | ✅ |
| `plans/` | 里程碑实施计划：`2026-09-07-redteam-m21a-scan-foundation-plan.md`（六批 + 出口）；M21b-e 待派生 | ✅ |
| `private/` | **gitignored**：PR181 两份评审记录、P0 明细研究（§15/16 全文）、PR181 原始数据（pack / e3 / oracle / replay） | ❌ 远端公开仓，永不入库 |

- `private/` 只存在于本工作树磁盘；迁出前的原件已从 `.doc/` 移走，勿删工作树前先备份。
- design 锁定后派生的 M21a-e plan 放 `plans/`；主 checkout `.doc/roadmap/summary.md` 登记 M21 时链回本目录。
