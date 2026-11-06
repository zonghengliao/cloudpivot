#!/usr/bin/env bash
# =============================================================================
# SO-23 干净库（fresh）—— 用真后端跑**全部迁移**，得到零夹具的基线库
# =============================================================================
# 为什么不用现成库：本套件（erp_invariants.py）判「账对不对」，任何 e2e 夹具残留
# 都会让 A2/B1/B4/D1/E4 常红。验收要求「全绿 + BLOCKER 0」只能在干净库上取。
#
#   bash scripts/verify/so23_fresh_db.sh up     # 建库 + 起实例跑迁移 + 等 /health + 停实例
#   bash scripts/verify/so23_fresh_db.sh check  # 只打印迁移数 / 行数
#   bash scripts/verify/so23_fresh_db.sh down   # 停实例（幂等）
#
# 端口 18223 ∈ 隔离段 18000-18999；库名 cloudpivot_so23fresh；owner = alun。
# =============================================================================
set -uo pipefail
DB=${SO23_FRESH_DB:-cloudpivot_so23fresh}
PORT=${SO23_FRESH_PORT:-18223}
LOG=/tmp/so23_fresh_server.log
PIDF=/tmp/so23_fresh_server.pid
BACKEND=/opt/cloudpivot/backend

say() { printf '%s\n' "$*"; }
psql_super() { su postgres -s /bin/sh -c "psql -v ON_ERROR_STOP=1 -At $*"; }

stop_instance() {
  kill "$(cat "$PIDF" 2>/dev/null)" 2>/dev/null || true
  sleep 2
  kill -9 "$(cat "$PIDF" 2>/dev/null)" 2>/dev/null || true
  rm -f "$PIDF"
}

case "${1:-up}" in
  up)
    stop_instance
    exists=$(psql_super "-c \"select 1 from pg_database where datname='$DB'\"" | tr -d '[:space:]')
    if [ "$exists" = "1" ]; then
      psql_super "-c \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$DB'\"" >/dev/null
      psql_super "-c 'DROP DATABASE $DB'" >/dev/null || exit 1
    fi
    psql_super "-c 'CREATE DATABASE $DB OWNER alun'" >/dev/null || exit 1
    say "✅ 空库 $DB 建好（owner=alun）"

    cd "$BACKEND" || exit 1
    NIK_PROFILE=test \
    NIK_DATABASE_NAME="$DB" \
    NIK_DATABASE_USER=alun \
    NIK_DATABASE_PASSWORD=alun \
    NIK_SERVER_LISTEN=127.0.0.1:$PORT \
    NIK_ADMIN_PASSWORD='Admin@123456' \
    NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
    CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
    setsid nohup ./target/debug/cloudpivot > "$LOG" 2>&1 < /dev/null &
    echo $! > "$PIDF"

    code=""
    for _ in $(seq 1 90); do
      code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$PORT/health" || true)
      [ "$code" = "200" ] && break
      if ! kill -0 "$(cat "$PIDF")" 2>/dev/null; then
        say "FATAL: 实例提前退出"; tail -30 "$LOG"; exit 1
      fi
      sleep 2
    done
    say "health=$code pid=$(cat "$PIDF")"
    grep -E "数据库迁移完成|应用 [0-9]+ 个迁移" "$LOG" | tail -2
    stop_instance
    export PGPASSWORD=alun
    psql -h 127.0.0.1 -U alun -d "$DB" -tAF'|' -c \
      "select count(*)||' migrations, applied_via 非 executed/imported: '||
              (select count(*) from schema_migrations where applied_via not in ('executed','imported'))
         from schema_migrations"
    ;;
  check)
    export PGPASSWORD=alun
    psql -h 127.0.0.1 -U alun -d "$DB" -tAF'|' -c \
      "select count(*) as migrations from schema_migrations"
    psql -h 127.0.0.1 -U alun -d "$DB" -tAF'|' -c \
      "select (select count(*) from biz_sales_order) so,(select count(*) from biz_shipment) sh,
              (select count(*) from biz_stock_out) out_doc,(select count(*) from biz_receivable) ar,
              (select count(*) from biz_receipt) rc,(select count(*) from biz_invoice) iv,
              (select count(*) from biz_voucher) v"
    ;;
  down) stop_instance; say "stopped" ;;
  *) say "用法：$0 {up|check|down}"; exit 2 ;;
esac
