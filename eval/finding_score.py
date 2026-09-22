#!/usr/bin/env python3
"""findings × oracle 的主题级评分（M21b 出口验收 D4 / D5；零依赖）。

`scan_score.py` 评的是 scanner **疑点**；这里评 reviewer 产出的 **finding**，口径与 P0 同源：
quote 去空白定位 head 树 → 区间命中 oracle 条目（`_oracle.py`，E3 口径）→ 按条目的 `topic` 归到主题 →
**人工确认**「是不是在命名同一个缺陷机制」（同区域不同主张、只沾了后果侧的都不算）。

为什么按主题、不按条目：oracle 的条目本身来自若干次历史评审的产出（其中可能就有被比较的一方），
按条目比会让参与过构造 oracle 的那一方拿自己的答案给自己打分；主题是人工对齐过的缺陷机制，
同一缺陷换了措辞或锚点也归到一处。

两步：
  candidates  自动一步：逐条 finding 出候选主题，写确认表（JSON）——`verdict` 一栏留空待人工填 hit / miss；
              没落进任何 oracle 区间的 finding 单列 `outside`（不计分，供人看 oracle 有没有漏掉真问题）。
  judge       读填好的确认表 → 每轮 / 全部轮次覆盖了哪些主题、对照门槛与留出集。

用法：
  finding_score.py candidates <report.json>... --oracle <oracle.json> --repo <dir> [--head <sha>] [--set union] --out confirm.json
  finding_score.py judge confirm.json [--min-topics 6] [--baseline-topics T1,T2,...] [--holdout T9,T10,...] [--out judge.json]
  finding_score.py selftest

本脚本不含任何被评仓内容；仓路径 / head / oracle / 输出位置一律由参数给出（被评仓若是私有仓，输出请指向 gitignored 目录）。
"""
import argparse
import json
import os
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _oracle import anchor_quote, load_oracle, match_entries  # noqa: E402


def load_findings(path):
    """redteam-next / dev 的结果 JSON：顶层 `findings[]`。缺字段的条目照收（锚定时自然落空）。"""
    r = json.load(open(path, encoding="utf-8"))
    return r.get("findings") or []


def finding_key(run_idx, f):
    # id 在同一轮内唯一；跨轮同 id 是同一条（措辞一致）——确认表按「轮 × id」记，judge 再按主题去重。
    return f"r{run_idx}:{f.get('id') or f.get('title', '')[:40]}"


def cmd_candidates(a):
    entries, ohead = load_oracle(a.oracle, a.set)
    head = a.head or ohead
    if not head:
        raise SystemExit("缺 head：oracle 里没有，--head 也没给")
    by_id = {e["id"]: e for e in entries}
    rows = []
    for ri, path in enumerate(a.reports, start=1):
        for f in load_findings(path):
            file = f.get("file") or ""
            rng = anchor_quote(a.repo, head, file, f.get("quote") or "")
            anchored = rng is not None
            if not anchored:
                # E3 口径：定位失败按自报行号计（并标 unanchored），与 scan_score 同源
                rng = (int(f.get("line_start") or 0), int(f.get("line_end") or f.get("line_start") or 0))
            hits = match_entries(entries, file, rng) if rng[0] > 0 else []
            topics = sorted({e.get("topic") or "?" for e in hits})
            rows.append({
                "key": finding_key(ri, f),
                "run": ri,
                "id": f.get("id"),
                "file": file,
                "lines": list(rng),
                "anchored": anchored,
                "severity": f.get("severity"),
                "title": f.get("title"),
                "refuted": bool((f.get("verify") or {}).get("verdict") == "refuted"),
                # 每个候选主题配上 oracle 里那几条的标题，确认时对着读机制
                "candidates": [
                    {"topic": t, "oracle": [{"id": e["id"], "source": e.get("source"), "title": e.get("title")}
                                            for e in hits if (e.get("topic") or "?") == t]}
                    for t in topics
                ],
                "outside": not hits,
                # 人工填：{"<topic>": {"verdict": "hit" | "miss", "reason": "..."}}
                "verdict": {},
            })
    out = {"oracle": os.path.abspath(a.oracle), "set": a.set, "head": head, "reports": [os.path.abspath(p) for p in a.reports],
           "topics": sorted({e.get("topic") or "?" for e in by_id.values()}), "rows": rows}
    with open(a.out, "w", encoding="utf-8") as fh:
        json.dump(out, fh, ensure_ascii=False, indent=1)
    n_cand = sum(1 for r in rows if not r["outside"])
    print(f"findings {len(rows)}（{len(a.reports)} 轮）· 有候选主题 {n_cand} · oracle 外 {len(rows) - n_cand} "
          f"· 未锚定 {sum(1 for r in rows if not r['anchored'])} → {a.out}")
    return 0


