#!/usr/bin/env python3
"""P0 E1 · 风险规则零 LLM 回放（design §3.3 四类信号初版 → 热区 H0 + PR 级风险分 R）。

只读被评仓：diff 走 `git diff base...head`，文件内容走 `git show head:path`，fan-in 走 `git grep`，
不 checkout、不写目标仓。零第三方依赖（照 eval/harness.py 纪律）。def 正则与块配平移植自
src/diffsym/mod.rs（rs / go / ts 三语言）。

子命令：
  replay <repo> <base> <head> [--history-repo R] [--history-ref REF(默认 base)] [--ledger DB] [--before TS]
         [--theta 2.0] [--fanin-k 5] [--extapi-k 3] [--out FILE]
  score  <replay.json> <oracle.json> [--set NAME ...] [--theta θ] [--out FILE]
         命中口径：区间级（裁决）= 同文件且 [ls,le] 与 H0 区间有交集；文件级（诊断）= 同文件。
         空热区 = 与该 oracle 集合任何条目都无交集的 H0 区间。
  report --pair replay.json:oracle.json:set ... [--dist replay.json ...] [--thetas 1.5,2,2.5,3]
         θ 曲线 + (规模, |H0|, R) 表 + 三档 max_cells 起步值。
  compare <replay.json> <rust-out.json> [--tol 1e-6] [--show N] [--out FILE]
         Rust `redteam risk` 输出（区级 H0）vs Python v4 回放对拍：报区级 Jaccard 与 R 差；
         R 差 > 容差即退出码 1——改 θ / 权重必须过此门（D11 定案）。
  selftest
"""
import argparse
import json
import math
import os
import re
import sqlite3
import subprocess
import sys
import urllib.parse
from collections import defaultdict

# ---------------- 语言 def 正则（逐条照抄 src/diffsym/mod.rs） ----------------
LANGS = {
    "rs": [
        ("fn", r'^\s*(?:pub\s*(?:\([^)]*\))?\s*)?(?:default\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?(?:extern\s+"[^"]*"\s+)?fn\s+([A-Za-z_]\w*)'),
        ("struct", r'^\s*(?:pub\s*(?:\([^)]*\))?\s*)?struct\s+([A-Za-z_]\w*)'),
        ("enum", r'^\s*(?:pub\s*(?:\([^)]*\))?\s*)?enum\s+([A-Za-z_]\w*)'),
        ("trait", r'^\s*(?:pub\s*(?:\([^)]*\))?\s*)?(?:unsafe\s+)?trait\s+([A-Za-z_]\w*)'),
        ("type", r'^\s*(?:pub\s*(?:\([^)]*\))?\s*)?type\s+([A-Za-z_]\w*)'),
        ("macro", r'^\s*macro_rules!\s*([A-Za-z_]\w*)'),
        ("const", r'^\s*(?:pub\s*(?:\([^)]*\))?\s*)?const\s+([A-Za-z_]\w*)\s*:'),
        ("static", r'^\s*(?:pub\s*(?:\([^)]*\))?\s*)?static\s+(?:mut\s+)?([A-Za-z_]\w*)\s*:'),
    ],
    "go": [
        ("func", r'^\s*func\s+(?:\([^)]*\)\s*)?([A-Za-z_]\w*)'),
        ("type", r'^\s*type\s+([A-Za-z_]\w*)'),
        ("const", r'^\s*(?:const|var)\s+([A-Za-z_]\w*)'),
    ],
    "ts": [
        ("function", r'^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*([A-Za-z_$][\w$]*)'),
        ("class", r'^\s*(?:export\s+)?(?:default\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][\w$]*)'),
        ("type", r'^\s*(?:export\s+)?(?:declare\s+)?(?:interface|type|enum)\s+([A-Za-z_$][\w$]*)'),
        ("const", r'^\s*(?:export\s+)?(?:default\s+)?(?:const|let|var)\s+([A-Za-z_$][\w$]*)'),
        ("method", r'^\s*(?:(?:public|private|protected|static|async|readonly|get|set)\s+|\*\s*)*([A-Za-z_$][\w$]*)\s*\([^)]*\)\s*[:{]'),
    ],
}
EXT_LANG = {"rs": "rs", "go": "go", "ts": "ts", "tsx": "ts", "js": "ts", "jsx": "ts", "mts": "ts", "cts": "ts"}
TS_KEYWORDS = {"if", "for", "while", "switch", "catch", "return", "do", "else", "function", "constructor",
               "await", "yield", "typeof", "new"}
_COMPILED = {k: [(kind, re.compile(rx)) for kind, rx in v] for k, v in LANGS.items()}


def lang_for(path):
    ext = path.rsplit(".", 1)[-1] if "." in path else ""
    return EXT_LANG.get(ext)


def match_def(line, lang):
    if not lang:
        return None
    for kind, rx in _COMPILED[lang]:
        m = rx.match(line)
        if m:
            name = m.group(1)
            if kind == "method" and name in TS_KEYWORDS:
                return None
            return kind, name
    return None


def is_decl(kind, line):
    """结构信号与 enclosing 只认真正的声明：const/var 只在顶层（无缩进）计入，函数体内局部变量不算。"""
    if kind == "const":
        return not line[:1].isspace()
    return True


def match_decl(line, lang):
    m = match_def(line, lang)
    return m if m and is_decl(m[0], line) else None


def block_end(lines, i):
    depth, seen_open = 0, False
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


def decl_block_end(lines, i, lang):
    """R2.2：Go/TS 单行 const/var 声明无 `;` 结尾，brace 配平会吞到下一个函数——单行声明块尾即自身。"""
    m = match_decl(lines[i], lang)
    if m and m[0] == "const" and "{" not in lines[i] and not lines[i].rstrip().endswith("("):
        return i
    return block_end(lines, i)


def fanin_worthy(n):
    """R2.3：fan-in 只看像『符号』的名字——过滤短名、通用名与纯小写单词（cancel / resp / object 之类）。"""
    if len(n) < 4 or n.lower() in GENERIC_NAMES:
        return False
    if n.islower() and n.isalpha() and len(n) < 8:
        return False
    return True


def enclosing_def_start(lines, line0, lang):
    if not lines:
        return None
    line0 = min(line0, len(lines) - 1)
    for i in range(line0, -1, -1):
        if match_decl(lines[i], lang) and (i == line0 or block_end(lines, i) >= line0):
            return i
    return None


