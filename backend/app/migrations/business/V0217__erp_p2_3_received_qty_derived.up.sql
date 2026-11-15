-- ============================================================================
-- V0217__erp_p2_3_received_qty_derived.up.sql
--
-- 【P2-3】biz_purchase_order_item.received_qty 双真相收敛为「单真相」。
--
-- 改之前同一件事有两个算法：
--   A. 列 `received_qty`：收货过账时 +N（stockin_purchase::add_received_qty）、
--      收货单作废时 −N（stockin_purchase::revert_received_qty）；
--   B. 「待收明细」/ 过账闸 / 判收满：`POSTED_RECEIVED_SQL` 现算
--      （只算 status='POSTED' 且未软删的收货单）。
-- 两个数字会漂（并发 / 异常回滚 / 历史数据 / 手工改库），用户会遇到
-- 「采购单详情说已收完，点『去收货』待收列表说还能收」。
--
-- 本迁移选**方案 A：列退场，只留现算**（与项目「不留缓存列」的一贯口径一致）：
--   · 删列（连同它的 CHECK ck_biz_po_item_recv）；
--   · 所有读点（采购单详情 / 待收明细 / 过账闸 / 判收满 / 联动动作）统一走
--     `purchase_model::POSTED_RECEIVED_SQL`；
--   · 唯一真相 = 已过账收货单明细，无法漂移（R1 单一数据源）。
--
-- 附加索引：`POSTED_RECEIVED_SQL` 按「采购明细行 id」聚合收货明细。明细表
-- `biz_stock_in_item` 已有 `idx_biz_si_item_src (source_item_id)`（覆盖 join 内侧），
-- 但「一张采购单的已过账收货单」这一半没有走索引的入口（原来靠 20 行的全表扫，
-- 单据量上来后变慢）。补一条**部分索引**：只收 `POSTED` 且未软删的收货单头。
--   注：卡面写的是 `biz_stock_in_item (source_item_id) WHERE deleted_at IS NULL`，
--   但该表**没有** `deleted_at`（软删在单头 `biz_stock_in` 上），照字面写会直接报
--   `column does not exist`；因此部分条件落在单头，明细侧沿用既有索引。
-- ============================================================================

-- ① 删列（先删依赖它的 CHECK：PG 的 DROP COLUMN 会连带删掉列约束，但显式写出来
--    让 down 脚本能逐字对上、也让 review 一眼看到约束没被漏掉）
ALTER TABLE biz_purchase_order_item DROP CONSTRAINT IF EXISTS ck_biz_po_item_recv;
ALTER TABLE biz_purchase_order_item DROP COLUMN IF EXISTS received_qty;

COMMENT ON TABLE biz_purchase_order_item IS
    '采购明细（已收数量 P2-3 起为派生值：只算 status=''POSTED'' 且未软删的收货单，本表不落库）';

-- ② 派生聚合的索引：按来源采购单取「已过账、未软删」的收货单头
CREATE INDEX IF NOT EXISTS idx_biz_stock_in_source_posted
    ON biz_stock_in (source_order_id)
    WHERE status = 'POSTED' AND deleted_at IS NULL;
