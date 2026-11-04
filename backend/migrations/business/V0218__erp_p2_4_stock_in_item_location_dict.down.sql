-- ============================================================================
-- V0218__erp_p2_4_stock_in_item_location_dict.down.sql
--
-- 回滚：把 `location_dict_id` 列与它的索引删掉。
--
-- 无数据损失：`location_id`（库位账行）在 up 之前就已经在写，up 只是**补一列**
-- 用户意图；回滚后收货单退回到「只能按库位账行选库位」的老口径，
-- 库存 / 库位账 / 流水 / 凭证一行不动（本卡不回填、不冲销任何数量）。
-- ============================================================================

DROP INDEX IF EXISTS idx_biz_si_item_location_dict;
ALTER TABLE biz_stock_in_item DROP COLUMN IF EXISTS location_dict_id;

COMMENT ON COLUMN biz_stock_in_item.location_id IS
    '库位（弱引用 biz_inventory_location.id，可空）';
