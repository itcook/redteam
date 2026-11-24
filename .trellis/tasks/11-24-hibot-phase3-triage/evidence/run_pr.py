#!/usr/bin/env python3
"""Phase 3.8 对拍夹具：把一个真实 PR 组装成 hibot job（triage），跑本仓二进制并收集报告。

只用于验收（不属于产品代码）：
  run_pr.py --repo ~/hiapi/sites --owner zimacode-network --name hiapi-sites \
            --pr 333 --head 0305e813... --base b002f715... --config /tmp/.../deepseek.json

- 检出：`git clone --shared --no-checkout` 到 job/repo（临时目录，不动源仓工作树/分支）。
- pr_context：GraphQL（comments 剔除 hibot；is_bot = __typename == "Bot" 或 login 以 [bot] 结尾）。
- inputs：diff_patch / merge_base / pr_context，按 manifest 声明 sha256。
"""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time

QUERY = os.path.join(os.path.dirname(__file__), "pr.graphql")


def run(cmd, cwd=None, timeout=600, env=None, check=True):
    p = subprocess.run(cmd, cwd=cwd, timeout=timeout, env=env, capture_output=True, text=True)
    if check and p.returncode != 0:
        raise SystemExit(f"command failed ({p.returncode}): {cmd}\n{p.stderr}")
    return p


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def decl(path, abs_path, schema=None):
    d = {"path": path, "sha256": sha256_file(abs_path)}
    if schema:
        d["schema"] = schema
    return d


