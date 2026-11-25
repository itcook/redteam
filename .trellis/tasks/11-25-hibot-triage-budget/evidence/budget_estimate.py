#!/usr/bin/env python3
"""triage 整轮预算包线候选：A（串行求和）与 B（规模连续）对照计算。

数据来源（全部归档在本目录，可离线复核）：
- diff.hiapi-<repo>-pr-<n>.patch   历史运行的精确 diff_patch（与 /tmp/hibot-triage-eval/jobs 原件逐字节一致）
- pr-context.hiapi-<repo>-pr-<n>.json  历史运行的精确 pr_context
- manifest.hiapi-<repo>-pr-<n>.json   历史运行的 manifest（limits.wall_clock_s）
- metrics.phase3-8.jsonl           5 个 DeepSeek ok 轮的整轮墙钟
- report.glm54-1800c.json          backend #333 GLM-5.4 在 1800s + 1.5x 下唯一 ok 轮（1050.125s）

口径：
- 规模：diff 字节 = 历史 diff_patch 的 UTF-8 字节；files = `diff --git` 边界数；
  modules = 文件 parent dir（根文件 → "."，与 standards::module_of 一致）；
  text = title + body + linked issues(title+body) 的 UTF-8 字节（本任务要求的规模口径）。
- A 的 I0 输入沿用现有实现口径：body + linked_issues.body + 非 bot 且非作者的 comments。
- 公式复刻 src/triage/budget.rs 的常量与 clamp；分片复刻 balanced_chunks_weighted。
- 历史 ok 样本的 manifest 是 900/1200（time_factor=1.0）；本表按目标场景 manifest=1800
  （time_factor=1.5）统一计算，并另行列「按历史 manifest 计算的 A」。
- 这些值是**预算包线对照**，不是耗时预测。
"""
import json
import math
import os

HERE = os.path.dirname(os.path.abspath(__file__))

# ---- src/triage/budget.rs 常量（v1） ----
BASE_CELL_S = 60.0
PER_KB_S = 6.0
SAFETY = 1.15
MIN_CELL_S = 120.0
MAX_CELL_S = 600.0
I0_BASE_S = 120.0
I0_MAX_S = 240.0
I0_PER_KB_S = 2.0
TARGET_SLICE_KB = 45.0
MAX_SHARDS = 4
INTENT_EXTRA_S = 20.0
INTENT_PER_CLAIM_S = 0.5
DRIFT_PER_PACK_KB_S = 1.0

# ---- A 公式的显式假设（整轮 deadline 在 I0 前确定，claims 数未知）----
CLAIMS_UPPER_BOUND = 20          # claims 上界假设（成本 0.5s/条）
DRIFT_PACK_UPPER_KB = 10.0       # 单模块 standards pack 上界假设
CLOSE_OUT_S = 30.0               # 锚定/去重/tally/报告收口固定开销

# ---- B 公式系数（经验候选）----
B1 = dict(base=180.0, per_diff_kb=PER_KB_S, per_file=6.0, per_text_kb=2.0)
B2 = dict(base=220.0, per_diff_kb=7.5, per_file=7.5, per_text_kb=2.5)
B_MIN_TOTAL = 900.0              # 已批准：min(manifest, 900)


def kb(b):
    return b / 1024.0


def time_factor(cap):
    return min(1.5, max(1.0, cap / 1200.0))


def cell_timeout(diff_kb, extra_s):
    raw = (BASE_CELL_S + PER_KB_S * diff_kb + extra_s) * SAFETY
    return min(MAX_CELL_S, max(MIN_CELL_S, raw))


def scaled_cell(diff_kb, extra_s, f):
    return min(900.0, cell_timeout(diff_kb, extra_s) * min(1.5, max(1.0, f)))


def i0_timeout(text_kb):
    return min(I0_MAX_S, max(I0_BASE_S, I0_BASE_S + I0_PER_KB_S * text_kb))


def scaled_i0(text_kb, f):
    return min(360.0, i0_timeout(text_kb) * min(1.5, max(1.0, f)))


def shard_count(total_kb, files):
    if files == 0:
        return 1
    by_size = math.ceil(total_kb / TARGET_SLICE_KB)
    by_files = math.ceil(files / 6)
    return max(1, min(MAX_SHARDS, min(by_size, by_files)))


def balanced_chunks_weighted(items, n):
    """items: [(key, weight_bytes)]；复刻贪婪最轻 bin。"""
    if not items:
        return [[]]
    n = max(1, min(n, len(items)))
    bins = [[] for _ in range(n)]
    sums = [0] * n
    for key, w in items:
        i = min(range(n), key=lambda j: sums[j])
        sums[i] += w
        bins[i].append((key, w))
    return [b for b in bins if b]


