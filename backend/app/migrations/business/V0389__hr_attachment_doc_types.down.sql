-- ============================================================
-- V0389 回滚：附件单据类型收回 21 → 17 + 删 2 条 HR 附件规则（HR-05）
--
-- 顺序（**先护栏 → 再删规则 → 再收约束 → 最后断言**）：
--   ① 护栏 A：`biz_doc_attachment` 里还有 EMPLOYEE / EMPLOYEE_CONTRACT / LEAVE_REQUEST /
--      SALARY_SHEET 的附件 ⇒ **拒绝回滚**（收约束会让这些行变成「不合法的行」，
--      `ADD CONSTRAINT` 直接失败；就算跳过校验，附件也成了读得出来改不动的孤儿）
--   ② 删本迁移的 2 条规则（按 doc_type + required_at 精确删，不按 doc_type 通配 ——
--      客户可能自己配过同 doc_type 的别的规则）
--   ③ 护栏 B：还剩别的规则用到 4 个新 doc_type ⇒ 拒绝（先删它们再回滚）
--   ④ 两个 CHECK 收回 17 值（= V0368 的现网形态，**RECEIPT_REFUND 必须留着**：
--      照设计稿的 20 值列表收会连它一起摘掉，存量退款凭据附件立刻违反约束）
--   ⑤ 断言零残留：约束恰含 17 值、规则表无 4 个新 doc_type、无 file_role=OTHER
--
-- 幂等：`DROP CONSTRAINT IF EXISTS` + `ADD CONSTRAINT`（重建是幂等的）；
--       DELETE 不存在的行为 0 行影响。文件内**不写** BEGIN/COMMIT。
-- ============================================================

-- ============================================================
-- §1 护栏 A：新类型附件必须先清掉
-- ============================================================
DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('public.biz_doc_attachment') IS NULL THEN
        RAISE NOTICE 'V0389 回滚：biz_doc_attachment 不存在，跳过护栏 A';
        RETURN;
    END IF;

    SELECT count(*) INTO n
      FROM biz_doc_attachment
     WHERE doc_type IN ('EMPLOYEE', 'EMPLOYEE_CONTRACT', 'LEAVE_REQUEST', 'SALARY_SHEET');
    IF n > 0 THEN
        RAISE EXCEPTION
            'V0389 回滚被拒绝：biz_doc_attachment 还有 % 条 HR 新类型（EMPLOYEE / EMPLOYEE_CONTRACT / LEAVE_REQUEST / SALARY_SHEET）附件。收回 CHECK 会让这些行违反约束；请先把这些附件解绑（软删或改 doc_type）再回滚', n;
    END IF;

    RAISE NOTICE 'V0389 回滚护栏 A 通过：没有 HR 新类型的存量附件';
END $$;

-- ============================================================
-- §2 删 2 条 HR 附件规则（先删规则，后收约束：规则本身也用新 doc_type）
-- ============================================================
DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('public.biz_attachment_rule') IS NULL THEN
        RAISE NOTICE 'V0389 回滚：biz_attachment_rule 不存在，跳过删规则';
        RETURN;
    END IF;

    DELETE FROM biz_attachment_rule
     WHERE (doc_type = 'EMPLOYEE_CONTRACT' AND file_role = 'CONTRACT_SCAN' AND required_at = 'SUBMIT')
        OR (doc_type = 'SALARY_SHEET' AND file_role IS NULL AND required_at = 'ACTIVATE');
    GET DIAGNOSTICS n = ROW_COUNT;
    RAISE NOTICE 'V0389 回滚：删除 HR 附件规则 % 条（0 = 本来就没有，幂等）', n;
END $$;

-- ============================================================
-- §3 护栏 B：客户自建的「新 doc_type」规则要先处理
-- ============================================================
DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('public.biz_attachment_rule') IS NULL THEN
        RAISE NOTICE 'V0389 回滚：biz_attachment_rule 不存在，跳过护栏 B';
        RETURN;
    END IF;

    SELECT count(*) INTO n
      FROM biz_attachment_rule
     WHERE doc_type IN ('EMPLOYEE', 'EMPLOYEE_CONTRACT', 'LEAVE_REQUEST', 'SALARY_SHEET');
    IF n > 0 THEN
        RAISE EXCEPTION
            'V0389 回滚被拒绝：biz_attachment_rule 还有 % 条规则用到 HR 新 doc_type（不是本迁移种的两条），先删除或改型再回滚', n;
    END IF;

    RAISE NOTICE 'V0389 回滚护栏 B 通过：规则表已无 HR 新 doc_type';
END $$;

-- ============================================================
-- §4 两个 CHECK 收回 17 值（= V0368 之后的现网形态）
-- ============================================================
DO $$
BEGIN
    IF to_regclass('public.biz_doc_attachment') IS NULL THEN
        RAISE NOTICE 'V0389 回滚：biz_doc_attachment 不存在，跳过约束收回';
        RETURN;
    END IF;

    ALTER TABLE biz_doc_attachment DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_type;
    ALTER TABLE biz_doc_attachment
        ADD CONSTRAINT ck_biz_doc_attachment_type
        CHECK (doc_type IN ('CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                            'FIXED_ASSET', 'EXPENSE_REPORT',
                            'INVOICE', 'RECEIPT', 'STOCK_TAKE', 'PURCHASE_RETURN', 'SALES_RETURN',
                            'STOCK_TRANSFER', 'SUPPLIER', 'CUSTOMER', 'OPENING_BALANCE',
                            'RECEIPT_REFUND'));
