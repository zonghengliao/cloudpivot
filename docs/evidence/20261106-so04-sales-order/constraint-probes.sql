-- SO-04（V0264）约束实测脚本：在真实 PG 上逐条验证「插坏数据必须被拒、插对了必须成功」
-- 用法：PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_v0264_so04 -v ON_ERROR_STOP=0 -f <本文件>
-- 判据：A/C/D/E 必须 ERROR（约束名点名），B 必须插入成功且落在事务里回滚。

\echo '=== A. 金额恒等：total_with_tax=114 而 100+13=113 → 期望 ERROR ck_sales_order_amount ==='
INSERT INTO biz_sales_order (so_no, customer_id, order_date, total_amount, total_tax, total_with_tax)
VALUES ('EVID-CK-001', gen_random_uuid(), CURRENT_DATE, 100, 13, 114);

\echo '=== B. 金额恒等：114 改 113 → 期望 INSERT 0 1（随后 ROLLBACK，不留数据）==='
BEGIN;
INSERT INTO biz_sales_order (so_no, customer_id, order_date, total_amount, total_tax, total_with_tax)
VALUES ('EVID-CK-001', gen_random_uuid(), CURRENT_DATE, 100, 13, 113);
SELECT so_no, status, total_amount, total_tax, total_with_tax FROM biz_sales_order WHERE so_no='EVID-CK-001';
ROLLBACK;

\echo '=== C. 状态域：技术架构旧稿值 SHIPPED → 期望 ERROR ck_sales_order_status ==='
INSERT INTO biz_sales_order (so_no, customer_id, order_date, status)
VALUES ('EVID-CK-002', gen_random_uuid(), CURRENT_DATE, 'SHIPPED');

\echo '=== D. 明细超发 delivered_qty(11) > qty(10) → 期望 ERROR ck_sales_order_item_delivered ==='
BEGIN;
INSERT INTO biz_sales_order (id, so_no, customer_id, order_date)
VALUES ('d0264aaa-0000-4000-8000-000000000001', 'EVID-CK-003', gen_random_uuid(), CURRENT_DATE);
INSERT INTO biz_sales_order_item (so_id, line_no, material_id, qty, delivered_qty)
VALUES ('d0264aaa-0000-4000-8000-000000000001', 1, gen_random_uuid(), 10, 11);
ROLLBACK;

\echo '=== E. 明细零数量 qty=0 → 期望 ERROR ck_sales_order_item_qty ==='
BEGIN;
INSERT INTO biz_sales_order (id, so_no, customer_id, order_date)
VALUES ('d0264aaa-0000-4000-8000-000000000002', 'EVID-CK-004', gen_random_uuid(), CURRENT_DATE);
INSERT INTO biz_sales_order_item (so_id, line_no, material_id, qty)
VALUES ('d0264aaa-0000-4000-8000-000000000002', 1, gen_random_uuid(), 0);
ROLLBACK;

\echo '=== F. 单号唯一（存活行）：同一 so_no 插两次 → 期望第 2 次 ERROR uk_biz_sales_order_no ==='
BEGIN;
INSERT INTO biz_sales_order (so_no, customer_id, order_date) VALUES ('EVID-CK-005', gen_random_uuid(), CURRENT_DATE);
INSERT INTO biz_sales_order (so_no, customer_id, order_date) VALUES ('EVID-CK-005', gen_random_uuid(), CURRENT_DATE);
ROLLBACK;

\echo '=== G. 软删后单号可复用：软删 → 再插同号 → 期望 INSERT 0 1（随后 ROLLBACK）==='
BEGIN;
INSERT INTO biz_sales_order (so_no, customer_id, order_date) VALUES ('EVID-CK-006', gen_random_uuid(), CURRENT_DATE);
UPDATE biz_sales_order SET deleted_at = now() WHERE so_no = 'EVID-CK-006';
INSERT INTO biz_sales_order (so_no, customer_id, order_date) VALUES ('EVID-CK-006', gen_random_uuid(), CURRENT_DATE);
SELECT count(*) AS live_rows_with_same_no FROM biz_sales_order WHERE so_no='EVID-CK-006' AND deleted_at IS NULL;
ROLLBACK;

\echo '=== H. 级联：删单头 → 明细随外键消失 → 期望 0 ==='
BEGIN;
INSERT INTO biz_sales_order (id, so_no, customer_id, order_date)
VALUES ('d0264aaa-0000-4000-8000-000000000003', 'EVID-CK-007', gen_random_uuid(), CURRENT_DATE);
INSERT INTO biz_sales_order_item (so_id, line_no, material_id, qty)
VALUES ('d0264aaa-0000-4000-8000-000000000003', 1, gen_random_uuid(), 3);
DELETE FROM biz_sales_order WHERE id = 'd0264aaa-0000-4000-8000-000000000003';
SELECT count(*) AS items_left FROM biz_sales_order_item WHERE so_id = 'd0264aaa-0000-4000-8000-000000000003';
ROLLBACK;

\echo '=== I. 落库总览（应只剩种子取号规则，业务表 0 行）==='
SELECT (SELECT count(*) FROM biz_sales_order) AS orders, (SELECT count(*) FROM biz_sales_order_item) AS items,
       (SELECT current_no FROM biz_seq WHERE seq_type='SO') AS so_seq_current_no,
       (SELECT period_key FROM biz_seq WHERE seq_type='SO') AS so_seq_period_key;