# ---------------- git 只读访问 ----------------
def git(repo, *args, check=True):
    r = subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True, errors="replace")
    if check and r.returncode != 0:
        raise RuntimeError(f"git {' '.join(args[:3])}…: {r.stderr[:300]}")
    return r.stdout


def show_lines(repo, head, path):
    # `--end-of-options`：head 来自 argv（与 Rust 侧同族防御，挡 `--output=` 类 option 注入）
    r = subprocess.run(["git", "-C", repo, "show", "--end-of-options", f"{head}:{path}"], capture_output=True, text=True, errors="replace")
    return r.stdout.split("\n") if r.returncode == 0 else None


HUNK_RE = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")


def parse_diff(text):
    """unified diff → {path: {"hunks": [{"new_start", "new_count", "added": [(line_no, text)], "removed": [text]}],
    "adds", "dels", "deleted": bool, "binary": bool}}（path = 新路径）。

    状态机守卫（2026-09-10 M21b 前置核实修）：`+++ ` / `--- ` 仅在本文件首个 `@@` 之前当作文件头；
    进入 hunk 后列首 `++` / `--` 是内容行（新加行正文以 `++ ` 开头 → 行文本 `+++ …`；删除行正文以
    `-- ` 开头（SQL / Haskell 注释等）→ 行文本 `--- …`），不再误判文件头（旧行为：dels 少计、files 污染）。"""
    files, cur, hunk = {}, None, None
    new_line = 0
    for raw in text.split("\n"):
        if raw.startswith("diff --git "):
            cur, hunk = None, None
            continue
        in_hunk = hunk is not None
        if raw.startswith("+++ ") and not in_hunk:
            p = raw[4:].strip()
            if p == "/dev/null":
                cur = None
                continue
            p = p[2:] if p.startswith("b/") else p
            cur = files.setdefault(p, {"hunks": [], "adds": 0, "dels": 0, "deleted": False, "binary": False})
            continue
        if raw.startswith("--- ") and not in_hunk:
            continue
        if raw.startswith("Binary files") and cur is not None:
            cur["binary"] = True
            continue
        if cur is None:
            # 删除文件：+++ /dev/null 之后 cur=None，其 hunk 不进热区（head 树无此文件）
            continue
        m = HUNK_RE.match(raw)
        if m:
            ns, nc = int(m.group(3)), int(m.group(4) if m.group(4) is not None else 1)
            hunk = {"new_start": ns, "new_count": nc, "added": [], "removed": []}
            cur["hunks"].append(hunk)
            new_line = ns
            continue
        if hunk is None:
            continue
        if raw.startswith("+"):
            hunk["added"].append((new_line, raw[1:]))
            cur["adds"] += 1
            new_line += 1
        elif raw.startswith("-"):
            hunk["removed"].append(raw[1:])
            cur["dels"] += 1
        elif raw.startswith("\\"):
            pass
        else:
            new_line += 1
    return files


# ---------------- 信号规则（design §3.3 四类，v1） ----------------
PATH_SENSITIVE = re.compile(r"(auth|secur|crypt|payment|billing|config|setting|secret|token|permission|migrat|callback|webhook)", re.I)
KEYWORD_CLASSES = {
    "deser": re.compile(r"(?i)\b(unmarshal|deserializ|json\.parse|parseint|parsefloat|strconv\.|url\.parse|new url\(|querystring)"),
    "exec_sql": re.compile(r"(?i)(\bexec\.|os/exec|subprocess|\beval\(|\bunsafe\b|new function\(|child_process|db\.(exec|query|raw)\b|\bsql\.|select\s.+\sfrom\s|insert\s+into|update\s+\w+\s+set|delete\s+from|\.raw\()"),
    "condwrite": re.compile(r"(?i)(etag|if-none-match|if-match|onlyif|fingerprint|idempot|precondition)"),
    "timeout": re.compile(r"(?i)\b(timeout|deadline|retry|retries|backoff|attempt|context\.with(timeout|deadline|cancel)|abortsignal|settimeout|time\.after)"),
    "concurrency": re.compile(r"(go func|sync\.(Mutex|RWMutex|WaitGroup|Once)|atomic\.|\bchan\b|<-|Promise\.(all|race|allSettled)|\bMutex\b|Semaphore|\.[Ll]ock\(|\btx\.|Transaction|FOR UPDATE)"),
    "destructive": re.compile(r"(?i)\b(delete|remove|truncate|drop|overwrite|unlink|purge)\b"),
    "auth": re.compile(r"(?i)\b(auth|authorize|permission|role|token|secret|password|credential|signature|hmac|jwt)\b"),
    # R2.5：L3 / 配置类补三类（09-03 A/B 记录点名『配置矩阵×默认值组合洞』；L3 partial failure / 资源生命周期）
    "numeric_bounds": re.compile(r"(?i)(\b(min(imum)?|max(imum)?|clamp|ceil(ing)?|floor|margin|threshold|limit|bound(s|ary)?)\b|\* ?1024|/ ?1024|bytesPerSecond|time\.Duration|Seconds\(\)|Math\.(min|max|ceil|floor|round)|math\.(Min|Max|Ceil|Floor))"),
    "error_paths": re.compile(r"(?i)\b(throw|catch|panic|recover|unwrap\(|expect\(|errors?\.(Is|As|New|Wrap)|instanceof \w*Error|reject\()"),
    "resource_lifecycle": re.compile(r"(?i)\b(cancel|abort|close|release|dispose|drain|cleanup|defer|finally|reader|writer|stream)\b"),
}
SUBSTANTIVE = re.compile(r"^\s*(?![/#*]|$)\S")
COMMENT_RE = re.compile(r"//[^\n]*|#[^\n]*|/\*.*?\*/", re.S)


def strip_comments(text):
    return COMMENT_RE.sub("", text)


