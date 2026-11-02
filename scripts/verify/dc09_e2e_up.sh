#!/usr/bin/env bash
# ============================================================
# DC-09 真机验收前置：起一个**只被本卡污染**的隔离实例（干净库 + 最新迁移 + 种子 admin）
#
# 用法：
#   E2E_DB=cp_units_e2e E2E_PORT=18098 bash scripts/verify/dc09_e2e_up.sh
#   # 就绪后按卡面命令跑 d4 / d7：
#   D4_BASE=http://127.0.0.1:18098/api D4_DB_NAME=cp_units_e2e D4_DB_USER=alun D4_DB_PASSWORD=alun \
#     bash scripts/verify/d4_po_to_stockin.sh
#   D7_BASE_URL=http://127.0.0.1:18098 D7_DB_NAME=cp_units_e2e D7_DB_USER=alun D7_DB_PASSWORD=alun \
#     python3 scripts/verify/d7_batch_e2e.py
#
# 收尾：bash scripts/verify/dc09_e2e_down.sh
#
# ⚠️ 两个坑（都实测过）：
#   ① 必须在 `backend/` 下起进程 —— `config/config-test.toml` 是相对路径加载的，
#      在别处起会「没有数据库配置」，进程干跑 16 行日志后 `数据库未初始化` 直接 panic；
#   ② 迁移跑完不等于就绪：bootstrap 还要种 admin（`cloudpivot` 第一条记录），
#      没等到 admin 就杀进程，后面 60 条审批流用例会集体 65018 假红。
# ============================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

DB="${E2E_DB:-cp_units_e2e}"
PORT="${E2E_PORT:-18098}"
BIN="${E2E_BIN:-/opt/cloudpivot/backend/target/debug/cloudpivot}"
LOG="/tmp/dc09_e2e_${DB}.log"

[ -x "$BIN" ] || { echo "缺少二进制：$BIN（先 cargo build）"; exit 1; }

runuser -u postgres -- psql -qc "DROP DATABASE IF EXISTS $DB"
runuser -u postgres -- psql -qc "CREATE DATABASE $DB OWNER alun" || exit 1

cd /opt/cloudpivot/backend || exit 1
NIK_PROFILE=test NIK_DATABASE_NAME="$DB" NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
NIK_SERVER_LISTEN=127.0.0.1:"$PORT" NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
  setsid "$BIN" > "$LOG" 2>&1 < /dev/null &
PID=$!
echo "$PID" > "/tmp/dc09_e2e_${DB}.pid"

CODE=""
for _ in $(seq 1 120); do
  CODE=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$PORT/health" || true)
  [ "$CODE" = "200" ] && break
  kill -0 "$PID" 2>/dev/null || { echo "INSTANCE_DIED"; tail -20 "$LOG"; exit 1; }
  sleep 2
done

ADMINS=0
for _ in $(seq 1 60); do
  ADMINS=$(runuser -u postgres -- psql -d "$DB" -tAc \
    "select count(*) from sys_user u join sys_user_role ur on ur.user_id=u.id
       join sys_role r on r.id=ur.role_id where r.role_code='SUPER_ADMIN'" 2>/dev/null)
  [ "${ADMINS:-0}" -ge 1 ] && break
  sleep 2
done

echo "db=$DB port=$PORT pid=$PID health=$CODE super_admin=$ADMINS log=$LOG"
[ "$CODE" = "200" ] && [ "${ADMINS:-0}" -ge 1 ] || { echo "NOT_READY"; exit 1; }
