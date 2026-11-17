-- ============================================================================
-- V0451（down）：撤掉采购单的框架协议引用列
--
-- 数据护栏：**有单据挂了协议就拒绝回滚**。
--   这两列是「这张采购单当时按哪份框架协议下的、走的哪条协议价」的唯一记录。
--   drop column 之后，历史单再也说不清价格是怎么谈的、走了谁的额度 —— 那不是配置，
--   是账。所以先数行，非空就 RAISE EXCEPTION，让调用方先导出/归档再回滚。
--   （up 的 ④.6 已经把探针清干净，正常库这里只可能是真单据。）
--
-- 顺序：先删索引再删列（PG 会随列自动删索引，但显式写出来读得清楚）。
-- ============================================================================

DO $$
DECLARE
    v_head integer;
    v_item integer;
BEGIN
    SELECT count(*) INTO v_head FROM biz_purchase_order WHERE framework_agreement_id IS NOT NULL;
    SELECT count(*) INTO v_item FROM biz_purchase_order_item WHERE framework_agreement_item_id IS NOT NULL;
    IF v_head > 0 OR v_item > 0 THEN
        RAISE EXCEPTION 'V0451 回滚被拒绝：还有 % 张采购单挂了框架协议 / % 行明细引用了协议行 —— 请先导出或归档，再把引用清空后重试',
            v_head, v_item;
    END IF;
END $$;

DROP INDEX IF EXISTS idx_biz_poi_fwa_item;
DROP INDEX IF EXISTS idx_biz_po_fwa;

ALTER TABLE biz_purchase_order_item DROP COLUMN IF EXISTS framework_agreement_item_id;
ALTER TABLE biz_purchase_order DROP COLUMN IF EXISTS framework_agreement_no_snapshot;
ALTER TABLE biz_purchase_order DROP COLUMN IF EXISTS framework_agreement_id;
