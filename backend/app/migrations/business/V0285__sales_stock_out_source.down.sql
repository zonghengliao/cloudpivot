-- ======================================================
-- V0285 回滚：撤掉出库单来源两列与部分唯一索引
--
-- 回滚会让「销售发货 → 出库单」的来源追溯消失（发货单侧的
-- `biz_shipment.stock_out_id` 回链仍在，单向追溯不受影响）。
-- 已过账的销售出库单本身（三本账 / 凭证 / 流水）不动 —— 回滚只删元数据列。
-- ======================================================

DROP INDEX IF EXISTS uk_biz_so_source;
ALTER TABLE biz_stock_out DROP COLUMN IF EXISTS source_id;
ALTER TABLE biz_stock_out DROP COLUMN IF EXISTS source_type;

DO $$
DECLARE
    n INTEGER;
BEGIN
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_stock_out'
       AND column_name IN ('source_type', 'source_id');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0285 回滚自检失败：biz_stock_out 仍有 % 个来源列', n;
    END IF;
    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_so_source';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0285 回滚自检失败：uk_biz_so_source 仍存在';
    END IF;
END
$$;
