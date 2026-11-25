#!/usr/bin/env bash
# 真实夹具：#400 三组的真跑与对拍（不进 CI；需要本机 ~/hiapi/backend + 一个可用模型）。
#
# 用法：
#   scripts/verify-pr400.sh <group-a|group-b|group-c> [--model provider/id] [--bin PATH] [--out DIR] [--keep]
#
# 行为：切 detached worktree 到 H2 → 生成 job（diff H1..H2、merge_base=H1、fixture findings）→
#       真跑 `redteam verify` → 逐条对照 expected.json（同 / 异 / 无法判定）→ 落 report/result。
# 密钥：从 ${REDTEAM_HOME:-$HOME/.redteam}/secrets.json 读出，只经临时 env 注入子进程；不落盘、不打印。
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
GROUP=${1:?用法: scripts/verify-pr400.sh <group-a|group-b|group-c> [--model provider/id] [--bin PATH] [--out DIR] [--keep]}
shift

REPO=${HIBOT_BACKEND_REPO:-$HOME/hiapi/backend}
CACHE=${PR400_REPO_CACHE:-$ROOT/target/pr400-repo}
BRANCH=${PR400_BRANCH:-dev}
MODEL="deepseek/deepseek-flash"
BIN="$ROOT/target/debug/redteam"
OUT="$ROOT/target/pr400/$GROUP"
KEEP=0
while [ $# -gt 0 ]; do
  case "$1" in
    --model) MODEL=${2:?}; shift 2 ;;
    --bin)   BIN=${2:?};   shift 2 ;;
    --out)   OUT=${2:?};   shift 2 ;;
    --keep)  KEEP=1;       shift ;;
    *) echo "error: 未知参数 $1" >&2; exit 2 ;;
  esac
done

FIX="$ROOT/tests/fixtures/verify/pr400/$GROUP"
[ -d "$FIX" ] || { echo "error: 没有这一组：$GROUP" >&2; exit 2; }
[ -d "$REPO/.git" ] || { echo "error: 找不到后端仓：${REPO}（用 HIBOT_BACKEND_REPO 覆盖）" >&2; exit 2; }

# 契约 §2.6：job 的检出必须是**非 shallow** 的完整检出。本机源仓若是浅克隆（hibot 的 runner 会给完整检出），
# 就在缓存目录建一个 blob-filtered 的完整克隆（允许的 mirror 形态），不动源仓。
if [ "$(git -C "$REPO" rev-parse --is-shallow-repository 2>/dev/null || echo false)" = "true" ]; then
  SRC="$CACHE"
  if [ ! -d "$SRC/.git" ]; then
    URL=$(git -C "$REPO" remote get-url origin)
    echo "→ 源仓是浅克隆：建完整（blob-filtered）克隆 $SRC"
    rm -rf "$SRC"
    git clone --filter=blob:none --no-checkout --single-branch --branch "$BRANCH" "$URL" "$SRC" >/dev/null 2>&1 \
      || git clone --filter=blob:none --no-checkout --single-branch --branch "$BRANCH" "$(printf '%s' "$URL" | sed 's#^git@github.com:#https://github.com/#')" "$SRC" >/dev/null
  fi
else
  SRC="$REPO"
fi

