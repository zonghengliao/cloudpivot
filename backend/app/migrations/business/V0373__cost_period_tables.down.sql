-- ============================================================================
-- V0373 回滚：拆掉期间两表（biz_cost_period_item / biz_cost_period）
--
-- 顺序说明：
--   ① biz_cost_period_item 先拆 —— 它 FK 到 biz_cost_period（CASCADE），
--      虽然 CASCADE 会让 DROP 父表连带删子表，但显式先拆子表更清楚，
--      也让「索引零残留」的断言能逐表核对。
--   ② 下游风险：B1-4（二期三表）或 B3（结算）将来若有表 FK 到 biz_cost_period，
--      硬拆会抛 PG 内部依赖名。先扫 pg_constraint：有外部表引用就拒绝并点名。
--      用表名比较而不是 `'biz_cost_period'::regclass` —— 第二次执行 down 时表已删，
--      regclass 会抛 `relation does not exist`，down 就不幂等了。
--
-- 幂等：DROP TABLE IF EXISTS + 结尾断言 ⇒ 重复执行结果不变（第二次只打 NOTICE）。
-- ============================================================================

DO $$
DECLARE
    v_own  TEXT[] := ARRAY['biz_cost_period','biz_cost_period_item'];
    v_left TEXT;
BEGIN
    SELECT string_agg(DISTINCT f.conrelid::regclass::text, ', ') INTO v_left
      FROM pg_constraint f
     WHERE f.contype = 'f'
       AND f.confrelid::regclass::text = ANY (v_own)
       AND NOT (f.conrelid::regclass::text = ANY (v_own));
    IF v_left IS NOT NULL THEN
        RAISE EXCEPTION 'V0373 down 拒绝执行：下游表仍在引用期间两表（%），请先回滚建立它们的迁移，再回滚 V0373', v_left;
    END IF;
END $$;

DROP TABLE IF EXISTS biz_cost_period_item;
DROP TABLE IF EXISTS biz_cost_period;

DO $$
DECLARE
    v_left TEXT;
BEGIN
    SELECT string_agg(t, ', ' ORDER BY t) INTO v_left
      FROM unnest(ARRAY['biz_cost_period','biz_cost_period_item']) AS t
     WHERE to_regclass('public.' || t) IS NOT NULL;
    IF v_left IS NOT NULL THEN
        RAISE EXCEPTION 'V0373 down 自检失败：这些表还在：%', v_left;
    END IF;

    SELECT string_agg(indexname, ', ' ORDER BY indexname) INTO v_left
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND (tablename IN ('biz_cost_period','biz_cost_period_item')
            OR indexname IN ('uk_biz_cost_period','uk_biz_cost_period_no','idx_biz_cost_period_status',
                             'uk_biz_cost_item','idx_biz_cost_item_obj'));
    IF v_left IS NOT NULL THEN
        RAISE EXCEPTION 'V0373 down 自检失败：索引残留：%', v_left;
    END IF;

    RAISE NOTICE 'V0373 down 完成：期间两表及其索引、注释已全部删除（零残留）';
END $$;
