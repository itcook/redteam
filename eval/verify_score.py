#!/usr/bin/env python3
"""verifier 离线基线（M21b 批 D-4，design §8.1「去留由数据决定」〔D17〕）。

固定集 = 本仓自己的人裁台账导出（`fixtures/ledger-{dismissed,upheld}.jsonl`，见 export_ledger.py）：
  - `dismissed` = 人读后判定的误报 → **期望 REFUTED**（驳回率的分母）
  - `upheld`    = 人确认的真缺陷   → **期望 UPHELD**（误驳率的分母）

三步：`build`（零网络，锚定 + 物化 + 生成用例）→ `run`（逐条调 `redteam-next verify-once`）→
`score`（混淆矩阵 + 两率 + 预登记阈值裁定）。prompt / provider / 判决解析全在产品二进制里，
本脚本一行都不复刻——复刻一份，测的就不是产品了。零第三方依赖（照 harness.py 纪律）。

用法：
  python3 eval/verify_score.py build --repo . --out eval/out/verify-cases
  python3 eval/verify_score.py run eval/out/verify-cases --verifier deepseek/deepseek-flash \
      --bin ./target/release/redteam-next --out eval/out/verify-deepseek.json
  python3 eval/verify_score.py score eval/out/verify-deepseek.json [--baseline prev.json]
  python3 eval/verify_score.py selftest
"""
import argparse
import concurrent.futures
import json
import math
import os
import re
import subprocess
import sys
import time

# 预登记阈值（跑之前定死，D17 Q6）：两条同时满足才把 verifiers.enabled 的默认值打开。
# 误驳（把真缺陷驳掉）会静默删证据，故门收得紧；驳回率低于 30% 则这一路只是在烧 token。
MAX_FALSE_REFUTE = 0.05
MIN_REFUTE = 0.30


def strip_ws(s):
    return re.sub(r"\s+", "", s)


def git(repo, *args):
    r = subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True)
    return r.stdout if r.returncode == 0 else None


# ---------------- build：锚定 + 物化 + 生成用例（零网络） ----------------

def anchor_sha(repo, row, branch):
    """dismissed 行没有 head_sha，只有 updated_at——取该日在 `--branch` 上的末次提交：
    卡是当天建的，引用在那棵树上才对得上（HEAD 树里 22 条只剩 7 条能定位）。"""
    if row.get("head_sha"):
        return row["head_sha"], "head_sha"
    day = (row.get("updated_at") or "").strip()
    if not day:
        return None, "no_anchor"
    out = git(repo, "rev-list", "-1", f"--before={day} 23:59:59", branch)
    return (out.strip(), "updated_at") if out and out.strip() else (None, "no_commit_before_date")


def locate(text, quote):
    """去全部空白的子串定位（`_oracle.py` 同口径）→ 1-based [起,止] 行号；找不到 → None。
    行号只决定快照窗口（±60），不必逐字精确。"""
    flat, target = strip_ws(text), strip_ws(quote)
    if not target or target not in flat:
        return None
    off = flat.index(target)
    # 把去空白偏移映射回行号：逐行累加该行的非空白字符数
    start = end = None
    seen = 0
    for i, line in enumerate(text.split("\n"), start=1):
        n = len(strip_ws(line))
        if start is None and seen + n > off:
            start = i
        if seen + n >= off + len(target):
            end = i
            break
        seen += n
    return (start or 1, end or start or 1)


