#!/usr/bin/env bash
# =============================================================================
# SO-25 菜单/授权迁移应用器（隔离库 + 真后端启动，真跑迁移）
# =============================================================================
#   bash scripts/verify/so25_menu_apply.sh up     # 建隔离库 cloudpivot_so25 + 起后端 :18225（自动迁移）
#   bash scripts/verify/so25_menu_apply.sh start  # 不重建库，只重启实例（验「重启后迁移历史自洽」）
#   bash scripts/verify/so25_menu_apply.sh down   # 停实例
#
# 库：cloudpivot_so25 = 从 cloudpivot_so24（225 条迁移的干净装配库）逻辑复制
# 端口 18225 ∈ 隔离段 18000-18999（18223=SO-23 / 18224=SO-24，不撞）
#
# 为什么要真起后端：`[database.migration] auto_migrate = true` 是唯一的迁移执行路径
# （alun-db 在启动时 read_dir 扫描 backend/migrations 并按版本应用）。
# 直接 psql 灌 SQL 会绕开指纹/kind/applied_via 记账 —— 那证明不了「APPLY_OK」。
# =============================================================================
set -uo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$DIR/../.." && pwd)
DB=${SO25_DB:-cloudpivot_so25}
# 源库必须是「还没见过 V0267」的库：alun-db 会校验已应用迁移的 checksum，
# 若源库带着一份旧版 V0267 记账（例如别的验证实例在文件演进期自动套过一次），
# 复制出来的库启动即报 "migration … was modified after it was applied"。
# cloudpivot_so24（SO-24 的 e2e 库，225 条迁移、不含 sales_menu）是当前最干净的模板。
SRC=${SO25_SRC_DB:-cloudpivot_so24}
PORT=${SO25_PORT:-18225}
LOG=/tmp/so25_server.log
PIDF=/tmp/so25_server.pid
export PGPASSWORD=alun
PSQLQ() { psql -h 127.0.0.1 -U alun -d "$DB" -tAF'|' "$@"; }
psql_super() { su postgres -s /bin/sh -c "psql -v ON_ERROR_STOP=1 -At $*"; }

stop_instance() {
  [ -f "$PIDF" ] || return 0
  kill "$(cat "$PIDF")" 2>/dev/null || true
  sleep 2
  kill -9 "$(cat "$PIDF")" 2>/dev/null || true
  rm -f "$PIDF"
}

launch() {
  cd "$ROOT/backend" || exit 1
  NIK_PROFILE=test \
  NIK_DATABASE_NAME="$DB" \
  NIK_DATABASE_USER=alun \
  NIK_DATABASE_PASSWORD=alun \
  NIK_SERVER_LISTEN="127.0.0.1:$PORT" \
  NIK_ADMIN_PASSWORD='Admin@123456' \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
  CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
  setsid nohup ./target/debug/cloudpivot > "$LOG" 2>&1 < /dev/null &
  echo $! > "$PIDF"

  code=""
  for _ in $(seq 1 90); do
    code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$PORT/health" || true)
    [ "$code" = "200" ] && break
    kill -0 "$(cat "$PIDF")" 2>/dev/null || { echo "FATAL: 实例提前退出"; tail -40 "$LOG"; exit 1; }
    sleep 2
  done
  echo "health=$code pid=$(cat "$PIDF") log=$LOG"
  echo "--- 迁移记账（最近 6 条）---"
  PSQLQ -c "select version, name, kind, applied_via, success from schema_migrations order by applied_seq desc limit 6"
  echo "--- V0267 是否记账 ---"
  PSQLQ -c "select count(*) from schema_migrations where name = 'sales_menu' and success"
  echo "--- applied_via 分布（必须只有 executed/imported）---"
  PSQLQ -c "select applied_via, count(*) from schema_migrations group by 1 order by 1"
  echo "--- 本次启动应用的迁移数（重启应为 0）---"
  grep -oE "数据库迁移完成: 应用 [0-9]+ 个迁移" "$LOG" | tail -1
}

case "${1:-up}" in
up)
  stop_instance
  psql_super "-c \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$DB'\"" >/dev/null
  psql_super "-c 'DROP DATABASE IF EXISTS $DB'" >/dev/null || exit 1
  # 用逻辑复制（pg_dump|psql）而不是 CREATE DATABASE … TEMPLATE：
  # 模板库 cloudpivot_so23fresh 上常有别的验证卡实例在连（实测 10 个 idle 会话），
  # TEMPLATE 复制会报 "is being accessed by other users"，而杀别人的会话是越界操作。
  psql_super "-c 'CREATE DATABASE $DB OWNER alun'" >/dev/null || exit 1
  pg_dump -h 127.0.0.1 -U alun -d "$SRC" --no-owner --no-privileges \
    | psql -h 127.0.0.1 -U alun -d "$DB" -q -v ON_ERROR_STOP=1 >/dev/null || {
      echo "FATAL: $SRC → $DB 逻辑复制失败"; exit 1; }
  echo "✅ 库 $DB 已从 $SRC 逻辑复制（$(PSQLQ -c 'select count(*) from schema_migrations') 条迁移）"
  stale=$(PSQLQ -c "select count(*) from schema_migrations where name = 'sales_menu'")
  if [ "$stale" != "0" ]; then
    echo "FATAL: 源库 $SRC 已带 V0267(sales_menu) 记账 —— 复制出来的库会因为 checksum 不符而启动失败。"
    echo "       换一个不含 sales_menu 的源库：SO25_SRC_DB=<db> bash $0 up"
    stop_instance; exit 1
  fi
  launch
  ;;
start)
  stop_instance
  launch
  ;;
down)
  stop_instance
  echo "✅ 已停实例"
  ;;
*)
  echo "用法：$0 {up|start|down}"; exit 2
  ;;
esac
