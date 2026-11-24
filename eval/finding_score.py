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
    """redteam / dev 的结果 JSON：顶层 `findings[]`。缺字段的条目照收（锚定时自然落空）。"""
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
            # 行号来源（出口验收评审 #3）：**优先采信报告里的行号**——产品落盘前已经用 `locate_quote_near`
            # 按自报行号就近锚定、并改写了 line_start / line_end；而 `anchor_quote` 取的是去空白后
            # 「文件里第一次出现」，`return Ok(());` 这类重复短句会被挪到文件更早的别处，候选主题随之错位。
            # 只有报告没给行号时才退回按 quote 定位（scan_score 的 E3 口径）。
            ls, le = int(f.get("line_start") or 0), int(f.get("line_end") or f.get("line_start") or 0)
            if ls > 0:
                rng, lines_from = (ls, max(le, ls)), "report"
            else:
                q = anchor_quote(a.repo, head, file, f.get("quote") or "")
                rng, lines_from = (q, "quote") if q else ((0, 0), None)
            anchored = lines_from is not None
            # 行容差（2026-09-23 尸检定案）：候选表是**给人判**的，召回优先、精度由人工确认那一步保证。
            # 严格区间重叠会把「同一缺陷、锚点偏几行」的 finding 静默丢掉——实测 T10 就是这么丢的：
            # 四轮报告都有「checkpoint 把所有 mismatch 塌成一个不可区分的 collision」这条，
            # 与 oracle 的「checkpoint 元数据可诊断性」是同一机制，只因锚点差 10–15 行而从未进过确认表。
            # 近旁命中标 `match: "near"`，人工确认时自己判是不是同一个机制。
            exact_hits = match_entries(entries, file, rng) if anchored else []
            hits = match_entries(entries, file, (max(1, rng[0] - a.tolerance), rng[1] + a.tolerance)) if anchored else []
            exact_ids = {e["id"] for e in exact_hits}
            topics = sorted({e.get("topic") or "?" for e in hits})
            rows.append({
                "key": finding_key(ri, f),
                "run": ri,
                "id": f.get("id"),
                "file": file,
                "lines": list(rng),
                "anchored": anchored,
                "lines_from": lines_from,
                "severity": f.get("severity"),
                "title": f.get("title"),
                "refuted": bool((f.get("refute") or f.get("verify") or {}).get("verdict") == "refuted"),
                # 每个候选主题配上 oracle 里那几条的标题，确认时对着读机制
                "candidates": [
                    {"topic": t,
                     # 该主题下是否有**严格重叠**的条目；全是近旁则标 near（确认时更该逐条读机制）
                     "match": "exact" if any(e["id"] in exact_ids for e in hits if (e.get("topic") or "?") == t) else "near",
                     "oracle": [{"id": e["id"], "source": e.get("source"), "title": e.get("title"),
                                 "match": "exact" if e["id"] in exact_ids else "near",
                                 "lines": [e["anchors"][0]["line_start"], e["anchors"][0]["line_end"]] if e.get("anchors") else None}
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
    n_near = sum(1 for r in rows for c in r["candidates"] if c["match"] == "near")
    print(f"findings {len(rows)}（{len(a.reports)} 轮）· 有候选主题 {n_cand}（其中近旁命中 {n_near} 个主题槽，容差 ±{a.tolerance} 行）"
          f"· oracle 外 {len(rows) - n_cand} · 未锚定 {sum(1 for r in rows if not r['anchored'])} → {a.out}")
    return 0


def judge(confirm, min_topics, baseline, holdout):
    """确认表 → 主题覆盖。只认人工填了 `hit` 的；被 verifier 驳回的另计（它们不计入报告头条，但若命中的是真缺陷就是误驳）。"""
    # 轮次按**传入的报告数**定（出口验收评审 #2）：只从 rows 推会让零产出的那一轮整个消失，
    # 「每轮 ≥ N 主题」于是只在有产出的轮上判——恰恰漏掉最该拦的那一轮。
    runs = list(range(1, len(confirm.get("reports") or []) + 1)) or sorted({r["run"] for r in confirm["rows"]})
    per_run, per_run_refuted = {i: set() for i in runs}, {i: set() for i in runs}
    pending = 0
    bad = []
    for r in confirm["rows"]:
        verdicts = r.get("verdict") or {}
        hits_in_row = []
        for c in r["candidates"]:
            v = verdicts.get(c["topic"])
            if not v:
                pending += 1
                continue
            # 值只认 hit / miss（评审 #5）：写成 "Hit" / "yes"、或只填了 reason 没填 verdict，
            # 旧逻辑会静默算成 miss——覆盖数偏低，还不触发「还有未填」的提示。
            val = v.get("verdict") if isinstance(v, dict) else None
            if val not in ("hit", "miss"):
                bad.append(f"{r['key']} / {c['topic']}: verdict = {val!r}（只认 hit / miss）")
                continue
            if val == "hit":
                hits_in_row.append(c["topic"])
        # 一条 finding 至多命名一个缺陷机制（评审 #4）：宽范围的 finding 可能跨过几个 oracle 条目、
        # 带出几个候选主题，但人工确认时要挑定它说的是哪一个——同时标两个 hit 会让一条 finding 撑起两个主题。
        if len(hits_in_row) > 1:
            bad.append(f"{r['key']}: 一条 finding 标了 {len(hits_in_row)} 个 hit {hits_in_row}——至多一个")
            continue
        for t in hits_in_row:
            (per_run_refuted if r["refuted"] else per_run)[r["run"]].add(t)
    if bad:
        raise ValueError("确认表有 " + str(len(bad)) + " 处要改：\n  " + "\n  ".join(bad))
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
    try:
        res = judge(confirm, a.min_topics, _expand(_topic_list(a.baseline_topics), topics), _expand(_topic_list(a.holdout), topics))
    except ValueError as e:
        raise SystemExit(str(e))
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
            {"id": "f1", "file": "src/a.rs", "line_start": 0, "line_end": 0, "title": "leak", "quote": "let token=secret();"},
            {"id": "f2", "file": "src/a.rs", "line_start": 5, "line_end": 5, "title": "write", "quote": "not in file"},
            {"id": "f3", "file": "src/a.rs", "line_start": 9, "line_end": 9, "title": "noise", "quote": "noise();"},
            {"id": "f4", "file": "src/a.rs", "line_start": 7, "line_end": 7, "title": "tests", "quote": "y();",
             "refute": {"verdict": "refuted", "model": "v", "note": "n"}},
            # 重复短句：报告行号（第 8 行 z();…）优先于 quote 的「第一次出现」——这里 quote 在第 6 行也出现
            {"id": "f5", "file": "src/a.rs", "line_start": 8, "line_end": 8, "title": "dup", "quote": "x();"},
        ]}
        rpath = os.path.join(d, "rep.json")
        json.dump(rep, open(rpath, "w", encoding="utf-8"))
        cpath = os.path.join(d, "confirm.json")
        # 既有断言测的是「行号来源」等严格口径，显式钉 tolerance=0（合成仓只有 9 行，±10 会让所有条目互相命中）
        ns = argparse.Namespace(reports=[rpath], oracle=opath, repo=repo, head=None, set="union", out=cpath, tolerance=0)
        assert cmd_candidates(ns) == 0
        c = json.load(open(cpath, encoding="utf-8"))
        rows = {r["id"]: r for r in c["rows"]}
        assert rows["f1"]["lines"] == [2, 2] and rows["f1"]["lines_from"] == "quote", "报告无行号 → 退回 quote 定位"
        assert [x["topic"] for x in rows["f1"]["candidates"]] == ["T1-leak"], rows["f1"]
        assert rows["f2"]["lines_from"] == "report" and [x["topic"] for x in rows["f2"]["candidates"]] == ["T2-write"], "报告行号优先"
        assert rows["f5"]["lines"] == [8, 8] and [x["topic"] for x in rows["f5"]["candidates"]] == ["T9-tests"], \
            "重复短句按报告行号（第 8 行），不被 quote 的第一次出现（第 6 行）挪走"
        assert all(c["match"] == "exact" for r in c["rows"] for c in r["candidates"]), "tolerance=0 下只可能有严格命中"

        # 行容差（2026-09-23 尸检定案）：锚点偏几行的同机制 finding 要进候选表、并标 near，交人工判
        near_rep = {"findings": [
            {"id": "n1", "file": "src/a.rs", "line_start": 9, "line_end": 9, "title": "near leak", "quote": "noise();"},
        ]}
        npath = os.path.join(d, "near.json")
        json.dump(near_rep, open(npath, "w", encoding="utf-8"))
        ncpath = os.path.join(d, "near-confirm.json")
        strict = argparse.Namespace(reports=[npath], oracle=opath, repo=repo, head=None, set="union", out=ncpath, tolerance=0)
        assert cmd_candidates(strict) == 0
        assert json.load(open(ncpath, encoding="utf-8"))["rows"][0]["outside"], "严格口径下第 9 行不碰任何条目"
        loose = argparse.Namespace(reports=[npath], oracle=opath, repo=repo, head=None, set="union", out=ncpath, tolerance=3)
        assert cmd_candidates(loose) == 0
        nr = json.load(open(ncpath, encoding="utf-8"))["rows"][0]
        assert not nr["outside"], "±3 行内的条目该进候选"
        assert [x["topic"] for x in nr["candidates"]] == ["T9-tests"], nr["candidates"]
        assert nr["candidates"][0]["match"] == "near", "全是近旁 → 标 near"
        assert [o["match"] for o in nr["candidates"][0]["oracle"]] == ["near"], "条目级也要标 near（人工逐条读机制时要知道哪条是近旁）"
        assert nr["candidates"][0]["oracle"][0]["lines"] == [7, 8], "候选里带上 oracle 锚点行，人工确认时对得上"
        assert rows["f3"]["outside"] and not rows["f3"]["candidates"], "oracle 外单列"
        assert rows["f4"]["refuted"] and [x["topic"] for x in rows["f4"]["candidates"]] == ["T9-tests"], rows["f4"]

        # 未填 verdict → 什么都不算
        r0 = judge(c, 2, None, None)
        assert r0["pending"] == 4 and r0["union_count"] == 0, r0
        # verdict 值写错 → 报错而不是静默算 miss（评审 #5）
        rows["f1"]["verdict"] = {"T1-leak": {"verdict": "Hit"}}
        try:
            judge(c, 2, None, None)
            raise AssertionError("错值该报错")
        except ValueError as e:
            assert "只认 hit / miss" in str(e), e
        rows["f1"]["verdict"] = {"T1-leak": {"reason": "忘了填 verdict"}}
        try:
            judge(c, 2, None, None)
            raise AssertionError("缺 verdict 字段该报错")
        except ValueError:
            pass
        # 人工：f1 算、f2 不算（同区域不同主张）、f4 算但被驳回
        rows["f1"]["verdict"] = {"T1-leak": {"verdict": "hit", "reason": "同一机制"}}
        rows["f2"]["verdict"] = {"T2-write": {"verdict": "miss", "reason": "同区域不同主张"}}
        rows["f4"]["verdict"] = {"T9-tests": {"verdict": "hit", "reason": "同一机制"}}
        rows["f5"]["verdict"] = {"T9-tests": {"verdict": "miss", "reason": "不同机制"}}
        r1 = judge(c, 2, ["T1-leak", "T2-write"], ["T9-tests"])
        assert r1["pending"] == 0 and r1["union"] == ["T1-leak"], r1
        assert r1["each_run_passes_min"] is False and r1["union_ge_baseline"] is False, r1
        assert r1["missed_vs_baseline"] == ["T2-write"], r1
        assert r1["refuted_real_topics"] == {"1": ["T9-tests"]}, "被驳回的真主题不计入覆盖、但要单列"
        assert r1["holdout_hit"] == [], "驳回掉的不算留出集命中"
        assert _expand(["T1", "T9"], c["topics"]) == ["T1-leak", "T9-tests"], "主题简写展开"

        # 一条 finding 标两个 hit → 报错（评审 #4）
        wide = json.loads(json.dumps(c))
        wr = next(r for r in wide["rows"] if r["id"] == "f1")
        wr["candidates"].append({"topic": "T2-write", "oracle": []})
        wr["verdict"] = {"T1-leak": {"verdict": "hit"}, "T2-write": {"verdict": "hit"}}
        try:
            judge(wide, 2, None, None)
            raise AssertionError("一行两个 hit 该报错")
        except ValueError as e:
            assert "至多一个" in str(e), e

        # 零产出的那一轮不能消失（评审 #2）：传 2 份报告、第 2 份没有 finding
        zpath = os.path.join(d, "zero.json")
        json.dump({"findings": []}, open(zpath, "w", encoding="utf-8"))
        c2path = os.path.join(d, "confirm2.json")
        assert cmd_candidates(argparse.Namespace(reports=[rpath, zpath], oracle=opath, repo=repo, head=None,
                                                  set="union", out=c2path, tolerance=0)) == 0
        c2 = json.load(open(c2path, encoding="utf-8"))
        for r in c2["rows"]:
            for cand in r["candidates"]:
                r["verdict"][cand["topic"]] = {"verdict": "hit" if r["id"] == "f1" else "miss"}
        r2 = judge(c2, 1, None, None)
        assert r2["per_run_counts"] == {"1": 1, "2": 0}, r2
        assert r2["each_run_passes_min"] is False, "零产出那轮 0 个主题，门槛必须判不过"

        # 容差默认值端到端接线（命令行不给 --tolerance 时必须是 ±10，不是严格口径）
        parsed = build_parser().parse_args(["candidates", rpath, "--oracle", opath, "--repo", repo, "--out", cpath])
        assert parsed.tolerance == 10, f"默认容差应为 10，实际 {parsed.tolerance}"
    print("selftest ok: candidates（命中 / 未锚定 / oracle 外 / 驳回）+ judge（待填 / 门槛 / 基线 / 留出集 / 误驳侧证）")
    return 0


def build_parser():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    c = sub.add_parser("candidates", help="自动一步：逐条 finding 出候选主题，写确认表")
    c.add_argument("reports", nargs="+", help="结果 JSON（顶层 findings[]），每个算一轮")
    c.add_argument("--oracle", required=True)
    c.add_argument("--repo", required=True, help="被评仓（取 head 树内容做 quote 定位）")
    c.add_argument("--head", help="缺省取 oracle 里的 head")
    c.add_argument("--set", default="union")
    c.add_argument("--tolerance", type=int, default=10,
                   help="候选匹配的行容差（默认 ±10）：召回优先、精度交给人工确认；0 = 严格重叠（旧口径）")
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
    return ap


def main():
    a = build_parser().parse_args()
    sys.exit(a.fn(a))


if __name__ == "__main__":
    main()
