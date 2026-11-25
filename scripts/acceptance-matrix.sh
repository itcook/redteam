#!/usr/bin/env bash
# hibot 验收矩阵（消费计划 §9.4）：3 kind × 3 夹具（clean / known-vuln / partial）× 2 语言 = 18 组合。
#
# 行为：
#   1. `cargo test --test acceptance_matrix` 跑完 18 组合（fake backend，确定性、可复跑）；
#   2. 测试把每组的 `out/report.json` + `out/state.json` 落到 <out>/<kind>/<fixture>/<lang>/；
#   3. `HIBOT_SCHEMA_DIR` 存在时，用 hibot 的契约 schema（按 `$id` 注册）校验 capabilities 与 18 份报告/心跳；
#      未设置或目录缺失 → 打印 SKIP（矩阵本身仍然有效）。
#
# 用法：
#   scripts/acceptance-matrix.sh [--out DIR] [--schema-dir DIR]
#
# 环境：
#   HIBOT_SCHEMA_DIR        hibot 契约 schema 目录（`hibot/.docs/audit/schema`）
#
# 退出码：0 = 矩阵通过（schema 校验跳过也算通过）；1 = 矩阵或 schema 校验失败。
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT="$ROOT/target/acceptance-matrix"
SCHEMAS=${HIBOT_SCHEMA_DIR:-}

while [ $# -gt 0 ]; do
  case "$1" in
    --out)        OUT=${2:?--out 需要目录}; shift 2 ;;
    --schema-dir) SCHEMAS=${2:?--schema-dir 需要目录}; shift 2 ;;
    -h|--help)    sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "error: 未知参数 $1" >&2; exit 2 ;;
  esac
done

KINDS="review triage verify"
FIXTURES="clean known-vuln partial"
LANGS="zh_CN en_US"

rm -rf "$OUT"
mkdir -p "$OUT"

echo "== 1/3 跑矩阵（18 组合，fake backend）"
REDTEAM_ACCEPTANCE_OUT="$OUT" cargo test --manifest-path "$ROOT/Cargo.toml" --test acceptance_matrix -- --nocapture

echo "== 2/3 核对产物：18 份 report + 18 份 state"
missing=0
for kind in $KINDS; do
  for fixture in $FIXTURES; do
    for lang in $LANGS; do
      dir="$OUT/$kind/$fixture/$lang"
      for name in report.json state.json; do
        if [ ! -s "$dir/$name" ]; then
          echo "MISSING $dir/$name" >&2
          missing=$((missing + 1))
        fi
      done
    done
  done
done
[ "$missing" -eq 0 ] || { echo "error: 缺少 $missing 份产物" >&2; exit 1; }
echo "OK: 18 组 × 2 份产物齐全"

echo "== 3/3 hibot schema 校验"
LOG="$OUT/schema-check.log"
: > "$LOG"
if [ -z "$SCHEMAS" ] || [ ! -d "$SCHEMAS" ]; then
  echo "SKIP: 未设置 HIBOT_SCHEMA_DIR 或目录不存在（矩阵结果仍然有效）" | tee -a "$LOG"
  echo
  echo "done: $OUT"
  exit 0
fi

CHECK="python3 $ROOT/scripts/hibot-schema-check.py --schema-dir $SCHEMAS"
rc=0
$CHECK capabilities "$ROOT/capabilities/redteam.json" 2>&1 | tee -a "$LOG" || rc=1
for kind in $KINDS; do
  # shellcheck disable=SC2046
  $CHECK "report.$kind" $(find "$OUT/$kind" -name report.json | sort) 2>&1 | tee -a "$LOG" || rc=1
done
# shellcheck disable=SC2046
$CHECK state $(find "$OUT" -name state.json | sort) 2>&1 | tee -a "$LOG" || rc=1

if [ "$rc" -ne 0 ]; then
  echo "error: hibot schema 校验失败（见 ${LOG}）" >&2
  exit 1
fi
echo "OK: capabilities + 18 报告 + 18 心跳全部过 hibot schema"
echo
echo "done: ${OUT}（schema 日志 ${LOG}）"
