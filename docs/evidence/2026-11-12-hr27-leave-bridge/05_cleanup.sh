#!/usr/bin/env bash
# 清理 HR-27 演练库（跑完必须执行；演练库一律建在 145，绝不在 99 上留库）。
# 用法：PGPASSWORD=... bash 05_cleanup.sh [DBNAME]
set -uo pipefail
DBHOST=${DBHOST:-172.18.10.145}
DBUSER=${DBUSER:-cloudpivot}
DB=${1:-hr27_drill}
psql -h "$DBHOST" -U "$DBUSER" -d postgres -tAc "DROP DATABASE IF EXISTS $DB" 2>&1
psql -h "$DBHOST" -U "$DBUSER" -d postgres -tAc \
    "SELECT count(*) FROM pg_database WHERE datname LIKE 'hr27%'" 2>&1
