-- ============================================================
-- V0350：附件类型 12 → 16 + 文件角色 7 → 15（ATT-11「建议传」的 4 类单据）
--
-- # 这条迁移补的是什么
--
-- ATT-11 要给 4 类**建议传（不校验，只是能传）**的对象加附件区：
--
--   STOCK_TRANSFER    库存调拨单   —— 调拨审批单
--   SUPPLIER          供应商（主数据）—— 营业执照 / 开户许可 / 资质证书
--   CUSTOMER          客户（主数据）  —— 营业执照 / 信用资料
--   OPENING_BALANCE   期初余额      —— 期初依据（doc_id = 期初凭证 id）
--
-- 而 `V0347` 建好的 `ck_biz_doc_attachment_type` 只允许 12 个值、`V0345` 的
-- `ck_biz_doc_attachment_role` 只允许 7 个值 —— 不放开约束，上传会直接撞 23514
-- 变成 500（与 ATT-04 / ATT-06 两次踩的是同一个坑：**界面词表 ≠ 数据库事实**）。
--
-- 同时放开规则表的 `ck_biz_attachment_rule_doc_type`（`biz_attachment_rule.doc_type`）：
-- 附件规则管理界面（ATT-09）的单据类型下拉来自 `DocKind::ALL`，16 类都能选；
-- 约束不收宽的话「选了供应商 → 保存 400」——界面能选、后端 400 是最糟的一种漂移。
--
-- # 为什么期初余额的 doc_type 不是 VOUCHER
--
-- 期初没有单头行（`biz_opening_balance` 一个科目一行、保存即整期覆盖），
-- `doc_id` 指向该期间的**期初凭证**（`voucher_type='OPENING'`，V0189 唯一索引兜死）。
-- 用 `VOUCHER` 会让「期初依据」挂到任意凭证上；用 `OPENING_BALANCE` 才能在
-- `biz_doc_attachment.doc_type` 上说清「这份依据属于哪一期期初」。
--
-- # 幂等
--
--   · 三个约束都先看 `pg_get_constraintdef`：**已含新值就跳过**（不 DROP 再 ADD，
--     避免每次重放全表校验）。
--   · `COMMENT ON COLUMN` 天然幂等。
--   · 文件内**不写** BEGIN/COMMIT（迁移器自己在事务里跑）。
--
-- # 回滚
--
-- `V0350__attachment_advice_doc_types.down.sql`：把三个约束收回 12 / 7 值。
-- 已有新类型的附件 / 规则时主动 RAISE（不静默删数据）。
-- ============================================================

-- ============================================================
-- §1 biz_doc_attachment.doc_type：12 → 16
-- ============================================================
DO $$
DECLARE
    v_def text;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_type'
       AND conrelid = 'biz_doc_attachment'::regclass;

    IF v_def IS NULL OR position('OPENING_BALANCE' IN v_def) = 0 THEN
        ALTER TABLE biz_doc_attachment DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_type;
        ALTER TABLE biz_doc_attachment
            ADD CONSTRAINT ck_biz_doc_attachment_type
            CHECK (doc_type IN ('CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                                'FIXED_ASSET', 'EXPENSE_REPORT',
                                'INVOICE', 'RECEIPT', 'STOCK_TAKE', 'PURCHASE_RETURN', 'SALES_RETURN',
                                'STOCK_TRANSFER', 'SUPPLIER', 'CUSTOMER', 'OPENING_BALANCE'));
        RAISE NOTICE 'V0350：ck_biz_doc_attachment_type 已放宽到 16 值（旧定义 %）', coalesce(v_def, '<不存在>');
    ELSE
        RAISE NOTICE 'V0350：ck_biz_doc_attachment_type 已是 16 值，跳过';
    END IF;
END $$;

COMMENT ON COLUMN biz_doc_attachment.doc_type IS
    '单据类型：CONTRACT / PURCHASE_ORDER / STOCK_IN / PAYABLE / PAYMENT / FIXED_ASSET / EXPENSE_REPORT / INVOICE / RECEIPT / STOCK_TAKE / PURCHASE_RETURN / SALES_RETURN（必传规则覆盖）/ STOCK_TRANSFER / SUPPLIER / CUSTOMER / OPENING_BALANCE（ATT-11 建议传，不校验；期初的 doc_id = 期初凭证 id）——与 attachment::model::DocKind 一一对应，V0350 放宽到 16 值';

-- ============================================================
-- §2 biz_attachment_rule.doc_type：12 → 16（规则管理界面可选 = 可存）
-- ============================================================
DO $$
DECLARE
    v_def text;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_attachment_rule_doc_type'
       AND conrelid = 'biz_attachment_rule'::regclass;

    -- 表不存在（老库没跑过 V0347）时本段直接跳过：附件规则是 ATT-06 之后才有的能力
    IF v_def IS NULL THEN
        RAISE NOTICE 'V0350：biz_attachment_rule 或它的 doc_type 约束不存在，跳过';
    ELSIF position('OPENING_BALANCE' IN v_def) = 0 THEN
        ALTER TABLE biz_attachment_rule DROP CONSTRAINT IF EXISTS ck_biz_attachment_rule_doc_type;
        ALTER TABLE biz_attachment_rule
            ADD CONSTRAINT ck_biz_attachment_rule_doc_type
            CHECK (doc_type IN ('CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                                'FIXED_ASSET', 'EXPENSE_REPORT',
                                'INVOICE', 'RECEIPT', 'STOCK_TAKE', 'PURCHASE_RETURN', 'SALES_RETURN',
                                'STOCK_TRANSFER', 'SUPPLIER', 'CUSTOMER', 'OPENING_BALANCE'));
        RAISE NOTICE 'V0350：ck_biz_attachment_rule_doc_type 已放宽到 16 值';
    ELSE
        RAISE NOTICE 'V0350：ck_biz_attachment_rule_doc_type 已是 16 值，跳过';
    END IF;
