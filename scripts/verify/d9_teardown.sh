#!/usr/bin/env bash
# D9 收尾：停隔离实例 / 停 vite dev / 删临时套件库
set -uo pipefail
echo "== 1) 停隔离实例（按 pid 文件，别 pkill）=="
for f in /tmp/d9_e2e_server.pid /tmp/d9_server.pid /tmp/d9_full.pid /tmp/d9_inv.pid; do
  if [ -f "$f" ]; then
    kill "$(cat "$f")" 2>/dev/null || true
    sleep 1
    kill -9 "$(cat "$f")" 2>/dev/null || true
    rm -f "$f"
    echo "  stopped $(basename "$f")"
  fi
done

echo "== 2) 停 vite dev（5196）=="
pids=$(ss -lptn 'sport = :5196' 2>/dev/null | grep -o 'pid=[0-9]*' | cut -d= -f2 | sort -u || true)
for p in $pids; do kill "$p" 2>/dev/null || true; echo "  killed vite pid=$p"; done

echo "== 3) 删临时套件库（保留 cloudpivot_d9 供复跑单测）=="
for db in cloudpivot_d9full cloudpivot_d9inv; do
  runuser -u postgres -- psql -qc "DROP DATABASE IF EXISTS $db" 2>/dev/null && echo "  dropped $db"
done

echo "== 4) 复核 =="
pgrep -fa "target/debug/cloudpivot" | head -3 || echo "  无隔离实例进程"
runuser -u postgres -- psql -tAc "select datname from pg_database where datname like 'cloudpivot_d9%'" | sed 's/^/  db: /'
echo "done"
