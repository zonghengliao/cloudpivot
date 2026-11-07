#!/usr/bin/env bash
# t_92234f6b / 145 全新库（NIK_SEED_DEMO_USERS=true）首启 —— 只在 145 上跑
# 口径：只建/只动 cloudpivot_seed_emp + 8053 隔离实例；8033 服役实例、cloudpivot_dev、144 全不碰。
set -uo pipefail
ENVF=/etc/cloudpivot/cloudpivot-dev.env
DB=${1:-cloudpivot_seed_emp}
PORT=${2:-8053}
SEED=${3:-true}
TAG=${4:-fresh}
LOG=/root/backup/seedemp_${TAG}.log

# ① 库：不存在才建（绝不 DROP —— 库里可能有别人正在复核的数据）
EXIST=$(su - postgres -c "psql -At -c \"select 1 from pg_database where datname='${DB}'\"")
if [ -z "${EXIST:-}" ]; then
  su - postgres -c "psql -v ON_ERROR_STOP=1 -c \"CREATE DATABASE ${DB} OWNER cloudpivot\"" >/dev/null
  echo "db created: ${DB}"
else
  echo "db exists: ${DB}（不重建；如需全新首启请换库名）"
fi

# ② 目标端口上的旧实例（只按端口找 PID，避免误杀 8033/8050/8051/8052）
PID=$(ss -ltnp 2>/dev/null | grep ":${PORT} " | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2 || true)
if [ -n "${PID:-}" ]; then kill "$PID"; for i in $(seq 1 30); do ss -ltn | grep -q ":${PORT} " || break; sleep 1; done; fi
echo "port ${PORT} stopped (pid=${PID:-none})"

# ③ 首启
set -a; . "$ENVF"; set +a
export NIK_DATABASE_NAME="$DB" NIK_SERVER_LISTEN="127.0.0.1:$PORT" NIK_SEED_DEMO_USERS="$SEED"
cd /opt/cloudpivot/backend
setsid nohup ./target/debug/cloudpivot > "$LOG" 2>&1 < /dev/null &
echo $! > "/root/backup/.seedemp_${TAG}.pid"
for i in $(seq 1 240); do
  code=$(curl -s -m 2 -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)
  [ "$code" = "200" ] && { echo "health=200 after ${i}s"; break; }
  sleep 1
done
echo "---- 迁移 / 种子日志（真实输出）----"
grep -E "数据库迁移完成|演示账号种子完成|演示部门补漏" "$LOG" | tail -12
