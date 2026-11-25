#!/usr/bin/env python3
"""用 hibot 的契约 schema 校验执行单元产物（capabilities / report.<kind> / state / job.<kind>）。

用法：
    python3 scripts/hibot-schema-check.py --schema-dir DIR <target> FILE [FILE...]

    target ∈ capabilities | report.review | report.triage | report.verify | state | job.review | ...

schema 按 `$id` 注册（跨文件 `$ref` 走 `$id` 基准解析），因此只需要 schema 目录，不需要网络。

退出码：0 = 全部通过；1 = 有校验失败；3 = schema 目录/依赖缺失（调用方按「跳过」处理）。
"""
import argparse
import json
import sys
from pathlib import Path

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--schema-dir", required=True)
    ap.add_argument("target")
    ap.add_argument("files", nargs="+")
    args = ap.parse_args()

    schema_dir = Path(args.schema_dir)
    if not schema_dir.is_dir():
        print(f"SKIP: schema dir not found: {schema_dir}", file=sys.stderr)
        return 3
    try:
        from jsonschema import Draft202012Validator
        from referencing import Registry, Resource
        from referencing.jsonschema import DRAFT202012
    except ImportError as e:  # pragma: no cover - 依赖缺失按跳过处理
        print(f"SKIP: jsonschema/referencing unavailable: {e}", file=sys.stderr)
        return 3

    schemas = {}
    for f in sorted(schema_dir.glob("*.schema.json")):
        doc = json.loads(f.read_text())
        schemas[doc["$id"]] = doc
    registry = Registry()
    for sid, doc in schemas.items():
        registry = registry.with_resource(sid, Resource.from_contents(doc, default_specification=DRAFT202012))

    wanted = f"/{args.target}-1.json"
    matches = [sid for sid in schemas if sid.endswith(wanted)]
    if len(matches) != 1:
        print(f"FAIL: no unique schema for target {args.target!r} in {schema_dir}", file=sys.stderr)
        return 1
    schema_id = matches[0]
    validator = Draft202012Validator(schemas[schema_id], registry=registry)

    failed = 0
    for name in args.files:
        p = Path(name)
        try:
            instance = json.loads(p.read_text())
        except (OSError, json.JSONDecodeError) as e:
            print(f"FAIL {p}: unreadable ({e})")
            failed += 1
            continue
        errors = sorted(validator.iter_errors(instance), key=lambda e: list(e.absolute_path))
        if errors:
            failed += 1
            print(f"FAIL {p} ({schema_id})")
            for e in errors[:5]:
                print(f"  - {list(e.absolute_path)}: {e.message}")
        else:
            print(f"PASS {p} ({schema_id})")
    return 1 if failed else 0

if __name__ == "__main__":
    sys.exit(main())
