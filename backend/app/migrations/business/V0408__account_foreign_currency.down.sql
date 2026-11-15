-- ============================================================================
-- V0408__account_foreign_currency.down（M1-03 回滚）
--
-- 顺序（反着来，顺序错了会报「列被依赖」）：
--   ① 数据护栏 —— 已有科目标了外币核算就拒绝（这一位是人工录的业务信息，删了就真丢了）
--   ② 拆授权 → 拆菜单按钮行（码授不出去之前先把 `sys_role_menu` 清了）
--   ③ 拆部分索引 → 拆两条 CHECK → 拆列
--
-- 全部 `IF EXISTS` / 带 `IS NULL` 判断 ⇒ 重复执行不报错；
-- 刻意**不用** `'biz_account'::regclass` 这种「列已删就抛错」的写法（M1-01 down 已吃过）。
-- ============================================================================

-- ① 数据护栏：有标记就不许回滚
--
-- 列可能已经不存在（本迁移被跑了两次 down / 手工删过列）：先判列在不在再查值，
-- 否则第二次 down 会报 `column "is_foreign_currency" does not exist` —— 回滚路径不该
-- 因为「已经回滚过」而失败。
DO $$
DECLARE
    n bigint := 0;
BEGIN
    IF to_regclass('public.biz_account') IS NOT NULL
       AND EXISTS (
            SELECT 1 FROM information_schema.columns
             WHERE table_schema = 'public'
               AND table_name = 'biz_account'
               AND column_name = 'is_foreign_currency'
       ) THEN
        SELECT count(*) INTO n FROM biz_account WHERE is_foreign_currency = 1;
        IF n > 0 THEN
            RAISE EXCEPTION 'V0408 down 拒绝执行：biz_account 已有 % 行标记为外币核算（这些科目按原币记账，回滚会丢掉「哪些科目是外币核算」这一业务信息，重放 up 只会填回 0）', n;
        END IF;
    END IF;
    RAISE NOTICE 'V0408 down: 数据护栏通过（无外币核算科目）';
END $$;

-- ② 拆授权 / 菜单按钮行
DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000001720'::uuid;

DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000001720'::uuid;

-- ③ 拆索引 / 约束 / 列
DROP INDEX IF EXISTS idx_biz_account_foreign_currency;

ALTER TABLE biz_account DROP CONSTRAINT IF EXISTS ck_biz_account_fc_leaf;
ALTER TABLE biz_account DROP CONSTRAINT IF EXISTS ck_biz_account_foreign_currency;

ALTER TABLE biz_account DROP COLUMN IF EXISTS is_foreign_currency;

-- ④ 收尾自检：列 / 约束 / 索引 / 菜单行都必须真的没了
DO $$
DECLARE
    n integer := 0;
BEGIN
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_account'
       AND column_name = 'is_foreign_currency';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0408 down 失败：is_foreign_currency 列还在';
    END IF;

    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE conname IN ('ck_biz_account_foreign_currency', 'ck_biz_account_fc_leaf');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0408 down 失败：还有 % 条外币标记 CHECK 没拆', n;
    END IF;

    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'idx_biz_account_foreign_currency';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0408 down 失败：idx_biz_account_foreign_currency 还在';
    END IF;

    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001720'::uuid;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0408 down 失败：权限码菜单行还在';
    END IF;

    RAISE NOTICE 'V0408 down 完成：列 / 2 条 CHECK / 部分索引 / 菜单行与授权全部拆除';
END $$;
