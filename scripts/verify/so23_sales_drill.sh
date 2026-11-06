#!/usr/bin/env bash
# =============================================================================
# SO-23 正向/负向演练：给 S1–S8 造「非空覆盖」与「逐条变红」两类证据
# =============================================================================
#   bash scripts/verify/so23_sales_drill.sh pos   # 正向：完整自洽销售链 → S1–S8 全绿
#   bash scripts/verify/so23_sales_drill.sh neg   # 负向：逐条注入违约 → S1–S8 全红
#   bash scripts/verify/so23_sales_drill.sh both  # 两个都跑（默认）
#
# 库：cloudpivot_so23pos / cloudpivot_so23neg（都从 cloudpivot_so23fresh 模板复制，
#     fresh 由 so23_fresh_db.sh up 生成 = 空库 + 225 条迁移）。
# 端口：不占端口，纯 psql + 巡检脚本；建库走本机 postgres 超级用户。
# =============================================================================
set -uo pipefail
MODE=${1:-both}
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
POS=cloudpivot_so23pos
NEG=cloudpivot_so23neg
SRC=cloudpivot_so23fresh
export PGPASSWORD=alun
PSQL="psql -h 127.0.0.1 -U alun -v ON_ERROR_STOP=1"

su_psql() { su postgres -s /bin/sh -c "psql -v ON_ERROR_STOP=1 -At $*"; }

recreate_db() {
  local db=$1
  su_psql "-c \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$db'\"" >/dev/null
  su_psql "-c 'DROP DATABASE IF EXISTS $db'" >/dev/null || exit 1
  su_psql "-c 'CREATE DATABASE $db TEMPLATE $SRC OWNER alun'" >/dev/null || exit 1
  echo "== 库 $db 已从 $SRC 复制"
}

inv() {
  local db=$1; shift
  NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 NIK_DATABASE_USER=alun \
  NIK_DATABASE_PASSWORD=alun NIK_DATABASE_NAME="$db" \
    python3 "$DIR/erp_invariants.py" "$@"
}

run_pos() {
  echo; echo "################ 正向：完整销售链 → S1–S8 必须全绿 ################"
  recreate_db "$POS"
  $PSQL -d "$POS" -f "$DIR/so23_sales_fixture.sql" >/dev/null || { echo "夹具执行失败"; exit 1; }
  inv "$POS" --group 销售
}

run_neg() {
  echo; echo "################ 负向：逐条注入违约 → S1–S8 必须全红 ################"
  recreate_db "$NEG"
  $PSQL -d "$NEG" -f "$DIR/so23_sales_fixture.sql" >/dev/null || { echo "夹具执行失败"; exit 1; }
  $PSQL -d "$NEG" -f "$DIR/so23_sales_drill.sql" >/dev/null || { echo "演练注入失败"; exit 1; }
  inv "$NEG" --group 销售
}

case "$MODE" in
  pos) run_pos ;;
  neg) run_neg ;;
  both) run_pos; run_neg ;;
  *) echo "用法：$0 {pos|neg|both}"; exit 2 ;;
esac
