"""oracle 命中口径（零依赖）：`e3_contract_ab.py score` 与 `scan_score.py` 共用——
quote 去全部空白后在 `git show <head>:<path>` 内容里子串匹配 → 行区间；与 oracle 条目的 anchors 区间重叠即命中。
注意这是 **E3 口径，比产品宽**两点：产品 `positioning::locate_quote` 只把空白归一为单空格后按行 / 词序匹配（`foo (bar)` 对 `foo(bar)`
产品丢、这里命中）；产品对定位失败的疑点直接丢弃（D11），评分器按自报行号计命中并计 unanchored。与 P0 基线可比优先于与产品一致。
本模块不含任何被评仓内容；仓路径 / head / oracle 一律由调用方参数传入。"""
import json
import subprocess

_STRIP_CACHE = {}


def show_file(repo, head, path):
    r = subprocess.run(["git", "-C", repo, "show", f"{head}:{path}"], capture_output=True, text=True, errors="replace")
    return r.stdout if r.returncode == 0 else None


def anchor_quote(repo, head, path, quote):
    """E3 口径（见模块说明）：quote 去全部空白后子串匹配；返回 (line_start, line_end) 或 None。"""
    if not quote or not path:
        return None
    k = (repo, head, path)
    if k not in _STRIP_CACHE:
        content = show_file(repo, head, path)
        if content is None:
            _STRIP_CACHE[k] = None
        else:
            chars, lines_of = [], []
            ln = 1
            for ch in content:
                if ch == "\n":
                    ln += 1
                elif not ch.isspace():
                    chars.append(ch)
                    lines_of.append(ln)
            _STRIP_CACHE[k] = ("".join(chars), lines_of)
    if _STRIP_CACHE[k] is None:
        return None
    s, lines_of = _STRIP_CACHE[k]
    q = "".join(ch for ch in quote if not ch.isspace())
    if not q:
        return None
    i = s.find(q)
    if i < 0:
        return None
    return lines_of[i], lines_of[min(i + len(q) - 1, len(lines_of) - 1)]


def overlap(a, b):
    return a[0] <= b[1] and b[0] <= a[1]


def load_oracle(path, set_name="union"):
    """oracle JSON（`p0_oracle.py` 产出：{kind, head, entries[{id, anchors[{file,line_start,line_end}], topic?}], sets{name: [ids]}}）
    → (entries 子集, head)。set 缺失 → 全部条目。"""
    oracle = json.load(open(path, encoding="utf-8"))
    sets = oracle.get("sets") or {}
    if sets and set_name not in sets:
        raise ValueError(f"oracle 无子集 {set_name!r}（可选：{sorted(sets)}）")
    if set_name in sets:
        ids = set(sets[set_name])
        entries = [e for e in oracle["entries"] if e["id"] in ids]
    else:
        entries = list(oracle["entries"])
    return entries, oracle.get("head")


def match_entries(entries, path, rng):
    """区间级命中：同文件且与任一 anchor 重叠的 oracle 条目。"""
    return [e for e in entries if any(x["file"] == path and overlap(rng, (x["line_start"], x["line_end"])) for x in e.get("anchors", []))]
