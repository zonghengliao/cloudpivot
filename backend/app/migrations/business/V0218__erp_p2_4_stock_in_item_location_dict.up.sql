-- ============================================================================
-- V0218__erp_p2_4_stock_in_item_location_dict.up.sql
--
-- 【P2-4】收货明细记下「用户选的库位」—— 库位管理从摆设变回真功能。
--
-- 改之前：`biz_stock_in_item.location_id` 存的是**库位账行 id**
-- （`biz_inventory_location.id`，V0139 文件头 ① 的口径），而库位账行是
-- 「(物料, 仓库, 库位) 三元组」的库存账 —— 它只在**过账之后**才会存在。
-- 于是收货表单根本没法让用户选库位（选什么都要先有一行该物料的库存账），
-- 前端 0 处 location 字段、收货明细 0/1 填了库位，库位账退化成
-- 「一个巨大的未分配桶」（ARCH-3 §5 DC-22 实测：7/11 行是 NULL 桶）。
--
-- 本迁移只加**一列**：`location_dict_id UUID` = 用户选的库位字典 id
-- （`biz_location.id`，弱引用，与 `biz_inventory_location.location_id` 同一口径）。
--   · 建单/改单时校验它属于收货仓库、未软删、已启用（应用层）；
--   · 过账时按它 find-or-create 出库位账行，再把行 id 写回 `location_id`
--     —— 下游（成本账 / 出库 / 调拨 / 序列号）三处 `location_id` 的
--     既有语义**一个字节都不动**。
--
-- 两个 id 的分工（别混）：
--   location_dict_id = 用户意图（可以是「还没建账的库位」）
--   location_id      = 库位账行（过账后才落定，成本账与库位账按它对齐）
--
-- 回填：只回填**元数据**，把历史明细从「库位账行」反查回「库位字典」，
-- 让老单据的详情页也能显示库位编码。数量一分不动（不回填库存、不动未分配桶），
-- 符合卡面「不动未分配桶的历史数据」。
-- ============================================================================

-- ① 新列：用户选择的库位（库位字典 id，可空 = 未指定 → 默认库位 / 未分配桶）
ALTER TABLE biz_stock_in_item
    ADD COLUMN IF NOT EXISTS location_dict_id UUID;

COMMENT ON COLUMN biz_stock_in_item.location_dict_id IS
    '用户选的库位（弱引用 biz_location.id，同一收货仓库）；NULL = 未指定 → 过账时用仓库默认库位，无默认则进「未分配」桶';
COMMENT ON COLUMN biz_stock_in_item.location_id IS
    '库位账行（弱引用 biz_inventory_location.id）；P2-4 起由过账按 location_dict_id find-or-create 后回写，NULL = 未分配库位桶';

-- ② 回填：历史明细把「库位账行」翻成「库位字典」（只是补元数据，不碰数量）
UPDATE biz_stock_in_item i
   SET location_dict_id = il.location_id
  FROM biz_inventory_location il
 WHERE i.location_id IS NOT NULL
   AND i.location_dict_id IS NULL
   AND il.id = i.location_id
   AND il.location_id IS NOT NULL;

-- ③ 索引：按库位查「哪些收货行进了这个库位」（库位账明细 / 库位追溯会用到）
CREATE INDEX IF NOT EXISTS idx_biz_si_item_location_dict
    ON biz_stock_in_item (location_dict_id)
    WHERE location_dict_id IS NOT NULL;
