-- ============================================================================
-- V0217__erp_p2_3_received_qty_derived.down.sql
--
-- 回滚：把 `received_qty` 列加回来，并按**当前唯一真相**（已过账收货单明细）
-- 回填一次，让回滚后的库回到「列 = 现算」的自洽状态（不回到改之前的漂移态：
-- 那份漂移数据没有保留价值，且本卡不允许手工改历史数据）。
--
-- ⚠️ 回填用 LEAST(sum, qty) 夹取：历史上存在「超收后作废」留下的
--    `sum(POSTED) > qty` 脏行（本库实测 0 行），若照原样写回会撞
--    ck_biz_po_item_recv 让整个 down 失败。夹取只影响回滚路径，
--    正向上线方向（up）不做任何数据修补。
-- ============================================================================

ALTER TABLE biz_purchase_order_item
    ADD COLUMN IF NOT EXISTS received_qty NUMERIC(18,4) NOT NULL DEFAULT 0;

UPDATE biz_purchase_order_item i
   SET received_qty = LEAST(
        i.qty,
        COALESCE((
            SELECT SUM(si.qty)
              FROM biz_stock_in s
              JOIN biz_stock_in_item si ON si.stock_in_id = s.id
             WHERE s.source_order_id = i.order_id
               AND si.source_item_id = i.id
               AND s.status = 'POSTED'
               AND s.deleted_at IS NULL
        ), 0)
   );

ALTER TABLE biz_purchase_order_item
    ADD CONSTRAINT ck_biz_po_item_recv CHECK (received_qty >= 0 AND received_qty <= qty);

DROP INDEX IF EXISTS idx_biz_stock_in_source_posted;

COMMENT ON TABLE biz_purchase_order_item IS '采购明细';
