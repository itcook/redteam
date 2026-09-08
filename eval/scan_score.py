#!/usr/bin/env python3
"""scanner 疑点对 oracle 的离线回归评分（M21a 批 D-5，design §10.7 离线回归集；零依赖）。改模板必跑。

  scan_score.py score <input> [<input> ...] --oracle <oracle.json> --repo <dir> --head <sha>
                      [--set union] [--out score.json] [--baseline prev-score.json] [--fail-on-regression]
  scan_score.py selftest

<input> 三种形态自动识别：
  1. redteam-next 结果 JSON：{"cells": [{"output": {"role": "scanner", "suspicions": [...]}, ...}]}——全部 scanner cell 的疑点合为一次运行；
  2. E3 run JSON：{"run": n, "items": [...]}（`e3_contract_ab.py run` 产出）；目录则取其中全部 run-*.json；
  3. 裸 ScanOutput：{"suspicions": [...]}。
口径与 E3 B 臂一致（`_oracle.py`）：quote 去空白子串定位（定位失败按自报行号、计 unanchored）→ 与 oracle 条目 anchors 区间重叠即命中。
基线（P0 E3 B 臂三跑，oracle-pr181 union）：union_oracle_hit 9、mean_oracle_hit 5.33、mean_noise 0.55。
被评仓路径 / oracle / 原始输出一律参数传入；本脚本与产出不含被评仓内容。"""
import argparse
import json
import os
import re
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _oracle import anchor_quote, load_oracle, match_entries  # noqa: E402


def load_runs(paths):
    """输入 → [(name, items, meta)]。"""
    runs = []
    for p in paths:
        if os.path.isdir(p):
            for f in sorted(x for x in os.listdir(p) if re.match(r"run-\d+\.json$", x)):
                runs.extend(load_runs([os.path.join(p, f)]))
            continue
        d = json.load(open(p, encoding="utf-8"))
        name = os.path.basename(p)
        if isinstance(d, dict) and "cells" in d:
            items = []
            for c in d.get("cells", []):
                out = c.get("output") or {}
                if out.get("role") == "scanner":
                    items.extend(out.get("suspicions", []))
            runs.append((name, items, {"cells": len(d.get("cells", []))}))
        elif isinstance(d, dict) and "items" in d:
            runs.append((f"{name}#run{d.get('run', '')}", d.get("items", []), {"wall_s": d.get("wall_s"), "parse_ok": d.get("parse_ok")}))
        elif isinstance(d, dict) and "suspicions" in d:
            runs.append((name, d.get("suspicions", []), {}))
        else:
            raise SystemExit(f"{p}: 不认识的输入形态（需 cells[] / items[] / suspicions[]）")
    return runs


def score_runs(runs, entries, repo, head):
    per_run, all_hit_ids, table = [], set(), []
    for name, items, meta in runs:
        hits, unanch, run_hit_ids = 0, 0, set()
        for n, it in enumerate(items, 1):
            path = it.get("file") or ""
            rng = anchor_quote(repo, head, path, it.get("quote", ""))
            anchored = rng is not None
            if rng is None:
                unanch += 1
                try:
                    rng = (int(it.get("line_start") or 1), int(it.get("line_end") or it.get("line_start") or 1))
                except (TypeError, ValueError):
                    rng = (1, 1)
            matched = match_entries(entries, path, rng)
            if matched:
                hits += 1
                run_hit_ids |= {e["id"] for e in matched}
            table.append({"run": name, "n": n, "file": path, "lines": list(rng), "anchored": anchored,
                          "confidence": it.get("confidence"), "category": it.get("category"),
                          "hint": (it.get("hint") or "")[:140],
                          "auto_match": sorted({f"{e['id']}:{e.get('topic') or ''}" for e in matched})})
        all_hit_ids |= run_hit_ids
        per_run.append({"run": name, "items": len(items), "hit_items": hits, "n_oracle_hit": len(run_hit_ids),
                        "oracle_ids_hit": sorted(run_hit_ids),
                        "noise": round((len(items) - hits) / len(items), 3) if items else None,
                        "unanchored": unanch, **meta})
    mean = lambda k: round(sum(x[k] for x in per_run if x.get(k) is not None) / max(1, sum(1 for x in per_run if x.get(k) is not None)), 2)
    summary = {"oracle_n": len(entries), "runs": per_run, "mean_items": mean("items"), "mean_oracle_hit": mean("n_oracle_hit"),
               "union_oracle_hit": len(all_hit_ids), "union_ids": sorted(all_hit_ids), "mean_noise": mean("noise"),
               "mean_unanchored": mean("unanchored")}
    return summary, table


