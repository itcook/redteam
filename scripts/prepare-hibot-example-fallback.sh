#!/usr/bin/env bash
# **兼容工具（fallback / 对照），不是产品路径，也不是主证据来源。**
#
# hibot 的示例脚本在 `91834c2`（2026-11-25）之前产出**不合契约**的 job 目录，三处缺陷：
#   1. `inputs/*.diff.patch` 是无 `diff --git` 头的节选——契约要求 runner 本地
#      `git diff merge-base..head` 的完整产物；
#   2. `paths.repo` 是 `git archive` 展开的树（没有 `.git`）——契约要求 detached、非 shallow 的完整检出；
#   3. triage/review 的 `pr.base_sha` 是仓库里不存在的占位 sha。
# 官方脚本现在已修好，**主路径请用它**（本机 backend 若是 shallow，按它自己的文档覆盖 checkout）：
#
#   HIBOT_BACKEND_REPO=<非 shallow 克隆> \
#     ~/hibot/.docs/examples/audit-pr-468/prepare-job.sh <kind> <absolute-target-dir>
#
# 本脚本保留两个用途：
#   1. 老快照 / 未修复示例的对照重放（自包含重建：克隆 + worktree + diff + base_sha + sha256）；
#   2. 本机 `~/hiapi/backend` 是 shallow 时，官方 `git worktree add` 仍得到 shallow 检出（引擎会拒），
#      本脚本可先建 blob-filtered 非 shallow 克隆，再按与官方相同的规则生成 job。
# 只读示例目录与 backend 仓，不写回 `~/hibot`。
#
# 用法：
#   scripts/prepare-hibot-example-fallback.sh [--example DIR] [--backend REPO] [--out DIR] [--cache DIR] [--config FILE]
#
# `--config FILE`：用给定配置替换示例里的 `config/executor.redteam.json`（示例的 `HIBOT_LLM_*` key 未必在本机可用）。
#
# 默认：
#   --example ~/hibot/.docs/examples/audit-pr-468
#   --backend ~/hiapi/backend
#   --out     <repo>/target/hibot-example
#   --cache   <repo>/target/hibot-example-cache   （blob-filtered 全量克隆，首次联网）
#
# 产物：<out>/<triage|review|verify>/{job/,repo/}，可直接 `redteam <kind> --job-dir <out>/<kind>/job`。
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
EXAMPLE=${HIBOT_EXAMPLE_DIR:-$HOME/hibot/.docs/examples/audit-pr-468}
BACKEND=${HIBOT_BACKEND_REPO:-$HOME/hiapi/backend}
OUT="$ROOT/target/hibot-example"
CACHE="$ROOT/target/hibot-example-cache"
CONFIG=""

while [ $# -gt 0 ]; do
  case "$1" in
    --example) EXAMPLE=${2:?--example 需要目录}; shift 2 ;;
    --backend) BACKEND=${2:?--backend 需要目录}; shift 2 ;;
    --out)     OUT=${2:?--out 需要目录}; shift 2 ;;
    --cache)   CACHE=${2:?--cache 需要目录}; shift 2 ;;
    --config)  CONFIG=${2:?--config 需要文件}; shift 2 ;;
    -h|--help) sed -n '2,/^set -euo pipefail/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "error: 未知参数 $1" >&2; exit 2 ;;
  esac
done

[ -d "$EXAMPLE" ] || { echo "error: 找不到示例目录 $EXAMPLE" >&2; exit 2; }
[ -d "$BACKEND/.git" ] || { echo "error: 找不到 backend 仓 $BACKEND" >&2; exit 2; }

manifest_get() { # <job.json> <dotted path>
  python3 - "$1" "$2" <<'PY'
import json, sys
node = json.load(open(sys.argv[1]))
for key in sys.argv[2].split("."):
    node = node[key]
print(node)
PY
}