def parse_patch(path):
    text = open(path, encoding="utf-8", errors="replace").read()
    files = []
    cur_path = None
    cur_lines = []
    for line in text.splitlines(keepends=True):
        if line.startswith("diff --git "):
            if cur_path is not None:
                files.append((cur_path, len("".join(cur_lines).encode("utf-8"))))
            rest = line[len("diff --git "):]
            a = rest.split(" b/")[0]
            cur_path = a[2:] if a.startswith("a/") else a
            cur_lines = [line]
        else:
            cur_lines.append(line)
    if cur_path is not None:
        files.append((cur_path, len("".join(cur_lines).encode("utf-8"))))
    return files


def module_of(path):
    d = os.path.dirname(path)
    return d if d else "."


def pr_text_bytes(pr):
    def b(s):
        return len((s or "").encode("utf-8"))
    return b(pr.get("title")) + b(pr.get("body")) + sum(
        b(i.get("title")) + b(i.get("body")) for i in (pr.get("linked_issues") or [])
    )


def i0_text_bytes(pr):
    def b(s):
        return len((s or "").encode("utf-8"))
    return (
        b(pr.get("body"))
        + sum(b(i.get("body")) for i in (pr.get("linked_issues") or []))
        + sum(
            b(c.get("body"))
            for c in (pr.get("comments") or [])
            if not c.get("is_bot") and c.get("author") != pr.get("author")
        )
    )


def estimate_a(files, pr, manifest_cap):
    """串行求和包线：I0 + Σ(security cells) + Σ(drift cells) + Σ(intent slices) + 收口。"""
    f = time_factor(manifest_cap)
    text_kb = kb(i0_text_bytes(pr))
    i0 = scaled_i0(text_kb, f)
    if not files:
        return math.ceil(i0 + CLOSE_OUT_S), dict(i0=i0, n_sec=0, n_drift=0)
    total_kb = kb(sum(w for _, w in files))
    n_sec = shard_count(total_kb, len(files))
    modules = sorted({module_of(p) for p, _ in files})
    n_drift = min(n_sec, max(len(modules), 1))

    sec_shards = balanced_chunks_weighted(files, n_sec)
    sec_cells = sum(scaled_cell(kb(sum(w for _, w in shard)), 0.0, f) for shard in sec_shards)

    mod_bytes = {m: sum(w for p, w in files if module_of(p) == m) for m in modules}
    drift_shards = balanced_chunks_weighted([(m, mod_bytes[m]) for m in modules], n_drift)
    drift_cells = sum(
        scaled_cell(kb(sum(mod_bytes[m] for m, _ in shard)), DRIFT_PACK_UPPER_KB * DRIFT_PER_PACK_KB_S, f)
        for shard in drift_shards
    )

    intent_extra = INTENT_EXTRA_S + CLAIMS_UPPER_BOUND * INTENT_PER_CLAIM_S
    intent_cells = sum(scaled_cell(kb(sum(w for _, w in shard)), intent_extra, f) for shard in sec_shards)

    total = i0 + sec_cells + drift_cells + intent_cells + CLOSE_OUT_S
    return math.ceil(total), dict(i0=i0, n_sec=n_sec, n_drift=n_drift, sec=sec_cells, drift=drift_cells, intent=intent_cells)


def estimate_b(diff_kb, n_files, text_kb, coeff, manifest_cap):
    raw = coeff["base"] + coeff["per_diff_kb"] * diff_kb + coeff["per_file"] * n_files + coeff["per_text_kb"] * text_kb
    return raw, math.ceil(min(manifest_cap, max(B_MIN_TOTAL, raw)))

SAMPLE_JOBS = {
    "backend#333": ("backend", 333, "report.glm54-1800c.json", 1050.125),
    "cli#18": ("cli", 18, None, None),
    "sites#235": ("sites", 235, None, None),
    "sites#277": ("sites", 277, None, None),
    "sites#328": ("sites", 328, None, None),
}

TARGET_MANIFEST = 1800