def cmd_score(a):
    entries, oracle_head = load_oracle(a.oracle, a.set)
    head = a.head or oracle_head
    if not head:
        raise SystemExit("--head 缺失且 oracle 无 head")
    runs = load_runs(a.inputs)
    if not runs:
        raise SystemExit("无输入运行")
    summary, table = score_runs(runs, entries, a.repo, head)
    summary["set"] = a.set
    line = (f"runs={len(runs)} oracle_n={summary['oracle_n']} mean_items={summary['mean_items']} mean_hit={summary['mean_oracle_hit']} "
            f"union_hit={summary['union_oracle_hit']} mean_noise={summary['mean_noise']} mean_unanchored={summary['mean_unanchored']}")
    regressed = False
    if a.baseline:
        b = json.load(open(a.baseline, encoding="utf-8"))
        b = b.get("summary", b)
        d_union = summary["union_oracle_hit"] - int(b.get("union_oracle_hit") or 0)
        d_mean = round(summary["mean_oracle_hit"] - float(b.get("mean_oracle_hit") or 0), 2)
        d_noise = round(summary["mean_noise"] - float(b.get("mean_noise") or 0), 3) if summary["mean_noise"] is not None else None
        summary["delta"] = {"union_oracle_hit": d_union, "mean_oracle_hit": d_mean, "mean_noise": d_noise}
        line += f"\nΔ vs baseline: union_hit {d_union:+d} mean_hit {d_mean:+.2f} mean_noise {d_noise:+.3f}" if d_noise is not None else f"\nΔ vs baseline: union_hit {d_union:+d} mean_hit {d_mean:+.2f}"
        regressed = d_union < 0
    if a.out:
        json.dump({"summary": summary, "table": table}, open(a.out, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    print(line)
    if a.fail_on_regression and regressed:
        print("REGRESSION: union_oracle_hit 低于基线", file=sys.stderr)
        return 1
    return 0


def cmd_selftest(_a):
    """合成仓 + 合成 oracle：quote 命中 / 自报行号命中（unanchored）/ 噪音各一。"""
    with tempfile.TemporaryDirectory() as d:
        repo = os.path.join(d, "repo")
        os.makedirs(os.path.join(repo, "src"))
        src = "fn main() {}\nlet token = secret();\nok();\nfn other() {}\ndb.write(token);\nx();\ny();\nz();\nnoise();\n"
        open(os.path.join(repo, "src", "a.rs"), "w", encoding="utf-8").write(src)
        for cmd in (["init", "-q", "-b", "main"], ["add", "-A"], ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "c"]):
            subprocess.run(["git", "-C", repo] + cmd, check=True, capture_output=True)
        head = subprocess.run(["git", "-C", repo, "rev-parse", "HEAD"], capture_output=True, text=True, check=True).stdout.strip()
        oracle = {"kind": "selftest", "head": head, "sets": {"union": ["A", "B"]}, "entries": [
            {"id": "A", "anchors": [{"file": "src/a.rs", "line_start": 2, "line_end": 2}], "topic": "leak"},
            {"id": "B", "anchors": [{"file": "src/a.rs", "line_start": 5, "line_end": 5}], "topic": "write"},
        ]}
        opath = os.path.join(d, "oracle.json")
        json.dump(oracle, open(opath, "w"))
        run = {"suspicions": [
            {"file": "src/a.rs", "line_start": 99, "line_end": 99, "hint": "h", "confidence": 0.5, "quote": "let token=secret();"},
            {"file": "src/a.rs", "line_start": 5, "line_end": 5, "hint": "h", "confidence": 0.5, "quote": "not in file"},
            {"file": "src/a.rs", "line_start": 9, "line_end": 9, "hint": "h", "confidence": 0.5, "quote": "noise();"},
        ]}
        rpath = os.path.join(d, "run.json")
        json.dump(run, open(rpath, "w"))
        entries, ohead = load_oracle(opath, "union")
        summary, table = score_runs(load_runs([rpath]), entries, repo, ohead)
        r = summary["runs"][0]
        assert (r["items"], r["hit_items"], r["unanchored"], summary["union_oracle_hit"]) == (3, 2, 1, 2), summary
        assert r["noise"] == round(1 / 3, 3), r
        assert table[0]["lines"] == [2, 2] and table[0]["anchored"], table[0]
        assert table[1]["lines"] == [5, 5] and not table[1]["anchored"], table[1]
        # redteam-next 结果形态
        res = {"cells": [{"output": {"role": "scanner", "suspicions": run["suspicions"][:1]}}, {"output": {"role": "reviewer", "findings": []}}]}
        rpath2 = os.path.join(d, "result.json")
        json.dump(res, open(rpath2, "w"))
        s2, _ = score_runs(load_runs([rpath2]), entries, repo, ohead)
        assert (s2["runs"][0]["items"], s2["union_oracle_hit"]) == (1, 1), s2
    print("selftest ok: hit/unanchored/noise + cells[] input")
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("score")
    s.add_argument("inputs", nargs="+")
    s.add_argument("--oracle", required=True)
    s.add_argument("--repo", required=True)
    s.add_argument("--head", default=None, help="缺省取 oracle.head")
    s.add_argument("--set", default="union")
    s.add_argument("--out", default=None)
    s.add_argument("--baseline", default=None, help="上次 score JSON（summary 或整体）")
    s.add_argument("--fail-on-regression", action="store_true", help="union_oracle_hit 低于基线时退出码 1")
    s.set_defaults(fn=cmd_score)
    t = sub.add_parser("selftest")
    t.set_defaults(fn=cmd_selftest)
    a = ap.parse_args()
    sys.exit(a.fn(a))


if __name__ == "__main__":
    main()
