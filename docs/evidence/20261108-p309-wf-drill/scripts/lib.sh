#!/usr/bin/env bash
# ============================================================================
# P3-09 · 工作流全链路回归演练 · 公共函数（source 使用，不单独执行）
#
# 口径与 P3-02~P3-08 的证据脚本一致：
#   · 隔离实例：**全新空库**（每次 drop/create）+ 独立端口 18091 + 二进制快照
#     `/root/p309/bin/cloudpivot`（md5 7df255ef3b8f3dacbd5c207b569e9000 =
#     38b813c + t_886189f8「END 汇聚口径」改动 的 debug 构建产物；
#     原 P3-09 演练用的 /root/p308/bin/cloudpivot 是 506fa34 的产物，已不含本卡修复）；
#   · 三把密钥与 admin 口令只在 600 权限的环境文件里（仓库不落任何口令/密钥）；
#   · 144 生产库零读写（只连 127.0.0.1 本机测试库）。
#
# 注意：本文件的 `http()` 会真的带上 Bearer 令牌；仓库里 P3-07/P3-08 的 lib.sh
# 曾被脱敏成 `Bearer ***`，拷过来会「所有授权请求 401」。这里自己写一份。
# ============================================================================
set -uo pipefail

EV="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RAW="$EV/raw"
mkdir -p "$RAW"

PORT=${PORT:-18091}
DB=${DB:-cloudpivot_p309_drill}
ENVF=${ENVF:-/root/p309/instance.env}
BIN=${BIN:-/root/p309/bin/cloudpivot}
RUN_DIR=${RUN_DIR:-/opt/cloudpivot/backend}
ILOG=${ILOG:-/root/p309/instance.log}
IPID=${IPID:-/root/p309/instance.pid}

say() { printf '%s\n' "$*"; }

port_pid() {
  ss -ltnp 2>/dev/null | sed -n "s/.*:$PORT .*pid=\([0-9]*\).*/\1/p" | head -1
}

stop_instance() {
  local pid i
  pid=$(cat "$IPID" 2>/dev/null) || pid=$(port_pid)
  [ -n "${pid:-}" ] || return 0
  kill "$pid" 2>/dev/null || return 0
  for i in $(seq 1 40); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 1
  done
  kill -9 "$pid" 2>/dev/null || true
  rm -f "$IPID"
}

start_instance() {
  local i pid
  [ -f "$ENVF" ] || { say "环境文件不存在：$ENVF"; return 1; }
  ( cd "$RUN_DIR" && setsid env TMPDIR=/root $(grep -v '^#' "$ENVF" | grep -v '^$' | tr '\n' ' ') \
      NIK_DATABASE_NAME="$DB" NIK_SERVER_LISTEN="127.0.0.1:$PORT" \
      "$BIN" >"$ILOG" 2>&1 </dev/null & )
  for i in $(seq 1 120); do
    pid=$(port_pid)
    if [ -n "$pid" ]; then echo "$pid" >"$IPID"; return 0; fi
    sleep 1
  done
  return 1
}

wait_http() {
  local i=0
  while [ "$i" -lt 240 ]; do
    if curl -fsS -o /dev/null "http://127.0.0.1:$PORT/health" 2>/dev/null; then return 0; fi
    i=$((i + 1)); sleep 1
  done
  return 1
}
