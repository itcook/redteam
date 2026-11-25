#!/usr/bin/env bash
# 单 job 真跑：墙钟上限、退出码、三帧 state.json 采样（Phase 6 / 9.3 手工联调）。
#
# 用法：run-one.sh <bin> <kind> <job-dir> <out-dir> [limit-s]
# 产物：<out>/{stdout.log,stderr.log,exit-code.txt,wall-clock-s.txt,report.json,state-final.json,frames/frame-N.json}
set -uo pipefail
BIN=${1:?bin}; KIND=${2:?kind}; JOB=${3:?job-dir}; OUT=${4:?out-dir}; LIMIT=${5:-600}
mkdir -p "$OUT/frames"

# 心跳每 2s 一帧；三次采样落在 prepare/running 段（真跑通常远超 18s）。
(
  for i in 1 2 3; do
    sleep 6
    [ -f "$JOB/out/state.json" ] && cp "$JOB/out/state.json" "$OUT/frames/frame-$i.json"
  done
) &
SAMPLER=$!

start=$(date +%s)
( cd /tmp && timeout "$LIMIT" "$BIN" "$KIND" --job-dir "$JOB" > "$OUT/stdout.log" 2> "$OUT/stderr.log" )
code=$?
end=$(date +%s)
wait "$SAMPLER" 2>/dev/null

echo "$code" > "$OUT/exit-code.txt"
echo "$((end - start))" > "$OUT/wall-clock-s.txt"
cp "$JOB/out/report.json" "$OUT/report.json" 2>/dev/null || true
cp "$JOB/out/state.json" "$OUT/state-final.json" 2>/dev/null || true
echo "kind=$KIND exit=$code wall=$((end - start))s job=$JOB"