def graphql_pr(owner, name, number):
    out = run(
        ["gh", "api", "graphql", "-F", f"query=@{QUERY}", "-f", f"owner={owner}", "-f", f"name={name}", "-F", f"number={number}"],
        timeout=120,
    ).stdout
    data = json.loads(out)["data"]["repository"]["pullRequest"]
    rollup = ((data.get("commits") or {}).get("nodes") or [{}])[0].get("commit", {}).get("statusCheckRollup") or {}
    state = {"SUCCESS": "success", "FAILURE": "failure", "ERROR": "failure", "PENDING": "pending", "EXPECTED": "pending"}.get(rollup.get("state"), "pending")
    failing = []
    for c in ((rollup.get("contexts") or {}).get("nodes") or []):
        if c.get("__typename") == "CheckRun" and c.get("conclusion") in ("FAILURE", "TIMED_OUT", "ACTION_REQUIRED"):
            failing.append(c.get("name") or "check")
        elif c.get("__typename") == "StatusContext" and c.get("state") in ("FAILURE", "ERROR"):
            failing.append(c.get("context") or "status")
    comments = []
    for c in ((data.get("comments") or {}).get("nodes") or []):
        author = c.get("author") or {}
        login = author.get("login") or ""
        if "hibot" in login.lower():
            continue  # hibot 自己的 sticky/命令回复在组装时剔除
        comments.append({
            "author": login,
            "body": c.get("body") or "",
            "created_at": c.get("createdAt") or "",
            "is_bot": author.get("__typename") == "Bot" or login.endswith("[bot]"),
        })
    issues = []
    for i in ((data.get("closingIssuesReferences") or {}).get("nodes") or []):
        issues.append({
            "number": i["number"],
            "title": i.get("title") or "",
            "state": (i.get("state") or "OPEN").lower(),
            "body": i.get("body") or "",
        })
    files = []
    for f in ((data.get("files") or {}).get("nodes") or []):
        ct = (f.get("changeType") or "MODIFIED").lower()
        files.append({"path": f["path"], "additions": f.get("additions") or 0, "deletions": f.get("deletions") or 0, "status": ct})
    author = data.get("author") or {}
    return {
        "schema": "hibot.audit.pr-context/1",
        "title": data.get("title") or "",
        "body": data.get("body") or "",
        "author": author.get("login") or "",
        "labels": [l["name"] for l in ((data.get("labels") or {}).get("nodes") or [])],
        "comments": comments,
        "linked_issues": issues,
        "ci": {"state": state, "failing": failing},
        "changed_files": files,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--owner", required=True)
    ap.add_argument("--name", required=True)
    ap.add_argument("--pr", type=int, required=True)
    ap.add_argument("--head", required=True)
    ap.add_argument("--base", required=True)
    ap.add_argument("--config", required=True)
    ap.add_argument("--job-root", default="/tmp/hibot-triage-eval/jobs")
    ap.add_argument("--clone-url", default=None, help="用远端 URL 克隆（源仓是 shallow 时用；默认 --shared 本地源仓）")
    ap.add_argument("--binary", default="target/debug/redteam")
    ap.add_argument("--workspace", default=os.path.expanduser("~/hobby/.worktrees/redteam/hibot-audit-exec"))
    ap.add_argument("--timeout", type=int, default=600)
    ap.add_argument("--evidence", default=None, help="copy report.json here")
    ap.add_argument("--label", default="", help="evidence 文件名后缀，避免同一 PR 多次跑互相覆盖")
    args = ap.parse_args()

    root = os.path.join(args.job_root, f"{args.name}-pr-{args.pr}{args.label}")
    job = os.path.join(root, "job")
    shutil.rmtree(root, ignore_errors=True)
    for d in ("config", "input", "out"):
        os.makedirs(os.path.join(job, d), exist_ok=True)
    shutil.copyfile(args.config, os.path.join(job, "config/executor.redteam.json"))

    repo = os.path.join(root, "repo")
    if args.clone_url:
        run(["git", "clone", "--no-checkout", "--filter=blob:none", "--quiet", args.clone_url, repo], timeout=600)
    else:
        run(["git", "clone", "--shared", "--no-checkout", "--quiet", args.repo, repo], timeout=300)
    run(["git", "checkout", "-q", "--detach", args.head], cwd=repo, timeout=300)
    mb = run(["git", "merge-base", args.base, args.head], cwd=repo).stdout.strip()
    diff = run(["git", "diff", f"{mb}..{args.head}"], cwd=repo).stdout
    with open(os.path.join(job, "input/diff.patch"), "w") as f:
        f.write(diff)
    with open(os.path.join(job, "input/merge-base.sha"), "w") as f:
        f.write(mb + "\n")
    pr = graphql_pr(args.owner, args.name, args.pr)
    with open(os.path.join(job, "input/pr-context.json"), "w") as f:
        json.dump(pr, f, ensure_ascii=False, indent=2)

    inputs = {
        "diff_patch": decl("input/diff.patch", os.path.join(job, "input/diff.patch")),
        "merge_base": decl("input/merge-base.sha", os.path.join(job, "input/merge-base.sha")),
        "pr_context": decl("input/pr-context.json", os.path.join(job, "input/pr-context.json"), "hibot.audit.pr-context/1"),
    }
    manifest = {
        "schema": "hibot.audit.job.triage/1",
        "job_id": f"eval-pr-{args.pr}",
        "kind": "triage",
        "repo": {"id": 1, "full_name": f"{args.owner}/{args.name}"},
        "pr": {"number": args.pr, "head_sha": args.head, "base_sha": args.base},
        "executor": {"name": "redteam", "version": "0.1.0"},
        "limits": {"wall_clock_s": args.timeout},
        "paths": {"repo": "../repo", "out": "out", "config": "config/executor.redteam.json"},
        "inputs": inputs,
    }
    with open(os.path.join(job, "job.json"), "w") as f:
        json.dump(manifest, f, ensure_ascii=False, indent=2)

    t0 = time.time()
    env = dict(os.environ)
    proc = subprocess.run([os.path.join(args.workspace, args.binary), "triage", "--job-dir", job], env=env, capture_output=True, text=True, timeout=args.timeout + 60)
    elapsed = round(time.time() - t0, 1)
    report_path = os.path.join(job, "out/report.json")
    if not os.path.exists(report_path):
        print(json.dumps({"pr": args.pr, "exit": proc.returncode, "seconds": elapsed, "error": "no report", "stderr": proc.stderr[-4000:]}, ensure_ascii=False))
        sys.exit(1)
    report = json.load(open(report_path))
    if args.evidence:
        os.makedirs(args.evidence, exist_ok=True)
        shutil.copyfile(report_path, os.path.join(args.evidence, f"report.{args.name}-pr-{args.pr}{args.label}.json"))
        shutil.copyfile(os.path.join(job, "input/pr-context.json"), os.path.join(args.evidence, f"pr-context.{args.name}-pr-{args.pr}{args.label}.json"))
    summary = report.get("summary") or {}
    axes = report.get("extensions", {}).get("redteam", {}).get("axes", [])
    metrics = {
        "pr": args.pr,
        "exit": proc.returncode,
        "seconds": elapsed,
        "status": report.get("status"),
        "status_reason": report.get("status_reason"),
        "summary": summary,
        "findings": len(report.get("findings") or []),
        "blockers": len(report.get("extensions", {}).get("redteam", {}).get("blockers") or []),
        "usage": report.get("usage"),
        "axes": [
            {"axis": a.get("axis"), "ok": a.get("ok"), "rows": a.get("rows"), "cells": a.get("cells"), "dropped": a.get("dropped"), "error": a.get("error")}
            for a in axes
        ],
        "pr_context": {
            "title": pr["title"],
            "author": pr["author"],
            "comments": len(pr["comments"]),
            "linked_issues": len(pr["linked_issues"]),
            "ci": pr["ci"],
            "files": len(pr["changed_files"]),
        },
        "merge_base": mb,
        "stderr_tail": proc.stderr[-1000:],
    }
    print(json.dumps(metrics, ensure_ascii=False))


if __name__ == "__main__":
    main()
