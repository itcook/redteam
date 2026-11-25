#!/usr/bin/env python3
"""P0 验证门 · oracle 组装（零第三方依赖；被 risk_replay.py `score` 与 e3_contract_ab.py `score` 消费）。

两种 oracle：
  pr181  三方 findings：产品 cells（proj.db）+ rpiv 报告（`**Where**` 行的 `path:line[-line]` 锚）+ 人工对齐文件
         （extra 条目、topics 分组、strict_matched 列表）→ sets: topic_diff / strict_diff / union。
  hobby  本仓 proj.db 全部 branch-scope 作业的 verify=upheld findings，带 (job, base, head)；head 缺失时按
         created_at 从 git 历史推断并标 head_inferred；每条以 quote 在 head 树逐字复核（anchored 标记）。

产物 JSON：
  {"kind", "head", "entries": [{id, source, title, severity, confidence, anchors: [{file, line_start, line_end}],
    topic, job, base, head, head_inferred, anchored, approx}], "topics": {...}, "sets": {name: [id...]}}

用法：
  p0_oracle.py pr181 --db <proj.db> --rpiv <report.md> --curated <curated.json> --repo <dir> --out <json>
  p0_oracle.py hobby --db <proj.db> --repo <dir> [--ref <fallback-ref>] --out <json>
所有路径经参数传入；本文件不含任何被评仓的内容。
"""
import argparse
import json
import os
import re
import sqlite3
import subprocess
import sys


def sh(args, cwd=None, check=True):
    r = subprocess.run(args, cwd=cwd, capture_output=True, text=True)
    if check and r.returncode != 0:
        raise RuntimeError(f"{args}: {r.stderr[:400]}")
    return r.stdout


def show_file(repo, head, path):
    r = subprocess.run(["git", "-C", repo, "show", f"{head}:{path}"], capture_output=True, text=True,
                       errors="replace")
    return r.stdout if r.returncode == 0 else None


def quote_anchored(repo, head, path, quote):
    """产品同口径：quote 去全部空白后在文件中子串匹配（免疫 reformat）。"""
    if not quote:
        return None
    content = show_file(repo, head, path)
    if content is None:
        return False
    strip = lambda s: "".join(ch for ch in s if not ch.isspace())
    return strip(quote) in strip(content)


def product_findings(db, job, model):
    con = sqlite3.connect(db)
    row = con.execute("select record_json from cells where job_id=? and cell like ?", (job, f"%@{model}")).fetchone()
    if not row:
        raise SystemExit(f"cell {job}@{model} not found in {db}")
    rec = json.loads(row[0])
    # v3 记录 findings 平铺；v4（M21a schema v4）挪到 output{role,…} 下，scanner 记录无 findings → 空
    return rec.get("output", rec).get("findings", [])


ANCHOR_RE = re.compile(r"`([^`\s:]+):(\d+)(?:-(\d+))?`|`:(\d+)(?:-(\d+))?`")


def parse_rpiv(md_path):
    """`### I1 …` / `### Q3 …` 标题 + 紧随的 **Where** 段中的反引号锚。`\\`:443\\`` 形式沿用前一路径。"""
    entries = []
    cur = None
    in_where = False
    for line in open(md_path, encoding="utf-8"):
        m = re.match(r"^### ([IQ]\d+)\s+\S+\s+(.*)$", line)
        if m:
            cur = {"id": f"rpiv-{m.group(1)}", "source": "rpiv", "title": m.group(2).strip(), "anchors": []}
            entries.append(cur)
            in_where = False
            continue
        if cur is None:
            continue
        if line.strip().startswith("**Where**"):
            in_where = True
            continue
        if in_where:
            if not line.strip():
                if cur["anchors"]:
                    in_where = False
                continue
            if line.startswith("**"):
                in_where = False
                continue
            last_file = cur["anchors"][-1]["file"] if cur["anchors"] else None
            for f, a, b, a2, b2 in ANCHOR_RE.findall(line):
                if f:
                    ls, le = int(a), int(b or a)
                    cur["anchors"].append({"file": f, "line_start": ls, "line_end": le})
                    last_file = f
                elif last_file:
                    ls, le = int(a2), int(b2 or a2)
                    cur["anchors"].append({"file": last_file, "line_start": ls, "line_end": le})
    return entries


