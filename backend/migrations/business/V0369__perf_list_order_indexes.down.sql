-- ============================================================================
-- V0369 回滚：删掉两张列表页排序索引
--
-- 只删索引、不碰数据。删后 GET /business/task/page 与
-- GET /business/erp/voucher/page 会退化为「全表扫 + top-N 排序」
-- （10 万行实测 64.1 ms / 45.6 ms，见 V0369 up 的注释）。
-- ============================================================================

DROP INDEX IF EXISTS idx_biz_task_list_order;
DROP INDEX IF EXISTS idx_biz_voucher_list_order;