CONTROL_FLOW = re.compile(r"\b(if|for|while|switch|match|return|await|try|catch|select|defer|throw|loop|yield)\b")
FIX_COMMIT = re.compile(r"(?i)\b(fix|bug|hotfix|revert|regress)")
TEST_PATH = re.compile(r"(_test\.go$|\.test\.(ts|js|tsx)$|(^|/)tests?/|(^|/)__tests__/|_spec\.)")
DOC_PATH = re.compile(r"\.(md|json|ya?ml|toml|txt|lock|svg|csv)$", re.I)
GENERIC_NAMES = {"main", "init", "new", "string", "error", "handler", "handle", "run", "get", "set", "test", "default",
                 "index", "start", "stop", "close", "open", "read", "write", "name", "type", "value", "data", "config"}
NODE_BUILTINS = {"fs", "path", "http", "https", "crypto", "url", "stream", "buffer", "events", "util", "os", "zlib",
                 "child_process", "assert", "net", "tls", "dns", "readline", "worker_threads", "timers", "process"}
RUST_INTERNAL = {"crate", "super", "self", "std", "core", "alloc"}
W = {"path_glob": 1.0, "keyword": 0.5, "keyword_cap": 1.5, "sig_changed": 1.0, "def_new": 0.5, "def_deleted": 1.0,
     "public_widen": 0.5, "fanin": 1.0, "churn": 0.5, "extapi": 1.0, "hist_fix3": 0.5, "hist_fix6": 1.0,
     "ledger_upheld": 1.0, "ledger_dismissed": 0.5, "test_mult": 0.3, "doc_mult": 0.2,
     "new_file": 0.5, "ripple": 1.0, "decl_only_mult": 0.5}
ADJ_GAP = 5
NO_DEF_PAD = 10
RIPPLE_MAX_SITES = 4


def parse_imports(path, lines, go_module=None):
    """返回 [(prefix_or_name, kind)]：kind = 'ns'（`pkg.X` 形态）| 'name'（裸标识符）。只取外部（非相对、非标准库、非本模块）。"""
    lang = lang_for(path)
    out = []
    text = "\n".join(lines)
    if lang == "go":
        specs = []
        for m in re.finditer(r'^\s*import\s+(?:(\w+)\s+)?"([^"]+)"', text, re.M):
            specs.append((m.group(1), m.group(2)))
        for blk in re.finditer(r'^\s*import\s*\((.*?)^\s*\)', text, re.M | re.S):
            for m in re.finditer(r'^\s*(?:(\w+)\s+)?"([^"]+)"', blk.group(1), re.M):
                specs.append((m.group(1), m.group(2)))
        for alias, p in specs:
            first = p.split("/")[0]
            if "." not in first:
                continue  # 标准库
            if go_module and p.startswith(go_module):
                continue  # 本模块
            if alias in ("_", "."):
                continue
            out.append((alias or p.rstrip("/").split("/")[-1], "ns"))
    elif lang == "ts":
        for m in re.finditer(r'^\s*import\s+(type\s+)?(.+?)\s+from\s+["\']([^"\']+)["\']', text, re.M):
            spec = m.group(3)
            if m.group(1) or spec.startswith((".", "/")) or spec.startswith("node:") or spec.split("/")[0] in NODE_BUILTINS:
                continue
            clause = m.group(2).strip()
            ns = re.match(r"\*\s+as\s+(\w+)", clause)
            if ns:
                out.append((ns.group(1), "ns"))
                continue
            default = re.match(r"([A-Za-z_$][\w$]*)\s*(?:,|$)", clause)
            if default:
                out.append((default.group(1), "name"))
            braces = re.search(r"\{(.*?)\}", clause, re.S)
            if braces:
                for item in braces.group(1).split(","):
                    item = item.strip()
                    if not item or item.startswith("type "):
                        continue
                    name = item.split(" as ")[-1].strip()
                    if re.match(r"^[A-Za-z_$][\w$]*$", name):
                        out.append((name, "name"))
    elif lang == "rs":
        for m in re.finditer(r'^\s*(?:pub\s*(?:\([^)]*\))?\s*)?use\s+([^;]+);', text, re.M):
            spec = m.group(1).strip()
            first = spec.split("::")[0].strip()
            if first in RUST_INTERNAL or first.startswith("{"):
                continue
            tail = spec.rsplit("::", 1)[-1]
            names = re.findall(r"[A-Za-z_]\w*", tail) if tail.startswith("{") else [tail.split(" as ")[-1].strip()]
            for n in names:
                if n and n != "self" and n != "*":
                    out.append((n, "name"))
    return out


def extapi_density(imports, added_text):
    n = 0
    for name, kind in imports:
        if kind == "ns":
            n += len(re.findall(r"\b" + re.escape(name) + r"\.[A-Za-z_]\w*", added_text))
        else:
            n += len(re.findall(r"\b" + re.escape(name) + r"\b\s*[(.<]", added_text))
    return n


