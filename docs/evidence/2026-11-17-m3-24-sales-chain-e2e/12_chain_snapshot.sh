#!/usr/bin/env bash
# M3-24 全链库侧取证（跑在 M324_KEEP_FIXTURES=1 之后的保留夹具上）
#
# ⚠ 全部查询都**按本卡夹具收窄**（`M324-CUST` 客户 → 报价/订单 → 发货 → 出库 → 应收/凭证）：
#   演练库是从 `cloudpivot_test` 克隆的，里面自带演示数据（历史订单、其他演练的应收、
#   未结的审批实例…）。不收窄的话「全链自洽」那几条会被别人的数据染红，
#   看起来像本卡事故，实际是取证脚本自己没写 WHERE。
set -uo pipefail
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
DB="${1:-cp_m324_e2e_dev}"
Q() { psql -h 172.18.10.145 -U cloudpivot -d "$DB" -A -F ' | ' -c "$1"; }

# 本卡夹具的 id 集合（一条 CTE，所有小节复用同一口径）
SCOPE="WITH q AS (SELECT id, quote_no, status, converted_so_id FROM biz_quotation
                   WHERE customer_id IN (SELECT id FROM biz_customer WHERE code = 'M324-CUST') AND deleted_at IS NULL),
       o AS (SELECT b.* FROM biz_sales_order b JOIN q ON b.id = q.converted_so_id),
       s AS (SELECT b.* FROM biz_shipment b JOIN o ON b.so_id = o.id),
       so AS (SELECT b.* FROM biz_stock_out b JOIN s ON b.id = s.stock_out_id),
       ar AS (SELECT b.* FROM biz_receivable b JOIN so ON b.source_id = so.id AND b.source_type = 'SALES_OUT'),
       v AS (SELECT b.* FROM biz_voucher b JOIN so ON b.source_id = so.id AND b.source_type = 'STOCK_OUT')"

echo "== ① 单据链（报价 → 订单 → 发货 → 出库）"
Q "$SCOPE SELECT q.quote_no, q.status AS quote_status, q.converted_so_id IS NOT NULL AS has_backlink,
          o.so_no, o.status AS so_status, o.total_amount, o.total_tax, o.total_with_tax,
          s.ship_no, s.status AS ship_status, s.total_qty AS ship_qty,
          so.out_no, so.status AS out_status, so.source_type, so.out_type,
          so.total_qty AS out_qty, so.total_amount AS cost_amount, so.posted_at IS NOT NULL AS posted
     FROM q JOIN o ON o.id = q.converted_so_id JOIN s ON s.so_id = o.id JOIN so ON so.id = s.stock_out_id"

echo
echo "== ② 订单明细（delivered_qty 回写；金额 = 报价行整列搬）"
Q "$SCOPE SELECT i.line_no, m.material_code, i.qty, i.price, i.tax_rate, i.amount, i.tax_amount, i.delivered_qty
     FROM biz_sales_order_item i JOIN o ON i.so_id = o.id JOIN biz_material m ON m.id = i.material_id
    ORDER BY i.line_no"

echo
echo "== ③ 三本账 / 在途（签收后：库存 100→90 / 95，在途归零）"
Q "SELECT m.material_code,
          (SELECT SUM(qty) FROM biz_inventory i WHERE i.material_id = m.id) AS cost_qty,
          (SELECT SUM(qty) FROM biz_inventory_location l WHERE l.material_id = m.id) AS location_qty,
          (SELECT SUM(qty_in_transit) FROM biz_inventory_in_transit t WHERE t.material_id = m.id) AS transit_qty
     FROM biz_material m WHERE m.material_code IN ('M324-M1','M324-M2') ORDER BY 1"
Q "SELECT m.material_code, t.txn_type, t.biz_type, t.qty, t.before_qty, t.after_qty, t.doc_no
     FROM biz_inventory_txn t JOIN biz_material m ON m.id = t.material_id
    WHERE m.material_code IN ('M324-M1','M324-M2') ORDER BY m.material_code, t.created_at"

echo
echo "== ④ 应收（SALES_OUT，金额 = 发货明细含税 2260）"
Q "$SCOPE SELECT ar.ar_no, ar.source_type, ar.source_no, ar.amount, ar.received_amount,
          ar.unsettled_amount, ar.status, ar.period_code, ar.due_date::text
     FROM ar"

echo
echo "== ⑤ 成本结转凭证（借 6401 主营业务成本 / 贷 1405 库存商品，借贷各 150）"
Q "$SCOPE SELECT v.voucher_no, v.period, v.source_type, v.source_no, v.total_debit, v.total_credit,
          v.entry_count, v.status
     FROM v"
Q "$SCOPE SELECT e.line_no, e.account_code, e.account_name, e.debit_amount, e.credit_amount, e.summary
     FROM biz_voucher_entry e JOIN v ON v.id = e.voucher_id ORDER BY e.account_code, e.line_no"

echo
echo "== ⑥ 审批实例（两条流程都 APPROVED，无 PENDING 死单）"
Q "$SCOPE SELECT i.instance_no, i.biz_type, i.status,
          (i.status IN ('APPROVED','REJECTED','CANCELLED','WITHDRAWN')) AS settled,
          CASE WHEN i.biz_id IN (SELECT id FROM q) THEN 'QUOTATION' ELSE 'SALE_ORDER' END AS doc
     FROM biz_flow_instance i
    WHERE i.biz_id IN (SELECT id FROM q) OR i.biz_id IN (SELECT id FROM o)
    ORDER BY i.created_at"

echo
echo "== ⑦ 状态轨迹（本链四张单据，按时间序）"
Q "$SCOPE SELECT l.entity_type, l.entity_no, l.from_status, l.to_status, l.action,
          COALESCE(l.reason, '(无)') AS reason
     FROM biz_status_log l
    WHERE (l.entity_type = 'QUOTATION' AND l.entity_id IN (SELECT id FROM q))
       OR (l.entity_type = 'SALE_ORDER' AND l.entity_id IN (SELECT id FROM o))
       OR (l.entity_type = 'SHIPMENT' AND l.entity_id IN (SELECT id FROM s))
       OR (l.entity_type = 'STOCK_OUT' AND l.entity_id IN (SELECT id FROM so))
    ORDER BY l.created_at"

echo
echo "== ⑧ 全链自洽（每格都必须 = 0）"
Q "$SCOPE SELECT
     (SELECT count(*) FROM biz_flow_instance i
       WHERE i.status = 'PENDING' AND (i.biz_id IN (SELECT id FROM q) OR i.biz_id IN (SELECT id FROM o))) AS pending_instances,
     (SELECT count(*) FROM ar WHERE amount <> 2260) AS ar_amount_mismatch,
     (SELECT count(*) FROM v WHERE total_debit <> total_credit OR total_debit <> 150) AS voucher_unbalanced,
     (SELECT count(*) FROM biz_inventory_in_transit WHERE status = 'IN_TRANSIT'
        AND material_id IN (SELECT id FROM biz_material WHERE material_code IN ('M324-M1','M324-M2'))) AS open_transit,
     (SELECT count(*) FROM so WHERE source_type <> 'SALES' OR status <> 'POSTED') AS stock_out_bad,
     (SELECT count(*) FROM o WHERE status <> 'OUTBOUND') AS order_not_outbound,
     (SELECT count(*) FROM q WHERE status <> 'ACCEPTED' OR converted_so_id IS NULL) AS quote_bad,
     (SELECT count(*) FROM biz_status_log l WHERE l.entity_type = 'QUOTATION'
        AND l.entity_id NOT IN (SELECT id FROM biz_quotation)) AS quotation_orphans"