def build_pr181(a):
    cur = json.load(open(a.curated, encoding="utf-8"))
    head = a.head or cur["head"]
    entries = []
    for pc in cur["product_cells"]:
        for f in product_findings(a.db, pc["job"], pc["model"]):
            entries.append({
                "id": f["id"][:8], "source": pc["source"], "title": f.get("title", ""),
                "severity": f.get("severity"), "confidence": f.get("confidence"),
                "anchors": [{"file": f["file"], "line_start": f.get("line_start") or 1, "line_end": f.get("line_end") or f.get("line_start") or 1}],
                "job": pc["job"], "base": None, "head": head, "head_inferred": False, "approx": False,
                "anchored": quote_anchored(a.repo, head, f["file"], f.get("quote", "")),
            })
    for e in parse_rpiv(a.rpiv):
        e.update({"severity": None, "confidence": None, "job": None, "base": None, "head": head,
                  "head_inferred": False, "approx": False, "anchored": None})
        entries.append(e)
    for e in cur.get("extra", []):
        e = dict(e)
        e.setdefault("severity", None); e.setdefault("confidence", None)
        e.update({"job": None, "base": None, "head": head, "head_inferred": False, "anchored": None})
        entries.append(e)
    topic_of = {}
    for t, ids in cur["topics"].items():
        for i in ids:
            topic_of[i] = t
    for e in entries:
        e["topic"] = topic_of.get(e["id"])
    ids = {e["id"] for e in entries}
    missing = [i for i in topic_of if i not in ids]
    untopiced = [e["id"] for e in entries if not e["topic"]]
    if missing or untopiced:
        raise SystemExit(f"topic alignment mismatch: curated-but-absent={missing} entries-without-topic={untopiced}")
    ds_topics = {e["topic"] for e in entries if e["source"] == "deepseek"}
    strict_matched = set(cur.get("strict_matched_by_deepseek", []))
    sets = {
        "topic_diff": [e["id"] for e in entries if e["source"] == "glm" and e["topic"] not in ds_topics],
        "strict_diff": [e["id"] for e in entries if e["source"] == "glm" and e["id"] not in strict_matched],
        "union": [e["id"] for e in entries],
    }
    out = {"kind": "pr181", "head": head, "entries": entries, "topics": cur["topics"], "sets": sets}
    json.dump(out, open(a.out, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    by_src = {}
    for e in entries:
        by_src[e["source"]] = by_src.get(e["source"], 0) + 1
    anchored = sum(1 for e in entries if e["anchored"] is True)
    unanch = [e["id"] for e in entries if e["anchored"] is False]
    print(f"pr181 oracle → {a.out}: {by_src}; sets topic_diff={len(sets['topic_diff'])} {sets['topic_diff']} "
          f"strict_diff={len(sets['strict_diff'])} union={len(sets['union'])}; quote-anchored {anchored}, unanchored {unanch}")


def infer_head(repo, ref, created_at):
    """作业无 head_sha：取 ref 历史上 created_at 之前最后一个提交（仅推断，需 quote 复核）。"""
    out = sh(["git", "-C", repo, "log", "-1", f"--until={created_at}", "--format=%H", ref], check=False).strip()
    return out or None


def build_hobby(a):
    con = sqlite3.connect(a.db)
    rows = con.execute("select r.job_id, r.json, j.args_json, j.created_at from results r join jobs j on r.job_id=j.job_id order by j.created_at").fetchall()
    entries, seen = [], set()
    skipped = []
    for jid, js, args, created in rows:
        r = json.loads(js)
        args = json.loads(args) if args else {}
        base = args.get("base")
        if not base or args.get("scope") != "branch":
            skipped.append((jid, "no-base/working-tree"))
            continue
        head = r.get("run", {}).get("head_sha") or None
        inferred = False
        if not head:
            head = infer_head(a.repo, a.ref, created)
            inferred = True
            if not head:
                skipped.append((jid, "head-uninferable"))
                continue
        for f in r["findings"]:
            # 0.8 术语分家：新报告写 refute，历史 results 行仍是 verify
            if (f.get("refute") or f.get("verify") or {}).get("verdict") != "upheld":
                continue
            key = (f["id"], head)
            if key in seen:
                continue
            seen.add(key)
            entries.append({
                "id": f"{f['id'][:8]}@{head[:7]}", "source": "hobby", "title": f.get("title", ""),
                "severity": f.get("severity"), "confidence": f.get("confidence"),
                "anchors": [{"file": f["file"], "line_start": f.get("line_start") or 1, "line_end": f.get("line_end") or f.get("line_start") or 1}],
                "topic": None, "job": jid, "base": base, "head": head, "head_inferred": inferred, "approx": False,
                "created_at": created,
                "anchored": quote_anchored(a.repo, head, f["file"], f.get("quote", "")),
            })
    by_diff = {}
    for e in entries:
        k = f"{e['base']}..{e['head'][:7]}"
        by_diff.setdefault(k, []).append(e["id"])
    sets = {k: v for k, v in by_diff.items()}
    sets["union"] = [e["id"] for e in entries]
    out = {"kind": "hobby", "head": None, "entries": entries, "topics": {}, "sets": sets}
    json.dump(out, open(a.out, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    anchored = sum(1 for e in entries if e["anchored"] is True)
    print(f"hobby oracle → {a.out}: {len(entries)} upheld (dedup by id×head), quote-anchored {anchored}; per diff:")
    for k, v in by_diff.items():
        inf = any(e["head_inferred"] for e in entries if e["base"] + ".." + e["head"][:7] == k)
        print(f"  {k}: {len(v)}{' (head inferred)' if inf else ''}")
    print(f"  skipped jobs: {skipped}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("pr181")
    p.add_argument("--db", required=True); p.add_argument("--rpiv", required=True); p.add_argument("--curated", required=True)
    p.add_argument("--repo", required=True); p.add_argument("--head"); p.add_argument("--out", required=True)
    p.set_defaults(fn=build_pr181)
    h = sub.add_parser("hobby")
    h.add_argument("--db", required=True); h.add_argument("--repo", required=True); h.add_argument("--ref", default="HEAD")
    h.add_argument("--out", required=True)
    h.set_defaults(fn=build_hobby)
    a = ap.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