class Replayer:
    def __init__(self, repo, base, head, history_repo=None, history_ref=None, ledger=None, before=None,
                 theta=2.0, fanin_k=5, extapi_k=3, ripple=True):
        self.repo, self.base, self.head = repo, base, head
        self.ripple = ripple
        self.history_repo = history_repo or repo
        self.history_ref = history_ref or base
        self.ledger, self.before = ledger, before
        self.theta, self.fanin_k, self.extapi_k = theta, fanin_k, extapi_k
        self._fanin_cache = {}
        self._hist_cache = {}
        self._ledger_cache = None
        gm = show_lines(repo, head, "go.mod")
        self.go_module = None
        if gm:
            for l in gm:
                if l.startswith("module "):
                    self.go_module = l.split()[1].strip()

    # ---- 单文件 → 区间与信号 ----
    def fanin(self, name, own_file):
        if name in self._fanin_cache:
            return self._fanin_cache[name]
        out = git(self.repo, "grep", "-w", "-c", "-e", name, "--end-of-options", self.head, "--", ".", f":(exclude){own_file}", check=False)
        n = 0
        for l in out.split("\n"):
            # 形如 <rev>:<path>:<count>
            parts = l.rsplit(":", 2)
            if len(parts) == 3 and lang_for(parts[1]) and not TEST_PATH.search(parts[1]):
                try:
                    n += int(parts[2])
                except ValueError:
                    pass
        self._fanin_cache[name] = n
        return n

    def hist_fix_count(self, path):
        if path in self._hist_cache:
            return self._hist_cache[path]
        out = git(self.history_repo, "log", "--since=12.months.ago", "--format=%s", "--end-of-options", self.history_ref, "--", path, check=False)
        n = sum(1 for s in out.split("\n") if s and FIX_COMMIT.search(s))
        self._hist_cache[path] = n
        return n

    def ledger_signals(self, path):
        if not self.ledger:
            return []
        if self._ledger_cache is None:
            self._ledger_cache = {"upheld": defaultdict(list), "dismissed": defaultdict(list)}
            con = sqlite3.connect("file:" + urllib.parse.quote(str(self.ledger)) + "?mode=ro", uri=True)
            for f, upd in con.execute("select file, updated_at from dismissed"):
                # dismissed.updated_at 只有日期：按『严格早于作业当天』计，宁漏勿泄
                if not self.before or upd[:10] < self.before[:10]:
                    self._ledger_cache["dismissed"][f].append(upd)
            for js, created in con.execute("select json, created_at from results"):
                if self.before and created >= self.before:
                    continue
                for fd in json.loads(js).get("findings", []):
                    if (fd.get("verify") or {}).get("verdict") == "upheld":
                        self._ledger_cache["upheld"][fd["file"]].append(created)
        sig = []
        if self._ledger_cache["upheld"].get(path):
            sig.append(("hist", f"ledger upheld×{len(self._ledger_cache['upheld'][path])}", W["ledger_upheld"]))
        if self._ledger_cache["dismissed"].get(path):
            sig.append(("hist", f"ledger dismissed×{len(self._ledger_cache['dismissed'][path])}", W["ledger_dismissed"]))
        return sig

    def zones_for_file(self, path, info, is_new_file):
        lang = lang_for(path)
        lines = show_lines(self.repo, self.head, path)
        if lines is None:
            return []
        # 1) hunk → 每个 enclosing def 一个原始区间（R2.1：同一 hunk 跨多个 def 时按 def 拆，不取并集）；
        #    无 def 的行 → hunk ± NO_DEF_PAD 的 padded 区间。added 行按行号归属区间；removed 行归属该 hunk 首个区间。
        raw = []
        for h in info["hunks"]:
            s = h["new_start"]
            e = s + max(h["new_count"], 1) - 1
            groups, nodef = {}, []
            for ln in range(s, e + 1):
                d = enclosing_def_start(lines, ln - 1, lang) if lang else None
                if d is None:
                    nodef.append(ln)
                elif d not in groups:
                    groups[d] = ((d + 1, decl_block_end(lines, d, lang) + 1), match_decl(lines[d], lang)[1])
            hz = []
            for d in sorted(groups):
                rng, sym = groups[d]
                hz.append({"range": rng, "symbols": {sym}, "added": [], "removed": [], "padded": False})
            nodef = [ln for ln in nodef if ln - 1 < len(lines) and SUBSTANTIVE.match(lines[ln - 1])]
            if nodef:
                rng = (max(1, min(nodef) - NO_DEF_PAD), min(len(lines), max(nodef) + NO_DEF_PAD))
                hz.append({"range": rng, "symbols": set(), "added": [], "removed": [], "padded": True})
            for ln, t in h["added"]:
                tgt = next((z for z in hz if z["range"][0] <= ln <= z["range"][1]), hz[0] if hz else None)
                if tgt is not None:
                    tgt["added"].append(t)
            if hz:
                hz[0]["removed"].extend(h["removed"])
            raw += hz
        # 2) 同文件合并：def 区间只按重叠合并（相邻函数各自成区，保持区间级口径）；padded 区间按相邻合并
        raw.sort(key=lambda z: z["range"])
        merged = []
        for z in raw:
            gap = ADJ_GAP if (merged and merged[-1]["padded"] and z["padded"]) else 0
            same_kind = merged and merged[-1]["padded"] == z["padded"]
            if same_kind and z["range"][0] <= merged[-1]["range"][1] + gap:
                m = merged[-1]
                m["range"] = (m["range"][0], max(m["range"][1], z["range"][1]))
                m["symbols"] |= z["symbols"]
                m["added"] += z["added"]
                m["removed"] += z["removed"]
                m["padded"] = m["padded"] and z["padded"]
            else:
                merged.append(z)
        # 3) 文件级信号
        file_sigs = []
        if PATH_SENSITIVE.search(path):
            file_sigs.append(("path", "sensitive path", W["path_glob"]))
        fx = self.hist_fix_count(path)
        if fx >= 6:
            file_sigs.append(("hist", f"fix commits×{fx}", W["hist_fix6"]))
        elif fx >= 3:
            file_sigs.append(("hist", f"fix commits×{fx}", W["hist_fix3"]))
        file_sigs += self.ledger_signals(path)
        if is_new_file:
            file_sigs.append(("struct", "new file", W["new_file"]))
        imports = parse_imports(path, lines, self.go_module) if lang else []
        is_test, is_doc = bool(TEST_PATH.search(path)), bool(DOC_PATH.search(path))
        cfg_test_line = next((i + 1 for i, l in enumerate(lines) if l.strip().startswith("#[cfg(test)]")), None) if lang == "rs" else None

        def finalize(z, sigs, text_for_kw):
            kw = [k for k, rx in KEYWORD_CLASSES.items() if rx.search(text_for_kw)]
            if kw:
                sigs.append(("path", "keywords:" + ",".join(kw), min(W["keyword"] * len(kw), W["keyword_cap"])))
            if imports and any(k == "path" for k, _, _ in sigs) and z["added"]:
                dens = extapi_density(imports, "\n".join(z["added"]))
                if dens >= self.extapi_k:
                    sigs.append(("extapi", f"external calls×{dens}", W["extapi"]))
            body = "\n".join(lines[z["range"][0] - 1:z["range"][1]])
            decl_only = not CONTROL_FLOW.search(strip_comments(body))
            in_test = is_test or (cfg_test_line is not None and z["range"][0] >= cfg_test_line)
            mult = (W["decl_only_mult"] if decl_only else 1.0) * (W["test_mult"] if in_test else (W["doc_mult"] if is_doc else 1.0))
            score = sum(w for _, _, w in sigs) * mult
            mults = ([{"kind": "mult", "detail": "declaration-only", "w": W["decl_only_mult"]}] if decl_only else []) + \
                    ([{"kind": "mult", "detail": "test code", "w": W["test_mult"]}] if in_test else [])
            return {"file": path, "ranges": [list(z["range"])], "symbols": sorted(z["symbols"]),
                    "signals": [{"kind": k, "detail": d, "w": w} for k, d, w in sigs] + mults,
                    "score": round(score, 2), "lines": z["range"][1] - z["range"][0] + 1, "ripple": z.get("ripple", False),
                    "changed_lines": len(z["added"]) + len(z["removed"]), "mult": mult}

        out = []
        changed_names = set()
        for z in merged:
            sigs = list(file_sigs)
            added, removed = z["added"], z["removed"]
            if lang:
                rdefs = {match_decl(t, lang)[1]: t for t in removed if match_decl(t, lang)}
                adefs = {match_decl(t, lang)[1]: t for t in added if match_decl(t, lang)}
                if rdefs:
                    deleted = [n for n in rdefs if n not in adefs]
                    if deleted:
                        sigs.append(("struct", "def deleted:" + ",".join(sorted(deleted)[:4]), W["def_deleted"]))
                    else:
                        sigs.append(("struct", "signature changed:" + ",".join(sorted(rdefs)[:4]), W["sig_changed"]))
                elif adefs:
                    sigs.append(("struct", "def new:" + ",".join(sorted(adefs)[:4]), W["def_new"]))
                widened = [n for n, t in adefs.items() if n not in rdefs and (
                    re.match(r"^\s*(pub\b|export\b)", t) or (lang == "go" and n[:1].isupper()))]
                if widened:
                    sigs.append(("struct", "public widened:" + ",".join(sorted(widened)[:4]), W["public_widen"]))
                names = {n for n in (z["symbols"] | set(adefs) | set(rdefs)) if fanin_worthy(n)}
                changed_names |= names
                hi = [(n, self.fanin(n, path)) for n in sorted(names)]
                hi = [(n, c) for n, c in hi if c >= self.fanin_k]
                if hi:
                    sigs.append(("struct", "fan-in:" + ",".join(f"{n}={c}" for n, c in hi[:4]), W["fanin"]))
            out.append(finalize(z, sigs, "\n".join(added)))
        # 4) 涟漪卫星区（R2.4，design §3.3「高 fan-in 符号被改 → 涟漪」的同文件形态）：改动符号在本文件的引用点
        #    → 其 enclosing def 成区（不与主区重叠）；关键词类看区体而非 added 行。
        if self.ripple and lang and changed_names:
            taken = [tuple(z["ranges"][0]) for z in out]
            sat = {}
            for name in sorted(changed_names):
                rx = re.compile(r"\b" + re.escape(name) + r"\b")
                sites = [i for i, l in enumerate(lines) if rx.search(l) and not any(a <= i + 1 <= b for a, b in taken)]
                if not sites or len(sites) > RIPPLE_MAX_SITES:
                    continue  # 无引用点，或弥散（类型名 / 全局配置被全文件引用）→ 不成卫星区，交 fan-in
                for i in sites:
                    ln = i + 1
                    d = enclosing_def_start(lines, i, lang)
                    if d is None:
                        continue
                    rng = (d + 1, decl_block_end(lines, d, lang) + 1)
                    if any(overlap(rng, t) for t in taken):
                        continue
                    sat.setdefault(rng, {"range": rng, "symbols": {match_decl(lines[d], lang)[1]}, "added": [], "removed": [],
                                         "padded": False, "ripple": True, "refs": set()})["refs"].add(name)
            for rng, z in sorted(sat.items()):
                sigs = list(file_sigs) + [("struct", "ripple:" + ",".join(sorted(z["refs"])[:4]), W["ripple"])]
                body = "\n".join(lines[rng[0] - 1:rng[1]])
                out.append(finalize(z, sigs, body))
        return out

    def run(self):
        if self.ledger and not self.before:
            print("warning: --ledger without --before — ledger rows created during/after the replayed job leak the oracle into signals", file=sys.stderr)
        text = git(self.repo, "diff", "--no-color", "--no-ext-diff", "-M", "--end-of-options", f"{self.base}...{self.head}")
        files = parse_diff(text)
        code = {p: i for p, i in files.items() if not DOC_PATH.search(p) and not i["binary"]}
        churns = sorted(i["adds"] + i["dels"] for i in code.values())
        q3 = churns[int(len(churns) * 0.75)] if churns else 0
        base_tree = set(git(self.repo, "ls-tree", "-r", "--name-only", self.base).split("\n"))
        zones = []
        for p, info in files.items():
            if info["binary"]:
                continue
            zones += self.zones_for_file(p, info, p not in base_tree)
        # v4.b 区间级 churn：区间改动行数位于全 diff 区间 Q3 及以上（≥ 4 区时）→ +0.5（原文件级 churn 给整文件每区加分，过钝）
        churns = sorted(z["changed_lines"] for z in zones)
        if len(churns) >= 4:
            zq3 = churns[int(len(churns) * 0.75)]
            for z in zones:
                if z["changed_lines"] >= zq3 and z["changed_lines"] > 0:
                    z["signals"].append({"kind": "struct", "detail": f"churn Q3 ({z['changed_lines']} lines)", "w": W["churn"]})
                    z["score"] = round(z["score"] + W["churn"] * z["mult"], 2)
        code_lines = sum(i["adds"] + i["dels"] for i in code.values())
        h0 = [z for z in zones if qualifies(z, self.theta)]
        R = round(sum(z["score"] for z in h0) / (1 + math.log(1 + code_lines)), 3) if code_lines else 0.0
        return {
            "repo": os.path.abspath(self.repo), "base": self.base, "head": self.head,
            "size": {"files": len(files), "code_files": len(code), "adds": sum(i["adds"] for i in files.values()),
                     "dels": sum(i["dels"] for i in files.values()), "hunks": sum(len(i["hunks"]) for i in files.values()),
                     "code_lines": code_lines},
            "params": {"theta": self.theta, "fanin_k": self.fanin_k, "extapi_k": self.extapi_k, "ripple": self.ripple,
                       "history_repo": os.path.abspath(self.history_repo), "history_ref": self.history_ref,
                       "ledger": self.ledger, "before": self.before, "weights": W},
            "zones": zones, "h0": h0, "theta": self.theta, "R": R,
            "max_zone": max((z["score"] for z in zones), default=0.0),
        }


