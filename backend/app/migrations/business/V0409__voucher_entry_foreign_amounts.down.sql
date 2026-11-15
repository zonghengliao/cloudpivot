-- ============================================================================
-- V0409__voucher_entry_foreign_amounts.down（M1-04 回滚）
--
-- 顺序（反着来）：① 数据护栏 → ② 拆部分索引 → ③ 拆 CHECK → ④ 拆两列 → ⑤ 收尾自检
--
-- 数据护栏：库里**只要有一行带原币金额**就拒绝回滚。原币金额是录入时的业务事实（用户选的
-- 币种、敲的原币数），回滚会把它整列删掉，重放 up 只能得到 NULL —— 那不是「撤销一次结构变更」，
-- 是丢业务数据。要回滚得先把那些外币凭证作废 / 红冲。
--
-- 全部 `IF EXISTS` / 先判列在不在 ⇒ 重复执行不报错（本仓 V0406/V0408 down 都吃过
-- 「第二次 down 报 column does not exist」的亏）。
-- ============================================================================

-- ① 数据护栏：有原币金额就不许回滚（列可能已不存在 → 先判列在不在）
DO $$
DECLARE
    n bigint := 0;
BEGIN
    IF to_regclass('public.biz_voucher_entry') IS NOT NULL
       AND EXISTS (
            SELECT 1 FROM information_schema.columns
             WHERE table_schema = 'public'
               AND table_name = 'biz_voucher_entry'
               AND column_name = 'fc_debit_amount'
       ) THEN
        SELECT count(*) INTO n
          FROM biz_voucher_entry
         WHERE fc_debit_amount IS NOT NULL OR fc_credit_amount IS NOT NULL;
        IF n > 0 THEN
            RAISE EXCEPTION 'V0409 down 拒绝执行：biz_voucher_entry 已有 % 行带原币金额（外币凭证的原币金额是录入时的业务事实，回滚会整列丢掉、重放 up 只能得到 NULL）。请先把那些外币凭证红冲 / 作废再回滚', n;
        END IF;
    END IF;
    RAISE NOTICE 'V0409 down: 数据护栏通过（无外币分录）';
END $$;

-- ② 拆部分索引
DROP INDEX IF EXISTS idx_biz_voucher_entry_fc;

-- ③ 拆 CHECK
ALTER TABLE biz_voucher_entry DROP CONSTRAINT IF EXISTS ck_biz_voucher_entry_fc_side;

-- ④ 拆两列
ALTER TABLE biz_voucher_entry DROP COLUMN IF EXISTS fc_debit_amount;
ALTER TABLE biz_voucher_entry DROP COLUMN IF EXISTS fc_credit_amount;

-- ⑤ 收尾自检：列 / 约束 / 索引都必须真的没了
DO $$
DECLARE
    n integer := 0;
BEGIN
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_voucher_entry'
       AND column_name IN ('fc_debit_amount', 'fc_credit_amount');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0409 down 失败：原币列还有 % 个没拆', n;
    END IF;

    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE conname = 'ck_biz_voucher_entry_fc_side';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0409 down 失败：ck_biz_voucher_entry_fc_side 还在';
    END IF;

    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'idx_biz_voucher_entry_fc';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0409 down 失败：idx_biz_voucher_entry_fc 还在';
    END IF;

    RAISE NOTICE 'V0409 down 完成：原币两列 / 1 条 CHECK / 部分索引全部拆除';
END $$;
