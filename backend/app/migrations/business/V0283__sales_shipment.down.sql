-- ======================================================
-- V0283 回滚：发货单两张表 + SH 取号规则
--
-- 顺序：先删明细（外键子表）再删单头；biz_seq 只删本迁移种下的那一行（按确定性 id），
-- 不动其它 seq_type（SO/PO/IN/OUT…）。
-- ⚠ 回滚会**丢单据数据**（本表是业务单据，down 仅用于迁移演练 / 隔离库），
--   生产回滚前先备份（`pg_dump -t biz_shipment -t biz_shipment_item`）。
-- ======================================================

DROP TABLE IF EXISTS biz_shipment_item;
DROP TABLE IF EXISTS biz_shipment;

DELETE FROM biz_seq
 WHERE id = 'd0283000-0000-4000-8000-000000000001'::uuid
   AND seq_type = 'SH';
