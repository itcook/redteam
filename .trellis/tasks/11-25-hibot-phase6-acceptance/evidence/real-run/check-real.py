#!/usr/bin/env python3
"""真跑报告校验（Phase 6 / 9.3）：schema + 指纹复算 + quote 逐字 + verify 覆盖 + 心跳三帧。

用法：
    python3 check-real.py --root <evidence/real-run> --jobs <target/phase6-real> --schema-dir <hibot schema dir>

只读，不改任何产物；逐项打印 PASS/FAIL，退出码 1 表示有失败。
"""
import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path

KINDS = ("triage", "review", "verify")
PHASES = ("preparing", "running", "finishing")


def normalize(s: str) -> str:
    """契约：逐行 trim、丢空行、单空格 join（与 src/job/fingerprint.rs 同口径）。"""
    return " ".join(line.strip() for line in s.splitlines() if line.strip())


def fingerprint_of(file: str, quote: str, title: str) -> str:
    h = hashlib.sha256()
    h.update(file.encode())
    h.update(b"\0")
    h.update(normalize(quote).encode())
    h.update(b"\0")
    h.update(normalize(title).encode())
    return "sha256:" + h.hexdigest()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", required=True, help="evidence/real-run")
    ap.add_argument("--jobs", required=True, help="prepare-job 产出的 <kind>/job 根")
    ap.add_argument("--schema-dir", required=True)
    ap.add_argument("--checker", required=True, help="scripts/hibot-schema-check.py")
    args = ap.parse_args()

    root = Path(args.root)
    jobs = Path(args.jobs)
    failures = []

    def check(ok: bool, label: str, detail: str = "") -> None:
        print(f"{'PASS' if ok else 'FAIL'} {label}" + (f" — {detail}" if detail and not ok else ""))
        if not ok:
            failures.append(label)

    for kind in KINDS:
        out = root / kind
        report_path = out / "report.json"
        job_dir = jobs / kind / "job"
        repo = jobs / kind / "repo"
        if not report_path.is_file():
            check(False, f"{kind}: report.json 存在")
            continue
        report = json.loads(report_path.read_text())
        manifest = json.loads((job_dir / "job.json").read_text())

        # 1) hibot 契约 schema
        res = subprocess.run(
            [sys.executable, args.checker, "--schema-dir", args.schema_dir, f"report.{kind}", str(report_path)],
            capture_output=True,
            text=True,
        )
        check(res.returncode == 0, f"{kind}: 报告过 hibot schema", res.stdout + res.stderr)

        # 2) 报告回显 == manifest
        check(report["input"]["head_sha"] == manifest["pr"]["head_sha"], f"{kind}: head_sha 回显一致")
        check(report["input"]["base_sha"] == manifest["pr"]["base_sha"], f"{kind}: base_sha 回显一致")

        # 3) 指纹复算 + quote 逐字（finding 级）
        for i, f in enumerate(report.get("findings") or []):
            want = fingerprint_of(f["file"], f["quote"], f["title"])
            check(f["fingerprint"] == want, f"{kind}: findings[{i}] 指纹可复算", f"{f['fingerprint']} != {want}")
            text = (repo / f["file"]).read_text(errors="replace")
            check(f["quote"] in text, f"{kind}: findings[{i}] quote 逐字命中 ({f['file']})", repr(f["quote"][:80]))

        # 4) verify 覆盖不变量 + 证据 quote
        if kind == "verify":
            inputs = json.loads((job_dir / manifest["inputs"]["findings"]["path"]).read_text())
            file_by_fp = {x["fingerprint"]: x["file"] for x in inputs}
            want = sorted({x["fingerprint"] for x in inputs})
            got = sorted(v["finding_fingerprint"] for v in report["verifications"])
            check(got == want, "verify: 输入指纹集合 == 输出集合", f"{got} != {want}")
            check(len(got) == len(set(got)), "verify: 输出无重复指纹")
            counts = report["extensions"]["redteam"]["verify"]["counts"]
            check(sum(counts.values()) == len(got), "verify: counts 之和 == verifications 条数", f"{counts} vs {len(got)}")
            for i, v in enumerate(report["verifications"]):
                if "quote" in v:
                    rel = file_by_fp.get(v["finding_fingerprint"])
                    ok = rel is not None and v["quote"] in (repo / rel).read_text(errors="replace")
                    check(ok, f"verify: verifications[{i}] quote 逐字命中 ({rel})", repr(v["quote"][:80]))

        # 5) status 与机器字段一致性
        rt = report["extensions"]["redteam"]
        if report["status"] == "ok":
            check("status_reason" not in report, f"{kind}: ok 不写 status_reason")
            check("status_code" not in rt, f"{kind}: ok 不写 status_code")
        else:
            check(bool(report.get("status_reason")), f"{kind}: partial/error 必须有人读 reason")
        check("cost" not in json.dumps(report["usage"]).lower(), f"{kind}: usage 无 cost")

        # 6) 心跳：3 帧 + 终帧 finishing，phase 单调且不写终态
        frames = sorted((out / "frames").glob("frame-*.json"))
        check(len(frames) == 3, f"{kind}: 采到 3 帧 state.json", f"got {len(frames)}")
        seq = []
        for fp in frames:
            d = json.loads(fp.read_text())
            seq.append(d["phase"])
            check(d.get("schema") == "hibot.audit.state/1", f"{kind}: {fp.name} schema")
            check(d.get("job_id") == manifest["job_id"], f"{kind}: {fp.name} job_id")
            check(d.get("phase") in PHASES, f"{kind}: {fp.name} phase ∈ {PHASES}", d.get("phase"))
        check(seq == sorted(seq, key=PHASES.index), f"{kind}: 心跳 phase 单调前进", str(seq))
        final = json.loads((out / "state-final.json").read_text())
        check(final["phase"] == "finishing", f"{kind}: 终帧 finishing", final["phase"])
        check((out / "wall-clock-s.txt").read_text().strip().isdigit(), f"{kind}: 墙钟已记录")

    print()
    if failures:
        print(f"FAILED: {len(failures)} 项")
        return 1
    print("ALL PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