read_meta() { python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$FIX/meta.json" "$1"; }
H1=$(read_meta h1)
H2=$(read_meta h2)
git -C "$SRC" cat-file -e "$H1^{commit}" 2>/dev/null || { echo "error: $H1 不在 ${REPO}（先 git fetch）" >&2; exit 2; }
git -C "$SRC" cat-file -e "$H2^{commit}" 2>/dev/null || { echo "error: $H2 不在 ${REPO}（先 git fetch）" >&2; exit 2; }

PROVIDER=${MODEL%%/*}
MODEL_ID=${MODEL##*/}
API_BASE=${PR400_API_BASE:-https://api.deepseek.com}
API_KIND=${PR400_API_KIND:-openai-completions}

KEY=$(MODEL="$MODEL" python3 - "$MODEL" <<'PY'
import json, os, sys
provider, model = sys.argv[1].split("/", 1)
home = os.environ.get("REDTEAM_HOME") or os.path.expanduser("~/.redteam")
path = os.path.join(home, "secrets.json")
try:
    data = json.load(open(path))
except Exception:
    sys.exit(0)
node = (data.get("providers") or {}).get(provider) or {}
key = (node.get("models") or {}).get(model) or node.get("apiKey") or ""
print(key)
PY
)
[ -n "$KEY" ] || { echo "error: 在 secrets.json 里没找到 $MODEL 的 key（也可 export PR400_VERIFY_KEY）" >&2; exit 2; }
KEY=${PR400_VERIFY_KEY:-$KEY}

[ -x "$BIN" ] || { echo "→ 构建 redteam（$BIN 不存在）"; (cd "$ROOT" && cargo build -q); }

WORK=$(mktemp -d "${TMPDIR:-/tmp}/pr400-XXXXXX")
cleanup() {
  git -C "$SRC" worktree remove --force "$WORK/repo" >/dev/null 2>&1 || true
  [ "$KEEP" = 1 ] || rm -rf "$WORK"
}
trap cleanup EXIT

echo "→ worktree $H2"
# 缓存克隆里补一次 fetch，防止单分支缓存落后于已合入的 PR 提交
git -C "$SRC" cat-file -e "$H1^{commit}" 2>/dev/null && git -C "$SRC" cat-file -e "$H2^{commit}" 2>/dev/null \
  || git -C "$SRC" fetch -q origin "$H1" "$H2" 2>/dev/null || true
git -C "$SRC" worktree add --detach "$WORK/repo" "$H2" >/dev/null

JOB="$WORK/job"
mkdir -p "$JOB/config" "$JOB/input" "$JOB/out"
cat > "$JOB/config/executor.redteam.json" <<JSON
{
  "settings": {
    "language": "zh_CN",
    "verify": {"models": [{"provider": "$PROVIDER", "id": "$MODEL_ID"}]}
  },
  "providers": {
    "$PROVIDER": {
      "api": "$API_KIND",
      "baseUrl": "$API_BASE",
      "apiKeyEnv": "PR400_VERIFY_KEY",
      "models": [{"id": "$MODEL_ID", "maxOutputTokens": 8192}]
    }
  }
}
JSON

git -C "$SRC" diff "$H1..$H2" > "$JOB/input/diff.patch"
printf '%s\n' "$H1" > "$JOB/input/merge-base.sha"
cp "$FIX/findings.json" "$JOB/input/findings.json"

python3 - "$JOB" "$H1" "$H2" "$GROUP" <<'PY'
import hashlib, json, os, sys
job, h1, h2, group = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
def sha(p):
    return hashlib.sha256(open(p, "rb").read()).hexdigest()
manifest = {
    "schema": "hibot.audit.job.verify/1",
    "job_id": f"pr400-{group}",
    "kind": "verify",
    "repo": {"id": 400, "full_name": "zimacode-network/hiapi-backend"},
    "pr": {"number": 400, "head_sha": h2, "base_sha": h1},
    "executor": {"name": "redteam", "version": "0.1.0"},
    "limits": {"wall_clock_s": 900},
    "paths": {"repo": "../repo", "out": "out", "config": "config/executor.redteam.json"},
    "inputs": {
        "findings": {"path": "input/findings.json", "sha256": sha(os.path.join(job, "input/findings.json")),
                     "schema": "hibot.audit.findings/1"},
        "diff_patch": {"path": "input/diff.patch", "sha256": sha(os.path.join(job, "input/diff.patch"))},
        "merge_base": {"path": "input/merge-base.sha", "sha256": sha(os.path.join(job, "input/merge-base.sha"))},
    },
}
open(os.path.join(job, "job.json"), "w").write(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
PY

echo "→ redteam verify（${MODEL}）"
set +e
PR400_VERIFY_KEY="$KEY" "$BIN" verify --job-dir "$JOB"
CODE=$?
set -e
echo "→ 退出码 $CODE"

mkdir -p "$OUT"
if [ -f "$JOB/out/report.json" ]; then
  cp "$JOB/out/report.json" "$OUT/report.json"
fi
cp "$JOB/job.json" "$OUT/job.json"

python3 - "$FIX" "$JOB" "$OUT" <<'PY'
import json, os, sys
fix, job, out = sys.argv[1], sys.argv[2], sys.argv[3]
report_path = os.path.join(job, "out", "report.json")
if not os.path.exists(report_path):
    print("！没有 report.json（作业失败）")
    sys.exit(1)
report = json.load(open(report_path))
expected = json.load(open(os.path.join(fix, "expected.json")))
actual = {v["finding_fingerprint"]: v for v in report.get("verifications", [])}

rows, same, diff = [], 0, 0
for fp, exp in expected.items():
    got = actual.get(fp)
    if got is None:
        verdict, status, evidence = "无法判定", "（缺）", "报告里没有这条"
        diff += 1
    else:
        status = got.get("status", "?")
        verdict = "同" if status == exp["status"] else "异"
        same += verdict == "同"
        diff += verdict == "异"
        evidence = (got.get("evidence") or "").replace("\n", " ")[:120]
    rows.append({"fingerprint": fp, "expected": exp["status"], "actual": status, "verdict": verdict,
                 "note": exp.get("note", ""), "evidence": evidence})

print()
print(f"== {os.path.basename(fix)} · status={report.get('status')} · 同 {same} / 异 {diff} ==")
for r in rows:
    print(f"{r['fingerprint'][:19]}  expected={r['expected']:<12} actual={r['actual']:<12} {r['verdict']}")
    if r["verdict"] != "同":
        print(f"    expected: {r['note']}")
        print(f"    actual  : {r['evidence']}")
ext = (report.get("extensions") or {}).get("redteam", {}).get("verify", {})
print(f"usage: wall_clock_ms={report.get('usage', {}).get('wall_clock_ms')} "
      f"tokens in/out={report.get('usage', {}).get('input_tokens')}/{report.get('usage', {}).get('output_tokens')}")
print(f"verify: {json.dumps(ext, ensure_ascii=False)}")
json.dump({"group": os.path.basename(fix), "status": report.get("status"), "same": same, "diff": diff,
           "rows": rows, "extensions": ext, "usage": report.get("usage", {})},
          open(os.path.join(out, "result.json"), "w"), ensure_ascii=False, indent=2)
print(f"→ {os.path.join(out, 'result.json')}")
PY
