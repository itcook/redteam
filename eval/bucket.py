#!/usr/bin/env python3
"""名字桶(name-bucket)口径重算:grep 命中 vs「同名全部符号 GT 引用之并集」。

动机:rust-analyzer 把 trait 分发调用点挂在 trait decl 名下,per-symbol 口径会把
grep 的名字桶判成假阳;卷宗语义下名字桶才是穷人版真实交付物。
残余噪声再按行形态分类(doc 注释/字符串/属性/其他)。
"""
import json
import glob
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import harness as H

NOISE_KIND = [
    ("doc_comment", re.compile(r'^\s*(///|//!|//)')),
    ("attr", re.compile(r'^\s*#\[')),
    ("use_decl", re.compile(r'^\s*(pub\s+)?use\s')),
]


def classify_noise(text, name):
    for kind, rx in NOISE_KIND:
        if rx.match(text):
            return kind
    # name 出现在字符串字面量内(粗略:两侧引号)
    m = re.search(r'"[^"]*\b' + re.escape(name) + r'\b[^"]*"', text)
    if m:
        return "string_lit"
    return "code_other"


def def_sites(tree, name):
    """全仓该名字的 def 行(穷人版 def 正则口径)。"""
    sites = []
    for path, line, text in H.rg_refs(tree, name):
        d = H.match_def(text)
        if d and d[1] == name:
            sites.append((path, line, text))
    return sites


def main():
    agg = {}  # scenario -> dict
    noise_total = {}
    for fn in sorted(glob.glob(os.path.join(H.OUT, "*.json"))):
        j = json.load(open(fn))
        tree = H.materialize(j["commit"])
        lsif = H.parse_lsif(os.path.join(tree, "lsif.json"), tree)
        for s in j["symbols"]:
            if s.get("gt") != "ok":
                continue
            name = s["name"]
            hits = H.rg_refs(tree, name)
            defs = def_sites(tree, name)
            def_locs = {(p, l) for p, l, _ in defs}
            grep_refs = {(p, l): t for p, l, t in hits if (p, l) not in def_locs}
            union_gt = set()
            for p, l, t in defs:
                d = H.match_def(t)
                col = d[2] if d else 0
                gt = H.gt_for_symbol({"file": p, "line": l, "name": name, "col": col}, lsif, tree)
                if gt:
                    union_gt |= gt["refs"]
            union_gt -= def_locs
            tp = set(grep_refs) & union_gt
            fp = set(grep_refs) - union_gt
            a = agg.setdefault(s["scenario"], dict(gt=0, tp=0, grep=0, syms=0))
            a["syms"] += 1
            a["gt"] += len(union_gt)
            a["tp"] += len(tp)
            a["grep"] += len(grep_refs)
            miss = union_gt - set(grep_refs)
            if miss:
                print(f"!! GT-only(桶口径漏检) {j['commit']} {name}: {sorted(miss)[:5]}")
            for loc in fp:
                k = classify_noise(grep_refs[loc], name)
                noise_total[k] = noise_total.get(k, 0) + 1

    print(f"\n{'scenario':<20}{'syms':>5}{'GT∪':>7}{'TP':>6}{'grep':>6}{'recall':>8}{'prec':>7}")
    for sc, a in sorted(agg.items()):
        rec = a["tp"] / a["gt"] if a["gt"] else float("nan")
        prec = a["tp"] / a["grep"] if a["grep"] else float("nan")
        print(f"{sc:<20}{a['syms']:>5}{a['gt']:>7}{a['tp']:>6}{a['grep']:>6}{rec:>8.3f}{prec:>7.3f}")
    print("\n残余噪声形态分布:", dict(sorted(noise_total.items(), key=lambda kv: -kv[1])))


if __name__ == "__main__":
    main()
