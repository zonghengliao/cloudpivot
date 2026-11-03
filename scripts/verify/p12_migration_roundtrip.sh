#!/usr/bin/env bash
# V0210 迁移 up / down / up / down / up 三轮回放（隔离库 cp_p12）
set -uo pipefail
export PGPASSWORD=${PGPASSWORD:-alun}
DB=${P12_DB_NAME:-cp_p12}
UP=/opt/cloudpivot/backend/migrations/business/V0210__erp_business_reminder.up.sql
DOWN=/opt/cloudpivot/backend/migrations/business/V0210__erp_business_reminder.down.sql
run() { psql -h 127.0.0.1 -U alun -d "$DB" -X -q -v ON_ERROR_STOP=1 -f "$1" 2>&1 | sed 's/^/    /'; return ${PIPESTATUS[0]}; }
snapshot() {
  psql -h 127.0.0.1 -U alun -d "$DB" -X -q -tAc "
    SELECT 'templates=' || (SELECT count(*) FROM sys_notice_template WHERE template_code LIKE 'ERP_REMINDER_%')
        || ' jobs='   || (SELECT count(*) FROM sys_job WHERE job_name LIKE 'erp\_%\_scan')
        || ' menu='   || (SELECT count(*) FROM sys_menu WHERE auth_code = 'business:erp:reminder:run')
        || ' cfg='    || (SELECT count(*) FROM sys_config WHERE config_key LIKE 'erp.reminder.%' AND deleted_at IS NULL)
        || ' ledger_table=' || (SELECT count(*) FROM information_schema.tables WHERE table_name='biz_reminder_log')
        || ' uniq_idx='     || (SELECT count(*) FROM pg_indexes WHERE indexname='uk_biz_reminder_log_dedupe')"
}
fail=0
for round in 1 2; do
  echo "== 第 ${round} 轮 down"
  run "$DOWN" || fail=1
  echo "   状态：$(snapshot)"
  echo "== 第 ${round} 轮 up"
  run "$UP" || fail=1
  echo "   状态：$(snapshot)"
done
echo "== 终态核对（应 templates=4 jobs=4 menu=1 cfg=2 ledger_table=1 uniq_idx=1）"
final=$(snapshot); echo "   $final"
case "$final" in
  *"templates=4 jobs=4 menu=1 cfg=2 ledger_table=1 uniq_idx=1"*) echo "== 终态 OK" ;;
  *) echo "== 终态不符，失败"; fail=1 ;;
esac
exit $fail
