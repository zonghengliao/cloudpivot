-- ======================================================
-- V0264 回滚：销售订单两张表 + SO 取号规则
--
-- 顺序：先删明细（外键子表）再删单头；biz_seq 只删本迁移种下的那一行（按确定性 id），
-- 不动其它 seq_type（PO/IN/OUT/AR…）。
-- ⚠ 回滚会**丢单据数据**（本表是业务单据，down 仅用于迁移演练 / 隔离库），
--   生产回滚前先备份（`pg_dump -t biz_sales_order -t biz_sales_order_item`）。
-- ======================================================

DROP TABLE IF EXISTS biz_sales_order_item;
DROP TABLE IF EXISTS biz_sales_order;

DELETE FROM biz_seq
 WHERE id = 'd0264000-0000-4000-8000-000000000001'::uuid
   AND seq_type = 'SO';
