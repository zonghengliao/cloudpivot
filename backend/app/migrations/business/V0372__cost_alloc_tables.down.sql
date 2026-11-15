-- ============================================================================
-- V0372 回滚：拆掉分摊两表（biz_cost_alloc_result / biz_cost_alloc_rule）
--
-- 顺序说明：
--   ① biz_cost_alloc_result 先拆 —— 它 FK 到 biz_cost_alloc_rule（RESTRICT），
--      先拆父表 PG 会拒绝；两张表同属本迁移，引擎按文件内语句顺序执行。
--   ② 真正的风险在**下游**：V0373（期间两表）与 B1-4（二期三表）若有人加了指向
--      这两张表的 FK，硬拆会让 PG 抛内部依赖名（看不出该先回滚哪张卡）。
--      所以先扫 pg_constraint：有外部表 FK 到本迁移的表就拒绝执行并点名。
--      用表名比较而不是 `'biz_cost_alloc_rule'::regclass` —— 后者在表已删掉时
--      （第二次执行 down）会抛 `relation does not exist`，down 就不幂等了。
--
-- 幂等：DROP TABLE IF EXISTS + 结尾断言 ⇒ 重复执行结果不变（第二次只打 NOTICE）。
-- ============================================================================

DO $$
DECLARE
    v_own  TEXT[] := ARRAY['biz_cost_alloc_rule','biz_cost_alloc_result'];
    v_left TEXT;
BEGIN
    SELECT string_agg(DISTINCT f.conrelid::regclass::text, ', ') INTO v_left
      FROM pg_constraint f
     WHERE f.contype = 'f'
       AND f.confrelid::regclass::text = ANY (v_own)
       AND NOT (f.conrelid::regclass::text = ANY (v_own));
    IF v_left IS NOT NULL THEN
        RAISE EXCEPTION 'V0372 down 拒绝执行：下游表仍在引用分摊两表（%），请先回滚建立它们的迁移（V0373 及之后），再回滚 V0372', v_left;
    END IF;
END $$;

DROP TABLE IF EXISTS biz_cost_alloc_result;
DROP TABLE IF EXISTS biz_cost_alloc_rule;

DO $$
DECLARE
    v_left TEXT;
BEGIN
    SELECT string_agg(t, ', ' ORDER BY t) INTO v_left
      FROM unnest(ARRAY['biz_cost_alloc_rule','biz_cost_alloc_result']) AS t
     WHERE to_regclass('public.' || t) IS NOT NULL;
    IF v_left IS NOT NULL THEN
        RAISE EXCEPTION 'V0372 down 自检失败：这些表还在：%', v_left;
    END IF;

    SELECT string_agg(indexname, ', ' ORDER BY indexname) INTO v_left
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND (tablename IN ('biz_cost_alloc_rule','biz_cost_alloc_result')
            OR indexname IN ('uk_biz_cost_rule_code','idx_biz_cost_rule_dept',
                             'uk_biz_cost_alloc_result','idx_biz_cost_alloc_result_period'));
    IF v_left IS NOT NULL THEN
        RAISE EXCEPTION 'V0372 down 自检失败：索引残留：%', v_left;
    END IF;

    RAISE NOTICE 'V0372 down 完成：分摊两表及其索引、注释已全部删除（零残留）';
END $$;