# ---------------- score ----------------
def overlap(a, b):
    return a[0] <= b[1] and b[0] <= a[1]


MIN_KINDS = 2


def qualifies(z, theta):
    """v4：入 H0 需 score ≥ θ 且至少 MIN_KINDS 类独立信号（path / struct / extapi / hist），关键词类单独不成热区。"""
    kinds = {s["kind"] for s in z["signals"] if s["kind"] != "mult"}
    return z["score"] >= theta and len(kinds) >= MIN_KINDS


def h0_at(replay, theta):
    return [z for z in replay["zones"] if qualifies(z, theta)]


def score_set(replay, oracle, set_name, theta=None):
    theta = replay["theta"] if theta is None else theta
    h0 = h0_at(replay, theta)
    ids = set(oracle["sets"][set_name])
    entries = [e for e in oracle["entries"] if e["id"] in ids and e.get("anchored") is not False]
    dropped = [e["id"] for e in oracle["entries"] if e["id"] in ids and e.get("anchored") is False]
    hits_r, hits_f, misses, in_any = 0, 0, [], 0
    for e in entries:
        in_any += any(a["file"] == z["file"] and any(overlap((a["line_start"], a["line_end"]), tuple(r)) for r in z["ranges"])
                      for a in e["anchors"] for z in replay["zones"])
        r_hit = any(a["file"] == z["file"] and any(overlap((a["line_start"], a["line_end"]), tuple(r)) for r in z["ranges"])
                    for a in e["anchors"] for z in h0)
        f_hit = any(a["file"] == z["file"] for a in e["anchors"] for z in h0)
        hits_r += r_hit
        hits_f += f_hit
        if not r_hit:
            misses.append({"id": e["id"], "file_hit": f_hit, "anchors": e["anchors"], "topic": e.get("topic")})
    empty_r = [z for z in h0 if not any(a["file"] == z["file"] and any(overlap((a["line_start"], a["line_end"]), tuple(r)) for r in z["ranges"])
                                        for e in entries for a in e["anchors"])]
    files_with = {a["file"] for e in entries for a in e["anchors"]}
    empty_f = [z for z in h0 if z["file"] not in files_with]
    n = len(entries)
    return {
        "set": set_name, "n": n, "theta": theta, "h0_count": len(h0), "in_any_zone": in_any, "dropped_unanchored": dropped,
        "hit_range": hits_r, "hit_range_rate": round(hits_r / n, 3) if n else None,
        "hit_file": hits_f, "hit_file_rate": round(hits_f / n, 3) if n else None,
        "empty_zones_range": len(empty_r), "empty_zones_range_rate": round(len(empty_r) / len(h0), 3) if h0 else None,
        "empty_zones_file": len(empty_f), "empty_zones_file_rate": round(len(empty_f) / len(h0), 3) if h0 else None,
        "misses": misses,
        "empty_zone_list": [{"file": z["file"], "ranges": z["ranges"], "score": z["score"]} for z in empty_r],
    }


