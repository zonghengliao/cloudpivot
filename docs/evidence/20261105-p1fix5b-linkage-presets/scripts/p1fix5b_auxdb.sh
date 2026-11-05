#!/usr/bin/env bash
# P1-FIX-5B：建两个辅助库 —— 单测库（真库用例）+ 不变量库（迁移完就停，不放夹具）。
#
#   bash p1fix5b_auxdb.sh
#
# 用法：脚本**借实例启动完成迁移**（应用启动即 `应用 N 个迁移`），迁移完立刻停进程，
# 库里只留 schema + bootstrap 种子数据（与 d3_invariants 的「干净库」同口径）。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=${REPO:-/root/lnk5b-wt}
BIN=${BIN:-/root/p1fix4-target/debug/cloudpivot}

prepare() {
  local db=$1 port=$2
  LNK_DB="$db" RECREATE=1 bash "$HERE/p1fix5b_dbsetup.sh" >/dev/null || return 1
  LNK_DB="$db" LNK_PORT="$port" LNK_LOG="/root/${db}-server.log" \
    LNK_PIDFILE="/root/${db}.pid" REPO="$REPO" BIN="$BIN" \
    bash "$HERE/p1fix5b_instance.sh" start | tail -2
  LNK_DB="$db" LNK_PORT="$port" LNK_LOG="/root/${db}-server.log" \
    LNK_PIDFILE="/root/${db}.pid" REPO="$REPO" BIN="$BIN" \
    bash "$HERE/p1fix5b_instance.sh" stop
  PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$db" -Atc \
    "select '${db}: versioned_migrations=' || count(*) from schema_migrations where kind='versioned'"
}

prepare "${LNK_UT_DB:-cloudpivot_lnk5b_ut}" "${LNK_UT_PORT:-18131}"
prepare "${LNK_INV_DB:-cloudpivot_lnk5b_inv}" "${LNK_INV_PORT:-18132}"
