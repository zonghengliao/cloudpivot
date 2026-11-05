#!/usr/bin/env bash
# P2-1C：起一个隔离实例（拆分后二进制）并在干净库上跑 W3c 调拨 e2e
#   用法：bash /root/p21c/run_e2e_split.sh [port] [db] [bin] [tag]
set -uo pipefail
PORT=${1:-18116}
DB=${2:-cp_p21c_e2e_split}
BIN=${3:-/opt/cloudpivot/backend/target/debug/cloudpivot}
TAG=${4:-split}
REPO=/opt/cloudpivot
LOG=/root/p21c/server_e2e_${TAG}.log
PIDFILE=/root/p21c/server_e2e_${TAG}.pid

# 1) 重建库（owner=alun）
su - postgres -c "dropdb --force --if-exists $DB" >/dev/null 2>&1
su - postgres -c "createdb -O alun $DB" || { echo "createdb 失败"; exit 1; }
echo "==> 建库 $DB（owner=alun）"

# 2) 起实例（profile=test 需要 JWT secret 与存储主密钥）
export NIK_PROFILE=test
export NIK_DATABASE_NAME="$DB"
export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_USER=alun
export NIK_DATABASE_PASSWORD=alun
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export NIK_ADMIN_PASSWORD=Admin@123456

: > "$LOG"
cd "$REPO/backend" || exit 1
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
echo $! > "$PIDFILE"
ok=0
for _ in $(seq 1 240); do
  grep -q "数据库迁移完成" "$LOG" 2>/dev/null && { ok=1; break; }
  kill -0 "$(cat "$PIDFILE")" 2>/dev/null || break
  sleep 1
done
sleep 3
if [ "$ok" != "1" ]; then
  echo "!! 迁移没完成，日志尾部："; tail -20 "$LOG"; exit 1
fi
echo "==> 实例 :$PORT 就绪（pid=$(cat "$PIDFILE")，日志 $LOG）"
curl -s -o /dev/null -w "health=%{http_code}\n" "http://127.0.0.1:$PORT/health"

# 3) 跑 e2e（BOOTSTRAP=0：库与实例已就绪）
W3C_BOOTSTRAP=0 \
W3C_BASE="http://127.0.0.1:$PORT/api" \
W3C_PORT="$PORT" \
W3C_DB_NAME="$DB" \
W3C_DB_USER=alun \
W3C_DB_PASSWORD=alun \
W3C_ADMIN=admin \
W3C_ADMIN_PASSWORD=Admin@123456 \
W3C_REPO=/root/p21c-base \
  bash /root/p21c/e2e_p21c.sh > "/root/p21c/e2e_${TAG}_new.log" 2>&1
RC=$?
echo "==> e2e 退出码 = $RC"
grep -E "^\[FAIL\]|汇总" "/root/p21c/e2e_${TAG}_new.log" | tail -10
kill "$(cat "$PIDFILE")" 2>/dev/null
exit "$RC"
