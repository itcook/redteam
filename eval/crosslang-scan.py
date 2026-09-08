#!/usr/bin/env python3
"""大型 Go/TS 仓穷人版卷宗可行性扫描(纯 grep,无 oracle；EVAL_REPO 指向被扫仓):
1. 近期 fix commits 提 changed symbols(Go/TS 各自 def 正则)
2. 每 symbol 名字桶规模(rg -w)+ 同名 def 数(歧义度)
3. per-bundle 卷宗体积估算(±3行档 ~60 tok/引用点,redteam 仓校准值)
4. TS default export 的 import 改名率(查全洞发生率)
"""
import json, os, re, subprocess, sys
from collections import defaultdict

REPO = os.path.expanduser(os.environ.get("EVAL_REPO", "."))

GO_DEFS = [
    re.compile(r'^func\s+(?:\([^)]+\)\s+)?([A-Za-z_]\w*)\s*[(\[]'),
    re.compile(r'^type\s+([A-Za-z_]\w*)\s+(?:struct|interface)'),
    re.compile(r'^(?:var|const)\s+([A-Za-z_]\w*)\s'),
]
TS_DEFS = [
    re.compile(r'^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s+([A-Za-z_]\w*)'),
    re.compile(r'^\s*(?:export\s+)?(?:const|let)\s+([A-Za-z_]\w*)\s*=\s*(?:async\s*)?(?:\(|function|React\.)'),
    re.compile(r'^\s*(?:export\s+)?(?:abstract\s+)?class\s+([A-Za-z_]\w*)'),
    re.compile(r'^\s*(?:export\s+)?(?:interface|enum)\s+([A-Za-z_]\w*)'),
    re.compile(r'^\s*(?:export\s+)?type\s+([A-Za-z_]\w*)\s*='),
]
KW = {'if','for','while','switch','return','catch','constructor','function','default','async','await','new','delete','typeof','in','of','case','do','else','try','finally','with','yield','static','get','set'}

def sh(args, cwd=REPO):
    return subprocess.run(args, cwd=cwd, capture_output=True, text=True).stdout

def lang_of(path):
    if path.endswith('.go'): return 'go'
    if re.search(r'\.(ts|tsx|js|jsx)$', path) and 'node_modules' not in path: return 'ts'
    return None

def defs_in(line, lang):
    for rx in (GO_DEFS if lang == 'go' else TS_DEFS):
        m = rx.match(line)
        if m and m.group(1) not in KW and len(m.group(1)) >= 2:
            return m.group(1)
    return None

def rg_count(name, lang):
    """(词边界命中行数, 同名 def 行数)"""
    glob = ['-g', '*.go', '-g', '!vendor'] if lang == 'go' else \
           ['-g', '*.{ts,tsx,js,jsx}', '-g', '!node_modules', '-g', '!dist', '-g', '!*.min.js']
    # 显式给 '.',否则 stdin 为管道时 rg 启发式改搜 stdin(heredoc 场景全 0)
    out = sh(['rg', '-wF', '--no-heading', '-N', *glob, '--', name, '.'])
    lines = out.splitlines()
    d = sum(1 for l in lines if defs_in(l.split(':', 1)[-1], lang))
    return len(lines), d

def pick_commits(n=24):
    log = sh(['git', 'log', '--grep', 'fix', '-i', '--format=%h %s', '-200'])
    picked = []
    for ln in log.splitlines():
        h, s = ln.split(' ', 1)
        files = [f for f in sh(['git', 'show', '--format=', '--name-only', h]).splitlines() if lang_of(f)]
        if not files or len(files) > 30:  # 排空样本与超大重构
            continue
        picked.append((h, s, files))
        if len(picked) >= n: break
    return picked

def changed_symbols(commit):
    diff = sh(['git', 'show', '--format=', '-M', '-U3', commit])
    syms, path, lang = set(), None, None
    for raw in diff.splitlines():
        if raw.startswith('+++ '):
            path = raw[6:] if raw != '+++ /dev/null' else None
            lang = lang_of(path) if path else None
            continue
        if lang is None: continue
        m = re.match(r'^@@ .* @@ (.*)$', raw)  # git hunk 头函数上下文(Go 内置 driver 可用)
        if m:
            n = defs_in(m.group(1), lang)
            if n: syms.add((n, lang))
        if raw[:1] in '+-' and raw[:3] not in ('+++', '---'):
            n = defs_in(raw[1:], lang)
            if n: syms.add((n, lang))
    return syms

TOK_PER_REF = 60  # redteam 仓 ±3 行档校准值

def main():
    rows = []
    for h, subj, files in pick_commits():
        syms = changed_symbols(h)
        if not syms: continue
        total, c10, c20, det = 0, 0, 0, []
        for name, lang in syms:
            hits, ndef = rg_count(name, lang)
            refs = max(0, hits - ndef)
            total += refs; c10 += min(refs, 10); c20 += min(refs, 20)
            det.append((name, lang, refs, ndef))
        rows.append((h, subj[:44], len(syms), total, c10, c20, det))
    print(f"{'commit':<10}{'syms':>5}{'refs':>6}{'tok(∞)':>9}{'tok(c10)':>9}{'tok(c20)':>9}  subject")
    over = 0
    for h, s, n, t, c10, c20, _ in rows:
        tok, t10, t20 = t*TOK_PER_REF, c10*TOK_PER_REF, c20*TOK_PER_REF
        if t10 > 8000: over += 1
        print(f"{h:<10}{n:>5}{t:>6}{tok:>9}{t10:>9}{t20:>9}  {s}")
    print(f"\ncap10 后仍超 8K: {over}/{len(rows)}")
    print("\n=== 爆桶 top(refs>80) 与高歧义(defs>3) ===")
    for h, s, n, t, _, _, det in rows:
        for name, lang, refs, ndef in sorted(det, key=lambda x: -x[2]):
            if refs > 80 or ndef > 3:
                print(f"  {h} [{lang}] {name}: refs={refs} defs={ndef}")

if __name__ == '__main__':
    main()
