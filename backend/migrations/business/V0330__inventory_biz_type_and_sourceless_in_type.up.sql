-- ======================================================
-- V0330：库存流水 biz_type 记错 + 无源入库 in_type 语义（FIX-7）
--
-- 两件事，都是「落在库里的值与被引用的词表 / 单据语义对不上」：
--
-- ① 销售出库流水记成了 `OTHER_OUT`（应为 `SALE_OUT`）
--    · 销售链出库单由发货单过账自动生成：`biz_stock_out.out_type='OTHER'` +
--      `source_type='SALES'`（V0285）——四种 out_type 里没有「销售」；
--    · 过账只按 out_type 取 biz_type ⇒ 销售出库与手工「其他出库」共用一个值，
--      按 `biz_type` 统计销售出库直接归零（报表 / 追溯 / 巡检全给假答案）；
--    · 代码侧已改成 `(out_type, source_type)` 一起判（`stockout_base::biz_type_of_doc`，
--      取值来自词表 `TxnBizType::SaleOut`）→ 本迁移把**存量**改过来。
--    判定条件带 `t.biz_type = 'OTHER_OUT'`，所以重复执行「更新 0 行」，不会误伤
--    手工其他出库（它们的 doc 没有 `source_type='SALES'`）。
--
-- ② 无源入库单 `in_type` 还是 `PURCHASE`（应为 `SOURCELESS`）
--    · `is_sourceless=true` = 没有采购单（期初 / 复制 / 补录），供应商由用户显式填；
--    · 老 `create` 路径把 `'PURCHASE'` 直接拼进 SQL ⇒ 无源单自称「有采购单」，
--      与 `is_sourceless` 自相矛盾，也让「没走采购的入库」混进采购口径；
--    · 本迁移只**加宽** `ck_biz_si_type`（PURCHASE/RETURN/OTHER → +SOURCELESS），
--      并把 `ck_biz_si_supplier_scope` 改成「PURCHASE / SOURCELESS 都必须有供应商，
--      其余必须没有」——与 `ck_biz_si_supplier`（PURCHASE 必须有供应商）叠加后仍然两向。
--
-- 分界点（报表口径）：本迁移之后新开的无源单落 `SOURCELESS`；存量无源单本迁移
-- **也一并从 PURCHASE 迁到 SOURCELESS**（`is_sourceless` 位就是判据，来源无损），
-- 因此没有「迁移前 / 迁移后」两套 in_type 口径 —— 按 `in_type` 统计直接可用。
--
-- 幂等：
--   · 两条数据 UPDATE 都带值谓词（`biz_type='OTHER_OUT'` / `in_type='PURCHASE'`），
--     第二遍是 0 行；
--   · 约束一律 `DROP CONSTRAINT IF EXISTS` + `ADD CONSTRAINT`（PG 没有 ADD CONSTRAINT IF NOT EXISTS）。
--
-- 回滚（down）：约束改回原样、两类存量值改回去。**不删任何行、不动金额**。
-- ======================================================

-- ---------- ① 销售出库流水：OTHER_OUT → SALE_OUT ----------
COMMENT ON COLUMN biz_inventory_txn.biz_type IS
    '库存变动业务类型（词表 TxnBizType）。出库侧：SALE_OUT 销售出库（source_type=SALES）/ '
    'ISSUE_OUT 领用 / SCRAP_OUT 报废 / LOSS_OUT 盘亏 / OTHER_OUT 手工其他出库 / STOCK_OUT_CANCEL 出库作废';

UPDATE biz_inventory_txn t
   SET biz_type = 'SALE_OUT'
  FROM biz_stock_out s
 WHERE t.doc_type = 'STOCK_OUT'
   AND t.doc_id = s.id
   AND s.source_type = 'SALES'
   AND t.biz_type = 'OTHER_OUT';

-- ---------- ② 收货单 in_type：加 SOURCELESS 并迁存量无源单 ----------
ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_type;
ALTER TABLE biz_stock_in
    ADD CONSTRAINT ck_biz_si_type
    CHECK (in_type IN ('PURCHASE', 'RETURN', 'OTHER', 'SOURCELESS'));