END $$;

COMMENT ON COLUMN biz_doc_attachment.doc_type IS
    '单据类型：CONTRACT / PURCHASE_ORDER / STOCK_IN / PAYABLE / PAYMENT / FIXED_ASSET / EXPENSE_REPORT / INVOICE / RECEIPT / STOCK_TAKE / PURCHASE_RETURN / SALES_RETURN / STOCK_TRANSFER / SUPPLIER / CUSTOMER / OPENING_BALANCE / RECEIPT_REFUND（V0389 回滚后为 17 值）';

DO $$
BEGIN
    IF to_regclass('public.biz_attachment_rule') IS NULL THEN
        RAISE NOTICE 'V0389 回滚：biz_attachment_rule 不存在，规则表约束跳过';
        RETURN;
    END IF;

    ALTER TABLE biz_attachment_rule DROP CONSTRAINT IF EXISTS ck_biz_attachment_rule_doc_type;
    ALTER TABLE biz_attachment_rule
        ADD CONSTRAINT ck_biz_attachment_rule_doc_type
        CHECK (doc_type IN ('CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                            'FIXED_ASSET', 'EXPENSE_REPORT',
                            'INVOICE', 'RECEIPT', 'STOCK_TAKE', 'PURCHASE_RETURN', 'SALES_RETURN',
                            'STOCK_TRANSFER', 'SUPPLIER', 'CUSTOMER', 'OPENING_BALANCE',
                            'RECEIPT_REFUND'));
END $$;

-- ============================================================
-- §5 零残留断言
-- ============================================================
DO $$
DECLARE
    v_def     text;
    v_vals    text[];
    n_wrong   integer;
    n         integer;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_type'
       AND conrelid = 'biz_doc_attachment'::regclass;
    IF v_def IS NULL THEN
        RAISE EXCEPTION 'V0389 回滚失败：ck_biz_doc_attachment_type 不存在（表上已无 doc_type 约束）';
    END IF;

    SELECT array_agg(m[1]) INTO v_vals
      FROM regexp_matches(v_def, '''([A-Z_]{2,40})''', 'g') AS m;

    -- 17 个值必须齐（含 RECEIPT_REFUND —— 丢掉它 = 存量退款凭据附件违规）
    SELECT count(*) INTO n_wrong
      FROM unnest(ARRAY['CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                        'FIXED_ASSET', 'EXPENSE_REPORT', 'INVOICE', 'RECEIPT', 'STOCK_TAKE',
                        'PURCHASE_RETURN', 'SALES_RETURN', 'STOCK_TRANSFER', 'SUPPLIER', 'CUSTOMER',
                        'OPENING_BALANCE', 'RECEIPT_REFUND']) AS e(v)
     WHERE NOT (e.v = ANY (coalesce(v_vals, ARRAY[]::text[])));
    IF n_wrong <> 0 THEN
        RAISE EXCEPTION 'V0389 回滚失败：收回后的约束缺 % 个老值：%', n_wrong, v_def;
    END IF;

    -- 4 个新值必须都没了（没收干净 = 回滚没回干净）
    SELECT count(*) INTO n
      FROM unnest(ARRAY['EMPLOYEE', 'EMPLOYEE_CONTRACT', 'LEAVE_REQUEST', 'SALARY_SHEET']) AS e(v)
     WHERE position('''' || e.v || '''' IN v_def) > 0;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0389 回滚失败：约束里还有 % 个 HR 新值：%', n, v_def;
    END IF;

    -- 存量附件不许因为收约束而变脏（护栏 A 之后这里必须仍是 0）
    SELECT count(*) INTO n
      FROM biz_doc_attachment
     WHERE doc_type NOT IN ('CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                            'FIXED_ASSET', 'EXPENSE_REPORT', 'INVOICE', 'RECEIPT', 'STOCK_TAKE',
                            'PURCHASE_RETURN', 'SALES_RETURN', 'STOCK_TRANSFER', 'SUPPLIER', 'CUSTOMER',
                            'OPENING_BALANCE', 'RECEIPT_REFUND');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0389 回滚失败：有 % 行附件的 doc_type 落在 17 值之外', n;
    END IF;

    -- 规则表：2 条种子必须删净，且不留 file_role=OTHER 的历史脏值
    IF to_regclass('public.biz_attachment_rule') IS NOT NULL THEN
        SELECT count(*) INTO n
          FROM biz_attachment_rule
         WHERE deleted_at IS NULL
           AND ((doc_type = 'EMPLOYEE_CONTRACT' AND file_role = 'CONTRACT_SCAN' AND required_at = 'SUBMIT')
             OR (doc_type = 'SALARY_SHEET' AND file_role IS NULL AND required_at = 'ACTIVATE'));
        IF n <> 0 THEN
            RAISE EXCEPTION 'V0389 回滚失败：仍有 % 条 HR 附件规则残留', n;
        END IF;

        SELECT count(*) INTO n FROM biz_attachment_rule WHERE file_role = 'OTHER' AND deleted_at IS NULL;
        IF n <> 0 THEN
            RAISE EXCEPTION 'V0389 回滚失败：规则表仍有 % 条 file_role=OTHER 的行', n;
        END IF;
    END IF;

    RAISE NOTICE 'V0389 回滚完成：附件单据类型收回 17 值（含 RECEIPT_REFUND），2 条 HR 附件规则已删除，存量附件 0 行违规';
END $$;
