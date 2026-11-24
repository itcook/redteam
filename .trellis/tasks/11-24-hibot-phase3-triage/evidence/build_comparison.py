#!/usr/bin/env python3
"""3.8 对拍证据：把 evidence/report.*.json 与人工 pr-triage 产物合成 COMPARISON.md + metrics.jsonl。"""
import glob
import json
import os
import re

HERE = os.path.dirname(__file__)
HIA = os.path.expanduser("~/hiapi")

REPORT_RE = re.compile(r"report\.(?:hiapi-(?P<repo>[a-z]+)-)?pr-(?P<pr>\d+)(?P<smoke>-smoke)?\.json$")


def human_baselines(repo, pr):
    rows = []
    for f in glob.glob(os.path.join(HIA, repo, ".rpiv/artifacts/triage/*.md")):
        txt = open(f, encoding="utf-8", errors="replace").read()
        def get(k):
            m = re.search(rf"^{k}:\s*(\S+)", txt, re.M)
            return m.group(1) if m else ""
        if get("pr_number") != str(pr):
            continue
        rows.append({
            "artifact": os.path.basename(f),
            "security_tier": int(get("security_flag") or 0),
            "blockers": int(get("blockers_count") or 0),
            "structural": int(get("structural_count") or 0),
            "local": int(get("local_count") or 0),
            "intent": int(get("intent_undelivered") or 0),
            "scope_creep": int(get("scope_creep_count") or 0),
            "risk": get("risk"),
            "drift": get("convention_drift"),
        })
    return rows


def main():
    metrics = []
    for f in sorted(glob.glob(os.path.join(HERE, "report.*.json"))):
        name = os.path.basename(f)
        m = REPORT_RE.match(name)
        if not m:
            continue
        repo = m.group("repo") or "sites"
        pr = int(m.group("pr"))
        r = json.load(open(f))
        rt = r.get("extensions", {}).get("redteam", {})
        axes = {a["axis"]: a for a in rt.get("axes", [])}
        metrics.append({
            "repo": repo,
            "pr": pr,
            "smoke": bool(m.group("smoke")),
            "model": next((x.get("model") for x in r.get("findings", [])), "deepseek"),
            "status": r.get("status"),
            "status_reason": r.get("status_reason"),
            "summary": r.get("summary"),
            "findings": len(r.get("findings") or []),
            "blockers": len(rt.get("blockers") or []),
            "usage": r.get("usage"),
            "axes": {k: {"ok": v.get("ok"), "rows": v.get("rows"), "cells": v.get("cells"),
                         "dropped_total": (v.get("dropped") or {}).get("dropped_total")}
                     for k, v in axes.items()},
            "report": name,
        })

    with open(os.path.join(HERE, "metrics.jsonl"), "w") as out:
        for m in metrics:
            out.write(json.dumps(m, ensure_ascii=False) + "\n")

    lines = []
    lines.append("# Phase 3.8 · 真实 PR 对拍（2026-11-24）\n")
    lines.append("模型：DeepSeek Flash（`deepseek-flash`，job 模式只从 env 取 key）；单次 ≤900s；"
                 "总跑数 8/10（含 1 次 GLM infra 失败）。原始报告见同目录 `report.*.json`。\n")
    lines.append("## 机读汇总\n")
    lines.append("| repo | PR | status | tier | blockers | struct | local | intent | creep | risk | drift | findings | in/out tokens | 报告 |")
    lines.append("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for m in metrics:
        if m["smoke"]:
            continue
        s = m["summary"] or {}
        u = m["usage"] or {}
        lines.append(
            f"| {m['repo']} | #{m['pr']} | {m['status']} | {s.get('security_tier','—')} | {s.get('blockers_count','—')} | "
            f"{s.get('structural_count','—')} | {s.get('local_count','—')} | {s.get('intent_undelivered','—')} | "
            f"{s.get('scope_creep_count','—')} | {s.get('risk','—')} | {s.get('convention_drift','—')} | "
            f"{m['findings']} | {u.get('input_tokens','—')}/{u.get('output_tokens','—')} | `{m['report']}` |"
        )
    lines.append("\n## 与人工 pr-triage 的差异\n")
    lines.append("人工值来自 `~/hiapi/<repo>/.rpiv/artifacts/triage/*.md` 的 frontmatter；同一 PR 多次分诊时全部列出。\n")
    for m in metrics:
        if m["smoke"]:
            continue
        hs = human_baselines(m["repo"], m["pr"])
        ours = m["summary"] or {}
        lines.append(f"### {m['repo']} #{m['pr']}\n")
        if not hs:
            lines.append("- 人工基线：无（仅作冒烟/精度观察）。")
        for h in hs:
            lines.append(
                f"- 人工（`{h['artifact']}`）：tier {h['security_tier']} / blockers {h['blockers']} "
                f"({h['structural']} struct + {h['intent']} intent) / local {h['local']} / creep {h['scope_creep']} / "
                f"risk {h['risk']} / drift {h['drift']}"
            )
        if hs:
            lines.append(
                f"- 本次：tier {ours.get('security_tier')} / blockers {ours.get('blockers_count')} "
                f"({ours.get('structural_count')} struct + {ours.get('intent_undelivered')} intent) / "
                f"local {ours.get('local_count')} / creep {ours.get('scope_creep_count')} / "
                f"risk {ours.get('risk')} / drift {ours.get('convention_drift')}"
            )
        lines.append("")

    lines.append("## 结论与已知差异\n")
    lines.append("1. **无「干净判 tier 2」**：4 个 tier0 干净 PR（sites #277/#235/#328、cli #18）本次全部 tier0。")
    lines.append("2. **tier2 漏报 1 例**：backend #333 人工 tier2（surface-4 LLM→特权写，conf 8–9）；DeepSeek 两轮均 0 行"
                 "（A2 已补「缓解不打断可达性」定义仍未命中），按协调者裁决记为 model/定义差异，不伪造人工结论。"
                 "GLM-5.3 尝试因 900s 墙钟 + `finish_reason=max_tokens, len=0` infra 失败，未形成对比。")
    lines.append("3. **risk 口径差异**：`blockers≥3 → high` 使 sites #235(人工 medium)、sites #328(medium)、cli #18(medium) 全部判 high；"
                 "该公式是 grill 定案的 `[拟议]` 规则，本次不改，记入差异。")
    lines.append("4. **漂移计数**：sites #235 总量与人工精确一致（7=7），但构成不同（我们 4 struct+3 intent，人工 3 struct+4 intent）；"
                 "sites #277 我们 3 blockers vs 人工 8（欠报 5）。")
    lines.append("5. **安全轴丢弃率**：backend #333 A 轮 1 行候选、1 行 <8 被丢（100%）；A2/A2 后 0 行候选。"
                 "sites #328 漂移轴 1 行 unanchored 被丢（已披露在 `axes[].dropped.dropped_rows`）。")
    lines.append("")
    open(os.path.join(HERE, "COMPARISON.md"), "w").write("\n".join(lines) + "\n")
    print(f"wrote {len(metrics)} metrics rows, {len(lines)} comparison lines")


if __name__ == "__main__":
    main()
