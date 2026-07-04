#!/usr/bin/env python3
"""M14 2-pre:穷人版卷宗查全率评测(见 .doc/worktree-task.md)。

穷人版链路(被评测对象,刻意保持产品可实现的零依赖启发式):
  diff hunk → 正则提 changed symbols → rg -wF 词边界搜引用 → 缩进/括号取 enclosing block
Ground truth:rust-analyzer lsif 全量索引(评测侧工具,不进产品)。

用法:
  python3 harness.py run <commit>     # 单 commit 端到端,写 out/<commit>.json
  python3 harness.py report           # 聚合 out/*.json,打分场景统计表
"""
import json
import os
import re
import subprocess
import sys
from collections import defaultdict

REPO = os.environ.get("EVAL_REPO") or os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(REPO, "eval", "out")
TREES = os.environ.get(
    "EVAL_TREES",
    "/private/tmp/claude-501/-Users-weilai-hobby--worktrees-breakpoints-dossier-recall-eval/8a575419-e8fd-4d56-a8be-b57fae99e142/scratchpad/trees",
)
CARGO_TARGET = os.path.join(os.path.dirname(TREES), "cargo-target-shared")

# ---------- 穷人版:def 行正则(产品侧可照抄的启发式) ----------
DEF_PATTERNS = [
    ("fn", re.compile(
        r'^\s*(?:pub\s*(?:\([^)]*\))?\s*)?(?:default\s+)?(?:const\s+)?(?:async\s+)?'
        r'(?:unsafe\s+)?(?:extern\s+"[^"]*"\s+)?fn\s+([A-Za-z_]\w*)')),
    ("struct", re.compile(r'^\s*(?:pub\s*(?:\([^)]*\))?\s*)?struct\s+([A-Za-z_]\w*)')),
    ("enum", re.compile(r'^\s*(?:pub\s*(?:\([^)]*\))?\s*)?enum\s+([A-Za-z_]\w*)')),
    ("trait", re.compile(r'^\s*(?:pub\s*(?:\([^)]*\))?\s*)?(?:unsafe\s+)?trait\s+([A-Za-z_]\w*)')),
    ("type", re.compile(r'^\s*(?:pub\s*(?:\([^)]*\))?\s*)?type\s+([A-Za-z_]\w*)')),
    ("macro", re.compile(r'^\s*macro_rules!\s*([A-Za-z_]\w*)')),
    ("const", re.compile(r'^\s*(?:pub\s*(?:\([^)]*\))?\s*)?const\s+([A-Za-z_]\w*)\s*:')),
    ("static", re.compile(r'^\s*(?:pub\s*(?:\([^)]*\))?\s*)?static\s+(?:mut\s+)?([A-Za-z_]\w*)\s*:')),
]
IMPL_RE = re.compile(
    r'^\s*impl(?:<[^>]*>)?\s+(?:(?P<trait>[A-Za-z_][\w:]*)(?:<[^>]*>)?\s+for\s+)?(?P<type>[A-Za-z_][\w:]*)')


def match_def(line):
    for kind, rx in DEF_PATTERNS:
        m = rx.match(line)
        if m:
            return kind, m.group(1), m.start(1)
    return None


def sh(args, cwd=None, check=True):
    r = subprocess.run(args, cwd=cwd, capture_output=True, text=True)
    if check and r.returncode != 0:
        raise RuntimeError(f"{args}: {r.stderr[:500]}")
    return r.stdout


# ---------- 材料化 commit 树 ----------
def materialize(commit):
    d = os.path.join(TREES, commit)
    if not os.path.isdir(os.path.join(d, "src")):
        os.makedirs(d, exist_ok=True)
        subprocess.run(f"git -C '{REPO}' archive {commit} | tar -x -C '{d}'",
                       shell=True, check=True)
    return d


# ---------- 缩进/括号启发式:enclosing 块 ----------
def block_end(lines, i):
    """从 0-based 行 i 的 def 行起,括号配平找块尾行(0-based);无块(;结尾)返回 i。"""
    depth = 0
    seen_open = False
    for j in range(i, min(i + 4000, len(lines))):
        for ch in lines[j]:
            if ch == "{":
                depth += 1
                seen_open = True
            elif ch == "}":
                depth -= 1
        if seen_open and depth <= 0:
            return j
        if not seen_open and lines[j].rstrip().endswith(";"):
            return j
    return i