def cmd_build(a):
    # 固定集含中文：读也钉死 utf-8（写侧本来就是），别靠 locale
    rows_d = [json.loads(l) for l in open(os.path.join(a.fixtures, "ledger-dismissed.jsonl"), encoding="utf-8")]
    rows_u = [json.loads(l) for l in open(os.path.join(a.fixtures, "ledger-upheld.jsonl"), encoding="utf-8")]

    # oracle 自相矛盾的 id（两边都有）：同一条既被人裁误报又被人确认，两边都不要。
    both = {r["id"] for r in rows_d} & {r["id"] for r in rows_u}

    cases, dropped = [], []
    seen = set()
    for row, oracle in [(r, "dismissed") for r in rows_d] + [(r, "upheld") for r in rows_u]:
        rid = row["id"]
        def drop(why):
            dropped.append({"id": rid, "oracle": oracle, "file": row.get("file"), "why": why})
        if rid in both:
            drop("id_in_both_ledgers"); continue
        if oracle == "upheld" and str(row.get("in_dismissed_ledger")) == "True":
            # export_ledger.py 标出的「同 id 也被人裁误报」行——README 明说消费方须排除。
            drop("in_dismissed_ledger"); continue
        if rid in seen:
            drop("duplicate_id"); continue        # upheld 按 job×finding 计行，同 id 会重复
        if not (row.get("quote") or "").strip():
            # 产品的 parse 层直接丢掉无 quote 的 finding（dropped_no_quote）——这种形状线上不存在。
            drop("no_quote"); continue
        sha, how = anchor_sha(a.repo, row, a.branch)
        if not sha:
            drop(how); continue
        text = git(a.repo, "show", f"{sha}:{row['file']}")
        if text is None:
            drop("file_missing_at_anchor"); continue
        span = locate(text, row["quote"])
        if not span:
            # 固定集自己的声明与它记的树对不上（导出截断 / 行号漂 / 合成引用）。
            # 这种条目上 REFUTED 其实是对的，算进误驳会冤枉模型——排除并披露条数。
            drop("quote_not_found_at_anchor"); continue
        seen.add(rid)

        # 物化：只写这一条需要的那个文件，树按 sha 复用（不 checkout 整棵树）
        root = os.path.join(a.out, "trees", sha[:12])
        dest = os.path.join(root, row["file"])
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        if not os.path.exists(dest):
            with open(dest, "w", encoding="utf-8") as fh:
                fh.write(text)

        ls = int(row.get("line_start") or 0) or span[0]
        le = int(row.get("line_end") or 0) or span[1]
        finding = {
            "id": rid, "file": row["file"], "line_start": ls, "line_end": le,
            "severity": row.get("severity") or "medium", "title": row.get("title") or "",
            "body": row.get("body") or "", "quote": row["quote"],
            "confidence": float(row.get("confidence") or 0.0),
        }
        fpath = os.path.join(a.out, "findings", f"{rid}.json")
        os.makedirs(os.path.dirname(fpath), exist_ok=True)
        with open(fpath, "w", encoding="utf-8") as fh:
            json.dump(finding, fh, ensure_ascii=False)
        cases.append({"id": rid, "oracle": oracle, "anchor": sha, "anchor_from": how,
                      "root": root, "finding": fpath, "file": row["file"],
                      "located": list(span), "lines_from": "row" if row.get("line_start") else "located"})

    os.makedirs(a.out, exist_ok=True)
    with open(os.path.join(a.out, "cases.json"), "w", encoding="utf-8") as fh:
        json.dump({"cases": cases, "dropped": dropped, "fixtures": os.path.abspath(a.fixtures),
                   "branch": a.branch, "built_at": time.strftime("%Y-%m-%dT%H:%M:%S")}, fh,
                  ensure_ascii=False, indent=1)
    n_d = sum(1 for c in cases if c["oracle"] == "dismissed")
    print(f"用例 {len(cases)}（dismissed {n_d} / upheld {len(cases) - n_d}），排除 {len(dropped)}")
    why = {}
    for d in dropped:
        why[d["why"]] = why.get(d["why"], 0) + 1
    for k, v in sorted(why.items(), key=lambda kv: -kv[1]):
        print(f"  排除 {k}: {v}")
    return 0


# ---------------- run：逐条过产品二进制 ----------------

def one_case(binpath, case, verifier, timeout):
    cmd = [binpath, "verify-once", case["finding"], "--root", case["root"]]
    if verifier:
        cmd += ["--verifier", verifier]
    t0 = time.time()
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return {**case, "ok": False, "verdict": None, "error": f"timeout after {timeout}s",
                "wall_s": round(time.time() - t0, 1)}
    line = (p.stdout or "").strip().splitlines()
    out = {}
    if line:
        try:
            out = json.loads(line[-1])
        except json.JSONDecodeError:
            out = {}
    if not out:
        return {**case, "ok": False, "verdict": None, "error": (p.stderr or "no output")[:300],
                "wall_s": round(time.time() - t0, 1)}
    return {**case, **out, "wall_s": round(time.time() - t0, 1)}


