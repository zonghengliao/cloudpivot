#!/usr/bin/env bash
# SO-20 隔离实例：起一个指向一次性库 cloudpivot_so20 的后端（端口 18123），
# 跑完 curl 实测后停掉。首次启动会应用 224 个迁移（含 V0295 发票表 / V0296 权限码）。
set -uo pipefail
REPO=${REPO:-/opt/cloudpivot}
BIN=${BIN:-$REPO/backend/target/debug/cloudpivot}
DB=${DB:-cloudpivot_so20}
PORT=${PORT:-18123}
LOG=${LOG:-/root/so20-boot.log}
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
export X_CONFIG_SECRET_KEY="$(openssl rand -hex 32)"
export NIK_ADMIN_PASSWORD="${NIK_ADMIN_PASSWORD:-Admin@123456}"

: > "$LOG"
cd "$REPO/backend" || exit 1
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
PID=$!
echo "==> 启动 pid=$PID 库=$DB 端口=$PORT"

ok=0
for _ in $(seq 1 180); do
  grep -q "启动完成" "$LOG" 2>/dev/null && { ok=1; break; }
  kill -0 "$PID" 2>/dev/null || break
  sleep 1
done

echo "==> 迁移与启动日志"
grep -E "数据库迁移完成|迁移失败|启动完成|自检通过|FATAL|ERROR" "$LOG" | head -20

echo
echo "==> 迁移落库行（V0295 / V0296）"
psql -h 127.0.0.1 -U alun -d "$DB" -Atc \
  "select kind || ' | ' || version || ' | ' || name || ' | ' || applied_via || ' | success=' || success || ' | ' || execution_ms || 'ms' from schema_migrations where version in ('295','296') order by version"

echo
echo "==> GET /health ×2"
for i in 1 2; do
  code=$(curl -s -o /tmp/so20-health.json -w '%{http_code}' "http://127.0.0.1:$PORT/health")
  echo "round $i: http=$code body=$(cat /tmp/so20-health.json)"
done

echo
echo "==> PID / 端口"
echo "pid=$PID port=$PORT"
ss -lntp 2>/dev/null | grep ":$PORT " | sed 's/^/    /'
if [ "$ok" != "1" ]; then echo "!! 未启动成功（见 $LOG）" >&2; tail -30 "$LOG"; exit 1; fi
echo "==> 实例就绪"
