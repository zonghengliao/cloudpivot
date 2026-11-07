#!/usr/bin/env bash
# REVERIFY（t_2f92ee87）：重建「全新库（NIK_SEED_DEMO_USERS=true）」并起 8051 隔离实例
# 口径：只动 145 上的 cloudpivot_rev_demo / 8051；8033 服役实例与 cloudpivot_dev 不碰；144 不碰。
# 用途：上一轮 harness 的登录探测把演示账号口令改成未知值，且「开箱即用」的验收本就要求干净库，
#       所以这里 drop + create + 带 seed 首启，一次拿到「261 迁移 / 10 账号 / 16 流程」的干净基线。
set -euo pipefail
ENVF=/etc/cloudpivot/cloudpivot-dev.env
DB=${DB:-cloudpivot_rev_demo}
PORT=${PORT:-8051}
SEED=${SEED:-true}
TAG=${TAG:-demo}
ENVF_PW=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' "$ENVF")
export PGPASSWORD="$ENVF_PW"

# ① 停目标端口上的实例（只按端口找 PID，避免 pkill 误杀 8033/8050/8052）
PID=$(ss -ltnp 2>/dev/null | grep ":${PORT} " | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2 || true)
if [ -n "${PID:-}" ]; then kill "$PID"; for i in $(seq 1 30); do ss -ltn | grep -q ":${PORT} " || break; sleep 1; done; fi
echo "port ${PORT} stopped (pid=${PID:-none})"

# ② drop + create（连接还在时 drop 会失败，所以先确认端口已释放）
su - postgres -c "psql -v ON_ERROR_STOP=1 -c \"DROP DATABASE IF EXISTS $DB\"" >/dev/null
su - postgres -c "psql -v ON_ERROR_STOP=1 -c \"CREATE DATABASE $DB OWNER cloudpivot\"" >/dev/null
echo "db recreated: $DB"

# ③ 首启（SEED=true 走演示账号种子；false = 只有 admin，对应 FIX-1 引导场景）
set -a; . "$ENVF"; set +a
export NIK_DATABASE_NAME="$DB" NIK_SERVER_LISTEN="127.0.0.1:$PORT" NIK_SEED_DEMO_USERS="$SEED"
cd /opt/cloudpivot/backend
setsid nohup ./target/debug/cloudpivot-rev > "/root/backup/rev_${TAG}_reset.log" 2>&1 < /dev/null &
echo $! > "/root/backup/.rev_${TAG}.pid"
for i in $(seq 1 180); do
  code=$(curl -s -m 2 -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)
  [ "$code" = "200" ] && { echo "health=200 after ${i}s"; break; }
  sleep 1
done
grep -E "数据库迁移完成|演示账号种子完成" "/root/backup/rev_${TAG}_reset.log" | tail -3 || true

# ④ 基线断言
export PGPASSWORD="$ENVF_PW"
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -tAc "select 'migrations='||count(*) from schema_migrations"
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -tAc "select 'users='||count(*) from sys_user where deleted_at is null"
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -tAc "select 'flows_published='||count(*) from biz_flow_def where status='PUBLISHED'"
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -tAc "select 'materials='||count(*) from biz_material"
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -tAc "select 'customers='||count(*) from biz_customer"
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -tAc "select 'warehouses='||count(*) from biz_warehouse"
