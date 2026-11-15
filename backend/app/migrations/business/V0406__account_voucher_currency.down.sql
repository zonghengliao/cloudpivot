-- ============================================================================
-- V0406 down：撤掉 biz_account.currency_code + biz_voucher.currency_code / exchange_rate
--
-- 【顺序】先拆约束与索引，再删列（删列本身也会带走依赖它的约束，显式拆只是为了
--   在日志里留下「按哪个名字拆的」，便于排障）。
--
-- 【护栏：有外币数据就拒绝回滚】
--   这三列承载的是**业务信息**（哪个科目/哪张凭证是外币、按什么汇率折的），
--   不是纯结构。库里一旦已经有 currency_code <> 'CNY' 或 exchange_rate <> 1 的行，
--   删列就等于把这些信息永久丢掉、且无法从别的表还原（`biz_exchange_rate` 只有
--   汇率，不知道哪张凭证用了它）。
--   ⇒ down 只在「刚升级、还没有任何外币业务」时才允许执行；否则 RAISE EXCEPTION，
--     拒绝信息里写清该怎么办（这些数据本身就是 up 之后产生的，回滚本就该先处理）。
--
-- 【幂等】DROP CONSTRAINT / DROP INDEX / DROP COLUMN 全部带 IF EXISTS；
--   护栏查询用 `information_schema.columns` 先判列在不在（不用 `'tbl'::regclass`，
--   第二次执行 down 时列已不存在，那种写法会直接抛错）。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 护栏：库里已有外币数据则拒绝回滚（见文件头）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n bigint;
BEGIN
    IF to_regclass('public.biz_account') IS NOT NULL
       AND EXISTS (
            SELECT 1 FROM information_schema.columns
             WHERE table_schema = 'public' AND table_name = 'biz_account'
               AND column_name = 'currency_code'
       ) THEN
        EXECUTE 'SELECT count(*) FROM biz_account WHERE currency_code IS DISTINCT FROM ''CNY''' INTO n;
        IF n > 0 THEN
            RAISE EXCEPTION 'V0406 down 拒绝执行：biz_account 已有 % 行外币科目，回滚会连币种信息一起丢掉（要回滚请先把这些科目改回 CNY）', n;
        END IF;
    END IF;

    IF to_regclass('public.biz_voucher') IS NOT NULL
       AND EXISTS (
            SELECT 1 FROM information_schema.columns
             WHERE table_schema = 'public' AND table_name = 'biz_voucher'
               AND column_name = 'currency_code'
       ) THEN
        EXECUTE 'SELECT count(*) FROM biz_voucher WHERE currency_code IS DISTINCT FROM ''CNY'' OR exchange_rate IS DISTINCT FROM 1' INTO n;
        IF n > 0 THEN
            RAISE EXCEPTION 'V0406 down 拒绝执行：biz_voucher 已有 % 行外币凭证（币种或汇率非默认），回滚会丢掉汇率锁定信息（要回滚请先红冲/作废这些外币凭证）', n;
        END IF;
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- ② 拆约束 + 索引
-- ---------------------------------------------------------------------------
ALTER TABLE biz_account DROP CONSTRAINT IF EXISTS ck_biz_account_currency_code;
ALTER TABLE biz_voucher DROP CONSTRAINT IF EXISTS ck_biz_voucher_currency_code;
ALTER TABLE biz_voucher DROP CONSTRAINT IF EXISTS ck_biz_voucher_exchange_rate;

DROP INDEX IF EXISTS idx_biz_account_currency;
DROP INDEX IF EXISTS idx_biz_voucher_currency;

-- ---------------------------------------------------------------------------
-- ③ 删列
-- ---------------------------------------------------------------------------
ALTER TABLE biz_account DROP COLUMN IF EXISTS currency_code;
ALTER TABLE biz_voucher DROP COLUMN IF EXISTS currency_code;
ALTER TABLE biz_voucher DROP COLUMN IF EXISTS exchange_rate;

-- ---------------------------------------------------------------------------
-- ④ 自检：三列 / 三条约束 / 两条索引都必须确实没了
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n integer;
BEGIN
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public'
       AND ((table_name = 'biz_account' AND column_name = 'currency_code')
         OR (table_name = 'biz_voucher' AND column_name IN ('currency_code', 'exchange_rate')));
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0406 down 失败：仍有 % 列未删除', n;
    END IF;

    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE conname IN ('ck_biz_account_currency_code', 'ck_biz_voucher_currency_code', 'ck_biz_voucher_exchange_rate');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0406 down 失败：仍有 % 条约束未删除', n;
    END IF;

    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = 'public' AND indexname IN ('idx_biz_account_currency', 'idx_biz_voucher_currency');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0406 down 失败：仍有 % 条索引未删除', n;
    END IF;

    RAISE NOTICE 'V0406 down 完成：biz_account / biz_voucher 的币种与汇率三列、3 条 CHECK、2 条索引均已移除';
END $$;