def judge(confirm, min_topics, baseline, holdout):
    """确认表 → 主题覆盖。只认人工填了 `hit` 的；被 verifier 驳回的另计（它们不计入报告头条，但若命中的是真缺陷就是误驳）。"""
    runs = sorted({r["run"] for r in confirm["rows"]})
    per_run, per_run_refuted = {i: set() for i in runs}, {i: set() for i in runs}
    pending = 0
    for r in confirm["rows"]:
        for c in r["candidates"]:
            v = (r.get("verdict") or {}).get(c["topic"])
            if not v:
                pending += 1
                continue
            if v.get("verdict") == "hit":
                (per_run_refuted if r["refuted"] else per_run)[r["run"]].add(c["topic"])
    union = set().union(*per_run.values()) if per_run else set()
    res = {
        "pending": pending,
        "runs": {str(i): sorted(per_run[i]) for i in runs},
        "per_run_counts": {str(i): len(per_run[i]) for i in runs},
        "union": sorted(union),
        "union_count": len(union),
        # 驳回掉的真主题：verifier 把一条人工确认过的真缺陷驳了——D2 的误驳侧证
        "refuted_real_topics": {str(i): sorted(per_run_refuted[i] - per_run[i]) for i in runs},
        "min_topics": min_topics,
        "each_run_passes_min": all(len(per_run[i]) >= min_topics for i in runs) if runs else None,
    }
    if baseline is not None:
        res["baseline_count"] = len(baseline)
        res["union_ge_baseline"] = len(union) >= len(baseline)
        res["missed_vs_baseline"] = sorted(set(baseline) - union)
        res["beyond_baseline"] = sorted(union - set(baseline))
    if holdout is not None:
        res["holdout"] = sorted(holdout)
        res["holdout_hit"] = sorted(union & set(holdout))
    return res


def _topic_list(s):
    return [t.strip() for t in s.split(",") if t.strip()] if s else None


def _expand(prefixes, topics):
    """`T1,T9` 这种简写 → oracle 里的完整主题名（按「名字以 T1- 开头」匹配）；已是全名的原样保留。"""
    if prefixes is None:
        return None
    out = []
    for p in prefixes:
        full = [t for t in topics if t == p or t.startswith(p + "-")]
        if not full:
            raise SystemExit(f"主题 {p!r} 在 oracle 里找不到")
        out.extend(full)
    return out


def cmd_judge(a):
    confirm = json.load(open(a.confirm, encoding="utf-8"))
    topics = confirm.get("topics") or []
    res = judge(confirm, a.min_topics, _expand(_topic_list(a.baseline_topics), topics), _expand(_topic_list(a.holdout), topics))
    if res["pending"]:
        print(f"⚠ 还有 {res['pending']} 个候选没填 verdict——以下只算已确认的")
    for i, c in res["per_run_counts"].items():
        print(f"第 {i} 轮：{c} 个主题  {res['runs'][i]}")
    print(f"全部轮次并集：{res['union_count']} 个主题；每轮 ≥ {a.min_topics}：{res['each_run_passes_min']}")
    if "baseline_count" in res:
        print(f"对照基线 {res['baseline_count']} 个主题：并集 ≥ 基线 = {res['union_ge_baseline']}；"
              f"漏 {res['missed_vs_baseline']}；超出 {res['beyond_baseline']}")
    if "holdout" in res:
        print(f"留出集命中：{res['holdout_hit']} / {res['holdout']}")
    refuted = {k: v for k, v in res["refuted_real_topics"].items() if v}
    if refuted:
        print(f"被 verifier 驳回的真主题（误驳侧证）：{refuted}")
    if a.out:
        with open(a.out, "w", encoding="utf-8") as fh:
            json.dump(res, fh, ensure_ascii=False, indent=1)
    return 0


