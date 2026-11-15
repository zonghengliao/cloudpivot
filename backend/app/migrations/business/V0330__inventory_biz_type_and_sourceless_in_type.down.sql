-- ======================================================
-- V0330 down：撤销「销售出库 biz_type / 无源入库 in_type」两处口径修正
--
-- 只回滚**本迁移**动过的东西，不删行、不动金额：
--   ① 销售出库流水 SALE_OUT → OTHER_OUT（判据仍是 doc + source_type，手工其他出库不受影响）；
--   ② 无源入库单 SOURCELESS → PURCHASE（回到 V0330 之前的「无源也记 PURCHASE」）；
--   ③ 两个 CHECK 恢复 V0145 / V0198 原文；列注释恢复为 V0137 / V0145 的写法。
--
-- 幂等：两条 UPDATE 都带值谓词（`= 'SALE_OUT'` / `= 'SOURCELESS'`），第二遍 0 行；
-- 约束是 DROP IF EXISTS + ADD。down 之后 `ck_biz_si_supplier_scope` 不允许
-- 非 PURCHASE 带供应商 —— 所以 ② 必须先于 ③ 执行（SOURCELESS 行一存在就撞约束）。
-- ======================================================

-- ---------- ② 先回填 in_type，免得撞下面的旧约束 ----------
UPDATE biz_stock_in
   SET in_type = 'PURCHASE'
 WHERE in_type = 'SOURCELESS';

-- ---------- ① 销售出库流水改回 OTHER_OUT ----------
UPDATE biz_inventory_txn t
   SET biz_type = 'OTHER_OUT'
  FROM biz_stock_out s
 WHERE t.doc_type = 'STOCK_OUT'
   AND t.doc_id = s.id
   AND s.source_type = 'SALES'
   AND t.biz_type = 'SALE_OUT';

-- ---------- ③ 约束 / 注释回退 ----------
ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_type;
ALTER TABLE biz_stock_in
    ADD CONSTRAINT ck_biz_si_type
    CHECK (in_type IN ('PURCHASE', 'RETURN', 'OTHER'));

ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_supplier_scope;
ALTER TABLE biz_stock_in
    ADD CONSTRAINT ck_biz_si_supplier_scope
    CHECK (CASE WHEN in_type = 'PURCHASE' THEN TRUE ELSE supplier_id IS NULL END);

COMMENT ON COLUMN biz_stock_in.in_type IS
    '入库类型：PURCHASE 采购入库 / RETURN 退货入库 / OTHER 其他入库';
COMMENT ON COLUMN biz_inventory_txn.biz_type IS
    '库存变动业务类型（词表 TxnBizType），见 backend/src/business/shared/doc_kind.rs';

-- ---------- ④ 反向自检 ----------
DO $$
DECLARE
    left_sourceless INT;
    left_sale_out   INT;
    in_type_check   TEXT;
BEGIN
    SELECT count(*) INTO left_sourceless FROM biz_stock_in WHERE in_type = 'SOURCELESS';
    IF left_sourceless > 0 THEN
        RAISE EXCEPTION 'V0330 down 自检失败：仍有 % 张单 in_type=SOURCELESS', left_sourceless;
    END IF;

    SELECT count(*) INTO left_sale_out
      FROM biz_inventory_txn t
      JOIN biz_stock_out s ON s.id = t.doc_id
     WHERE t.doc_type = 'STOCK_OUT' AND s.source_type = 'SALES' AND t.biz_type = 'SALE_OUT';
    IF left_sale_out > 0 THEN
        RAISE EXCEPTION 'V0330 down 自检失败：仍有 % 条销售出库流水记着 SALE_OUT', left_sale_out;
    END IF;

    SELECT pg_get_constraintdef(oid) INTO in_type_check
      FROM pg_constraint
     WHERE conname = 'ck_biz_si_type' AND conrelid = 'public.biz_stock_in'::regclass;
    IF in_type_check IS NULL OR in_type_check LIKE '%SOURCELESS%' THEN
        RAISE EXCEPTION 'V0330 down 自检失败：ck_biz_si_type 仍含 SOURCELESS（实际 %）',
            coalesce(in_type_check, '<缺失>');
    END IF;
END
$$;
