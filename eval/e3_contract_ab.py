#!/usr/bin/env python3
"""P0 E3 · 疑点契约 vs finding 契约召回（DeepSeek 直连 chat-completions，复刻产品单发请求形状）。

零第三方依赖（urllib）。输入是 A 臂 prompt 落盘产物（system + user；原落盘器 `examples/dump_pack.rs` 已随 M21a 批 C 拆除，
产品等价物 = `redteam review --pack-only` 的 `.redteam/packs/<job>/cell-*.md`）；B 臂只替换
`<finding_bar>` / `<output_contract>` 两段与 system 里的 schema 短语，其余字节相同（`variant` 落盘 diff 并断言）。

子命令：
  variant --pack <A dir> --src <redteam repo> --out <B dir>
  run     --arm A|B --pack <dir> --n 3 [--start 1] [--max-tokens 32768] [--model deepseek-v4-flash]
          [--base-url https://api.deepseek.com] [--parallel 1] --out <runs dir>      需 env DEEPSEEK_API_KEY
  score   --arm A|B --runs <dir> --oracle <oracle.json> --repo <dir> --head <sha> [--set union] --out <summary.json>
          区间级命中 oracle（quote 定位；定位失败回退模型行号并标 unanchored）；产出人工主题确认表骨架 <out>.md

请求形状照 src/provider/single_shot.rs + backends.rs：system + user，首轮无 response_format，stream=true +
stream_options.include_usage，显式 max_tokens；parse 失败追加 REPAIR_PROMPT + response_format=json_object 一次。
SSE 判定按 .trellis/spec/backend/error-handling.md：整体 JSON 优先、行首锚定 `data:`。
"""
import argparse
import concurrent.futures
import difflib
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

REPAIR_PROMPT = ("Your previous response was not valid JSON (it may have been truncated). "
                 "Re-emit ONLY the complete findings JSON object matching the schema. If space is tight, shorten each "
                 "finding's body/recommendation text, but do NOT drop any finding and do NOT omit any required field.")

FINDING_BAR_B = """<finding_bar>
List every suspicion you have. Threshold: SUSPECTED IS ENOUGH — you do not need to prove it, argue it, or worry about false positives; the deep-dive and verification stages downstream will do that. Do NOT filter for "material" and do NOT hold back weak leads. Each suspicion is a short pointer (one sentence hint, under 50 tokens): where, and what smells wrong. Report ALL suspicions in THIS pass — never hold any back for a later round. Also report which areas you actually scanned, so coverage can be reconciled.
</finding_bar>"""

OUTPUT_CONTRACT_B = """<output_contract>
Return ONLY JSON matching this schema: an object with a "suspicions" array, an optional "needs" array (max 5 entries) and a "scanned_areas" array.
Each suspicion has: file (path relative to the review root), line_start>=1, line_end>=1, hint (one sentence, under 50 tokens: what looks wrong and why it would matter), category (one of L0|L1|L2|L3|L4|L5, the rule families listed above), confidence 0..1 (how likely this is a real defect), quote.
- quote: the EXACT verbatim source text of the suspicious lines, copied character-for-character from the file. This is used to locate the suspicion; a quote that does not appear in the file will be DISCARDED.
- A suspicion is a pointer, not a finding: no argument, no impact analysis, no fix. Keep each one under 50 tokens.
If nothing looks suspicious, return {"suspicions": [], "scanned_areas": [...]}.
Every suspicion must be grounded in code you actually read — do not invent files, lines, or behavior.

Optionally include a top-level "needs" array (max 5 entries) when a suspicion can only be resolved with evidence NOT present in the provided context: each entry is
{"path": "<repo-relative file>", "lines": "<start-end, optional>", "reason": "<what you must verify>", "kind": "evidence" | "api"}.
Declare needs ONLY for genuinely decisive evidence. Do NOT declare needs for files already shown in full above.

"scanned_areas": [{"file": "<path>", "ranges": ["<start-end>", ...]}] — every region you actually examined, changed or not.
</output_contract>"""

