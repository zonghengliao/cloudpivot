#!/usr/bin/env bash
# E2E-01 采购域全流程：只读终验快照（汇总所有本卡单据 + 勾稽 SQL + 列表接口）
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE=http://127.0.0.1:8033/api
WORK=/tmp/e2e01
[ -f "$WORK/state.sh" ] && . "$WORK/state.sh"
PSQLQ() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off "$@"; }
sec() { printf '\n===== %s =====\n' "$*"; }

sec "0. 环境"
printf 'backend=%s  db=%s  warehouse=%s(%s)\n' "$BASE" "$NIK_DATABASE_NAME" "$(PSQLQ -c "select warehouse_code from biz_warehouse where id='$WH_ID';")" "$WH_ID"

sec "1. 本卡单据清单（供应商 / 询价 / 申请 / 采购单 / 收货 / 应付 / 付款 / 退货）"
PSQLQ -c "select supplier_code||' | '||supplier_name||' | 状态='||status from biz_supplier where supplier_code like 'E2E01-%' order by supplier_code;"
echo '-- 询价单'
PSQLQ -c "select inquiry_no||' | '||status||' | 定标方式='||coalesce(award_mode,'-') from biz_price_inquiry where remark like 'E2E-01%';"
echo '-- 采购申请'
PSQLQ -c "select req_no||' | '||status||' | 合并到='||coalesce(po_no,'-') from biz_purchase_requisition where remark like 'E2E-01%' order by req_no;"
echo '-- 采购订单'
PSQLQ -c "select order_no||' | 供应商='||supplier_name_snapshot||' | '||status||' | 不含税='||total_amount||' 税='||total_tax||' 价税='||total_with_tax from biz_purchase_order where remark like '%E2E-01%' or remark like '由询价单 PIQ-20261109-000002%' or remark like '合并自 PRQ-20261109-000005%' or id='$PO_MERGED' order by order_no;"
echo '-- 收货单'
PSQLQ -c "select s.in_no||' | '||s.status||' | 数量='||s.total_qty||' 价税='||s.total_with_tax||' | 来源='||coalesce((select order_no from biz_purchase_order o where o.id=s.source_order_id),'-')||' | '||coalesce(s.remark,'') from biz_stock_in s where s.warehouse_id='$WH_ID' and s.deleted_at is null order by s.in_no;"
echo '-- 应付'
PSQLQ -c "select p.payable_no||' | '||p.status||' | 金额='||p.amount||' 已付='||p.paid_amount||' 未付='||p.unpaid_amount||' | 来源='||coalesce(s.in_no,p.source_no,'-') from biz_payable p left join biz_stock_in s on s.id=p.source_id and p.source_type='STOCK_IN' where p.deleted_at is null and (s.warehouse_id='$WH_ID' or p.id='$AP1') order by p.payable_no;"
echo '-- 付款记录'
PSQLQ -c "select payment_no||' | '||amount||' | '||coalesce(pay_method,'-')||' | 应付='||(select payable_no from biz_payable where id=payable_id) from biz_payment where payable_id='$AP1' order by payment_no;"
echo '-- 采购退货'
PSQLQ -c "select return_no||' | '||status||' | 数量='||total_qty||' 不含税='||total_amount||' 税='||total_tax||' 价税='||total_with_tax||' | 入库='||(select in_no from biz_stock_in where id=stock_in_id) from biz_purchase_return where id='$RT1';"
echo '-- 供应商应退款（付款后发生的退货）'
PSQLQ -c "select sr_no||' | '||status||' | 金额='||amount||' 未收='||unsettled_amount||' | 来源='||source_no from biz_supplier_receivable where source_id='$RT1';"

sec "2. 三级勾稽：采购订单 → 入库单 → 应付"
PSQLQ -c "select o.order_no
      ||' | 订单数量='||i.qty
      ||' | 已过账收货='||coalesce((select sum(si.qty) from biz_stock_in_item si join biz_stock_in s on s.id=si.stock_in_id where si.source_item_id=i.id and s.status='POSTED' and s.deleted_at is null),0)
      ||' | 订单价税='||o.total_with_tax
      ||' | 收货价税='||coalesce((select sum(s.total_with_tax) from biz_stock_in s where s.source_order_id=o.id and s.status='POSTED' and s.deleted_at is null),0)
      ||' | 应付='||coalesce((select sum(p.amount) from biz_payable p join biz_stock_in s on s.id=p.source_id where s.source_order_id=o.id and p.source_type='STOCK_IN' and p.deleted_at is null),0)
      ||' | 状态='||o.status
   from biz_purchase_order o join biz_purchase_order_item i on i.order_id=o.id
  where o.id in ('$PO_FROM_PIQ1','$PO_FROM_PIQ2') order by o.order_no;"
echo '-- 入库↔应付 金额不一致行数（应 0）'
PSQLQ -c "select count(*) from biz_payable p join biz_stock_in s on s.id=p.source_id where p.source_type='STOCK_IN' and p.deleted_at is null and s.deleted_at is null and s.warehouse_id='$WH_ID' and p.amount<>s.total_with_tax;"
echo '-- 应付恒等式 unpaid = amount - paid（应 0 行违反）'
PSQLQ -c "select count(*) from biz_payable where deleted_at is null and unpaid_amount <> amount - paid_amount;"

