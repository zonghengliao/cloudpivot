#!/usr/bin/env bash
# ============================================================================
# 通用 psql 执行器（生产 144 与本地演练库共用）
#
#   bash run_sql.sh <file.sql> [db_name]
#
# 连接解析优先级：
#   ① 环境变量 NIK_DATABASE_*（调用方显式覆盖，演练库用这个）
#   ② 存在 /etc/cloudpivot/cloudpivot.env（= 144 生产）→ source 它
#   ③ 都没设 → 本地演练默认 127.0.0.1:5432 alun/alun（99 上的测试库）
# ============================================================================
set -uo pipefail

SQL_FILE="${1:?用法: run_sql.sh <file.sql> [db_name]}"
DB_NAME="${2:-}"

ENV_FILE="${ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
if [ -z "${NIK_DATABASE_USER:-}" ] && [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    set -a; . "$ENV_FILE"; set +a
fi

export PGPASSWORD="${NIK_DATABASE_PASSWORD:-${PGPASSWORD:-alun}}"
DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
DB_USER="${NIK_DATABASE_USER:-alun}"
DB_NAME="${DB_NAME:-${NIK_DATABASE_NAME:-cloudpivot}}"

echo "---- run_sql: $SQL_FILE -> $DB_NAME @ $DB_HOST:$DB_PORT (user=$DB_USER) ----"
psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -X -v ON_ERROR_STOP=1 -f "$SQL_FILE"