SECTION_RE = {k: re.compile(rf"<{k}>.*?</{k}>", re.S) for k in ("finding_bar", "output_contract")}
SYSTEM_PHRASE_A = 'an object with a "findings" array'
SYSTEM_PHRASE_B = 'an object with a "suspicions" array'


# ---------------- variant ----------------
def rust_string_const(src_path, name):
    """从 Rust 源提取 `const NAME: &str = "…";` 的实际值（处理 `\\`+换行续行与 `\\"` 转义）。"""
    text = open(src_path, encoding="utf-8").read()
    m = re.search(rf"const {name}: &str =\s*\n?\s*\"(.*?)\";", text, re.S)
    if not m:
        raise SystemExit(f"{name} not found in {src_path}")
    raw = m.group(1)
    raw = re.sub(r"\\\n\s*", "", raw)  # 续行：反斜杠+换行+后续缩进 → 空
    return raw.replace('\\"', '"')


def cmd_variant(a):
    user_a = open(os.path.join(a.pack, "prompt.user.md"), encoding="utf-8").read()
    sys_a = open(os.path.join(a.pack, "prompt.system.txt"), encoding="utf-8").read()
    src_sys = rust_string_const(os.path.join(a.src, "src/provider/single_shot.rs"), "NO_TOOLS_SYSTEM")
    assert sys_a == src_sys, "dumped system prompt differs from src/provider/single_shot.rs NO_TOOLS_SYSTEM"
    src_repair = rust_string_const(os.path.join(a.src, "src/provider/parse.rs"), "REPAIR_PROMPT")
    assert REPAIR_PROMPT == src_repair, "REPAIR_PROMPT drifted from src/provider/parse.rs"
    for k in SECTION_RE:
        assert len(SECTION_RE[k].findall(user_a)) == 1, f"expected exactly one <{k}> section"
    user_b = SECTION_RE["finding_bar"].sub(lambda _: FINDING_BAR_B, user_a)
    user_b = SECTION_RE["output_contract"].sub(lambda _: OUTPUT_CONTRACT_B, user_b)
    assert SYSTEM_PHRASE_A in sys_a
    sys_b = sys_a.replace(SYSTEM_PHRASE_A, SYSTEM_PHRASE_B)
    # 差异断言：两段置空后逐字相同
    mask = lambda t: SECTION_RE["output_contract"].sub("<X/>", SECTION_RE["finding_bar"].sub("<X/>", t))
    assert mask(user_a) == mask(user_b), "A/B differ outside the two contract sections"
    os.makedirs(a.out, exist_ok=True)
    open(os.path.join(a.out, "prompt.user.md"), "w", encoding="utf-8").write(user_b)
    open(os.path.join(a.out, "prompt.system.txt"), "w", encoding="utf-8").write(sys_b)
    diff = "".join(difflib.unified_diff(user_a.splitlines(True), user_b.splitlines(True), "A/prompt.user.md", "B/prompt.user.md"))
    diff += "".join(difflib.unified_diff([sys_a + "\n"], [sys_b + "\n"], "A/prompt.system.txt", "B/prompt.system.txt"))
    open(os.path.join(a.out, "variant-diff.txt"), "w", encoding="utf-8").write(diff)
    print(f"variant B → {a.out}: user {len(user_a)}→{len(user_b)} bytes, system {len(sys_a)}→{len(sys_b)}; "
          f"diff hunks={diff.count('@@ ') // 2}; system==src ✓; masked-equal ✓")