sec "3. 库存 = 流水（按物料+仓库）"
PSQLQ -c "with inv as (select material_id, warehouse_id, sum(qty) qty from biz_inventory where warehouse_id='$WH_ID' group by 1,2),
               txn as (select material_id, warehouse_id, sum(case when txn_type='IN' then qty else -qty end) qty from biz_inventory_txn where warehouse_id='$WH_ID' group by 1,2)
          select m.material_code||' 库存='||coalesce(inv.qty,0)||' 流水='||coalesce(txn.qty,0)||' 差异='||(coalesce(inv.qty,0)-coalesce(txn.qty,0))
            from inv full join txn using (material_id, warehouse_id) join biz_material m on m.id=coalesce(inv.material_id,txn.material_id) order by 1;"
echo '-- 差异行数（应 0）'
PSQLQ -c "with inv as (select material_id, warehouse_id, sum(qty) qty from biz_inventory where warehouse_id='$WH_ID' group by 1,2),
               txn as (select material_id, warehouse_id, sum(case when txn_type='IN' then qty else -qty end) qty from biz_inventory_txn where warehouse_id='$WH_ID' group by 1,2)
          select count(*) from inv full join txn using (material_id, warehouse_id) where coalesce(inv.qty,0)<>coalesce(txn.qty,0);"

sec "4. 流水明细（本仓，方向/数量/业务类型）"
PSQLQ -c "select t.txn_type||' | '||m.material_code||' | '||t.qty||' | '||t.biz_type||' | before='||t.before_qty||' after='||t.after_qty from biz_inventory_txn t join biz_material m on m.id=t.material_id where t.warehouse_id='$WH_ID' order by t.created_at;"

sec "5. 凭证与分录（本卡单据）"
PSQLQ -c "select v.voucher_no||' | '||v.status||' | '||v.summary||' | 借='||(select sum(debit_amount) from biz_voucher_entry where voucher_id=v.id)||' 贷='||(select sum(credit_amount) from biz_voucher_entry where voucher_id=v.id)
   from biz_voucher v
  where v.source_id in (select id from biz_stock_in where warehouse_id='$WH_ID')
     or v.source_id in (select stock_out_id from biz_purchase_return where id='$RT1')
     or v.source_id in (select id from biz_payment where payable_id='$AP1')
  order by v.voucher_no;"
echo '-- 分录明细'
PSQLQ -c "select v.voucher_no||' | '||e.line_no||' | '||e.account_code||' '||e.account_name||' | 借 '||e.debit_amount||' | 贷 '||e.credit_amount
   from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id
  where v.voucher_no in (select vv.voucher_no from biz_voucher vv
     where vv.source_id in (select id from biz_stock_in where warehouse_id='$WH_ID')
        or vv.source_id in (select stock_out_id from biz_purchase_return where id='$RT1')
        or vv.source_id in (select id from biz_payment where payable_id='$AP1'))
  order by v.voucher_no, e.line_no;"
echo '-- 借贷不平凭证数（应 0）'
PSQLQ -c "select count(*) from (select v.id from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id
   where v.source_id in (select id from biz_stock_in where warehouse_id='$WH_ID') or v.source_id in (select stock_out_id from biz_purchase_return where id='$RT1') or v.source_id in (select id from biz_payment where payable_id='$AP1')
   group by v.id having sum(e.debit_amount)<>sum(e.credit_amount)) t;"

sec "6. 状态机日志（收货单 B：BUG-01 打回证据）"
PSQLQ -c "select entity_no||' | '||coalesce(from_status,'-')||'→'||to_status||' | '||coalesce(left(reason,90),'') from biz_status_log where entity_id in ('$SI1','$SI2','$SIA','$SIB') order by created_at;"

sec "7. 列表接口（HTTP 状态）"
T_ADMIN=$(curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" | jq -r '.data.access_token')
for p in "/business/erp/purchase-order/page?page=1&page_size=5" \
         "/business/erp/purchase-order-item/page?page=1&page_size=5" \
         "/business/erp/stock-in/page?page=1&page_size=5" \
         "/business/erp/payable/page?page=1&page_size=5" \
         "/business/erp/purchase-return/page?page=1&page_size=5" \
         "/business/erp/purchase-requisition/page?page=1&page_size=5" \
         "/business/erp/price-inquiry/page?page=1&page_size=5" \
         "/business/erp/supplier/page?page=1&page_size=5"; do
  code=$(curl -s -o /tmp/fv.json -w '%{http_code}' "$BASE$p" -H "Authorization: Bea""rer $T_ADMIN")
  printf '%s | HTTP %s | total=%s\n' "$p" "$code" "$(jq -r '.data.total // "-"' /tmp/fv.json)"
done

sec "8. 六本账计数"
PSQLQ -c "select '库存行', count(*) from biz_inventory where warehouse_id='$WH_ID'
 union all select '流水行', count(*) from biz_inventory_txn where warehouse_id='$WH_ID'
 union all select '凭证', count(*) from biz_voucher where source_id in (select id from biz_stock_in where warehouse_id='$WH_ID') or source_id in (select stock_out_id from biz_purchase_return where id='$RT1') or source_id in (select id from biz_payment where payable_id='$AP1')
 union all select '应付', count(*) from biz_payable p join biz_stock_in s on s.id=p.source_id where s.warehouse_id='$WH_ID' and p.deleted_at is null
 union all select '付款', count(*) from biz_payment where payable_id='$AP1'
 union all select '退货', count(*) from biz_purchase_return where id='$RT1'
 union all select '状态日志', count(*) from biz_status_log where entity_id in ('$SI1','$SI2','$SIA','$SIB');"
