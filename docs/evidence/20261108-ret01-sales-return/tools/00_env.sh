#!/usr/bin/env bash
# =============================================================================
# RET-01 演练环境（145 测试机）：建隔离库 cloudpivot_ret01 + 起后端 :18225
#   bash 00_env.sh          # 建库 + 起实例 + 打印环境指纹
#   bash 00_env.sh down     # 停实例（幂等）
# 为什么用隔离库：cloudpivot_dev 是多人共用的 145 测试库（nginx:80 → 8033），
#   演练要往库里写客户/订单/退货/凭证，写进去会砸别人的不变量巡检。
#   隔离库由空库首启自动跑完全部迁移（不克隆模板库，避开「别人改过已应用迁移 → 校验和失败」）。
# =============================================================================
set -uo pipefail
DB=${RET01_DB:-cloudpivot_ret01}
PORT=${RET01_PORT:-18225}
LOG=/tmp/ret01_server.log
PIDF=/tmp/ret01_server.pid
BIN=/opt/cloudpivot/backend/target/debug/cloudpivot

stop() {
  if [ -f "$PIDF" ]; then
    kill "$(cat "$PIDF")" 2>/dev/null || true
    sleep 2
    kill -9 "$(cat "$PIDF")" 2>/dev/null || true
    rm -f "$PIDF"
  fi
  # 只清自己那一个端口的残留（绝不 pkill 全机 cloudpivot：145 上还有别人的实例）
  fuser -k "${PORT}/tcp" 2>/dev/null || true
  sleep 1
}

if [ "${1:-up}" = "down" ]; then stop; echo "stopped"; exit 0; fi

set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQL="psql -h 127.0.0.1 -U $NIK_DATABASE_USER -At"

stop
su postgres -s /bin/sh -c "psql -v ON_ERROR_STOP=1 -At -c \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$DB'\" -c \"DROP DATABASE IF EXISTS $DB\" -c \"CREATE DATABASE $DB OWNER $NIK_DATABASE_USER\"" >/dev/null || { echo "FATAL: 建库失败"; exit 1; }
echo "✅ 空库 $DB 已创建（owner $NIK_DATABASE_USER），迁移由后端首启自跑"

cd /opt/cloudpivot/backend || exit 1
export NIK_DATABASE_NAME="$DB"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_ADMIN_PASSWORD='Admin@123456'
setsid nohup "$BIN" >"$LOG" 2>&1 </dev/null &
echo $! >"$PIDF"

code=""
for _ in $(seq 1 120); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$PORT/health" || true)
  [ "$code" = "200" ] && break
  if ! kill -0 "$(cat "$PIDF")" 2>/dev/null; then echo "FATAL: 实例提前退出"; tail -40 "$LOG"; exit 1; fi
  sleep 2
done
if [ "$code" != "200" ]; then echo "FATAL: health=$code"; tail -40 "$LOG"; exit 1; fi
if grep -q '服务启动失败' "$LOG"; then echo "FATAL: 日志里有启动失败"; grep -n '服务启动失败' "$LOG" | head; exit 1; fi

echo "✅ 实例健康 health=$code port=$PORT pid=$(cat "$PIDF") log=$LOG"
echo "--- 环境指纹"
echo "git_head=$(cd /opt/cloudpivot && git rev-parse HEAD)"
echo "binary_sha256=$(sha256sum "$BIN" | cut -d' ' -f1)"
echo "binary_mtime=$(date -r "$BIN" '+%Y-%m-%d %H:%M:%S')"
echo "migrations=$($PSQL -d "$DB" -c 'select count(*) from schema_migrations where success')"
echo "health_body=$(curl -s -m 5 http://127.0.0.1:$PORT/health)"
SEED_SQL="select 'users='||(select count(*) from sys_user)||' mat='||(select count(*) from biz_material)||' wh='||(select count(*) from biz_warehouse)||' unit='||(select count(*) from biz_unit)||' role='||(select count(*) from sys_role)||' so='||(select count(*) from biz_sales_order)||' sr='||(select count(*) from biz_sales_return)"
echo "seed=$($PSQL -d "$DB" -c "$SEED_SQL")"
