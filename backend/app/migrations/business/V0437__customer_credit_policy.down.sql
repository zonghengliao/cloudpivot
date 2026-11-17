-- ============================================================================
-- V0437 down：撤掉客户超限策略列（回到 V0313 的形状）
--
-- **只删本迁移加的东西**：约束 ck_biz_customer_credit_policy + 列 credit_policy。
-- 不动 credit_limit / credit_used / biz_credit_log —— 那是 V0260 / V0313 的。
-- 回滚后提交守卫回到「硬编拦截」（默认策略消失 ⇒ 代码里 policy 读不到时 fail-closed 到 BLOCK）。
-- ============================================================================

DO $$
DECLARE
    n INT;
BEGIN
    IF to_regclass('public.biz_customer') IS NULL THEN
        RAISE NOTICE 'V0437 down：biz_customer 不存在，跳过';
        RETURN;
    END IF;

    ALTER TABLE biz_customer DROP CONSTRAINT IF EXISTS ck_biz_customer_credit_policy;
    ALTER TABLE biz_customer DROP COLUMN IF EXISTS credit_policy;

    -- 断言：列与约束都不在了
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_customer'
       AND column_name = 'credit_policy';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0437 down 断言失败：credit_policy 列未删掉';
    END IF;
    SELECT count(*) INTO n FROM pg_constraint WHERE conname = 'ck_biz_customer_credit_policy';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0437 down 断言失败：ck_biz_customer_credit_policy 未删掉';
    END IF;

    -- 断言：额度口径两列必须还在（回滚不能顺手把别人的列删了）
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_customer'
       AND column_name IN ('credit_limit', 'credit_used', 'credit_days');
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0437 down 断言失败：credit_limit / credit_used / credit_days 应仍有 3 列，实际 %', n;
    END IF;

    RAISE NOTICE 'V0437 down：credit_policy 已移除，额度口径列原样保留 ✓';
END $$;