def enclosing_chain(lines, line0):
    """0-based 行 line0 的外围 def 链(内→外),元素 (行号0based, kind, name)。"""
    chain = []
    for i in range(line0, -1, -1):
        line = lines[i]
        d = match_def(line)
        if d:
            kind, name, _ = d
            if i == line0 or block_end(lines, i) >= line0:
                chain.append((i, kind, name))
            continue
        m = IMPL_RE.match(line)
        if m and block_end(lines, i) >= line0:
            chain.append((i, "impl_trait" if m.group("trait") else "impl", m.group("type")))
    return chain


def classify(kind, chain_above):
    """symbol 场景:kind + 外围上下文 → 评测分桶。"""
    if kind == "fn":
        for _, k, _ in chain_above:
            if k == "impl_trait":
                return "trait_impl_method"
            if k == "impl":
                return "inherent_method"
            if k == "trait":
                return "trait_decl_method"
        return "free_fn"
    if kind in ("struct", "enum", "type"):
        return "type_def"
    if kind == "trait":
        return "trait_def"
    if kind == "macro":
        return "macro_def"
    return "const_static"


# ---------- 穷人版 symbol 提取(diff → changed symbols) ----------
HUNK_RE = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")


def extract_symbols(commit, tree):
    diff = sh(["git", "-C", REPO, "show", "--format=", "-M", "-U0", commit, "--", "*.rs"])
    symbols = {}  # (file, name, kind) -> dict
    removed_names = set()
    path = None
    file_lines = None
    for raw in diff.splitlines():
        if raw.startswith("+++ "):
            path = None if raw == "+++ /dev/null" else raw[6:]
            file_lines = None
            if path:
                p = os.path.join(tree, path)
                file_lines = open(p).read().splitlines() if os.path.exists(p) else None
            continue
        if raw.startswith("--- ") or path is None or file_lines is None:
            if raw.startswith("-"):
                d = match_def(raw[1:])
                if d:
                    removed_names.add(d[1])
            continue
        m = HUNK_RE.match(raw)
        if m:
            new_start = int(m.group(3))
            new_count = int(m.group(4) or "1")
            anchor0 = max(0, min(new_start - 1, len(file_lines) - 1))
            chain = enclosing_chain(file_lines, anchor0)
            # 命中范围内的直接 def 行 + hunk 的 enclosing def 全收
            cand = list(chain[:1])  # 最内层 enclosing
            for ln0 in range(new_start - 1, new_start - 1 + max(new_count, 0)):
                if 0 <= ln0 < len(file_lines):
                    d = match_def(file_lines[ln0])
                    if d:
                        cand.append((ln0, d[0], d[1]))
            for ln0, kind, name in cand:
                if kind in ("impl", "impl_trait"):
                    continue  # impl 头本身不是 symbol,类型名由 struct/enum def 承载
                key = (path, name, kind)
                if key not in symbols:
                    outer = enclosing_chain(file_lines, ln0)[1:] if kind == "fn" else []
                    d2 = match_def(file_lines[ln0])
                    col = d2[2] if d2 and d2[1] == name else 0
                    symbols[key] = {
                        "name": name, "kind": kind, "file": path,
                        "line": ln0 + 1, "col": col,
                        "scenario": classify(kind, outer),
                    }
            continue
        if raw.startswith("-"):
            d = match_def(raw[1:])
            if d:
                removed_names.add(d[1])
    vanished = sorted(n for n in removed_names
                      if not any(s["name"] == n for s in symbols.values()))
    return list(symbols.values()), vanished


# ---------- 穷人版引用搜索:rg -wF ----------
def rg_refs(tree, name):
    # 显式给 '.':stdin 为管道时(heredoc 调用)rg 启发式会改搜 stdin,静默全 0
    r = subprocess.run(["rg", "-wF", "--json", "-t", "rust", "--", name, "."],
                       cwd=tree, capture_output=True, text=True)
    hits = []
    for ln in r.stdout.splitlines():
        try:
            j = json.loads(ln)
        except json.JSONDecodeError:
            continue
        if j.get("type") == "match":
            d = j["data"]
            path = d["path"]["text"]
            path = path[2:] if path.startswith("./") else path
            hits.append((path, d["line_number"],
                         d["lines"]["text"].rstrip("\n")))
    return hits


