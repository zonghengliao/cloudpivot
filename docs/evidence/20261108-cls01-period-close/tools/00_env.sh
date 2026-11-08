#!/usr/bin/env bash
# =============================================================================
# CLS-01 演练环境（145 测试机）：建隔离库 cloudpivot_cls01 + 起后端 127.0.0.1:18228
#   bash 00_env.sh          # 建库 + 起实例 + 打印环境指纹
#   bash 00_env.sh down     # 停实例（幂等）
# 为什么用隔离库：cloudpivot_dev 是多人共用的 145 测试库（nginx:80 → 8033），
#   本卡要封 2026 全 12 个月并对 2026 做年结（写 YEAR_END 结转凭证 + 2027 期初台账），
#   写进共用库会砸别人的期间状态与不变量巡检。
#   隔离库由空库首启自动跑完全部迁移（不克隆模板库，避开「别人改过已应用迁移 → 校验和失败」）。
# =============================================================================
set -uo pipefail
DB=${CLS01_DB:-cloudpivot_cls01}
PORT=${CLS01_PORT:-18228}
LOG=/tmp/cls01_server.log
PIDF=/tmp/cls01_server.pid
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
su postgres -s /bin/sh -c "psql -v ON_ERROR_STOP=1 -At -c \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$DB'\"" >/dev/null
su postgres -s /bin/sh -c "psql -v ON_ERROR_STOP=1 -At -c \"DROP DATABASE IF EXISTS $DB\"" >/dev/null
su postgres -s /bin/sh -c "psql -v ON_ERROR_STOP=1 -At -c \"CREATE DATABASE $DB OWNER $NIK_DATABASE_USER\"" >/dev/null || { echo "FATAL: 建库失败"; exit 1; }
echo "✅ 空库 $DB 已创建（owner $NIK_DATABASE_USER），迁移由后端首启自跑"

cd /opt/cloudpivot/backend || exit 1
export NIK_DATABASE_NAME="$DB"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_ADMIN_PASSWORD='Admin@123456'
setsid nohup "$BIN" >"$LOG" 2>&1 </dev/null &
echo $! >"$PIDF"

code=""
for _ in $(seq 1 150); do
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
SEED_SQL="select 'users='||(select count(*) from sys_user)||' sup='||(select count(*) from biz_supplier)||' cust='||(select count(*) from biz_customer)||' mat='||(select count(*) from biz_material)||' wh='||(select count(*) from biz_warehouse)||' unit='||(select count(*) from biz_unit)||' role='||(select count(*) from sys_role)||' acct='||(select count(*) from biz_account)||' period='||(select count(*) from biz_accounting_period)||' expense_type='||(select count(*) from biz_expense_type)||' asset_cat='||(select count(*) from biz_asset_category)||' dept='||(select count(*) from sys_dept)"
echo "seed=$($PSQL -d "$DB" -c "$SEED_SQL")"
echo "periods=$($PSQL -d "$DB" -c "select count(*) filter (where status='OPEN')||' open / '||count(*) from biz_accounting_period")"
