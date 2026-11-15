-- ============================================================================
-- V0371 回滚：拆掉归集四表（biz_cost_collect / biz_cost_labor /
--                       biz_cost_overhead / biz_cost_rate）
--
-- 顺序说明：四张表之间**互不引用**，只各自 FK 到 biz_cost_object（V0370 的表），
--           所以拆它们不影响父表；真正的风险在**下游**：
--             V0372/V0373（B1-3）：biz_cost_alloc_rule / alloc_result / period / period_item
--             V0374（B1-4）：二期三表
--           正常回滚走引擎「严格逆序」，下游先拆干净；此处再拦一道 —— 用
--           pg_constraint 扫「有没有外部表 FK 到我这四张表」，比硬编码表名更耐改：
--           将来 B1-3 把 biz_cost_alloc_result 指向 biz_cost_collect 也照样拦得住。
--           被拦时提示先说清「该先回滚哪张卡」，而不是让 PG 抛内部依赖名。
--
-- 幂等：DROP TABLE IF EXISTS + 结尾断言 ⇒ 重复执行结果不变（第二次只打 NOTICE）。
-- ============================================================================

DO $$
DECLARE
    v_own  TEXT[] := ARRAY['biz_cost_collect','biz_cost_labor','biz_cost_overhead','biz_cost_rate'];
    v_left TEXT;
BEGIN
    -- 用表名比较而不是 `'biz_cost_labor'::regclass`：后者在本表已被删掉时（第二次执行 down）
    -- 会直接抛 `relation does not exist`，down 就不幂等了。
    SELECT string_agg(DISTINCT f.conrelid::regclass::text, ', ') INTO v_left
      FROM pg_constraint f
     WHERE f.contype = 'f'
       AND f.confrelid::regclass::text = ANY (v_own)
       AND NOT (f.conrelid::regclass::text = ANY (v_own));
    IF v_left IS NOT NULL THEN
        RAISE EXCEPTION 'V0371 down 拒绝执行：下游成本表仍在（%），请先回滚建立它们的迁移（V0372 及之后），再回滚 V0371', v_left;
    END IF;
END $$;

DROP TABLE IF EXISTS biz_cost_collect;
DROP TABLE IF EXISTS biz_cost_labor;
DROP TABLE IF EXISTS biz_cost_overhead;
DROP TABLE IF EXISTS biz_cost_rate;

DO $$
DECLARE
    v_left TEXT;
BEGIN
    SELECT string_agg(t, ', ' ORDER BY t) INTO v_left
      FROM unnest(ARRAY['biz_cost_collect','biz_cost_labor','biz_cost_overhead','biz_cost_rate']) AS t
     WHERE to_regclass('public.' || t) IS NOT NULL;
    IF v_left IS NOT NULL THEN
        RAISE EXCEPTION 'V0371 down 自检失败：这些表还在：%', v_left;
    END IF;

    SELECT string_agg(indexname, ', ' ORDER BY indexname) INTO v_left
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND (tablename IN ('biz_cost_collect','biz_cost_labor','biz_cost_overhead','biz_cost_rate')
            OR indexname IN ('uk_biz_cost_collect_src','idx_biz_cost_collect_obj','idx_biz_cost_collect_period',
                             'idx_biz_cost_labor_obj','idx_biz_cost_labor_emp','idx_biz_cost_labor_period',
                             'idx_biz_cost_oh_period','idx_biz_cost_oh_obj',
                             'uk_biz_cost_rate','idx_biz_cost_rate_lookup'));
    IF v_left IS NOT NULL THEN
        RAISE EXCEPTION 'V0371 down 自检失败：索引残留：%', v_left;
    END IF;

    RAISE NOTICE 'V0371 down 完成：归集四表及其索引、注释已全部删除（零残留）';
END $$;