# ---------- ground truth:rust-analyzer lsif ----------
def run_lsif(tree):
    cache = os.path.join(tree, "lsif.json")
    if not os.path.exists(cache):
        env = dict(os.environ, CARGO_TARGET_DIR=CARGO_TARGET)
        with open(cache, "w") as f:
            r = subprocess.run(["rust-analyzer", "lsif", "."], cwd=tree,
                               stdout=f, stderr=subprocess.PIPE, text=True, env=env)
        if r.returncode != 0:
            os.remove(cache)
            raise RuntimeError(f"lsif failed: {r.stderr[-800:]}")
    return cache


def parse_lsif(path, tree):
    doc_uri = {}
    range_pos = {}
    range_doc = {}
    nxt = {}
    refres = {}
    items = defaultdict(lambda: {"definitions": [], "references": []})
    prefix = "file://" + tree.rstrip("/") + "/"
    with open(path) as f:
        for ln in f:
            j = json.loads(ln)
            lab = j.get("label")
            if j["type"] == "vertex":
                if lab == "document":
                    uri = j["uri"]
                    doc_uri[j["id"]] = uri[len(prefix):] if uri.startswith(prefix) else uri
                elif lab == "range":
                    range_pos[j["id"]] = (j["start"]["line"], j["start"]["character"])
            else:
                if lab == "contains":
                    for rid in j["inVs"]:
                        range_doc[rid] = j["outV"]
                elif lab == "next":
                    nxt[j["outV"]] = j["inV"]
                elif lab == "textDocument/references":
                    refres[j["outV"]] = j["inV"]
                elif lab == "item" and j.get("property") in ("definitions", "references"):
                    for rid in j["inVs"]:
                        items[j["outV"]][j["property"]].append((j["document"], rid))
    return doc_uri, range_pos, range_doc, nxt, refres, items


def gt_for_symbol(sym, lsif, tree):
    doc_uri, range_pos, range_doc, nxt, refres, items = lsif
    text = open(os.path.join(tree, sym["file"])).read().splitlines()[sym["line"] - 1]
    name_col = text.find(sym["name"])  # def 行上 name 的列号,用于挑同行多 token
    best = None  # (col 距离, range_id)
    for rid, (line, char) in range_pos.items():
        if line != sym["line"] - 1 or rid not in nxt:
            continue
        if doc_uri.get(range_doc.get(rid)) != sym["file"]:
            continue
        d = abs(char - (name_col if name_col >= 0 else sym["col"]))
        if best is None or d < best[0]:
            best = (d, rid)
    if best is None or best[0] > 2:
        return None
    rs = nxt[best[1]]
    rr = refres.get(rs)
    if rr is None:
        return {"defs": set(), "refs": set()}
    got = items[rr]
    defs = {(doc_uri[d], range_pos[r][0] + 1) for d, r in got["definitions"]}
    refs = {(doc_uri[d], range_pos[r][0] + 1) for d, r in got["references"]}
    return {"defs": defs, "refs": refs - defs}


# ---------- 卷宗体积估算 ----------
def dossier_size(tree, refs_by_symbol):
    """±3 行档:每引用点 = 1 头行 + 7 行代码;返回 (bytes, est_tokens)。"""
    total = 0
    cache = {}
    for sym, hits in refs_by_symbol.items():
        for path, line, _ in hits:
            if path not in cache:
                try:
                    cache[path] = open(os.path.join(tree, path)).read().splitlines()
                except OSError:
                    cache[path] = []
            lines = cache[path]
            lo, hi = max(0, line - 4), min(len(lines), line + 3)
            total += len(f"{path}:{line} ({sym})\n") + sum(len(l) + 1 for l in lines[lo:hi])
    return total


