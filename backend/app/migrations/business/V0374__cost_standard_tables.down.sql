-- ============================================================================
-- V0374 回滚：拆掉二期三表（biz_cost_variance / biz_cost_standard_item / biz_cost_standard）
--
-- 顺序说明：
--   ① biz_cost_variance 先拆 —— 它 FK 到 biz_cost_standard（RESTRICT）与 biz_cost_object；
--      不先拆它，删 biz_cost_standard 会被 RESTRICT 拦住。
--   ② biz_cost_standard_item 次之 —— 它 FK 到 biz_cost_standard（CASCADE），
--      虽然 CASCADE 会连带删子表，但显式先拆更清楚，也让「索引零残留」能逐表核对。
--   ③ biz_cost_standard 最后。
--
-- 下游风险：二期 S-* 卡将来若有表 FK 到 biz_cost_standard（例如
--   biz_cost_variance_alloc 挂差异分摊），硬拆会抛 PG 内部依赖名。先扫 pg_constraint：
--   有外部表引用就拒绝并点名。用表名比较（::regclass::text）而不是
--   `'biz_cost_standard'::regclass` —— 第二次执行 down 时表已删，regclass 会抛
--   `relation does not exist`，down 就不幂等了。
--
-- 幂等：DROP TABLE IF EXISTS + 结尾断言 ⇒ 重复执行结果不变（第二次只打 NOTICE）。
-- ============================================================================

DO $$
DECLARE
    v_own  TEXT[] := ARRAY['biz_cost_standard','biz_cost_standard_item','biz_cost_variance'];
    v_left TEXT;
BEGIN
    SELECT string_agg(DISTINCT f.conrelid::regclass::text, ', ') INTO v_left
      FROM pg_constraint f
     WHERE f.contype = 'f'
       AND f.confrelid::regclass::text = ANY (v_own)
       AND NOT (f.conrelid::regclass::text = ANY (v_own));
    IF v_left IS NOT NULL THEN
        RAISE EXCEPTION 'V0374 down 拒绝执行：下游表仍在引用二期三表（%），请先回滚建立它们的迁移，再回滚 V0374', v_left;
    END IF;
END $$;

DROP TABLE IF EXISTS biz_cost_variance;
DROP TABLE IF EXISTS biz_cost_standard_item;
DROP TABLE IF EXISTS biz_cost_standard;

DO $$
DECLARE
    v_left TEXT;
BEGIN
    SELECT string_agg(t, ', ' ORDER BY t) INTO v_left
      FROM unnest(ARRAY['biz_cost_standard','biz_cost_standard_item','biz_cost_variance']) AS t
     WHERE to_regclass('public.' || t) IS NOT NULL;
    IF v_left IS NOT NULL THEN
        RAISE EXCEPTION 'V0374 down 自检失败：这些表还在：%', v_left;
    END IF;

    SELECT string_agg(indexname, ', ' ORDER BY indexname) INTO v_left
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND (tablename IN ('biz_cost_standard','biz_cost_standard_item','biz_cost_variance')
            OR indexname IN ('uk_biz_cost_std','uk_biz_cost_std_item',
                             'idx_biz_cost_var_period','idx_biz_cost_var_obj'));
    IF v_left IS NOT NULL THEN
        RAISE EXCEPTION 'V0374 down 自检失败：索引残留：%', v_left;
    END IF;

    RAISE NOTICE 'V0374 down 完成：二期三表及其索引、注释已全部删除（零残留）';
END $$;
