-- ======================================================
-- V0326：`over_amount` 去掉 `DEFAULT 0`（超支才有值，否则就是 NULL）
--
-- V0325 已经把两张表的 `over_amount` 放开成可空，但列上还留着历史上的 `DEFAULT 0`
-- —— 于是「新建但还没提交」的单据（DRAFT）会落 `over_amount = 0.0000`，与
-- 「NULL = 当时没超支」的口径打架（FIX-5 验收：未编预算 → over_amount=null）。
--
-- 本迁移只去默认值，不动任何已存在的数据：
--   · 提交/占用写入路径都是显式赋值（SQL 里 `NULLIF(?,'')::numeric`），默认值从来没被用到；
--   · 去掉之后，DRAFT 单据新建即 `NULL`，语义统一为「没超支」。
--
-- 幂等：`DROP DEFAULT` 本身幂等（没有默认值时是空操作），末尾自检确认两列 default 为空。
-- 回滚：down 把 default 加回 0（与 V0242/V0227 建表时的样子一致）。
-- ======================================================

ALTER TABLE biz_expense_report ALTER COLUMN over_amount DROP DEFAULT;
ALTER TABLE biz_budget_usage   ALTER COLUMN over_amount DROP DEFAULT;

DO $$
DECLARE
    report_default TEXT;
    usage_default  TEXT;
BEGIN
    SELECT coalesce(column_default, '') INTO report_default
      FROM information_schema.columns
     WHERE table_name = 'biz_expense_report' AND column_name = 'over_amount';
    SELECT coalesce(column_default, '') INTO usage_default
      FROM information_schema.columns
     WHERE table_name = 'biz_budget_usage' AND column_name = 'over_amount';

    IF report_default <> '' OR usage_default <> '' THEN
        RAISE EXCEPTION 'V0326 自检失败：over_amount 仍有默认值（report=% usage=%）',
            report_default, usage_default;
    END IF;
END
$$;
