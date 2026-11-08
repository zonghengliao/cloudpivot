#!/bin/bash
# CODE-01 在 145 上跑：迁移水位 + biz_seq 六条主数据规则（v2：补 PGPASSWORD）
set -u
set -a
. /etc/cloudpivot/cloudpivot-dev.env
set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PG="psql -h ${NIK_DATABASE_HOST:-127.0.0.1} -p ${NIK_DATABASE_PORT:-5432} -U $NIK_DATABASE_USER -d $NIK_DATABASE_NAME"

echo "== profile/db: profile=${NIK_PROFILE:-<未设置>} db=${NIK_DATABASE_NAME} user=${NIK_DATABASE_USER} listen=${NIK_SERVER_LISTEN:-?}"
echo "== 迁移水位"
$PG -Atc "select coalesce(max(version::int)::text,'(空)') from schema_migrations"
echo "== V0337 是否已应用（schema_migrations）"
$PG -Atc "select coalesce((select name||' / version='||version||' / seq='||applied_seq from schema_migrations where version='337'), '未应用')"
echo "== 六类主数据规则"
$PG -c "select seq_type, prefix, date_pattern, period_key, current_no, step, separator, status, pad_width
          from biz_seq where seq_type in ('SUP','CUS','MAT','WH','AC','ET') order by seq_type"
echo "== 汇总断言（应 6 行 / date_pattern 全 none / period_key 全 GLOBAL）"
$PG -Atc "select count(*) || ' 行, 位数=' || string_agg(seq_type||':'||pad_width::text, ',' order by seq_type)
            from biz_seq where seq_type in ('SUP','CUS','MAT','WH','AC','ET')
             and deleted_at is null and date_pattern='none' and period_key='GLOBAL' and status=1"
echo "== health"
PORT="${NIK_SERVER_LISTEN##*:}"
curl -s -o /dev/null -w "HTTP %{http_code}\n" "http://127.0.0.1:${PORT}/health"