# ---------- 单 commit 端到端 ----------
def run(commit):
    tree = materialize(commit)
    symbols, vanished = extract_symbols(commit, tree)
    print(f"[{commit}] symbols={len(symbols)} vanished={len(vanished)}", flush=True)
    lsif = parse_lsif(run_lsif(tree), tree)
    print(f"[{commit}] lsif parsed", flush=True)

    results = []
    refs_by_symbol = {}
    for sym in symbols:
        hits = rg_refs(tree, sym["name"])
        gt = gt_for_symbol(sym, lsif, tree)
        # 全仓同名 def 行(歧义度):def 正则命中且词边界含 name
        same_name_defs = set()
        for path, line, text in hits:
            d = match_def(text)
            if d and d[1] == sym["name"]:
                same_name_defs.add((path, line))
        grep_refs = {(p, l) for p, l, _ in hits} - same_name_defs
        refs_by_symbol[sym["name"]] = [(p, l, t) for p, l, t in hits
                                       if (p, l) in grep_refs]
        rec = dict(sym)
        rec["defs_same_name"] = len(same_name_defs)
        rec["grep_ref_count"] = len(grep_refs)
        if gt is None:
            rec["gt"] = "no_anchor"
        else:
            gt_refs = gt["refs"]
            tp = grep_refs & gt_refs
            rec["gt"] = "ok"
            rec["gt_ref_count"] = len(gt_refs)
            rec["tp"] = len(tp)
            rec["gt_only"] = sorted(f"{p}:{l}" for p, l in (gt_refs - grep_refs))[:20]
            rec["grep_only"] = sorted(f"{p}:{l}" for p, l in (grep_refs - gt_refs))[:40]
        results.append(rec)

    size_b = dossier_size(tree, refs_by_symbol)
    out = {
        "commit": commit,
        "subject": sh(["git", "-C", REPO, "show", "-s", "--format=%s", commit]).strip(),
        "vanished_symbols": vanished,
        "dossier_bytes_pm3": size_b,
        "dossier_est_tokens_pm3": int(size_b / 3.5),
        "symbols": results,
    }
    os.makedirs(OUT, exist_ok=True)
    with open(os.path.join(OUT, f"{commit}.json"), "w") as f:
        json.dump(out, f, ensure_ascii=False, indent=1)
    print(f"[{commit}] done: {len(results)} symbols, dossier≈{out['dossier_est_tokens_pm3']} tok", flush=True)


# ---------- 聚合报表 ----------
def report():
    rows = defaultdict(lambda: dict(gt=0, tp=0, grep=0, syms=0, no_anchor=0, ambig=0))
    per_commit = []
    for fn in sorted(os.listdir(OUT)):
        if not fn.endswith(".json"):
            continue
        j = json.load(open(os.path.join(OUT, fn)))
        per_commit.append((j["commit"], j["subject"][:50], len(j["symbols"]),
                           j["dossier_est_tokens_pm3"]))
        for s in j["symbols"]:
            r = rows[s["scenario"]]
            r["syms"] += 1
            if s.get("defs_same_name", 0) > 1:
                r["ambig"] += 1
            if s.get("gt") != "ok":
                r["no_anchor"] += 1
                continue
            r["gt"] += s["gt_ref_count"]
            r["tp"] += s["tp"]
            r["grep"] += s["grep_ref_count"]
    print(f"{'scenario':<20}{'syms':>5}{'ambig':>6}{'noGT':>5}{'GTrefs':>7}"
          f"{'TP':>6}{'grep':>6}{'recall':>8}{'prec':>7}")
    for sc, r in sorted(rows.items()):
        rec = r["tp"] / r["gt"] if r["gt"] else float("nan")
        prec = r["tp"] / r["grep"] if r["grep"] else float("nan")
        print(f"{sc:<20}{r['syms']:>5}{r['ambig']:>6}{r['no_anchor']:>5}{r['gt']:>7}"
              f"{r['tp']:>6}{r['grep']:>6}{rec:>8.3f}{prec:>7.3f}")
    print("\nper-commit dossier (±3行档, est tokens):")
    for c, subj, n, tok in per_commit:
        print(f"  {c} syms={n:<3} ≈{tok:>6} tok  {subj}")


if __name__ == "__main__":
    if sys.argv[1] == "run":
        run(sys.argv[2])
    elif sys.argv[1] == "report":
        report()