def cmd_replay(a):
    rp = Replayer(a.repo, a.base, a.head, a.history_repo, a.history_ref, a.ledger, a.before, a.theta, a.fanin_k, a.extapi_k, not a.no_ripple)
    out = rp.run()
    js = json.dumps(out, ensure_ascii=False, indent=1)
    if a.out:
        open(a.out, "w", encoding="utf-8").write(js)
    s = out["size"]
    print(f"replay {a.base}..{a.head[:9]} files={s['files']} code_lines={s['code_lines']} zones={len(out['zones'])} "
          f"H0(θ={a.theta})={len(out['h0'])} R={out['R']} max_zone={out['max_zone']}")
    for z in sorted(out["h0"], key=lambda z: -z["score"]):
        print(f"  {z['score']:5.2f} {'~' if z.get('ripple') else ' '}{z['file']}:{z['ranges'][0][0]}-{z['ranges'][0][1]} "
              f"[{'; '.join(s['detail'] for s in z['signals'])}]")


def cmd_score(a):
    replay = json.load(open(a.replay, encoding="utf-8"))
    oracle = json.load(open(a.oracle, encoding="utf-8"))
    sets = a.set or list(oracle["sets"].keys())
    res = [score_set(replay, oracle, s, a.theta) for s in sets]
    if a.out:
        json.dump(res, open(a.out, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    for r in res:
        print(f"{r['set']:14} n={r['n']:2} θ={r['theta']} |H0|={r['h0_count']:3} reachable={r['in_any_zone']}/{r['n']} hit_range={r['hit_range']}/{r['n']} ({r['hit_range_rate']}) "
              f"hit_file={r['hit_file']}/{r['n']} ({r['hit_file_rate']}) empty_range={r['empty_zones_range']} ({r['empty_zones_range_rate']}) "
              f"empty_file={r['empty_zones_file']} ({r['empty_zones_file_rate']})")
        for m in r["misses"]:
            print(f"    miss {m['id']} file_hit={m['file_hit']} {m['topic'] or ''} {[(x['file'].rsplit('/',1)[-1], x['line_start'], x['line_end']) for x in m['anchors']]}")


# ---------------- compare（Rust `risk` 输出 vs Python v4 回放的区级对拍，M21b 批 A 回归门） ----------------
def zone_range(z):
    r = z.get("ranges")
    if r:
        return (r[0][0], r[0][1])
    return (z.get("start") or 1, z.get("end") or 1)


def cmd_compare(a):
    """Rust `redteam risk` JSON vs Python v4 replay JSON：报 H0 区级 Jaccard 与 R 差；
    R 差 > 容差（默认 1e-6）退出码 1——改 θ / 权重必须过此门（D11）。"""
    py = json.load(open(a.replay, encoding="utf-8"))
    rs = json.load(open(a.rust, encoding="utf-8"))
    py_items = [(z["file"], zone_range(z)) for z in py.get("h0", [])]
    ru_items = [(z["file"], zone_range(z)) for z in rs.get("hotspots", [])]
    used, pairs = set(), 0
    ru_only = []
    for f2, r2 in ru_items:
        hit = False
        for j, (f1, r1) in enumerate(py_items):
            if j in used or f1 != f2:
                continue
            if overlap(r1, r2):
                used.add(j)
                pairs += 1
                hit = True
                break
        if not hit:
            ru_only.append((f2, r2))
    py_only = [(f, r) for j, (f, r) in enumerate(py_items) if j not in used]
    union = len(py_items) + len(ru_items) - pairs
    jaccard = pairs / union if union else 1.0
    theta_py, theta_ru = py.get("theta"), rs.get("theta")
    theta_ok = theta_py is not None and theta_ru is not None and abs(theta_py - theta_ru) <= 1e-9
    r_py, r_ru = py.get("R"), rs.get("r")
    delta = abs((r_py or 0.0) - (r_ru or 0.0))
    r_ok = delta <= a.tol
    jac_ok = jaccard >= a.min_jaccard - 1e-9
    ok = theta_ok and r_ok and jac_ok
    result = {"python_theta": theta_py, "rust_theta": theta_ru, "theta_ok": theta_ok,
              "python_R": r_py, "rust_r": r_ru, "r_delta": delta, "tol": a.tol, "r_ok": r_ok,
              "python_h0": len(py_items), "rust_hotspots": len(ru_items), "matched": pairs,
              "jaccard": round(jaccard, 4), "min_jaccard": a.min_jaccard, "jaccard_ok": jac_ok,
              "rust_only": [{"file": f, "range": list(r)} for f, r in ru_only],
              "python_only": [{"file": f, "range": list(r)} for f, r in py_only]}
    if a.out:
        json.dump(result, open(a.out, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    print(f"θ: python={theta_py} rust={theta_ru}: {'OK' if theta_ok else 'FAIL'}"
          + ("" if theta_ok else "（两侧 θ 不一致——replay 须与 Rust THETA 同值生成）"))
    print(f"R: python={r_py} rust={r_ru} Δ={delta:.6g} (tol {a.tol:g}): {'OK' if r_ok else 'FAIL'}")
    print(f"H0: python={len(py_items)} rust={len(ru_items)} matched={pairs} jaccard={jaccard:.3f} "
          f"(min {a.min_jaccard:g}): {'OK' if jac_ok else 'FAIL'}")
    for f, r in ru_only[:a.show]:
        print(f"  rust-only   {f}:{r[0]}-{r[1]}")
    for f, r in py_only[:a.show]:
        print(f"  python-only {f}:{r[0]}-{r[1]}")
    sys.exit(0 if ok else 1)


def cmd_report(a):
    thetas = [float(t) for t in a.thetas.split(",")]
    rows = []
    for pair in a.pair or []:
        parts = pair.split(":")
        rp, orc, st = parts[0], parts[1], parts[2]
        replay = json.load(open(rp, encoding="utf-8"))
        oracle = json.load(open(orc, encoding="utf-8"))
        line = f"{os.path.basename(rp)}:{st}"
        cells = []
        for t in thetas:
            r = score_set(replay, oracle, st, t)
            cells.append(f"θ{t}: hit {r['hit_range']}/{r['n']} empty {r['empty_zones_range']}/{r['h0_count']}")
        print(f"{line:48} " + " | ".join(cells))
        rows.append((replay, line))
    print("\n(规模, |H0|, R) 表（θ = 各 replay 自带）：")
    dist = []
    for rp in list(a.dist or []) + [p.split(":")[0] for p in (a.pair or [])]:
        replay = json.load(open(rp, encoding="utf-8"))
        s = replay["size"]
        dist.append((os.path.basename(rp), s["code_lines"], s["code_files"], len(replay["h0"]), replay["R"], replay["max_zone"]))
    dist.sort(key=lambda x: x[4])
    print(f"{'replay':40} {'code_lines':>10} {'files':>5} {'|H0|':>5} {'R':>7} {'max':>5}")
    for name, cl, cf, h, R, mx in dist:
        print(f"{name:40} {cl:10} {cf:5} {h:5} {R:7.3f} {mx:5.2f}")
    dist = [d for d in dist if d[1] > 0]  # 分档只看有代码行的 diff（docs-only 提交 R=0 会把三分位挤成零）
    if len(dist) >= 3:
        Rs = [d[4] for d in dist]
        lo, hi = Rs[len(Rs) // 3], Rs[(2 * len(Rs)) // 3]
        tiers = {"low": [d[3] for d in dist if d[4] < lo], "mid": [d[3] for d in dist if lo <= d[4] < hi], "high": [d[3] for d in dist if d[4] >= hi]}
        print(f"\nR 三分位切点 low<{lo:.3f}≤mid<{hi:.3f}≤high；各档 |H0| 中位数（max_cells 起步值）：")
        for k, v in tiers.items():
            v = sorted(v)
            med = v[len(v) // 2] if v else None
            print(f"  {k:4} n={len(v)} |H0| median={med} range={v[0] if v else None}-{v[-1] if v else None}")


# ---------------- selftest ----------------
def selftest():
    # def 正则三语言
    assert match_def("pub async fn do_read_file(x: u8) {", "rs") == ("fn", "do_read_file")
    assert match_def("func (s *Store) Claim(ctx context.Context) error {", "go") == ("func", "Claim")
    assert match_def("export async function fetchSource(url: string) {", "ts") == ("function", "fetchSource")
    assert match_def("  private async pipe(body: ReadableStream): Promise<void> {", "ts") == ("method", "pipe")
    assert match_def("  if (x) {", "ts") is None
    # 块配平 + 最内层外围
    src = "fn a() {\n    if x {\n        y();\n    }\n}\nfn b() {}".split("\n")
    assert block_end(src, 0) == 4 and block_end(src, 5) == 5
    src2 = "impl Foo {\n    fn bar(&self) {\n        let z = 1;\n    }\n}".split("\n")
    assert enclosing_def_start(src2, 2, "rs") == 1
    # diff 解析
    d = "diff --git a/x.go b/x.go\n--- a/x.go\n+++ b/x.go\n@@ -1,3 +1,4 @@\n a\n-b\n+c\n+d\n e\n"
    f = parse_diff(d)
    assert f["x.go"]["hunks"][0]["added"] == [(2, "c"), (3, "d")] and f["x.go"]["adds"] == 2 and f["x.go"]["dels"] == 1
    d2 = "diff --git a/g.go b/g.go\ndeleted file mode 100644\n--- a/g.go\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-x\n-y\n"
    assert parse_diff(d2) == {}
    # 状态机守卫（2026-09-10 M21b 前置修）：hunk 内列首 `-- ` / `++ ` 内容行不再误判文件头
    d3 = "diff --git a/s.sql b/s.sql\n--- a/s.sql\n+++ b/s.sql\n@@ -1,3 +1,3 @@\n a\n--- old comment\n+++ new comment\n"
    f3 = parse_diff(d3)
    assert list(f3) == ["s.sql"], f3
    assert f3["s.sql"]["dels"] == 1 and f3["s.sql"]["adds"] == 1, f3
    assert f3["s.sql"]["hunks"][0]["removed"] == ["-- old comment"], f3
    assert f3["s.sql"]["hunks"][0]["added"] == [(2, "++ new comment")], f3
    assert match_decl("    const resp = await fetch(u);", "ts") is None and match_decl("export const X = 1;", "ts")
    go3 = "func a() {\n\tx := 1\n}\n\nfunc b() {\n\ty := 2\n}".split("\n")
    assert enclosing_def_start(go3, 1, "go") == 0 and enclosing_def_start(go3, 5, "go") == 4
    go4 = "const x = 1\n\nfunc a() {\n\ty := 2\n}".split("\n")
    assert decl_block_end(go4, 0, "go") == 0 and decl_block_end(go4, 2, "go") == 4
    assert fanin_worthy("handleRequest") and fanin_worthy("wrap_body") and not fanin_worthy("cancel") and not fanin_worthy("main")
    assert "numeric_bounds" in [k for k, rx in KEYWORD_CLASSES.items() if rx.search("v = min(maximum, candidate)")]
    assert not CONTROL_FLOW.search("const A = 1;\nexport const B = 2;") and CONTROL_FLOW.search("if (x) return 1;")
    assert not CONTROL_FLOW.search(strip_comments("const A = 1; // return early if x\n/* for */"))
    assert SUBSTANTIVE.match("import x") and not SUBSTANTIVE.match("   ") and not SUBSTANTIVE.match("// c")
    # 交集
    assert overlap((10, 20), (20, 30)) and not overlap((10, 20), (21, 30))
    # 关键词类
    assert "condwrite" in [k for k, rx in KEYWORD_CLASSES.items() if rx.search('headers["If-None-Match"] = etag')]
    # import 解析
    go_lines = ['package x', 'import (', '\t"context"', '\t"github.com/aws/aws-sdk-go/s3"', '\tmy "github.com/org/backend/svc"', ')']
    assert parse_imports("a.go", go_lines, "github.com/org/backend") == [("s3", "ns")]
    ts_lines = ["import { R2Bucket, type Env } from '@cloudflare/workers-types'", "import x from './local'", "import * as z from 'zod'"]
    assert parse_imports("a.ts", ts_lines) == [("R2Bucket", "name"), ("z", "ns")]
    assert extapi_density([("s3", "ns"), ("R2Bucket", "name")], "s3.Put(a)\ns3.Get(b)\nR2Bucket(c)") == 3
    # score：合成 replay/oracle
    two = [{"kind": "path", "w": 1.0}, {"kind": "struct", "w": 1.0}]
    replay = {"theta": 2.0, "zones": [{"file": "a.go", "ranges": [[10, 20]], "score": 2.5, "signals": two}, {"file": "b.ts", "ranges": [[1, 5]], "score": 1.0, "signals": two},
                                      {"file": "c.go", "ranges": [[100, 120]], "score": 3.0, "signals": two},
                                      {"file": "d.go", "ranges": [[1, 9]], "score": 9.0, "signals": [{"kind": "path", "w": 9.0}]}]}
    oracle = {"entries": [{"id": "x", "anchors": [{"file": "a.go", "line_start": 15, "line_end": 16}], "topic": None},
                          {"id": "y", "anchors": [{"file": "b.ts", "line_start": 2, "line_end": 2}], "topic": None},
                          {"id": "w", "anchors": [{"file": "c.go", "line_start": 300, "line_end": 300}], "topic": None}],
              "sets": {"s": ["x", "y", "w"]}}
    r = score_set(replay, oracle, "s")
    assert (r["hit_range"], r["hit_file"], r["empty_zones_range"], r["empty_zones_file"]) == (1, 2, 1, 0), r
    assert r["h0_count"] == 2, "keywords-only zone (d.go) must not qualify"
    r15 = score_set(replay, oracle, "s", 0.5)
    assert r15["hit_range"] == 2
    print("selftest OK")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("replay")
    p.add_argument("repo"); p.add_argument("base"); p.add_argument("head")
    p.add_argument("--history-repo"); p.add_argument("--history-ref"); p.add_argument("--ledger"); p.add_argument("--before")
    p.add_argument("--theta", type=float, default=2.0); p.add_argument("--fanin-k", type=int, default=5)
    p.add_argument("--extapi-k", type=int, default=3); p.add_argument("--out")
    p.add_argument("--no-ripple", action="store_true", help="关闭同文件涟漪卫星区（对照用）")
    p.set_defaults(fn=cmd_replay)
    s = sub.add_parser("score")
    s.add_argument("replay"); s.add_argument("oracle"); s.add_argument("--set", action="append")
    s.add_argument("--theta", type=float); s.add_argument("--out")
    s.set_defaults(fn=cmd_score)
    r = sub.add_parser("report")
    r.add_argument("--pair", action="append"); r.add_argument("--dist", action="append")
    r.add_argument("--thetas", default="1.5,2,2.5,3")
    r.set_defaults(fn=cmd_report)
    c = sub.add_parser("compare")
    c.add_argument("replay", help="Python v4 回放 JSON（replay 子命令产物）")
    c.add_argument("rust", help="Rust `redteam risk` 输出 JSON")
    c.add_argument("--tol", type=float, default=1e-6, help="R 差容差（默认 1e-6）")
    c.add_argument("--show", type=int, default=10, help="最多列出的单边未配对区数（默认 10）")
    c.add_argument("--min-jaccard", type=float, default=1.0, help="区级 Jaccard 下限（默认 1.0：H0 区集须完全对齐）")
    c.add_argument("--out", help="对拍结果 JSON 落盘路径")
    c.set_defaults(fn=cmd_compare)
    t = sub.add_parser("selftest")
    t.set_defaults(fn=lambda a: selftest())
    a = ap.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
