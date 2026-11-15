-- ============================================================
-- V0350 回滚：附件类型 16 → 12、文件角色 15 → 7、规则类型 16 → 12
--
-- # 降级会丢什么（写明，不静默）
--
-- `SUPPLIER` / `CUSTOMER` / `OPENING_BALANCE` / `STOCK_TRANSFER` 四类附件与
-- 8 个新角色（BIZ_LICENSE / BANK_ACCOUNT_PERMIT / QUALIFICATION / CREDIT_FILE /
-- QUOTATION / CUSTOMER_PO / TRANSFER_APPROVAL / OPENING_EVIDENCE）在旧约束里
-- **没有对应值**。降级时若库里已有这类数据，直接 RAISE 并点名条数，
-- 由运维决定「先清理还是先备份」—— 迁移不做「悄悄删数据」这种事。
--
-- # 幂等
--
-- 约束定义已收窄（不含新值）时跳过；不存在时跳过。文件内不写 BEGIN/COMMIT。
-- ============================================================

-- ============================================================
-- §1 有数据就拒绝降级（点名是哪些值、各多少条）
-- ============================================================
DO $$
DECLARE
    v_rows integer;
BEGIN
    SELECT count(*) INTO v_rows
      FROM biz_doc_attachment
     WHERE deleted_at IS NULL
       AND doc_type IN ('STOCK_TRANSFER', 'SUPPLIER', 'CUSTOMER', 'OPENING_BALANCE');
    IF v_rows > 0 THEN
        RAISE EXCEPTION 'V0350 拒绝回滚：还有 % 条 ATT-11 新增类型的附件（STOCK_TRANSFER / SUPPLIER / CUSTOMER / OPENING_BALANCE），请先处理这些附件', v_rows;
    END IF;

    SELECT count(*) INTO v_rows
      FROM biz_doc_attachment
     WHERE deleted_at IS NULL
       AND file_role IN ('BIZ_LICENSE', 'BANK_ACCOUNT_PERMIT', 'QUALIFICATION', 'CREDIT_FILE',
                         'QUOTATION', 'CUSTOMER_PO', 'TRANSFER_APPROVAL', 'OPENING_EVIDENCE');
    IF v_rows > 0 THEN
        RAISE EXCEPTION 'V0350 拒绝回滚：还有 % 条附件用了 ATT-11 新增角色，请先改角色或删除', v_rows;
    END IF;

    IF to_regclass('biz_attachment_rule') IS NOT NULL THEN
        SELECT count(*) INTO v_rows
          FROM biz_attachment_rule
         WHERE doc_type IN ('STOCK_TRANSFER', 'SUPPLIER', 'CUSTOMER', 'OPENING_BALANCE');
        IF v_rows > 0 THEN
            RAISE EXCEPTION 'V0350 拒绝回滚：还有 % 条附件规则用了 ATT-11 新增类型，请先删除规则', v_rows;
        END IF;
    END IF;
END $$;

-- ============================================================
-- §2 收回三个约束
-- ============================================================
DO $$
DECLARE
    v_def text;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_type'
       AND conrelid = 'biz_doc_attachment'::regclass;
    IF v_def IS NOT NULL AND position('OPENING_BALANCE' IN v_def) > 0 THEN
        ALTER TABLE biz_doc_attachment DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_type;
        ALTER TABLE biz_doc_attachment
            ADD CONSTRAINT ck_biz_doc_attachment_type
            CHECK (doc_type IN ('CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                                'FIXED_ASSET', 'EXPENSE_REPORT',
                                'INVOICE', 'RECEIPT', 'STOCK_TAKE', 'PURCHASE_RETURN', 'SALES_RETURN'));
        RAISE NOTICE 'V0350 down：ck_biz_doc_attachment_type 已收回 12 值';
    END IF;
END $$;

DO $$
DECLARE
    v_def text;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_role'
       AND conrelid = 'biz_doc_attachment'::regclass;
    IF v_def IS NOT NULL AND position('OPENING_EVIDENCE' IN v_def) > 0 THEN
        ALTER TABLE biz_doc_attachment DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_role;
        ALTER TABLE biz_doc_attachment
            ADD CONSTRAINT ck_biz_doc_attachment_role
            CHECK (file_role IN ('CONTRACT_SCAN', 'SUPPLEMENT', 'ACCEPTANCE',
                                 'INVOICE_SCAN', 'BANK_RECEIPT', 'SIGN_RECEIPT', 'DEFAULT'));
        RAISE NOTICE 'V0350 down：ck_biz_doc_attachment_role 已收回 7 值';
    END IF;
END $$;

DO $$
DECLARE
    v_def text;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_attachment_rule_doc_type'
       AND conrelid = 'biz_attachment_rule'::regclass;
    IF v_def IS NOT NULL AND position('OPENING_BALANCE' IN v_def) > 0 THEN
        ALTER TABLE biz_attachment_rule DROP CONSTRAINT IF EXISTS ck_biz_attachment_rule_doc_type;
        ALTER TABLE biz_attachment_rule
            ADD CONSTRAINT ck_biz_attachment_rule_doc_type
            CHECK (doc_type IN ('CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                                'FIXED_ASSET', 'EXPENSE_REPORT',
                                'INVOICE', 'RECEIPT', 'STOCK_TAKE', 'PURCHASE_RETURN', 'SALES_RETURN'));
        RAISE NOTICE 'V0350 down：ck_biz_attachment_rule_doc_type 已收回 12 值';
    END IF;
END $$;
