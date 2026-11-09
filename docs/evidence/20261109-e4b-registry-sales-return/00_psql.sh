#!/usr/bin/env bash
# 在 145 上跑一段 SQL 文件：10_psql.sh <db> <sqlfile>
# 只读探针用（SELECT / 巡检），不做任何写库动作。
set -euo pipefail
DB="$1"; SQL="$2"
set -a
# shellcheck disable=SC1091
. /etc/cloudpivot/cloudpivot-dev.env
set +a
export PGPASSWORD="${NIK_DATABASE_PASSWORD}"
export PGOPTIONS='-c statement_timeout=120000'
psql -h 127.0.0.1 -p "${NIK_DATABASE_PORT:-5432}" -U "${NIK_DATABASE_USER:-cloudpivot}" \
     -d "$DB" -v ON_ERROR_STOP=1 -f "$SQL"
