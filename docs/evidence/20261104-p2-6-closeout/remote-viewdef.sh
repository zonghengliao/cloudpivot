#!/usr/bin/env bash
# 只读：导出线上 c_di18 / c_di19 视图定义里与豁免/分级相关的行，核对「线上口径 = 仓库 HEAD」
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
export PGOPTIONS='-c default_transaction_read_only=on'
PSQL="psql -X -q -h 127.0.0.1 -U $NIK_DATABASE_USER -d $NIK_DATABASE_NAME -tA"
echo "-- show default_transaction_read_only --"; $PSQL -c "show default_transaction_read_only;"
echo "-- c_di18 定义中的豁免相关行 --"
$PSQL -c "select pg_get_viewdef('di.c_di18'::regclass)" | grep -nE 'expires_on|current_date|CURRENT_DATE|waiver|已核查豁免'
echo "-- c_di19 定义中的分级相关行 --"
$PSQL -c "select pg_get_viewdef('di.c_di19'::regclass)" | grep -nE 'CASE|PENDING|INFO|WARN|biz_flow_instance'
echo "-- 与仓库 HEAD 明文对比（关键子串是否存在，大小写不敏感）--"
$PSQL -c "select 'has_ckmian(豁免): '||(position('已核查豁免#' in pg_get_viewdef('di.c_di18'::regclass))>0)::text
 ||' ; has_CURRENT_DATE: '||(position('CURRENT_DATE' in pg_get_viewdef('di.c_di18'::regclass))>0)::text
 ||' ; has_expires_on: '||(position('expires_on' in pg_get_viewdef('di.c_di18'::regclass))>0)::text
 ||' ; di19_has_PENDING: '||(position('PENDING' in pg_get_viewdef('di.c_di19'::regclass))>0)::text;"
