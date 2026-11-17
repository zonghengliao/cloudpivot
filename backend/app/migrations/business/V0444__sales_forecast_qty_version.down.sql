-- ============================================================================
-- V0444 down：撤掉销售预测的「预测数量 / 版本链 / 物料索引 / 当前版本视图」
--
-- 【顺序】视图 → 索引 → 约束 → 列（视图先拆，否则删列会被依赖挡住）。
--
-- 【护栏：有真实业务数据就拒绝回滚】
--   `forecast_qty` 与 `revised_from_id` 承载的是**业务信息**（这一版预测多少数量、这一版是从
--   哪一版改来的），不是纯结构：删列等于把这些信息永久丢掉，且无法从别的表还原
--   （`biz_sales_target` 是目标不是预测；`version_no` 只说明第几版，不知道谁取代谁）。
--   ⇒ 只在「刚升级、还没有任何数量预测 / 版本链」时才允许 down；否则 RAISE EXCEPTION，
--     拒绝信息里写清该怎么办（先导出/清空这些行，或先补数据迁移再回滚）。
--
-- 【幂等】DROP VIEW / DROP INDEX / DROP CONSTRAINT / DROP COLUMN 全部带 IF EXISTS；
--   护栏查询先判列在不在（不用 `'tbl'::regclass`，第二次执行 down 时列已不存在会抛错）。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 护栏：已有数量预测或版本链则拒绝回滚（见文件头）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n bigint;
BEGIN
    IF to_regclass('public.biz_sales_forecast') IS NOT NULL
       AND EXISTS (
            SELECT 1 FROM information_schema.columns
             WHERE table_schema = 'public' AND table_name = 'biz_sales_forecast'
               AND column_name = 'forecast_qty'
       ) THEN
        EXECUTE 'SELECT count(*) FROM biz_sales_forecast WHERE forecast_qty <> 0' INTO n;
        IF n > 0 THEN
            RAISE EXCEPTION 'V0444 down 拒绝执行：biz_sales_forecast 已有 % 行写了预测数量，回滚会把数量信息一起丢掉（要回滚请先导出这些行，或把 forecast_qty 归零后再执行）', n;
        END IF;
    END IF;

    IF to_regclass('public.biz_sales_forecast') IS NOT NULL
       AND EXISTS (
            SELECT 1 FROM information_schema.columns
             WHERE table_schema = 'public' AND table_name = 'biz_sales_forecast'
               AND column_name = 'revised_from_id'
       ) THEN
        EXECUTE 'SELECT count(*) FROM biz_sales_forecast WHERE revised_from_id IS NOT NULL' INTO n;
        IF n > 0 THEN
            RAISE EXCEPTION 'V0444 down 拒绝执行：biz_sales_forecast 已有 % 行挂了版本链（revised_from_id），回滚会丢掉「哪一版取代哪一版」的追溯信息（要回滚请先导出这些行或把 revised_from_id 置空）', n;
        END IF;
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- ② 拆视图（依赖 forecast_qty / revised_from_id 的列，必须先拆）
-- ---------------------------------------------------------------------------
DROP VIEW IF EXISTS v_biz_sales_forecast_current;

-- ---------------------------------------------------------------------------
-- ③ 拆索引 + 约束
-- ---------------------------------------------------------------------------
DROP INDEX IF EXISTS idx_biz_sales_forecast_material;
DROP INDEX IF EXISTS idx_biz_sales_forecast_revised_from;

ALTER TABLE biz_sales_forecast DROP CONSTRAINT IF EXISTS ck_biz_sf_qty;
ALTER TABLE biz_sales_forecast DROP CONSTRAINT IF EXISTS ck_biz_sf_revised_from;

-- ---------------------------------------------------------------------------
-- ④ 删列
-- ---------------------------------------------------------------------------
ALTER TABLE biz_sales_forecast DROP COLUMN IF EXISTS forecast_qty;
ALTER TABLE biz_sales_forecast DROP COLUMN IF EXISTS revised_from_id;

-- ---------------------------------------------------------------------------
-- ⑤ 自检：两列 / 两条约束 / 两条索引 / 视图都必须确实没了，且 V0318 的结构仍在
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n integer;
BEGIN
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_forecast'
       AND column_name IN ('forecast_qty', 'revised_from_id');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0444 down 失败：仍有 % 列未删除', n;
    END IF;

    SELECT count(*) INTO n FROM pg_constraint
     WHERE conname IN ('ck_biz_sf_qty', 'ck_biz_sf_revised_from');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0444 down 失败：仍有 % 条约束未删除', n;
    END IF;

    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('idx_biz_sales_forecast_material', 'idx_biz_sales_forecast_revised_from');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0444 down 失败：仍有 % 条索引未删除', n;
    END IF;

    IF to_regclass('public.v_biz_sales_forecast_current') IS NOT NULL THEN
        RAISE EXCEPTION 'V0444 down 失败：视图 v_biz_sales_forecast_current 还在';
    END IF;

    -- 回归护栏：V0318 的两个唯一索引 + 期间 CHECK 不许被这一刀带走
    IF to_regclass('public.uk_biz_sales_forecast_dim') IS NULL
       OR to_regclass('public.uk_biz_sales_forecast_no') IS NULL THEN
        RAISE EXCEPTION 'V0444 down 失败：V0318 的唯一索引被误删';
    END IF;
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_sales_forecast'::regclass AND conname = 'ck_biz_sf_period';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0444 down 失败：ck_biz_sf_period（月度/季度期间码）被误删';
    END IF;

    RAISE NOTICE 'V0444 down 完成：forecast_qty / revised_from_id 两列、2 条 CHECK、2 条索引、1 个视图均已移除；V0318 结构完好';
END $$;
