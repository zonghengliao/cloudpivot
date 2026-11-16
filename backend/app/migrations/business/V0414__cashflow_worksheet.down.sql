-- ============================================================================
-- V0414 down：删掉现金流底稿表 biz_cashflow_worksheet
--
-- 【护栏：表里已有底稿行就拒绝回滚】
--   本表装的是**编制结果**（某期间某项目多少钱、期初期末余额是多少）。删表无法从任何
--   别的表还原它 —— 报表能从凭证重算，但「会计手工调整过哪几行、调整成多少」这条
--   信息只存在于本表。所以 down 只在「刚升级、底稿还没录过」时才允许执行；
--   否则 RAISE EXCEPTION，拒绝信息里写清该怎么办（先把底稿导出/作废，再回滚）。
--
-- 【幂等】
--   `DROP TABLE IF EXISTS`（表本身会带走它的 4 条索引与 7 条 CHECK），
--   护栏查询先判 `to_regclass` 存在再数行 —— 不用 `'tbl'::regclass`（第二次执行
--   down 时表已经不存在，那种写法会直接抛错）。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 护栏：已有底稿行则拒绝回滚
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n bigint;
BEGIN
    IF to_regclass('public.biz_cashflow_worksheet') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_cashflow_worksheet' INTO n;
        IF n > 0 THEN
            RAISE EXCEPTION 'V0414 down 拒绝执行：biz_cashflow_worksheet 已有 % 行底稿，回滚会丢掉编制结果（要回滚请先导出/作废这些底稿行）', n;
        END IF;
        RAISE NOTICE 'V0414 down：表为空（0 行），可以安全回滚';
    ELSE
        RAISE NOTICE 'V0414 down：biz_cashflow_worksheet 不存在，按幂等路径跳过';
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- ② 删表（连带索引与约束）
-- ---------------------------------------------------------------------------
DROP TABLE IF EXISTS biz_cashflow_worksheet;

-- ---------------------------------------------------------------------------
-- ③ 收尾自检：表与索引必须确实没了
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n integer;
BEGIN
    SELECT count(*) INTO n
      FROM information_schema.tables
     WHERE table_schema = 'public' AND table_name = 'biz_cashflow_worksheet';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0414 down 自检失败：biz_cashflow_worksheet 仍然存在';
    END IF;

    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_cashflow_worksheet';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0414 down 自检失败：仍有 % 条索引挂在 biz_cashflow_worksheet 上', n;
    END IF;

    RAISE NOTICE 'V0414 down 完成：biz_cashflow_worksheet 已删除（索引/约束随表带走）';
END $$;
