#!/usr/bin/env python3
"""旧 proj.db 台账固定集导出（M21a 批 B-1，D10-2：schema v4 一次重置前先导出，不写迁移）。

零第三方依赖（sqlite3 + json）。读旧库三类数据，写 JSONL 固定集（verifier 离线回归集数据，design §8.1）：
  dismissed  `dismissed` 全表（含 withdrawn / stale，status 原样导出，消费方按需过滤）→ <out-dir>/ledger-dismissed.jsonl
  resolved   `resolved` 全表（M13 销案卡，--incremental-from 争议检测用）        → <out-dir>/ledger-resolved.jsonl
  upheld     `results.json` 中 verify.verdict == "upheld" 的 findings（模型判真）   → <out-dir>/ledger-upheld.jsonl
             按 job×finding 计行、不按 id 去重（同一 finding 在多轮各被 upheld 一次即多行），`job_id` / `head_sha` 溯源
             （quote 只能在当时的 HEAD 树里锚定，历史路径与 gitignored 路径在现树锚不到）；results 无轮次字段，`round` 留空串；
             `in_dismissed_ledger` = 同 id 也在 dismissed 表里（人裁误报覆盖模型判真，回归集消费方须据此排除或单独计）。

每行至少含 id / file / quote / title / severity / status / round。内容原样、不改写；固定集只应来自本仓自己的
proj.db（公司仓数据只落 .next/private/，永不入库）。jobs / cells / results 评审史不导出。

用法：
  python3 eval/export_ledger.py [--db .redteam/proj.db] [--out-dir eval/fixtures]
"""
import argparse
import json
import os
import pathlib
import sqlite3
import sys


def export_dismissed(conn):
    rows = conn.execute(
        "SELECT id,status,title,file,quote,severity,reason,round,updated_at FROM dismissed ORDER BY id"
    ).fetchall()
    keys = ["id", "status", "title", "file", "quote", "severity", "reason", "round", "updated_at"]
    return [dict(zip(keys, r)) for r in rows]


def export_resolved(conn):
    rows = conn.execute(
        "SELECT id,title,file,quote,severity,reason,round,updated_at FROM resolved ORDER BY id"
    ).fetchall()
    keys = ["id", "title", "file", "quote", "severity", "reason", "round", "updated_at"]
    return [dict(zip(keys, r), status="resolved") for r in rows]


def export_upheld(conn, dismissed_ids):
    out = []
    for job_id, js in conn.execute("SELECT job_id, json FROM results ORDER BY job_id"):
        try:
            report = json.loads(js)
        except json.JSONDecodeError as e:
            print(f"warn: results[{job_id}] 非法 JSON，跳过: {e}", file=sys.stderr)
            continue
        head_sha = (report.get("run") or {}).get("head_sha") or ""
        for f in report.get("findings", []):
            # 0.8 术语分家：新报告写 refute，历史 results 行仍是 verify
            v = f.get("refute") or f.get("verify") or {}
            if v.get("verdict") != "upheld":
                continue
            out.append({
                "id": f.get("id", ""),
                "status": "upheld",
                "in_dismissed_ledger": f.get("id") in dismissed_ids,
                "head_sha": head_sha,
                "title": f.get("title", ""),
                "file": f.get("file", ""),
                "quote": f.get("quote", ""),
                "severity": f.get("severity", ""),
                "round": "",
                "job_id": job_id,
                "line_start": f.get("line_start"),
                "line_end": f.get("line_end"),
                "confidence": f.get("confidence"),
                "body": f.get("body", ""),
                "recommendation": f.get("recommendation", ""),
                "verify_model": v.get("model", ""),
                "verify_note": v.get("note", ""),
            })
    return out


def write_jsonl(path, rows):
    with open(path, "w", encoding="utf-8") as fh:
        for r in rows:
            fh.write(json.dumps(r, ensure_ascii=False) + "\n")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--db", default=".redteam/proj.db", help="旧 proj.db 路径（缺省 .redteam/proj.db）")
    ap.add_argument("--out-dir", default="eval/fixtures", help="固定集输出目录（缺省 eval/fixtures）")
    a = ap.parse_args()
    if not os.path.isfile(a.db):
        print(f"error: 找不到 proj.db: {a.db}", file=sys.stderr)
        return 2
    # 只读打开；路径经 as_uri() 转义（含 `#` / `%` / 空格的路径裸拼进 file: URI 会被截断成「no such table」）
    conn = sqlite3.connect(pathlib.Path(a.db).resolve().as_uri() + "?mode=ro", uri=True)
    dismissed = export_dismissed(conn)
    resolved = export_resolved(conn)
    upheld = export_upheld(conn, {r["id"] for r in dismissed})
    os.makedirs(a.out_dir, exist_ok=True)
    write_jsonl(os.path.join(a.out_dir, "ledger-dismissed.jsonl"), dismissed)
    write_jsonl(os.path.join(a.out_dir, "ledger-resolved.jsonl"), resolved)
    write_jsonl(os.path.join(a.out_dir, "ledger-upheld.jsonl"), upheld)
    both = sum(1 for r in upheld if r["in_dismissed_ledger"])
    print(f"dismissed {len(dismissed)} / resolved {len(resolved)} / upheld {len(upheld)} (in_dismissed_ledger {both}) "
          f"→ {a.out_dir}/ledger-{{dismissed,resolved,upheld}}.jsonl")
    return 0


if __name__ == "__main__":
    sys.exit(main())