# ---------------- run ----------------
def sse_post(base_url, key, body, timeout=1800):
    """流式 POST；返回 {content, reasoning, finish_reason, usage, http_status, error, raw_events}。"""
    req = urllib.request.Request(base_url.rstrip("/") + "/chat/completions", data=json.dumps(body).encode(),
                                 headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json",
                                          "Accept": "text/event-stream"}, method="POST")
    out = {"content": "", "reasoning": "", "finish_reason": None, "usage": None, "http_status": None, "error": None, "events": 0}
    try:
        resp = urllib.request.urlopen(req, timeout=timeout)
    except urllib.error.HTTPError as e:
        out["http_status"] = e.code
        out["error"] = e.read().decode(errors="replace")[:2000]
        return out
    out["http_status"] = resp.status
    lines = []
    for raw in resp:
        line = raw.decode("utf-8", errors="replace").rstrip("\r\n")
        lines.append(line)
        if not line.startswith("data:"):
            continue
        payload = line[5:].strip()
        if payload == "[DONE]":
            break
        try:
            ev = json.loads(payload)
        except json.JSONDecodeError:
            continue
        out["events"] += 1
        if "error" in ev and not ev.get("choices"):
            out["error"] = json.dumps(ev["error"])[:2000]
            continue
        for ch in ev.get("choices") or []:
            d = ch.get("delta") or {}
            out["content"] += d.get("content") or ""
            out["reasoning"] += d.get("reasoning_content") or ""
            if ch.get("finish_reason"):
                out["finish_reason"] = ch["finish_reason"]
        if ev.get("usage"):
            out["usage"] = ev["usage"]
    if out["events"] == 0:
        # 非 SSE：整体 JSON（端点无视 stream / 200-内-error 体）
        whole = "\n".join(lines).strip()
        try:
            j = json.loads(whole)
            if "error" in j:
                out["error"] = json.dumps(j["error"])[:2000]
            else:
                ch = (j.get("choices") or [{}])[0]
                out["content"] = (ch.get("message") or {}).get("content") or ""
                out["finish_reason"] = ch.get("finish_reason")
                out["usage"] = j.get("usage")
        except json.JSONDecodeError:
            out["error"] = f"non-SSE non-JSON body: {whole[:500]}"
    return out


def parse_output(raw, key):
    """照 src/provider/parse.rs：切掉 </think> 之前、剥围栏、整体 JSON → 首个 '{' 起字符串感知配平扫描。"""
    i = raw.rfind("</think>")
    if i >= 0:
        raw = raw[i + len("</think>"):]
    t = raw.strip()
    for pre in ("```json", "```"):
        if t.startswith(pre):
            t = t[len(pre):]
    if t.endswith("```"):
        t = t[:-3]
    t = t.strip()
    ok = lambda v: isinstance(v, dict) and isinstance(v.get(key), list)
    try:
        v = json.loads(t)
        if ok(v):
            return v
    except json.JSONDecodeError:
        pass
    start = t.find("{")
    if start < 0:
        return None
    depth, in_str, esc = 0, False, False
    for j, c in enumerate(t[start:]):
        if in_str:
            if esc:
                esc = False
            elif c == "\\":
                esc = True
            elif c == '"':
                in_str = False
            continue
        if c == '"':
            in_str = True
        elif c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                try:
                    v = json.loads(t[start:start + j + 1])
                    return v if ok(v) else None
                except json.JSONDecodeError:
                    return None
    return None


