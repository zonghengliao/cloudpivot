#!/usr/bin/env bash
# 建一个「全量迁移 + V0393 已应用」的真库，供 Rust 集成测试
# `tests/hr_26_leave_flow_seed_db.rs` 使用（跑完由调用方 DROP）。
#
# 用法：PGPASSWORD=... bash hr26_seed_db.sh [DBNAME]
set -uo pipefail

ROOT=/opt/cloudpivot
MROOT=$ROOT/backend/migrations
DBHOST=${DBHOST:-172.18.10.145}
DBUSER=${DBUSER:-cloudpivot}
DB=${1:-hr26_seed_db}

q()  { psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -tAc "$1" 2>&1; }
adm(){ psql -h "$DBHOST" -U "$DBUSER" -d postgres -tAc "$1" 2>&1; }
qf() { psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -q -X -v ON_ERROR_STOP=1 --single-transaction -f "$1" 2>&1; }

adm "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
adm "CREATE DATABASE $DB" >/dev/null 2>&1 || { echo "FAIL: 建库失败"; exit 2; }

n=0
while IFS=$' \t' read -r ver f; do
    if ! qf "$f" >/dev/null 2>/tmp/hr26_seed_err.log; then
        echo "FAIL: v$ver $(basename "$f")"; grep -E "ERROR|FATAL" /tmp/hr26_seed_err.log | head -3; exit 2
    fi
    n=$((n+1))
done < <(ls "$MROOT"/*/*.up.sql | sed -E 's#.*/V([0-9]{4})__.*#\1 &#' | sort -n)
echo "应用迁移 $n 条（含 V0393）"
q "SELECT 'hr_leave def='||count(*) FROM biz_flow_def WHERE flow_code='hr_leave'"
q "SELECT 'version='||version_no||' hash='||graph_hash FROM biz_flow_def_version WHERE id='20000000-0000-4000-8000-0000000e0394'"