def main():
    metrics = {}
    for line in open(os.path.join(HERE, "metrics.phase3-8.jsonl"), encoding="utf-8"):
        d = json.loads(line)
        if not d.get("smoke"):
            metrics[f"{d['repo']}#{d['pr']}"] = d

    rows = []
    for label, (repo, pr, _, glm_s) in SAMPLE_JOBS.items():
        patch = parse_patch(os.path.join(HERE, f"diff.hiapi-{repo}-pr-{pr}.patch"))
        prc = json.load(open(os.path.join(HERE, f"pr-context.hiapi-{repo}-pr-{pr}.json"), encoding="utf-8"))
        man = json.load(open(os.path.join(HERE, f"manifest.hiapi-{repo}-pr-{pr}.json"), encoding="utf-8"))
        hist_cap = man["limits"]["wall_clock_s"]
        diff_bytes = os.path.getsize(os.path.join(HERE, f"diff.hiapi-{repo}-pr-{pr}.patch"))
        text_b = pr_text_bytes(prc)
        a1800, detail = estimate_a(patch, prc, TARGET_MANIFEST)
        a_hist, _ = estimate_a(patch, prc, hist_cap)
        b1_raw, b1 = estimate_b(kb(diff_bytes), len(patch), kb(text_b), B1, TARGET_MANIFEST)
        b2_raw, b2 = estimate_b(kb(diff_bytes), len(patch), kb(text_b), B2, TARGET_MANIFEST)
        m = metrics.get(label) or {}
        rows.append(dict(label=label, repo=repo, pr=pr, hist_cap=hist_cap,
                         diff_bytes=diff_bytes, files=len(patch),
                         modules=len({module_of(p) for p, _ in patch}),
                         text_b=text_b, i0_text_b=i0_text_bytes(prc),
                         a1800=a1800, a_hist=a_hist, a_detail=detail,
                         b1_raw=b1_raw, b1=b1, b2_raw=b2_raw, b2=b2,
                         deepseek_s=(m.get("usage") or {}).get("wall_clock_ms", 0) / 1000.0,
                         status=m.get("status"), glm_s=glm_s))

    # 小 PR / 空 diff 示例
    examples = []
    for name, diff_b, n_files, text_b in [
        ("小 PR（1 文件/1KB）", 1024, 1, 300),
        ("空 diff（仅声明核对）", 0, 0, 300),
    ]:
        fake_files = [("src/x.rs", diff_b)] if n_files else []
        fake_pr = {"title": "", "body": "x" * text_b, "comments": [], "linked_issues": [], "author": "a"}
        a, det = estimate_a(fake_files, fake_pr, TARGET_MANIFEST)
        b1_raw, b1 = estimate_b(kb(diff_b), n_files, kb(text_b), B1, TARGET_MANIFEST)
        b2_raw, b2 = estimate_b(kb(diff_b), n_files, kb(text_b), B2, TARGET_MANIFEST)
        examples.append(dict(name=name, a=a, b1_raw=b1_raw, b1=b1, b2_raw=b2_raw, b2=b2, detail=det))

    out = []
    out.append("| PR | diff KB | files | modules | text B | A 原始 | A→1800 | A→历史manifest | B1 原始 | B1→1800 | B2 原始 | B2→1800 | 实测 DeepSeek | GLM 锚点 |")
    out.append("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for r in rows:
        out.append(
            f"| {r['label']} | {kb(r['diff_bytes']):.1f} | {r['files']} | {r['modules']} | {r['text_b']} "
            f"| {r['a1800']} | {min(TARGET_MANIFEST, r['a1800'])} | {min(r['hist_cap'], r['a_hist'])}（cap {r['hist_cap']}） "
            f"| {r['b1_raw']:.1f} | {r['b1']} | {r['b2_raw']:.1f} | {r['b2']} "
            f"| {r['deepseek_s']:.1f}s ({r['status']}) | {(str(round(r['glm_s'], 1)) + 's') if r['glm_s'] else '—'} |"
        )
    out.append("")
    out.append("| 示例 | A 原始 | A→1800 | B1 原始 | B1→1800 | B2 原始 | B2→1800 |")
    out.append("|---|---|---|---|---|---|---|")
    for e in examples:
        out.append(
            f"| {e['name']} | {e['a']} | {min(TARGET_MANIFEST, e['a'])} "
            f"| {e['b1_raw']:.1f} | {e['b1']} | {e['b2_raw']:.1f} | {e['b2']} |"
        )
    out.append("")
    out.append("A 明细（manifest=1800）：")
    for r in rows:
        d = r["a_detail"]
        out.append(
            f"- {r['label']}: i0={d['i0']:.0f}s n_sec={d['n_sec']} n_drift={d['n_drift']} "
            f"Σsec={d.get('sec', 0):.0f} Σdrift={d.get('drift', 0):.0f} Σintent={d.get('intent', 0):.0f} + 收口 {CLOSE_OUT_S:.0f}s"
        )
    print("\n".join(out))


if __name__ == "__main__":
    main()