def one_run(a, idx, key, system, user):
    items_key = "findings" if a.arm == "A" else "suspicions"
    messages = [{"role": "system", "content": system}, {"role": "user", "content": user}]
    body = {"model": a.model, "messages": messages, "stream": True, "stream_options": {"include_usage": True},
            "max_tokens": a.max_tokens}
    t0 = time.time()
    r1 = sse_post(a.base_url, key, body)
    rec = {"arm": a.arm, "run": idx, "model": a.model, "max_tokens": a.max_tokens, "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(t0)),
           "turn1": {k: r1[k] for k in ("finish_reason", "usage", "http_status", "error", "events")},
           "content": r1["content"], "reasoning_chars": len(r1["reasoning"]), "parse_ok": False, "repaired": False, "repair": None}
    parsed = parse_output(r1["content"], items_key) if not r1["error"] else None
    if parsed is None and not r1["error"]:
        rep_prompt = REPAIR_PROMPT if a.arm == "A" else REPAIR_PROMPT.replace("findings", "suspicions").replace("finding's body/recommendation", "suspicion's hint")
        body2 = dict(body, messages=messages + [{"role": "assistant", "content": r1["content"]}, {"role": "user", "content": rep_prompt}],
                     response_format={"type": "json_object"})
        r2 = sse_post(a.base_url, key, body2)
        rec["repaired"] = True
        rec["repair"] = {k: r2[k] for k in ("finish_reason", "usage", "http_status", "error", "events")}
        rec["repair_content"] = r2["content"]
        parsed = parse_output(r2["content"], items_key) if not r2["error"] else None
    rec["wall_s"] = round(time.time() - t0, 1)
    rec["parse_ok"] = parsed is not None
    rec["items"] = (parsed or {}).get(items_key, [])
    rec["needs"] = (parsed or {}).get("needs", [])
    rec["scanned_areas"] = (parsed or {}).get("scanned_areas", [])
    path = os.path.join(a.out, f"run-{idx}.json")
    json.dump(rec, open(path, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    u = r1["usage"] or {}
    print(f"[{a.arm} run {idx}] wall={rec['wall_s']}s finish={r1['finish_reason']} parse_ok={rec['parse_ok']} repaired={rec['repaired']} "
          f"items={len(rec['items'])} needs={len(rec['needs'])} usage={{prompt:{u.get('prompt_tokens')}, completion:{u.get('completion_tokens')}, "
          f"details:{u.get('completion_tokens_details')}, cache_hit:{u.get('prompt_cache_hit_tokens')}}} err={r1['error'] and r1['error'][:120]}")
    return rec


def cmd_run(a):
    key = os.environ.get("DEEPSEEK_API_KEY")
    if not key:
        raise SystemExit("DEEPSEEK_API_KEY not set")
    system = open(os.path.join(a.pack, "prompt.system.txt"), encoding="utf-8").read()
    user = open(os.path.join(a.pack, "prompt.user.md"), encoding="utf-8").read()
    os.makedirs(a.out, exist_ok=True)
    idxs = list(range(a.start, a.start + a.n))
    with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, a.parallel)) as ex:
        list(ex.map(lambda i: one_run(a, i, key, system, user), idxs))


# ---------------- score ----------------
def show_file(repo, head, path):
    r = subprocess.run(["git", "-C", repo, "show", f"{head}:{path}"], capture_output=True, text=True, errors="replace")
    return r.stdout if r.returncode == 0 else None


_STRIP_CACHE = {}


def anchor_quote(repo, head, path, quote):
    """产品同口径：quote 去全部空白后子串匹配；返回 (line_start, line_end) 或 None。"""
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


