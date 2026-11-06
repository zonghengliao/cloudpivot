-- V0283 行为探针（在引擎迁移出来的干净库 cloudpivot_so08 上跑，不是手灌建表）
-- 期望：坏数据全部被拒；最后清理干净（residue = 0）。
\set ON_ERROR_STOP off
\echo '=== 0) 备料：客户 + 订单 + 订单行（qty=10，delivered_qty=4）'
INSERT INTO biz_customer (id, code, name, credit_limit, status)
VALUES ('d0283aaa-0000-4000-8000-000000000001', 'SH8P-CUST', 'SO-08 探针客户', 10000, 'ACTIVE');
INSERT INTO biz_sales_order (id, so_no, customer_id, order_date)
VALUES ('d0283aaa-0000-4000-8000-000000000002', 'SH8P-SO-1',
        'd0283aaa-0000-4000-8000-000000000001', CURRENT_DATE);
INSERT INTO biz_sales_order_item (id, so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount, delivered_qty)
VALUES ('d0283aaa-0000-4000-8000-000000000003', 'd0283aaa-0000-4000-8000-000000000002', 1,
        gen_random_uuid(), 10, 100, 0, 13, 0, 4);

\echo ''
\echo '=== A) status = SHIPPED（销售订单的终态，不属于发货单） → 期望 check_violation'
INSERT INTO biz_shipment (ship_no, so_id, customer_id, warehouse_id, ship_date, status)
VALUES ('SH8P-A', 'd0283aaa-0000-4000-8000-000000000002',
        'd0283aaa-0000-4000-8000-000000000001', gen_random_uuid(), CURRENT_DATE, 'SHIPPED');

\echo ''
\echo '=== B) total_qty = -1 → 期望 check_violation'
INSERT INTO biz_shipment (ship_no, so_id, customer_id, warehouse_id, ship_date, total_qty)
VALUES ('SH8P-B', 'd0283aaa-0000-4000-8000-000000000002',
        'd0283aaa-0000-4000-8000-000000000001', gen_random_uuid(), CURRENT_DATE, -1);

\echo ''
\echo '=== C) 正常发货单（不传 status / total_qty）→ 期望 DEFAULT DRAFT / 0'
INSERT INTO biz_shipment (id, ship_no, so_id, customer_id, warehouse_id, ship_date)
VALUES ('d0283aaa-0000-4000-8000-000000000004', 'SH8P-C',
        'd0283aaa-0000-4000-8000-000000000002',
        'd0283aaa-0000-4000-8000-000000000001', gen_random_uuid(), CURRENT_DATE);
SELECT ship_no, status, total_qty FROM biz_shipment WHERE ship_no = 'SH8P-C';

\echo ''
\echo '=== D) 明细 qty = 0 → 期望 check_violation（ck_biz_shipment_item_qty）'
INSERT INTO biz_shipment_item (shipment_id, so_item_id, material_id, qty)
VALUES ('d0283aaa-0000-4000-8000-000000000004',
        'd0283aaa-0000-4000-8000-000000000003', gen_random_uuid(), 0);

\echo ''
\echo '=== E) 明细指向不存在的订单行 → 期望 foreign_key_violation'
INSERT INTO biz_shipment_item (shipment_id, so_item_id, material_id, qty)
VALUES ('d0283aaa-0000-4000-8000-000000000004',
        '00000000-0000-4000-8000-0000ffffffff'::uuid, gen_random_uuid(), 1);

