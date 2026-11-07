#!/usr/bin/env bash
# ============================================================================
# FIX-5（t_9a23375a）145 取证 · 收尾：杀掉本卡的隔离实例（只动自己的 PID）
#
# 8033 的服役实例（cloudpivot-test）不在本脚本范围内，脚本末尾会复核它仍然健康。
# ============================================================================
set -uo pipefail

PIDFILE=/root/backup/.fix5_pid
if [ -f "$PIDFILE" ]; then
  OLD=$(cat "$PIDFILE")
  if kill -0 "$OLD" 2>/dev/null; then
    kill "$OLD"
    for _ in $(seq 1 20); do
      kill -0 "$OLD" 2>/dev/null || break
      sleep 0.5
    done
    echo "isolated instance stopped (pid=$OLD)"
  else
    echo "isolated instance already stopped (pid=$OLD)"
  fi
else
  echo "no pid file ($PIDFILE)"
fi

sleep 1
code=$(curl -s -m 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:8046/health || true)
echo "8046 health after stop = ${code:-000}（期望非 200）"
code8033=$(curl -s -m 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:8033/health || true)
echo "8033（服役实例，全程未动）health = ${code8033:-000}（期望 200）"