def cmd_run(a):
    meta = json.load(open(os.path.join(a.cases, "cases.json"), encoding="utf-8"))
    cases = meta["cases"]
    if a.limit:
        cases = cases[: a.limit]
    results = [None] * len(cases)
    done = 0
    with concurrent.futures.ThreadPoolExecutor(max_workers=a.parallel) as ex:
        futs = {ex.submit(one_case, a.bin, c, a.verifier, a.timeout): i for i, c in enumerate(cases)}
        for fut in concurrent.futures.as_completed(futs):
            i = futs[fut]
            results[i] = fut.result()
            done += 1
            r = results[i]
            print(f"[{done}/{len(cases)}] {r['id']} {r['oracle']:9s} → {r.get('verdict')} "
                  f"({r.get('wall_s')}s)", file=sys.stderr)
    payload = {"verifier": a.verifier, "bin": a.bin, "cases_dir": os.path.abspath(a.cases),
               "template_hash": next((r.get("template_hash") for r in results if r.get("template_hash")), None),
               "config_sha256": a.config_sha256, "ran_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
               "results": results}
    with open(a.out, "w", encoding="utf-8") as fh:
        json.dump(payload, fh, ensure_ascii=False, indent=1)
    print(f"写 {a.out}（{len(results)} 条）")
    return 0


# ---------------- score：混淆矩阵 + 两率 + 裁定 ----------------

def wilson(k, n, z=1.96):
    """Wilson 95% 区间——样本只有几十条时，点估计单看没意义。"""
    if n == 0:
        return (0.0, 0.0)
    p = k / n
    d = 1 + z * z / n
    c = p + z * z / (2 * n)
    half = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n))
    return (max(0.0, (c - half) / d), min(1.0, (c + half) / d))


def tally(results):
    t = {"dismissed": {"refuted": 0, "upheld": 0}, "upheld": {"refuted": 0, "upheld": 0},
         "failed": 0, "unparsed": 0}
    for r in results:
        if not r.get("ok") or r.get("verdict") not in ("refuted", "upheld"):
            t["failed"] += 1
            continue
        if not r.get("parsed", True):
            # 读不懂判决：产品按保守 upheld 收，这里照样计进矩阵，但单独留个数字。
            t["unparsed"] += 1
        t[r["oracle"]][r["verdict"]] += 1
    return t


def rates(t):
    nd = t["dismissed"]["refuted"] + t["dismissed"]["upheld"]
    nu = t["upheld"]["refuted"] + t["upheld"]["upheld"]
    refute = t["dismissed"]["refuted"] / nd if nd else 0.0
    false_refute = t["upheld"]["refuted"] / nu if nu else 0.0
    # 两边都得有样本才谈得上「过」：任一臂空集时 0/0 会被算成 0.0，于是「零证据」读成「零误驳」，
    # 把预登记的门判成通过。空集 = 判不了，不是判过。
    ok = None if nd == 0 or nu == 0 else (false_refute <= MAX_FALSE_REFUTE and refute >= MIN_REFUTE)
    return {"n_dismissed": nd, "n_upheld": nu, "refute_rate": refute, "false_refute_rate": false_refute,
            "refute_ci": wilson(t["dismissed"]["refuted"], nd),
            "false_refute_ci": wilson(t["upheld"]["refuted"], nu),
            "pass": ok}


def cmd_score(a):
    payload = json.load(open(a.run, encoding="utf-8"))
    t = tally(payload["results"])
    r = rates(t)
    print(f"模型 {payload.get('verifier')} · 模板 {payload.get('template_hash')} · 跑于 {payload.get('ran_at')}")
    print(f"混淆矩阵（行=人裁 oracle，列=复核判决）")
    print(f"  dismissed(期望 refuted): refuted {t['dismissed']['refuted']:3d} | upheld {t['dismissed']['upheld']:3d}")
    print(f"  upheld   (期望 upheld ): refuted {t['upheld']['refuted']:3d} | upheld {t['upheld']['upheld']:3d}")
    print(f"  调用失败 {t['failed']} · 判决读不懂（按保守 upheld 计） {t['unparsed']}")
    print(f"驳回率  {r['refute_rate']:.1%}  (n={r['n_dismissed']}, 95%CI {r['refute_ci'][0]:.1%}–{r['refute_ci'][1]:.1%})  门槛 ≥ {MIN_REFUTE:.0%}")
    print(f"误驳率  {r['false_refute_rate']:.1%}  (n={r['n_upheld']}, 95%CI {r['false_refute_ci'][0]:.1%}–{r['false_refute_ci'][1]:.1%})  门槛 ≤ {MAX_FALSE_REFUTE:.0%}")
    verdict = {True: "true", False: "false", None: "判不了（有一臂是空集）"}[r["pass"]]
    print(f"预登记裁定：verifiers.enabled 默认 {verdict}")
    if a.baseline:
        b = rates(tally(json.load(open(a.baseline, encoding="utf-8"))["results"]))
        print(f"对比基线：驳回 {b['refute_rate']:.1%} → {r['refute_rate']:.1%} · "
              f"误驳 {b['false_refute_rate']:.1%} → {r['false_refute_rate']:.1%}")
    if a.out:
        with open(a.out, "w", encoding="utf-8") as fh:
            json.dump({"verifier": payload.get("verifier"), "tally": t, **r}, fh, ensure_ascii=False, indent=1)
    return 0


