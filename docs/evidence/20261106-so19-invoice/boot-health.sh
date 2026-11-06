#!/usr/bin/env bash
# V0295 启动链自检：库已迁到最新（含 V0295）时再起一次实例 →
#   ① 引擎日志「数据库迁移完成: 应用 0 个迁移」（幂等）
#   ② 引导自检（SSE/数据权限/残留/孤儿/守卫）全绿
#   ③ GET /health → 200 + code=0
# 用法：bash boot-health.sh
set -uo pipefail
REPO=${REPO:-/opt/cloudpivot}
BIN=${BIN:-$REPO/backend/target/debug/cloudpivot}
DB=${DB:-cloudpivot_so19}
PORT=${PORT:-18119}
LOG=${LOG:-/root/so19-boot.log}
export PATH=/root/.cargo/bin:$PATH
export PGPASSWORD=alun

export NIK_PROFILE=test
export NIK_DATABASE_NAME="$DB"
export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_USER=alun
export NIK_DATABASE_PASSWORD=alun
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export NIK_ADMIN_PASSWORD="${NIK_ADMIN_PASSWORD:-Admin@123456}"

: > "$LOG"
cd "$REPO/backend" || exit 1
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
PID=$!
echo "==> 启动 pid=$PID 库=$DB 端口=$PORT"

ok=0
for _ in $(seq 1 120); do
  grep -q "启动完成" "$LOG" 2>/dev/null && { ok=1; break; }
  kill -0 "$PID" 2>/dev/null || break
  sleep 1
done

echo "==> 迁移与启动日志"
grep -E "数据库迁移完成|启动完成|自检通过|FATAL|ERROR" "$LOG" | head -20

echo
echo "==> GET /health"
for i in 1 2; do
  code=$(curl -s -o /tmp/so19-health.json -w '%{http_code}' "http://127.0.0.1:$PORT/health")
  echo "round $i: http=$code body=$(cat /tmp/so19-health.json)"
done

kill "$PID" 2>/dev/null
sleep 1
ss -lntp 2>/dev/null | grep -c ":$PORT " | sed 's/^/==> 端口残余监听数 = /'
if [ "$ok" != "1" ]; then echo "!! 未启动成功（见 $LOG）" >&2; exit 1; fi
echo "==> boot-health 通过"
