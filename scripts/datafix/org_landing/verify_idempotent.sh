#!/usr/bin/env bash
# ============================================================================
# ORGLANDING（P0-FIX-2B-A）· 幂等证明
# ----------------------------------------------------------------------------
# 做法：连跑两遍 apply.sh，**第二遍之后**受影响行的数据段必须与第一遍之后逐字节一致，
#       并且第二遍的 NOTICE 必须出现「本次共变更 0 行」。
#   如果第二遍又改了行，说明脚本里有「无条件 UPDATE」。
#
# 用法：ORGLANDING_DB=<演练库/目标库> bash scripts/datafix/org_landing/verify_idempotent.sh
# ============================================================================
set -euo pipefail

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

dump_affected() {  # $1 = 输出文件
    psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -X -q -At -F'|' > "$1" <<'SQL'
SELECT u.username, u.status, u.is_locked, u.user_type,
       coalesce(u.dept_id::text, 'NULL'), u.updated_at
  FROM sys_user u
 WHERE u.username IN ('admin','laoliu','laoliu_test','cl_43870')
 ORDER BY u.username;
SQL
    psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -X -q -At -F'|' >> "$1" <<'SQL'
SELECT e.pk, e.row_data::text
  FROM datafix_orglanding_snapshot e
 WHERE e.batch = 'ORGLANDING-20261105'
 ORDER BY e.pk;
SQL
}

show_notices() {  # $1 = 日志文件（只挑 ORGLANDING 的 NOTICE 打印，pipefail 下不许让 grep 的 1 变成致命错误）
    sed -n 's/^psql:[^ ]* //p' "$1" | grep -E 'NOTICE|变更' | sed -n '1,12p' || true
}

W="$(mktemp -d /tmp/orglanding-idem.XXXXXX)"

echo "== 第 1 遍 =="
FORCE=1 ORGLANDING_DB="$DB_NAME" bash "$HERE/apply.sh" > "$W/run1.log" 2>&1
show_notices "$W/run1.log"
dump_affected "$W/after1.txt"

echo "== 第 2 遍（同库再跑一次）=="
FORCE=1 ORGLANDING_DB="$DB_NAME" bash "$HERE/apply.sh" > "$W/run2.log" 2>&1
show_notices "$W/run2.log"
dump_affected "$W/after2.txt"

echo "== 对比两遍之后的受影响行 =="
if ! diff -u "$W/after1.txt" "$W/after2.txt" > "$W/diff.txt" 2>&1; then
    echo "FAIL：第二遍改变了数据，脚本不是幂等的：" >&2
    sed -n '1,40p' "$W/diff.txt" >&2
    echo "报告目录：$W" >&2
    exit 1
fi

if ! grep -q '本次共变更 0 行' "$W/run2.log"; then
    echo "FAIL：第二遍的 NOTICE 里没有「本次共变更 0 行」（幂等判据没被真实验证）：" >&2
    grep -E '变更' "$W/run2.log" >&2 || true
    echo "报告目录：$W" >&2
    exit 1
fi

echo "PASS：第二遍零改动（受影响 4 行 + 快照逐字节一致，NOTICE「本次共变更 0 行」已出现）"
echo "报告目录：$W"
