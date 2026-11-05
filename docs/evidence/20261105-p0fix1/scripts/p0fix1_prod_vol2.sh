#!/usr/bin/env bash
# P0-FIX-1：4 类提醒当前命中量（只读；列名与 reminder_stock.rs 口径一致：biz_inventory.qty）
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot.env 2>/dev/null; set +a
export PGPASSWORD="${NIK_DATABASE_PASSWORD:-}"
Q() { psql -h 127.0.0.1 -U cloudpivot -d cloudpivot -X -q -tAF' | ' -c "$1"; }

Q "select
     (select count(*) from biz_payable where deleted_at is null and status = 'OPEN'
        and unpaid_amount > 0 and due_date between current_date and current_date + 7) as payable_due_7d,
     (select count(*) from biz_purchase_order where deleted_at is null
        and status in ('APPROVED','PARTIAL') and expect_date < current_date) as po_overdue,
     (select count(*) from biz_contract where deleted_at is null and status = 'ACTIVE'
        and end_date between current_date and current_date + 30) as contract_expire_30d,
     (select count(*) from biz_inventory i join biz_material m on m.id = i.material_id
       where coalesce(m.safety_stock, 0) > 0 and i.qty < m.safety_stock) as low_stock_groups"

echo "-- 明细（低库存 / 合同到期）"
Q "select m.material_code, w.warehouse_name, i.qty::text, m.safety_stock::text
     from biz_inventory i join biz_material m on m.id = i.material_id
     join biz_warehouse w on w.id = i.warehouse_id
    where coalesce(m.safety_stock, 0) > 0 and i.qty < m.safety_stock limit 10"
Q "select contract_no, end_date::text, status from biz_contract
    where deleted_at is null and status = 'ACTIVE'
      and end_date between current_date and current_date + 30 limit 10"
Q "select payable_no, due_date::text, unpaid_amount::text, status from biz_payable
    where deleted_at is null and status = 'OPEN' and unpaid_amount > 0
      and due_date between current_date and current_date + 7 limit 10"
Q "select order_no, expect_date::text, status from biz_purchase_order
    where deleted_at is null and status in ('APPROVED','PARTIAL') and expect_date < current_date limit 10"