# 一次性 blob-filtered 全量克隆（非 shallow）；首次需要网络。
if [ ! -d "$CACHE/.git" ]; then
  url=$(git -C "$BACKEND" remote get-url origin)
  echo "→ 建 blob-filtered 全量克隆：$url → $CACHE"
  mkdir -p "$(dirname "$CACHE")"
  git clone --filter=blob:none --no-checkout --quiet "$url" "$CACHE"
fi

mkdir -p "$OUT"
git -C "$CACHE" worktree prune
for kind in triage review verify; do
  src="$EXAMPLE"
  dest="$OUT/$kind"
  echo "== $kind"
  rm -rf "$dest"
  mkdir -p "$dest/job"/{inputs,config,out}
  cp "$src/job.$kind.json" "$dest/job/job.json"
  cp -R "$src/inputs/." "$dest/job/inputs/"
  if [ -n "$CONFIG" ]; then
    cp "$CONFIG" "$dest/job/config/executor.redteam.json"
  else
    cp "$src/config/executor.redteam.json" "$dest/job/config/executor.redteam.json"
  fi
  cp "$src/report.triage.json" "$src/report.review.json" "$dest/job/" 2>/dev/null || true

  job="$dest/job"
  head=$(manifest_get "$job/job.json" pr.head_sha)
  base=$(manifest_get "$job/job.json" pr.base_sha)
  mb_rel=$(manifest_get "$job/job.json" inputs.merge_base.path)
  mb=$(tr -d '[:space:]' < "$job/$mb_rel")
  patch_rel=$(manifest_get "$job/job.json" inputs.diff_patch.path)

  git -C "$CACHE" cat-file -e "$head^{commit}" 2>/dev/null || git -C "$CACHE" fetch -q origin "$head"
  git -C "$CACHE" cat-file -e "$mb^{commit}" || { echo "error: $kind 的 merge_base $mb 不在仓里" >&2; exit 1; }

  # 1) 真检出：detached worktree at head（重跑时可能留有上次的注册项）
  git -C "$CACHE" worktree prune
  git -C "$CACHE" worktree add -f --detach --quiet "$dest/repo" "$head"
  # 预取本 diff 需要的 blob（引擎运行时设 GIT_NO_LAZY_FETCH=1，不会自己联网补）。
  git -C "$dest/repo" diff "$mb"..HEAD >/dev/null

  # 2) diff_patch = runner 真实产物，并重算 sha256
  git -C "$dest/repo" diff "$mb"..HEAD > "$job/$patch_rel"
  python3 - "$job/job.json" "$job/$patch_rel" "$patch_rel" <<'PY'
import hashlib, json, sys
path, patch_abs, rel = sys.argv[1], sys.argv[2], sys.argv[3]
m = json.load(open(path))
m["inputs"]["diff_patch"]["sha256"] = hashlib.sha256(open(patch_abs, "rb").read()).hexdigest()
open(path, "w").write(json.dumps(m, indent=2) + "\n")
PY

  # 3) pr.base_sha：示例的占位 sha 不存在时换成真实 merge-base（契约允许 base≠merge_base，但必须是真对象）
  if git -C "$CACHE" cat-file -e "$base^{commit}" 2>/dev/null; then
    echo "   base_sha 保留（$base 存在）"
  else
    python3 - "$job/job.json" "$mb" <<'PY'
import json, sys
path, mb = sys.argv[1], sys.argv[2]
m = json.load(open(path))
old = m["pr"]["base_sha"]
m["pr"]["base_sha"] = mb
open(path, "w").write(json.dumps(m, indent=2) + "\n")
print(f"   base_sha {old[:12]}…（仓库里不存在）→ {mb[:12]}…")
PY
  fi
  echo "   repo=$dest/repo head=${head:0:12} merge_base=${mb:0:12} patch=$(wc -c < "$job/$patch_rel" | tr -d ' ')B"
  echo "   job=$job"
done

echo
echo "done: $OUT"
echo "调用：redteam <triage|review|verify> --job-dir $OUT/<kind>/job"