-- 供应商与 in_type 的两向约束（V0198 原版：PURCHASE 才有供应商）。
-- FIX-7 起 SOURCELESS 也必须有供应商（无源 ≠ 无供货方），所以条件是 IN 而不是 =。
ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_supplier_scope;
ALTER TABLE biz_stock_in
    ADD CONSTRAINT ck_biz_si_supplier_scope
    CHECK (CASE WHEN in_type IN ('PURCHASE', 'SOURCELESS') THEN supplier_id IS NOT NULL
                ELSE supplier_id IS NULL END);

COMMENT ON COLUMN biz_stock_in.in_type IS
    '入库类型（词表 DocSubtype::IN_TYPES）：PURCHASE 采购入库（有采购单）/ RETURN 退货入库 / '
    'OTHER 其他入库（无采购单、无供应商）/ SOURCELESS 无源入库（无采购单、有供应商，is_sourceless=true）';

UPDATE biz_stock_in
   SET in_type = 'SOURCELESS'
 WHERE is_sourceless = TRUE
   AND in_type = 'PURCHASE';

-- ---------- ③ 自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    bad_sales_txn    INT;
    bad_sourceless   INT;
    in_type_check    TEXT;
    supplier_check   TEXT;
    unknown_in_type  BIGINT;
BEGIN
    -- ① 不该再有「SALES 来源的出库单 + OTHER_OUT 流水」
    SELECT count(*) INTO bad_sales_txn
      FROM biz_inventory_txn t
      JOIN biz_stock_out s ON s.id = t.doc_id
     WHERE t.doc_type = 'STOCK_OUT'
       AND s.source_type = 'SALES'
       AND t.biz_type = 'OTHER_OUT';
    IF bad_sales_txn > 0 THEN
        RAISE EXCEPTION 'V0330 自检失败：仍有 % 条销售出库流水记着 OTHER_OUT', bad_sales_txn;
    END IF;

    -- ② 不该再有「无源单 + PURCHASE」
    SELECT count(*) INTO bad_sourceless
      FROM biz_stock_in
     WHERE is_sourceless = TRUE AND in_type = 'PURCHASE';
    IF bad_sourceless > 0 THEN
        RAISE EXCEPTION 'V0330 自检失败：仍有 % 张无源入库单的 in_type 是 PURCHASE', bad_sourceless;
    END IF;

    -- 约束定义真的换掉了（读定义文本，比插探针行可靠）
    SELECT pg_get_constraintdef(oid) INTO in_type_check
      FROM pg_constraint
     WHERE conname = 'ck_biz_si_type' AND conrelid = 'public.biz_stock_in'::regclass;
    IF in_type_check IS NULL OR in_type_check NOT LIKE '%SOURCELESS%' THEN
        RAISE EXCEPTION 'V0330 自检失败：ck_biz_si_type 没包含 SOURCELESS（实际 %）',
            coalesce(in_type_check, '<缺失>');
    END IF;

    SELECT pg_get_constraintdef(oid) INTO supplier_check
      FROM pg_constraint
     WHERE conname = 'ck_biz_si_supplier_scope' AND conrelid = 'public.biz_stock_in'::regclass;
    IF supplier_check IS NULL OR supplier_check NOT LIKE '%SOURCELESS%' THEN
        RAISE EXCEPTION 'V0330 自检失败：ck_biz_si_supplier_scope 没覆盖 SOURCELESS（实际 %）',
            coalesce(supplier_check, '<缺失>');
    END IF;

    -- in_type 全域在词表内（SOURCELESS 允许出现，别的值不许）
    SELECT count(*) INTO unknown_in_type
      FROM biz_stock_in
     WHERE in_type NOT IN ('PURCHASE', 'RETURN', 'OTHER', 'SOURCELESS');
    IF unknown_in_type > 0 THEN
        RAISE EXCEPTION 'V0330 自检失败：有 % 张收货单的 in_type 不在词表内', unknown_in_type;
    END IF;
END
$$;
