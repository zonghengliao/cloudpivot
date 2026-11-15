-- ============================================================
-- V0215 回滚：撤掉采购单的信用额度评估三列 + 还原四条列注释。
--
-- 回滚后 `credit_limit` / `safety_stock` / `locked_qty` 回到「无注释」状态
-- （V0137 建列时本来就没有列注释），不是回到某个旧文案 —— 用 NULL 精确还原。
-- ============================================================

ALTER TABLE biz_purchase_order
    DROP COLUMN IF EXISTS credit_limit_snapshot,
    DROP COLUMN IF EXISTS credit_exposure,
    DROP COLUMN IF EXISTS over_credit_limit;

COMMENT ON COLUMN biz_supplier.credit_limit IS NULL;
COMMENT ON COLUMN biz_material.safety_stock IS NULL;
COMMENT ON COLUMN biz_inventory.locked_qty IS NULL;
COMMENT ON COLUMN biz_inventory_location.locked_qty IS NULL;

DO $$
DECLARE
    n_left integer;
BEGIN
    SELECT count(*) INTO n_left
      FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND table_name = 'biz_purchase_order'
       AND column_name IN ('over_credit_limit', 'credit_exposure', 'credit_limit_snapshot');
    IF n_left <> 0 THEN
        RAISE EXCEPTION 'V0215 down 自检失败：仍有 % 个信用额度列未删除', n_left;
    END IF;
    RAISE NOTICE 'V0215 down：信用额度三列已删除，列注释已还原为空';
END $$;
