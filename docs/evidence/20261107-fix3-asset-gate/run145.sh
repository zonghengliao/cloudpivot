#!/usr/bin/env bash
# FIX-3：在 145 上建全新库 + 起隔离实例（8041）+ 跑验收 + 收尾
# 前提：/opt/cloudpivot 已 pull 到本卡 commit，且已 build 出 target/debug/cloudpivot
# 用法（在 145 上）：bash /tmp/fix3/run145.sh
set -uo pipefail

DB="${DB:-cloudpivot_fix3}"
PORT="${PORT:-8041}"
ENVF=/etc/cloudpivot/cloudpivot-dev.env
WORK=/tmp/fix3
mkdir -p "$WORK/out"
cd /opt/cloudpivot/backend || exit 1

echo "=== 准备二进制（独立文件名，避免与服役实例抢同一个 inode）==="
cp -f target/debug/cloudpivot target/debug/cloudpivot-fix3 || exit 1
ls -l target/debug/cloudpivot-fix3 | sed 's/^/  /'

echo "=== 建库（不存在才建）==="
if psql -h 127.0.0.1 -U postgres -tAc "select 1 from pg_database where datname='$DB'" 2>/dev/null | grep -q 1; then
  echo "  库 $DB 已存在"
else
  su postgres -c "createdb -O cloudpivot $DB" && echo "  已建库 $DB（owner=cloudpivot）"
fi

echo "=== 起隔离实例 :$PORT ==="
pkill -f "cloudpivot-fix3" 2>/dev/null
sleep 1
set -a
# shellcheck disable=SC1090
. "$ENVF"
set +a
export NIK_DATABASE_NAME="$DB"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export CLOUDPIVOT_SEED_DEMO_USERS="${CLOUDPIVOT_SEED_DEMO_USERS:-false}"
nohup ./target/debug/cloudpivot-fix3 > "$WORK/server.log" 2>&1 &
echo $! > "$WORK/server.pid"
echo "  pid=$(cat "$WORK/server.pid") log=$WORK/server.log"

READY=0
for i in $(seq 1 90); do
  if curl -sS -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" 2>/dev/null | grep -q 200; then
    READY=1; break
  fi
  sleep 2
done
if [ "$READY" != "1" ]; then
  echo "❌ 实例没起来，最后 40 行日志："
  tail -40 "$WORK/server.log"
  exit 1
fi
echo "  ✅ /health=200（等了 $((i*2))s）"
grep -E "数据库迁移完成|启动完成" "$WORK/server.log" | tail -3 | sed 's/^/  /'
echo "  Address already in use 命中数：$(grep -c 'Address already in use' "$WORK/server.log" || true)"

echo
echo "=== 跑验收脚本 ==="
OUT="$WORK/out" DB="$DB" PORT="$PORT" bash "$WORK/verify145.sh"
RC=$?

echo
echo "=== 服务端日志里的处置闸 / 联动轨迹 ==="
grep -E "处置前置闸|跨单据联动已执行|提交后的联动动作执行结果" "$WORK/server.log" | tail -10 | sed 's/^/  /'

echo
echo "=== 收尾：停掉隔离实例（库保留，供复查）==="
kill "$(cat "$WORK/server.pid")" 2>/dev/null
sleep 1
pkill -f "cloudpivot-fix3" 2>/dev/null
echo "  已停（pid=$(cat "$WORK/server.pid")）"
exit $RC
