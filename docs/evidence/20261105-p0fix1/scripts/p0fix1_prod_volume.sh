#!/usr/bin/env bash
# P0-FIX-1：生产库业务量体检（只读）——把 4 个提醒现在会命中多少算清楚
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot.env 2>/dev/null; set +a
export PGPASSWORD="${NIK_DATABASE_PASSWORD:-}"
Q() { psql -h 127.0.0.1 -U cloudpivot -d cloudpivot -X -q -tAF' | ' -c "$1"; }

echo "== biz_inventory 列"
Q "select string_agg(column_name, ', ' order by ordinal_position)
     from information_schema.columns
    where table_schema='public' and table_name='biz_inventory'"

echo
echo "== 4 类提醒当前命中量"
Q "select
     (select count(*) from biz_payable where deleted_at is null and status = 'OPEN'
        and unpaid_amount > 0 and due_date between current_date and current_date + 7) as payable_due_7d,
     (select count(*) from biz_purchase_order where deleted_at is null
        and status in ('APPROVED','PARTIAL') and expect_date < current_date) as po_overdue,
     (select count(*) from biz_contract where deleted_at is null and status = 'ACTIVE'
        and end_date between current_date and current_date + 30) as contract_expire_30d,
     (select count(*) from biz_inventory where qty_on_hand < safety_stock) as low_stock_qty_on_hand"

echo
echo "== 提醒收件人规模（角色里有多少启用用户）"
Q "select r.role_code, count(u.id)::text
     from sys_role r left join sys_user_role ur on ur.role_id = r.id
     left join sys_user u on u.id = ur.user_id and u.deleted_at is null and u.status = 1
    where r.deleted_at is null and r.status = 1
      and r.role_code in ('FINANCE_MANAGER','PURCHASE_MANAGER','WAREHOUSE_MANAGER')
    group by r.role_code order by r.role_code"

echo
echo "== 业务单据基数（判断这库是不是真的在生产用）"
Q "select (select count(*) from biz_purchase_order)::text || ' PO / ' ||
          (select count(*) from biz_payable)::text || ' 应付 / ' ||
          (select count(*) from biz_contract)::text || ' 合同 / ' ||
          (select count(*) from biz_material)::text || ' 物料'"
