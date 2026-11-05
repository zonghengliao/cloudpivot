#!/usr/bin/env bash
# ============================================================================
# ORGLANDING（P0-FIX-2B-A）· 回滚往返演练
# ----------------------------------------------------------------------------
# 判据（两个方向都要对上）：
#   ① 回滚后：4 个账号 dept_id 全 NULL（= 落位前），且**逐字节等于**快照值；
#      henry.liao / verify-ai / qa.viewer 仍无部门；sys_dept.leader_id 仍全 NULL
#   ② 再应用一遍：4 个账号回到 HQ/HQ/TECH/HQ（down → up 可往返）
#
# 用法：ORGLANDING_DB=<演练库/目标库> bash scripts/datafix/org_landing/verify_rollback.sh
# 前提：该库已经 apply 过（否则 down 是空操作，验不出东西）。
# ============================================================================
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

ENV_FILE="${CLOUDPIVOT_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    set -a; . "$ENV_FILE"; set +a
fi
DB_PASSWORD="${NIK_DATABASE_PASSWORD:-${ALUN_DATABASE_PASSWORD:-}}"
: "${DB_PASSWORD:?需要 NIK_DATABASE_PASSWORD}"
export PGPASSWORD="$DB_PASSWORD"
DB_HOST="${NIK_DATABASE_HOST:-${ALUN_DATABASE_HOST:-127.0.0.1}}"
DB_PORT="${NIK_DATABASE_PORT:-${ALUN_DATABASE_PORT:-5432}}"
DB_USER="${NIK_DATABASE_USER:-${ALUN_DATABASE_USER:-cloudpivot}}"
DB_NAME="${ORGLANDING_DB:-${NIK_DATABASE_NAME:-${ALUN_DATABASE_NAME:-cloudpivot}}}"

state() {
    psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -X -q -At -F'|' <<'SQL'
SELECT u.username || '=' || coalesce(d.dept_code, 'NULL')
  FROM sys_user u LEFT JOIN sys_dept d ON d.id = u.dept_id
 WHERE u.username IN ('admin','laoliu','laoliu_test','cl_43870','henry.liao','verify-ai','qa.viewer')
 ORDER BY u.username;
SQL
}

snapshot_diff() {  # 4 个账号与快照的差异（回滚后必须为空）
    psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -X -q -At -F'|' <<'SQL'
SELECT u.username || ' current=' || coalesce(u.dept_id::text,'NULL')
       || ' snapshot=' || coalesce((e.row_data->>'dept_id'), 'NULL')
  FROM sys_user u
  JOIN datafix_orglanding_snapshot e
    ON e.batch = 'ORGLANDING-20261105' AND e.tbl = 'sys_user' AND e.pk = u.id::text
 WHERE u.dept_id IS DISTINCT FROM (e.row_data->>'dept_id')::uuid;
SQL
}

echo "== 回滚前 =="; state
echo "== 执行回滚 =="
ORGLANDING_DB="$DB_NAME" bash "$HERE/apply.sh" --rollback 2>&1 | grep -E 'NOTICE|回滚|ERROR' | sed 's/^psql:[^ ]* //' | head -10
echo "== 回滚后 =="; state
d=$(snapshot_diff)
if [ -n "$d" ]; then
    echo "FAIL：回滚后与快照不一致："; echo "$d"; exit 1
fi
echo "PASS：回滚后 4 个账号逐字节等于快照（dept_id 全 NULL）"

echo "== 再应用一遍（证明 down → up 可往返）=="
FORCE=1 ORGLANDING_DB="$DB_NAME" bash "$HERE/apply.sh" 2>&1 | grep -E 'NOTICE|应用|ERROR' | sed 's/^psql:[^ ]* //' | head -10
echo "== 重新应用后 =="; state
landed=$(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -X -q -At -c \
  "SELECT count(*) FROM sys_user u JOIN sys_dept d ON d.id=u.dept_id
    WHERE (u.username, d.dept_code) IN (('admin','HQ'),('laoliu','HQ'),('laoliu_test','TECH'),('cl_43870','HQ'))")
if [ "$landed" != "4" ]; then
    echo "FAIL：重新应用后落位行数 = $landed（预期 4）"; exit 1
fi
echo "PASS：down → up 往返成立（4 行回到 HQ/HQ/TECH/HQ）"
