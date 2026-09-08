#!/usr/bin/env python3
"""旧 proj.db 台账固定集导出（M21a 批 B-1，D10-2：schema v4 一次重置前先导出，不写迁移）。

零第三方依赖（sqlite3 + json）。读旧库两类数据，写 JSONL 固定集（verifier 离线回归集数据，design §8.1）：
  dismissed  `dismissed` 全表（已知假阳）                → <out-dir>/ledger-dismissed.jsonl
  upheld     `results.json` 中 verify.verdict == "upheld" 的 findings（已知真）→ <out-dir>/ledger-upheld.jsonl
             按 job×finding 计行、不按 id 去重（同一 finding 在多轮各被 upheld 一次即多行），`job_id` 溯源；
             results 无轮次字段，`round` 留空串。

每行至少含 id / file / quote / title / severity / status / round。内容原样、不改写；固定集只应来自本仓自己的
proj.db（公司仓数据只落 .next/private/，永不入库）。

用法：
  python3 eval/export_ledger.py [--db .redteam/proj.db] [--out-dir eval/fixtures]
"""
import argparse
import json
import os
import sqlite3
import sys


def export_dismissed(conn):
    rows = conn.execute(
        "SELECT id,status,title,file,quote,severity,reason,round,updated_at FROM dismissed ORDER BY id"
    ).fetchall()
    keys = ["id", "status", "title", "file", "quote", "severity", "reason", "round", "updated_at"]
    return [dict(zip(keys, r)) for r in rows]


def export_upheld(conn):
    out = []
    for job_id, js in conn.execute("SELECT job_id, json FROM results ORDER BY job_id"):
        try:
            report = json.loads(js)
        except json.JSONDecodeError as e:
            print(f"warn: results[{job_id}] 非法 JSON，跳过: {e}", file=sys.stderr)
            continue
        for f in report.get("findings", []):
            v = f.get("verify") or {}
            if v.get("verdict") != "upheld":
                continue
            out.append({
                "id": f.get("id", ""),
                "status": "upheld",
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
    conn = sqlite3.connect(f"file:{a.db}?mode=ro", uri=True)
    dismissed = export_dismissed(conn)
    upheld = export_upheld(conn)
    os.makedirs(a.out_dir, exist_ok=True)
    write_jsonl(os.path.join(a.out_dir, "ledger-dismissed.jsonl"), dismissed)
    write_jsonl(os.path.join(a.out_dir, "ledger-upheld.jsonl"), upheld)
    print(f"dismissed {len(dismissed)} / upheld {len(upheld)} → {a.out_dir}/ledger-{{dismissed,upheld}}.jsonl")
    return 0


if __name__ == "__main__":
    sys.exit(main())