\echo ''
\echo '=== F) 超发判定（shipment_model 的唯一口径）：订单 10 − 已发 4 − 未过账占用 0 = 6'
SELECT o.qty AS ordered, o.delivered_qty AS delivered,
       COALESCE((SELECT SUM(si.qty) FROM biz_shipment_item si
                   JOIN biz_shipment s ON s.id = si.shipment_id
                  WHERE si.so_item_id = o.id AND s.deleted_at IS NULL
                    AND s.status IN ('DRAFT','PENDING_POST')), 0) AS reserved,
       (o.qty - o.delivered_qty
        - COALESCE((SELECT SUM(si.qty) FROM biz_shipment_item si
                      JOIN biz_shipment s ON s.id = si.shipment_id
                     WHERE si.so_item_id = o.id AND s.deleted_at IS NULL
                       AND s.status IN ('DRAFT','PENDING_POST')), 0)) AS remaining,
       (CASE WHEN 7 <= o.qty - o.delivered_qty THEN 'ALLOW' ELSE 'BLOCKED' END) AS "qty=7",
       (CASE WHEN 6 <= o.qty - o.delivered_qty THEN 'ALLOW' ELSE 'BLOCKED' END) AS "qty=6",
       (CASE WHEN 6.0001 <= o.qty - o.delivered_qty THEN 'ALLOW' ELSE 'BLOCKED' END) AS "qty=6.0001"
  FROM biz_sales_order_item o
 WHERE o.id = 'd0283aaa-0000-4000-8000-000000000003';

\echo ''
\echo '=== G) 合法明细（发 6）→ 期望 INSERT 0 1'
INSERT INTO biz_shipment_item (shipment_id, so_item_id, material_id, qty, serial_nos)
VALUES ('d0283aaa-0000-4000-8000-000000000004',
        'd0283aaa-0000-4000-8000-000000000003', gen_random_uuid(), 6,
        ARRAY['SN-1','SN-2','SN-3','SN-4','SN-5','SN-6']);
SELECT qty, array_length(serial_nos, 1) AS serial_count
  FROM biz_shipment_item
 WHERE shipment_id = 'd0283aaa-0000-4000-8000-000000000004';

\echo ''
\echo '=== H) 被发货单引用的订单行不许删 → 期望 restrict_violation'
DELETE FROM biz_sales_order_item WHERE id = 'd0283aaa-0000-4000-8000-000000000003';

\echo ''
\echo '=== I) 存活行重号 → 期望 unique_violation（uk_biz_shipment_no）'
INSERT INTO biz_shipment (ship_no, so_id, customer_id, warehouse_id, ship_date)
VALUES ('SH8P-C', 'd0283aaa-0000-4000-8000-000000000002',
        'd0283aaa-0000-4000-8000-000000000001', gen_random_uuid(), CURRENT_DATE);

\echo ''
\echo '=== J) 软删后同号可复插 → 期望 INSERT 0 1，存活行 = 1'
UPDATE biz_shipment SET deleted_at = now() WHERE ship_no = 'SH8P-C' AND deleted_at IS NULL;
INSERT INTO biz_shipment (ship_no, so_id, customer_id, warehouse_id, ship_date)
VALUES ('SH8P-C', 'd0283aaa-0000-4000-8000-000000000002',
        'd0283aaa-0000-4000-8000-000000000001', gen_random_uuid(), CURRENT_DATE);
SELECT count(*) AS live_rows FROM biz_shipment WHERE ship_no = 'SH8P-C' AND deleted_at IS NULL;

\echo ''
\echo '=== K) 删单头 → 明细级联消失 → 期望 items_left = 0'
DELETE FROM biz_shipment WHERE so_id = 'd0283aaa-0000-4000-8000-000000000002';
SELECT count(*) AS items_left FROM biz_shipment_item;

\echo ''
\echo '=== L) 收尾：清探针（订单行 → 订单 → 客户）→ 期望 residue = 0'
DELETE FROM biz_sales_order_item WHERE so_id = 'd0283aaa-0000-4000-8000-000000000002';
DELETE FROM biz_sales_order WHERE id = 'd0283aaa-0000-4000-8000-000000000002';
DELETE FROM biz_customer WHERE code = 'SH8P-CUST';
SELECT (SELECT count(*) FROM biz_shipment) AS shipments,
       (SELECT count(*) FROM biz_shipment_item) AS shipment_items,
       (SELECT count(*) FROM biz_sales_order WHERE so_no LIKE 'SH8P-%') AS probe_orders,
       (SELECT count(*) FROM biz_customer WHERE code = 'SH8P-CUST') AS probe_customers;
