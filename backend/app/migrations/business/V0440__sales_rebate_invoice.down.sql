-- ============================================================================
-- V0440 down：销售返利 —— 生成红字发票（M3-17）
--
-- 回滚顺序与 up 相反：护栏 → 删菜单 → 还原来源白名单 → 删科目 → 删约束/索引/5 列。
--
-- ⚠ 为什么第一条是护栏：
--   一旦有结算单开过红字发票（`invoice_id IS NOT NULL`），删这 5 列就等于把
--   「票 ↔ 结算单」的关联从系统里抹掉 —— 而票本身还躺在 `biz_invoice` 与总账里，
--   对账时会出现「有一张红字票，没有任何单据认领它」。宁可拒绝回滚：
--   先把这些结算单红冲（业务的正常路径），再回来 down。
--
-- ⚠ 科目 2241.02：只要还有凭证分录在用它就不删（删了历史凭证的科目就指空了）。
-- ============================================================================

-- ---------- ① 护栏：已有红字发票的关联时拒绝回滚 ----------
DO $$
DECLARE
    n integer;
BEGIN
    SELECT count(*) INTO n
      FROM biz_sales_rebate_settlement
     WHERE invoice_id IS NOT NULL;
    IF n > 0 THEN
        RAISE EXCEPTION 'V0440 down 拒绝回滚：% 张结算单已开红字发票（invoice_id 非空）。请先按业务红冲，再回滚本迁移', n;
    END IF;

    SELECT count(*) INTO n
      FROM biz_voucher_entry e
      JOIN biz_account a ON a.id = e.account_id
     WHERE a.account_code = '2241.02';
    IF n > 0 THEN
        RAISE EXCEPTION 'V0440 down 拒绝回滚：科目 2241.02 已被 % 条凭证分录使用，删掉会让历史凭证指空', n;
    END IF;
END $$;

-- ---------- ② 菜单：先清角色授权（sys_role_menu 有外键）再删按钮行 ----------
DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000004393'::uuid;

DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000004393'::uuid;

-- ---------- ③ 还原 biz_invoice 的来源类型白名单（去掉 SALES_REBATE）----------
DO $$
DECLARE
    definition text;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO definition
      FROM pg_constraint
     WHERE conrelid = 'biz_invoice'::regclass
       AND conname  = 'ck_biz_invoice_source';

    IF definition IS NOT NULL AND position('SALES_REBATE' IN definition) > 0 THEN
        ALTER TABLE biz_invoice DROP CONSTRAINT ck_biz_invoice_source;
        ALTER TABLE biz_invoice
            ADD CONSTRAINT ck_biz_invoice_source
            CHECK (source_type IS NULL
                OR source_type IN ('SALES_ORDER','SHIPMENT','SALES_OUT','MANUAL','SALES_RETURN'));
    END IF;
END $$;

COMMENT ON COLUMN biz_invoice.source_type IS
    '来源类型：SALES_ORDER / SHIPMENT / SALES_OUT / MANUAL / SALES_RETURN';

-- ---------- ④ 科目种子：删 2241.02（只有无子科目、无引用时才可能走到这里）----------
DELETE FROM biz_account
 WHERE account_code = '2241.02'
   AND NOT EXISTS (SELECT 1 FROM biz_account c WHERE c.parent_id = biz_account.id);

-- ---------- ⑤ 约束 / 索引 / 5 列 ----------
ALTER TABLE biz_sales_rebate_settlement DROP CONSTRAINT IF EXISTS ck_biz_srs_settled_link;

DROP INDEX IF EXISTS uk_biz_srs_invoice;
DROP INDEX IF EXISTS idx_biz_srs_invoice;

ALTER TABLE biz_sales_rebate_settlement
    DROP COLUMN IF EXISTS invoice_id,
    DROP COLUMN IF EXISTS invoice_no,
    DROP COLUMN IF EXISTS voucher_id,
    DROP COLUMN IF EXISTS settled_at,
    DROP COLUMN IF EXISTS settled_by;

-- ---------- ⑥ 自检：列已回落到 35、约束与索引已消失 ----------
DO $$
DECLARE
    n integer;
BEGIN
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = current_schema() AND table_name = 'biz_sales_rebate_settlement';
    IF n <> 35 THEN
        RAISE EXCEPTION 'V0440 down 自检失败：biz_sales_rebate_settlement 应回落 35 列，实得 %', n;
    END IF;

    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE conrelid = 'biz_sales_rebate_settlement'::regclass
       AND conname  = 'ck_biz_srs_settled_link';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0440 down 自检失败：ck_biz_srs_settled_link 仍在';
    END IF;

    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = current_schema() AND tablename = 'biz_sales_rebate_settlement'
       AND indexname IN ('uk_biz_srs_invoice', 'idx_biz_srs_invoice');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0440 down 自检失败：红字发票的两个索引仍在（% 个）', n;
    END IF;

    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004393'::uuid;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0440 down 自检失败：开红字发票的按钮行仍在';
    END IF;

    RAISE NOTICE 'V0440 down 完成：结算单回落 35 列、按钮行/两个索引/约束已消失';
END $$;