def cmd_selftest(_a):
    """合成仓 + 合成 oracle：命中 / 未锚定按自报行号命中 / oracle 外 / 驳回的真主题 / 门槛与留出集。"""
    with tempfile.TemporaryDirectory() as d:
        repo = os.path.join(d, "repo")
        os.makedirs(os.path.join(repo, "src"))
        src = "fn main() {}\nlet token = secret();\nok();\nfn other() {}\ndb.write(token);\nx();\ny();\nz();\nnoise();\n"
        open(os.path.join(repo, "src", "a.rs"), "w", encoding="utf-8").write(src)
        for cmd in (["init", "-q", "-b", "main"], ["add", "-A"],
                    ["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "c"]):
            subprocess.run(["git", "-C", repo] + cmd, check=True, capture_output=True)
        head = subprocess.run(["git", "-C", repo, "rev-parse", "HEAD"], capture_output=True, text=True, check=True).stdout.strip()
        oracle = {"kind": "selftest", "head": head, "sets": {"union": ["A", "B", "C"]}, "entries": [
            {"id": "A", "source": "x", "title": "token leak", "anchors": [{"file": "src/a.rs", "line_start": 2, "line_end": 2}], "topic": "T1-leak"},
            {"id": "B", "source": "y", "title": "unchecked write", "anchors": [{"file": "src/a.rs", "line_start": 5, "line_end": 5}], "topic": "T2-write"},
            {"id": "C", "source": "z", "title": "test gap", "anchors": [{"file": "src/a.rs", "line_start": 7, "line_end": 8}], "topic": "T9-tests"},
        ]}
        opath = os.path.join(d, "oracle.json")
        json.dump(oracle, open(opath, "w", encoding="utf-8"))
        rep = {"findings": [
            {"id": "f1", "file": "src/a.rs", "line_start": 99, "line_end": 99, "title": "leak", "quote": "let token=secret();"},
            {"id": "f2", "file": "src/a.rs", "line_start": 5, "line_end": 5, "title": "write", "quote": "not in file"},
            {"id": "f3", "file": "src/a.rs", "line_start": 9, "line_end": 9, "title": "noise", "quote": "noise();"},
            {"id": "f4", "file": "src/a.rs", "line_start": 7, "line_end": 7, "title": "tests", "quote": "y();",
             "verify": {"verdict": "refuted", "model": "v", "note": "n"}},
        ]}
        rpath = os.path.join(d, "rep.json")
        json.dump(rep, open(rpath, "w", encoding="utf-8"))
        cpath = os.path.join(d, "confirm.json")
        ns = argparse.Namespace(reports=[rpath], oracle=opath, repo=repo, head=None, set="union", out=cpath)
        assert cmd_candidates(ns) == 0
        c = json.load(open(cpath, encoding="utf-8"))
        rows = {r["id"]: r for r in c["rows"]}
        assert rows["f1"]["anchored"] and rows["f1"]["lines"] == [2, 2], rows["f1"]
        assert [x["topic"] for x in rows["f1"]["candidates"]] == ["T1-leak"], rows["f1"]
        assert not rows["f2"]["anchored"] and [x["topic"] for x in rows["f2"]["candidates"]] == ["T2-write"], "未锚定按自报行号"
        assert rows["f3"]["outside"] and not rows["f3"]["candidates"], "oracle 外单列"
        assert rows["f4"]["refuted"] and [x["topic"] for x in rows["f4"]["candidates"]] == ["T9-tests"], rows["f4"]

        # 未填 verdict → 什么都不算
        r0 = judge(c, 2, None, None)
        assert r0["pending"] == 3 and r0["union_count"] == 0, r0
        # 人工：f1 算、f2 不算（同区域不同主张）、f4 算但被驳回
        rows["f1"]["verdict"] = {"T1-leak": {"verdict": "hit", "reason": "同一机制"}}
        rows["f2"]["verdict"] = {"T2-write": {"verdict": "miss", "reason": "同区域不同主张"}}
        rows["f4"]["verdict"] = {"T9-tests": {"verdict": "hit", "reason": "同一机制"}}
        r1 = judge(c, 2, ["T1-leak", "T2-write"], ["T9-tests"])
        assert r1["pending"] == 0 and r1["union"] == ["T1-leak"], r1
        assert r1["each_run_passes_min"] is False and r1["union_ge_baseline"] is False, r1
        assert r1["missed_vs_baseline"] == ["T2-write"], r1
        assert r1["refuted_real_topics"] == {"1": ["T9-tests"]}, "被驳回的真主题不计入覆盖、但要单列"
        assert r1["holdout_hit"] == [], "驳回掉的不算留出集命中"
        assert _expand(["T1", "T9"], c["topics"]) == ["T1-leak", "T9-tests"], "主题简写展开"
    print("selftest ok: candidates（命中 / 未锚定 / oracle 外 / 驳回）+ judge（待填 / 门槛 / 基线 / 留出集 / 误驳侧证）")
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    c = sub.add_parser("candidates", help="自动一步：逐条 finding 出候选主题，写确认表")
    c.add_argument("reports", nargs="+", help="结果 JSON（顶层 findings[]），每个算一轮")
    c.add_argument("--oracle", required=True)
    c.add_argument("--repo", required=True, help="被评仓（取 head 树内容做 quote 定位）")
    c.add_argument("--head", help="缺省取 oracle 里的 head")
    c.add_argument("--set", default="union")
    c.add_argument("--out", required=True)
    c.set_defaults(fn=cmd_candidates)

    j = sub.add_parser("judge", help="读填好的确认表 → 主题覆盖与门槛")
    j.add_argument("confirm")
    j.add_argument("--min-topics", type=int, default=6, help="每轮至少覆盖的主题数（出口验收第 1 项）")
    j.add_argument("--baseline-topics", help="基线覆盖的主题（逗号分隔，可简写 T1,T2）；给了就报并集 ≥ 基线否")
    j.add_argument("--holdout", help="留出集主题（逗号分隔，可简写）")
    j.add_argument("--out")
    j.set_defaults(fn=cmd_judge)

    s = sub.add_parser("selftest", help="合成仓自检")
    s.set_defaults(fn=cmd_selftest)

    a = ap.parse_args()
    sys.exit(a.fn(a))


if __name__ == "__main__":
    main()
