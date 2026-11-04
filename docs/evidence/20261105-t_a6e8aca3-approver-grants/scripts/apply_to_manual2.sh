#!/bin/bash
# 把 V0239 应用到演示库 cloudpivot_manual2，但**不重启**别人在跑的 18097 实例。
# 做法：另一个进程连同一个库、只在「数据库迁移完成」后立刻杀掉（迁移是事务提交的，18097 立刻可见）。
set -uo pipefail
S=/root/.hermes/profiles/backend-developer/cache/scratch
PORT=18163
LOG=$S/apply_manual2.log
: > $LOG

cd /opt/cloudpivot/backend
setsid env $(grep -v '^#' $S/fresh.env | grep -v '^NIK_DATABASE_NAME=' | grep -v '^NIK_SERVER_LISTEN=' | grep -v '^NIK_REDIS_URL=' | tr '\n' ' ') \
  NIK_DATABASE_NAME=cloudpivot_manual2 NIK_SERVER_LISTEN=127.0.0.1:$PORT NIK_REDIS_URL=redis://127.0.0.1:6379/14 \
  ./target/debug/cloudpivot > $LOG 2>&1 &

for i in $(seq 1 60); do
  if grep -aq "数据库迁移完成" $LOG; then break; fi
  sleep 1
done
grep -a "数据库迁移完成\|迁移失败\|ERROR" $LOG | head -5
sleep 1
pid=$(ss -lptnH "sport = :$PORT" | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2)
[ -n "${pid:-}" ] && kill "$pid" 2>/dev/null && echo "killed $pid"
echo "--- 无残留监听：$(ss -lnt | grep -c ":$PORT")"
