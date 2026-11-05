#!/usr/bin/env bash
# P0-FIX-1：生产库现状体检（只读）
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot.env 2>/dev/null; set +a
export PGPASSWORD="${NIK_DATABASE_PASSWORD:-}"
Q() { psql -h 127.0.0.1 -U cloudpivot -d cloudpivot -X -q -tAF' | ' -c "$1"; }

echo "== 库/服务"
Q "select current_database(), now()::timestamptz(0)"
systemctl is-active cloudpivot

echo
echo "== sys_job（erp_*_scan 4 行）job_name | cron | invoke_target | status | last_run_at(CST) | next_run_at(CST)"
Q "select job_name, cron_expr, invoke_target, status::text,
          coalesce(to_char(last_run_at at time zone 'Asia/Shanghai','YYYY-MM-DD HH24:MI'),'-'),
          coalesce(to_char(next_run_at at time zone 'Asia/Shanghai','YYYY-MM-DD HH24:MI'),'-')
     from sys_job where deleted_at is null and job_name like 'erp\_%\_scan' order by job_name"

echo
echo "== sys_job_log 行数 / sys_job 全部行数"
Q "select (select count(*) from sys_job_log)::text || ' / ' || (select count(*) from sys_job where deleted_at is null)::text"

echo
echo "== 现在触发会命中多少业务（不会发通知，只读）"
Q "select
     (select count(*) from biz_payable where deleted_at is null and status = 'OPEN'
        and unpaid_amount > 0 and due_date between current_date and current_date + 7) as payable_due_7d,
     (select count(*) from biz_purchase_order where deleted_at is null
        and status in ('APPROVED','PARTIAL') and expect_date < current_date) as po_overdue,
     (select count(*) from biz_contract where deleted_at is null and status = 'ACTIVE'
        and end_date between current_date and current_date + 30) as contract_expire_30d,
     (select count(*) from biz_inventory where quantity < safety_stock) as low_stock"

echo
echo "== 台账 biz_reminder_log / 通知 sys_notice 里 ERP 提醒的历史"
Q "select (select count(*) from biz_reminder_log)::text || ' / ' ||
          (select count(*) from sys_notice where biz_type in ('payable_due','po_overdue','contract_expire','low_stock'))::text"

echo
echo "== 迁移表里 V0241 及以后（有没有 V0242-V0245）"
Q "select version::text || ' ' || name from schema_migrations where version::text >= '241' order by version"
