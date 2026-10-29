#!/bin/bash
# V0111 空库 up→down→up 验证（库 cloudpivot_t5mig，应用账号执行）
set -uo pipefail
cd /opt/cloudpivot/backend
set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a
export PGPASSWORD="$ALUN_DATABASE_PASSWORD"
PSQL="psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_t5mig -v ON_ERROR_STOP=1 -q"

echo "== 0) 应用启动时已把空库推进到 V0111 =="
$PSQL -Atc "SELECT 'schema_migrations 含 111 → ' || count(*) FROM schema_migrations WHERE version = '111'"
$PSQL -Atc "SELECT 'operator_type: ' || data_type || '(' || character_maximum_length || ') nullable=' || is_nullable || ' default=' || coalesce(column_default,'-') FROM information_schema.columns WHERE table_name='sys_change_log' AND column_name='operator_type'"
$PSQL -Atc "SELECT 'CHECK 约束 → ' || count(*) FROM pg_constraint WHERE conname='ck_sys_change_log_operator_type' AND conrelid='sys_change_log'::regclass"
$PSQL -Atc "SELECT '部分索引 → ' || count(*) FROM pg_indexes WHERE tablename='sys_change_log' AND indexname='idx_sys_change_log_ai_operator'"

echo
echo "== 1) down（DROP 索引 + 约束 + 列）=="
$PSQL -f migrations/business/V0111__changelog_operator_type.down.sql
$PSQL -Atc "SELECT 'down 后 operator_type 列数 → ' || count(*) FROM information_schema.columns WHERE table_name='sys_change_log' AND column_name='operator_type'"
$PSQL -Atc "SELECT 'down 后 CHECK 数 → ' || count(*) FROM pg_constraint WHERE conname='ck_sys_change_log_operator_type'"
$PSQL -Atc "SELECT 'down 后 部分索引数 → ' || count(*) FROM pg_indexes WHERE indexname='idx_sys_change_log_ai_operator'"
$PSQL -Atc "SELECT 'down 后 分区残留 operator_type 列数 → ' || count(*) FROM information_schema.columns WHERE table_name LIKE 'sys_change_log_2026%' AND column_name='operator_type'"

echo
echo "== 2) up（重新建列 + 约束 + 索引，含自检 DO 块）=="
$PSQL -f migrations/business/V0111__changelog_operator_type.up.sql
$PSQL -Atc "SELECT 'up 后 operator_type: ' || data_type || '(' || character_maximum_length || ') nullable=' || is_nullable || ' default=' || coalesce(column_default,'-') FROM information_schema.columns WHERE table_name='sys_change_log' AND column_name='operator_type'"
$PSQL -Atc "SELECT 'up 后 分区同构列数（应为分区数 3）→ ' || count(*) FROM information_schema.columns WHERE table_name LIKE 'sys_change_log_2026%' AND column_name='operator_type'"
$PSQL -Atc "SELECT 'up 后 部分索引（父表+分区）→ ' || count(*) FROM pg_indexes WHERE indexname LIKE '%ai_operator%'"

echo
echo "== 3) 幂等：再跑一次 up（不应报错，只打 NOTICE）=="
$PSQL -f migrations/business/V0111__changelog_operator_type.up.sql 2>&1 | sed 's/^/    /'

echo
echo "== 4) 默认值对历史行的语义（新列在存量行上取 HUMAN）=="
$PSQL -Atc "SELECT '存量行 operator_type 取值分布 → ' || coalesce(string_agg(operator_type || ':' || cnt, ','), '(无行)') FROM (SELECT operator_type, count(*) cnt FROM sys_change_log GROUP BY 1) t"

echo
echo "== 5) 分区表约束继承（子分区上有同名 CHECK）=="
$PSQL -Atc "SELECT '分区上的 CHECK 数 → ' || count(*) FROM pg_constraint WHERE conname='ck_sys_change_log_operator_type' AND conrelid::regclass::text LIKE 'sys_change_log_2026%'"
