#!/usr/bin/env bash
# 重建演练库：删库 → 建空库 → 起隔离实例跑全量迁移 → 停机（只为拿一份「head 一致」的 schema）。
#
#   PGPASSWORD=<演练库口令> M531_HOST=172.18.10.145 bash m531-reset-db.sh
#
# 更推荐直接造种子完备库（真库用例与 HTTP 冒烟都用它）：
#   ERP_DB=cloudpivot_test_<卡号> PORT=18078 bash scripts/verify/make_full_suite_db.sh
set -uo pipefail
: "${PGPASSWORD:?需要 PGPASSWORD（演练库口令）}"
HOST=${M531_HOST:-172.18.10.145}
DB=${M531_DB:-cp_m531_dev}
HERE=$(cd "$(dirname "$0")" && pwd)
export PGPASSWORD
dropdb -h "$HOST" -U cloudpivot --if-exists "$DB" || exit 1
createdb -h "$HOST" -U cloudpivot "$DB" || exit 1
M531_DB="$DB" M531_PGHOST="$HOST" M531_LISTEN=127.0.0.1:18075 \
  M531_LOG="${TMPDIR:-/tmp}/m531-instance.log" M531_PIDFILE="${TMPDIR:-/tmp}/m531-instance.pid" \
  bash "$HERE/m531-start.sh" || exit 1
for _ in $(seq 1 40); do
  grep -q "启动完成" "${TMPDIR:-/tmp}/m531-instance.log" 2>/dev/null && break
  sleep 3
done
grep -E "数据库迁移完成|启动完成" "${TMPDIR:-/tmp}/m531-instance.log" | tail -3
curl -s -o /dev/null -w "health=%{http_code}\n" http://127.0.0.1:18075/health
PID=$(cat "${TMPDIR:-/tmp}/m531-instance.pid")
kill -TERM -"$PID" 2>/dev/null || kill -TERM "$PID"
sleep 3
ss -ltnp 2>/dev/null | grep 18075 || echo "instance stopped"
psql -h "$HOST" -U cloudpivot -d "$DB" -tAc "select count(*) as migrations from schema_migrations" \
  -tAc "select count(*) as tables from information_schema.tables where table_schema='public'"
