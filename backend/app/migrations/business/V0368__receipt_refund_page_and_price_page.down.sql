-- ============================================================================
-- V0368 回滚：两页收回「隐藏 + path/component 为空 + 一级菜单」，附件类型 17 → 16
--
-- 口径：回滚只关**界面可见性**，不删任何权限码（那是 V0295 / V0307 的事）。
--   ① …2950 收款退款单 → V0295 的 NULL / NULL / FALSE / parent NULL / sort 85 / title「收款退款单」
--   ② …3071 价格查询   → V0307 的 NULL / NULL / FALSE / parent NULL / sort 80 / title「价格查询」
--      （title 由 V0335 收敛过，回滚到 V0335 的值，不回 V0307 的长句 ——
--        V0335 的标题规范是独立约定，回滚本条不该把它一起退回）
--   ③ 两个附件约束收回 16 值
--
-- 安全前置：若已经存在 doc_type='RECEIPT_REFUND' 的附件（不论是否软删），
--   直接 RAISE —— 收回约束会让这些行变成「库里有、约束不认」的坏数据，
--   比起静默留下隐患，不如让运维看见「先把退款凭据处理掉再回滚」。
-- ============================================================================

-- ---------- 0) 前置：不允许在有 RECEIPT_REFUND 附件时收回约束 ----------
DO $$
DECLARE
    n integer;
BEGIN
    SELECT count(*) INTO n FROM biz_doc_attachment WHERE doc_type = 'RECEIPT_REFUND';
    IF n > 0 THEN
        RAISE EXCEPTION 'V0368 回滚中止：biz_doc_attachment 里已有 % 行 RECEIPT_REFUND 附件，先处理它们再回滚', n;
    END IF;
    SELECT count(*) INTO n FROM biz_attachment_rule WHERE doc_type = 'RECEIPT_REFUND';
    IF n > 0 THEN
        RAISE EXCEPTION 'V0368 回滚中止：biz_attachment_rule 里已有 % 行 RECEIPT_REFUND 规则，先删规则再回滚', n;
    END IF;
END $$;

-- ---------- 1) 收款退款单页还原成 V0295 的占位态 ----------
UPDATE sys_menu
   SET path       = NULL,
       component  = NULL,
       is_visible = FALSE,
       parent_id  = NULL,
       sort_order = 85,
       title      = '收款退款单',
       updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000002950'::uuid
   AND deleted_at IS NULL
   AND (path IS NOT NULL OR component IS NOT NULL OR is_visible IS DISTINCT FROM FALSE
        OR parent_id IS NOT NULL OR sort_order IS DISTINCT FROM 85);

-- ---------- 2) 价格查询页还原成 V0307 的占位态 ----------
UPDATE sys_menu
   SET path       = NULL,
       component  = NULL,
       is_visible = FALSE,
       parent_id  = NULL,
       sort_order = 80,
       title      = '价格查询',
       updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000003071'::uuid
   AND deleted_at IS NULL
   AND (path IS NOT NULL OR component IS NOT NULL OR is_visible IS DISTINCT FROM FALSE
        OR parent_id IS NOT NULL OR sort_order IS DISTINCT FROM 80);

-- ---------- 3) 附件类型 17 → 16 ----------
DO $$
DECLARE
    v_def text;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_type'
       AND conrelid = 'biz_doc_attachment'::regclass;

    IF v_def IS NOT NULL THEN
        ALTER TABLE biz_doc_attachment DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_type;
        ALTER TABLE biz_doc_attachment
            ADD CONSTRAINT ck_biz_doc_attachment_type
            CHECK (doc_type IN ('CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                                'FIXED_ASSET', 'EXPENSE_REPORT',
                                'INVOICE', 'RECEIPT', 'STOCK_TAKE', 'PURCHASE_RETURN', 'SALES_RETURN',
                                'STOCK_TRANSFER', 'SUPPLIER', 'CUSTOMER', 'OPENING_BALANCE'));
    END IF;

    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_attachment_rule_doc_type'
       AND conrelid = 'biz_attachment_rule'::regclass;

    IF v_def IS NOT NULL THEN
        ALTER TABLE biz_attachment_rule DROP CONSTRAINT IF EXISTS ck_biz_attachment_rule_doc_type;
        ALTER TABLE biz_attachment_rule
            ADD CONSTRAINT ck_biz_attachment_rule_doc_type
            CHECK (doc_type IN ('CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                                'FIXED_ASSET', 'EXPENSE_REPORT',
                                'INVOICE', 'RECEIPT', 'STOCK_TAKE', 'PURCHASE_RETURN', 'SALES_RETURN',
                                'STOCK_TRANSFER', 'SUPPLIER', 'CUSTOMER', 'OPENING_BALANCE'));
    END IF;
END $$;

COMMENT ON COLUMN biz_doc_attachment.doc_type IS
    '单据类型：CONTRACT / PURCHASE_ORDER / STOCK_IN / PAYABLE / PAYMENT / FIXED_ASSET / EXPENSE_REPORT / INVOICE / RECEIPT / STOCK_TAKE / PURCHASE_RETURN / SALES_RETURN（必传规则覆盖）/ STOCK_TRANSFER / SUPPLIER / CUSTOMER / OPENING_BALANCE（ATT-11 建议传，不校验；期初的 doc_id = 期初凭证 id）——与 attachment::model::DocKind 一一对应，V0350 放宽到 16 值';