END $$;

-- ============================================================
-- §3 biz_doc_attachment.file_role：7 → 15
--
-- 新增 8 个业务角色（ATT-11 卡面词表）：
--   BIZ_LICENSE         营业执照      （供应商 / 客户）
--   BANK_ACCOUNT_PERMIT 开户许可      （供应商）
--   QUALIFICATION       资质证书      （供应商）
--   CREDIT_FILE         信用资料      （客户）
--   QUOTATION           报价单        （采购订单）
--   CUSTOMER_PO         客户PO        （采购订单）
--   TRANSFER_APPROVAL   调拨审批单    （调拨单）
--   OPENING_EVIDENCE    期初依据      （期初余额）
--
-- 老值 SIGN_RECEIPT / 归一后的 INVOICE_SCAN / DEFAULT 全部保留（既有数据照常读）。
-- ============================================================
DO $$
DECLARE
    v_def text;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_role'
       AND conrelid = 'biz_doc_attachment'::regclass;

    IF v_def IS NULL OR position('OPENING_EVIDENCE' IN v_def) = 0 THEN
        ALTER TABLE biz_doc_attachment DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_role;
        ALTER TABLE biz_doc_attachment
            ADD CONSTRAINT ck_biz_doc_attachment_role
            CHECK (file_role IN ('CONTRACT_SCAN', 'SUPPLEMENT', 'ACCEPTANCE',
                                 'INVOICE_SCAN', 'BANK_RECEIPT', 'SIGN_RECEIPT',
                                 'BIZ_LICENSE', 'BANK_ACCOUNT_PERMIT', 'QUALIFICATION',
                                 'CREDIT_FILE', 'QUOTATION', 'CUSTOMER_PO',
                                 'TRANSFER_APPROVAL', 'OPENING_EVIDENCE',
                                 'DEFAULT'));
        RAISE NOTICE 'V0350：ck_biz_doc_attachment_role 已重建为 15 值（旧定义 %）', coalesce(v_def, '<不存在>');
    ELSE
        RAISE NOTICE 'V0350：ck_biz_doc_attachment_role 已是 15 值，跳过';
    END IF;
END $$;

COMMENT ON COLUMN biz_doc_attachment.file_role IS
    '文件角色（分栏口径）：CONTRACT_SCAN / SUPPLEMENT / ACCEPTANCE / INVOICE_SCAN / BANK_RECEIPT / SIGN_RECEIPT（老值兼容）/ BIZ_LICENSE / BANK_ACCOUNT_PERMIT / QUALIFICATION / CREDIT_FILE / QUOTATION / CUSTOMER_PO / TRANSFER_APPROVAL / OPENING_EVIDENCE / DEFAULT（V0350 放宽到 15 值）';

-- ============================================================
-- §4 自检（up 后必须全为真）
-- ============================================================
DO $$
DECLARE
    v_type text;
    v_role text;
    v_rule text;
    n_type integer;
    n_role integer;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO v_type
      FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_type'
       AND conrelid = 'biz_doc_attachment'::regclass;
    IF v_type IS NULL THEN
        RAISE EXCEPTION 'V0350 自检失败：ck_biz_doc_attachment_type 不存在';
    END IF;

    SELECT count(*) INTO n_type
      FROM unnest(ARRAY['STOCK_TRANSFER', 'SUPPLIER', 'CUSTOMER', 'OPENING_BALANCE']) AS v(code)
     WHERE position(v.code IN v_type) > 0;
    IF n_type <> 4 THEN
        RAISE EXCEPTION 'V0350 自检失败：附件类型约束里只找到 % 个新值（应 4）：%', n_type, v_type;
    END IF;

    SELECT pg_get_constraintdef(oid) INTO v_role
      FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_role'
       AND conrelid = 'biz_doc_attachment'::regclass;
    IF v_role IS NULL THEN
        RAISE EXCEPTION 'V0350 自检失败：ck_biz_doc_attachment_role 不存在';
    END IF;

    SELECT count(*) INTO n_role
      FROM unnest(ARRAY['BIZ_LICENSE', 'BANK_ACCOUNT_PERMIT', 'QUALIFICATION', 'CREDIT_FILE',
                        'QUOTATION', 'CUSTOMER_PO', 'TRANSFER_APPROVAL', 'OPENING_EVIDENCE']) AS v(code)
     WHERE position(v.code IN v_role) > 0;
    IF n_role <> 8 THEN
        RAISE EXCEPTION 'V0350 自检失败：角色约束里只找到 % 个新值（应 8）：%', n_role, v_role;
    END IF;

    SELECT pg_get_constraintdef(oid) INTO v_rule
      FROM pg_constraint
     WHERE conname = 'ck_biz_attachment_rule_doc_type'
       AND conrelid = 'biz_attachment_rule'::regclass;
    IF v_rule IS NOT NULL AND position('OPENING_BALANCE' IN v_rule) = 0 THEN
        RAISE EXCEPTION 'V0350 自检失败：规则表 doc_type 约束未含 OPENING_BALANCE：%', v_rule;
    END IF;

    RAISE NOTICE 'V0350 完成：附件类型 16 值 / 角色 15 值 / 规则类型 16 值';
END $$;
