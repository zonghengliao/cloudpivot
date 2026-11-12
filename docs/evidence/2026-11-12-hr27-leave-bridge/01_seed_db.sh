#!/usr/bin/env bash
# HR-27 演练库：建一个「全量迁移已应用」的真库，供
# `cargo test --lib business::hr::leave_bridge_db_tests` 使用（跑完必须 DROP）。
#
# 用法：PGPASSWORD=... bash 01_seed_db.sh [DBNAME]
#   演练库必须建在 145（172.18.10.145），不许建在 99。
set -uo pipefail

ROOT=/opt/cloudpivot
MROOT=$ROOT/backend/migrations
DBHOST=${DBHOST:-172.18.10.145}
DBUSER=${DBUSER:-cloudpivot}
DB=${1:-hr27_drill}
ERR=${TMPDIR:-/tmp}/hr27_seed_err.log

q()  { psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -tAc "$1" 2>&1; }
adm(){ psql -h "$DBHOST" -U "$DBUSER" -d postgres -tAc "$1" 2>&1; }
qf() { psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -q -X -v ON_ERROR_STOP=1 --single-transaction -f "$1" 2>&1; }

adm "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
adm "CREATE DATABASE $DB" >/dev/null 2>&1 || { echo "FAIL: 建库失败"; exit 2; }

n=0
while IFS=$' \t' read -r ver f; do
    if ! qf "$f" >/dev/null 2>"$ERR"; then
        echo "FAIL: v$ver $(basename "$f")"
        grep -E "ERROR|FATAL" "$ERR" | head -3
        exit 2
    fi
    n=$((n+1))
done < <(ls "$MROOT"/*/*.up.sql | sed -E 's#.*/V([0-9]{4})__.*#\1 &#' | sort -n)
echo "应用迁移 $n 条"
q "SELECT 'leave_flow_def='||count(*) FROM biz_flow_def WHERE flow_code='hr_leave'"
q "SELECT 'leave_table='||count(*) FROM information_schema.tables WHERE table_name='biz_leave_request'"