# ---------------- selftest（零网络） ----------------

def cmd_selftest(_a):
    text = "fn a() {\n    let x = 1;\n    guard(x);\n}\n"
    assert locate(text, "let x = 1;") == (2, 2), locate(text, "let x = 1;")
    assert locate(text, "letx=1;guard(x);") == (2, 3), "跨行去空白定位"
    assert locate(text, "nope") is None

    mk = lambda oracle, verdict, ok=True, parsed=True: {"oracle": oracle, "verdict": verdict, "ok": ok, "parsed": parsed}
    t = tally([mk("dismissed", "refuted"), mk("dismissed", "upheld"), mk("upheld", "upheld"),
               mk("upheld", "refuted"), mk("upheld", "upheld", ok=False), mk("dismissed", "upheld", parsed=False)])
    assert t["dismissed"] == {"refuted": 1, "upheld": 2} and t["upheld"] == {"refuted": 1, "upheld": 1}, t
    assert t["failed"] == 1 and t["unparsed"] == 1, t
    # 失败不进矩阵：把它算成 upheld 会虚高「守住」那一格
    r = rates(t)
    assert (r["n_dismissed"], r["n_upheld"]) == (3, 2), r
    assert abs(r["refute_rate"] - 1 / 3) < 1e-9 and abs(r["false_refute_rate"] - 0.5) < 1e-9, r
    assert r["pass"] is False, "误驳 50% 必须判不通过"

    # 空集不算通过：0/0 被算成 0.0 会把「零证据」读成「零误驳」
    empty = {"dismissed": {"refuted": 0, "upheld": 0}, "upheld": {"refuted": 0, "upheld": 19}, "failed": 3, "unparsed": 0}
    assert rates(empty)["pass"] is None, "有一臂是空集 → 判不了，不是判过"

    # 阈值边界：误驳恰好 5%、驳回恰好 30% → 通过（预登记口径是 ≤ / ≥）
    ok = {"dismissed": {"refuted": 3, "upheld": 7}, "upheld": {"refuted": 1, "upheld": 19}, "failed": 0, "unparsed": 0}
    assert rates(ok)["pass"] is True, rates(ok)
    bad = {"dismissed": {"refuted": 3, "upheld": 7}, "upheld": {"refuted": 2, "upheld": 18}, "failed": 0, "unparsed": 0}
    assert rates(bad)["pass"] is False, "误驳 10% 不该通过"
    lo, hi = wilson(1, 20)
    assert lo < 0.05 < hi, (lo, hi)
    print("selftest ok")
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    b = sub.add_parser("build", help="锚定固定集并物化证据树（零网络）")
    b.add_argument("--fixtures", default="eval/fixtures")
    b.add_argument("--repo", default=".", help="取历史树的仓（本仓自己的台账）")
    b.add_argument("--branch", default="dev", help="dismissed 行按 updated_at 在该分支上找当日末次提交")
    b.add_argument("--out", default="eval/out/verify-cases")
    b.set_defaults(fn=cmd_build)

    r = sub.add_parser("run", help="逐条过 `redteam-next verify-once`")
    r.add_argument("cases", help="build 的产物目录")
    r.add_argument("--bin", default="./target/release/redteam-next")
    r.add_argument("--verifier", required=True, help="provider/id")
    r.add_argument("--parallel", type=int, default=4)
    r.add_argument("--timeout", type=int, default=900)
    r.add_argument("--limit", type=int, default=0, help="只跑前 N 条（冒烟）")
    r.add_argument("--config-sha256", default=None, help="跑前的 ~/.redteam/config.json 指纹，记进产物")
    r.add_argument("--out", required=True)
    r.set_defaults(fn=cmd_run)

    s = sub.add_parser("score", help="混淆矩阵 + 两率 + 预登记裁定")
    s.add_argument("run")
    s.add_argument("--baseline")
    s.add_argument("--out")
    s.set_defaults(fn=cmd_score)

    t = sub.add_parser("selftest", help="零网络自检（定位口径 / 计数 / 阈值边界）")
    t.set_defaults(fn=cmd_selftest)

    a = ap.parse_args()
    sys.exit(a.fn(a))


if __name__ == "__main__":
    main()