def cmd_score(a):
    oracle = json.load(open(a.oracle, encoding="utf-8"))
    ids = set(oracle["sets"][a.set])
    entries = [e for e in oracle["entries"] if e["id"] in ids]
    runs = sorted(f for f in os.listdir(a.runs) if re.match(r"run-\d+\.json$", f))
    per_run, all_hit_ids, table = [], set(), []
    hit_confs = []
    for f in runs:
        r = json.load(open(os.path.join(a.runs, f), encoding="utf-8"))
        items, hits, unanch = r.get("items", []), 0, 0
        run_hit_ids = set()
        for n, it in enumerate(items, 1):
            path = it.get("file") or ""
            rng = anchor_quote(a.repo, a.head, path, it.get("quote", ""))
            if rng is None:
                unanch += 1
                try:
                    rng = (int(it.get("line_start") or 1), int(it.get("line_end") or it.get("line_start") or 1))
                except (TypeError, ValueError):
                    rng = (1, 1)
            matched = [e for e in entries if any(x["file"] == path and overlap(rng, (x["line_start"], x["line_end"])) for x in e["anchors"])]
            if matched:
                hits += 1
                run_hit_ids |= {e["id"] for e in matched}
                if it.get("confidence") is not None:
                    hit_confs.append(float(it["confidence"]))
            table.append({"run": r["run"], "n": n, "file": path, "lines": list(rng), "anchored": rng is not None and unanch == unanch,
                          "confidence": it.get("confidence"), "category": it.get("category") or it.get("severity"),
                          "text": (it.get("hint") or it.get("title") or "")[:140],
                          "auto_match": sorted({f"{e['id']}:{e.get('topic') or ''}" for e in matched}), "manual_topic": ""})
        all_hit_ids |= run_hit_ids
        u1 = (r.get("turn1") or {}).get("usage") or {}
        det = u1.get("completion_tokens_details") or {}
        per_run.append({"run": r["run"], "items": len(items), "hit_items": hits, "oracle_ids_hit": sorted(run_hit_ids), "n_oracle_hit": len(run_hit_ids),
                        "noise": round((len(items) - hits) / len(items), 3) if items else None, "unanchored": unanch,
                        "completion_tokens": u1.get("completion_tokens"), "reasoning_tokens": det.get("reasoning_tokens"),
                        "prompt_tokens": u1.get("prompt_tokens"), "wall_s": r.get("wall_s"), "finish_reason": (r.get("turn1") or {}).get("finish_reason"),
                        "parse_ok": r.get("parse_ok"), "repaired": r.get("repaired"), "needs": len(r.get("needs", [])),
                        "scanned_areas": len(r.get("scanned_areas", []))})
    mean = lambda k: round(sum(x[k] for x in per_run if x[k] is not None) / max(1, sum(1 for x in per_run if x[k] is not None)), 2)
    confs = sorted(hit_confs)
    summary = {
        "arm": a.arm, "set": a.set, "oracle_n": len(entries), "runs": per_run,
        "mean_items": mean("items"), "mean_oracle_hit": mean("n_oracle_hit"), "union_oracle_hit": len(all_hit_ids), "union_ids": sorted(all_hit_ids),
        "mean_noise": mean("noise"), "mean_completion_tokens": mean("completion_tokens"), "mean_wall_s": mean("wall_s"),
        "c_hi": {"min": confs[0] if confs else None, "p10": confs[int(len(confs) * 0.1)] if confs else None,
                 "median": confs[len(confs) // 2] if confs else None, "n": len(confs)},
        "category_dist": {}, "hint_len_words": None,
    }
    cats, lens = {}, []
    for row in table:
        cats[row["category"]] = cats.get(row["category"], 0) + 1
        lens.append(len(row["text"].split()))
    summary["category_dist"] = cats
    if lens:
        lens.sort()
        summary["hint_len_words"] = {"median": lens[len(lens) // 2], "max": lens[-1]}
    json.dump({"summary": summary, "table": table}, open(a.out, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    with open(a.out.rsplit(".", 1)[0] + ".md", "w", encoding="utf-8") as md:
        md.write(f"# E3 {a.arm} 臂人工主题确认表（auto_match 为区间级自动命中；manual_topic 人工填）\n\n| run | # | file:lines | conf | cat | auto_match | text | manual_topic |\n|---|---|---|---|---|---|---|---|\n")
        for row in table:
            md.write(f"| {row['run']} | {row['n']} | {row['file'].rsplit('/', 1)[-1]}:{row['lines'][0]}-{row['lines'][1]} | {row['confidence']} | {row['category']} | "
                     f"{' '.join(row['auto_match'])} | {row['text'].replace('|', '/')} | |\n")
    print(json.dumps(summary, ensure_ascii=False, indent=1))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    v = sub.add_parser("variant"); v.add_argument("--pack", required=True); v.add_argument("--src", required=True); v.add_argument("--out", required=True)
    v.set_defaults(fn=cmd_variant)
    r = sub.add_parser("run"); r.add_argument("--arm", choices="AB", required=True); r.add_argument("--pack", required=True)
    r.add_argument("--n", type=int, default=3); r.add_argument("--start", type=int, default=1); r.add_argument("--max-tokens", type=int, default=32768)
    r.add_argument("--model", default="deepseek-v4-flash"); r.add_argument("--base-url", default="https://api.deepseek.com")
    r.add_argument("--parallel", type=int, default=1); r.add_argument("--out", required=True)
    r.set_defaults(fn=cmd_run)
    s = sub.add_parser("score"); s.add_argument("--arm", choices="AB", required=True); s.add_argument("--runs", required=True)
    s.add_argument("--oracle", required=True); s.add_argument("--repo", required=True); s.add_argument("--head", required=True)
    s.add_argument("--set", default="union"); s.add_argument("--out", required=True)
    s.set_defaults(fn=cmd_score)
    a = ap.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
