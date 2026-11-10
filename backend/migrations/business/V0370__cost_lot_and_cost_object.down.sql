-- ============================================================================
-- V0370 回滚：拆掉成本核算地基两张表（biz_cost_object → biz_lot）
--
-- 顺序说明：本迁移里两张表**互不引用**（都只用弱引用列，不建 FK），
--           但后续迁移建的下游表会 FK 到 biz_cost_object：
--             V0371 起：biz_cost_collect / biz_cost_labor / biz_cost_overhead
--             V0373 起：biz_cost_alloc_result / biz_cost_period_item
--           正常回滚走引擎「严格逆序」，下游先拆干净；此处再拦一道，
--           避免有人手工单文件 RUN 时被 PG 的 dependency 错误糊一脸（那个报错
--           只给内部表名，看不出「该先回滚哪张卡」）。
--
-- 幂等：DROP TABLE IF EXISTS + 结尾断言 ⇒ 重复执行结果不变（第二次只打 NOTICE）。
-- ============================================================================

DO $$
DECLARE
    v_left TEXT;
BEGIN
    SELECT string_agg(t, ', ' ORDER BY t) INTO v_left
      FROM unnest(ARRAY['biz_cost_collect','biz_cost_labor','biz_cost_overhead',
                        'biz_cost_alloc_result','biz_cost_period_item']) AS t
     WHERE to_regclass('public.' || t) IS NOT NULL;
    IF v_left IS NOT NULL THEN
        RAISE EXCEPTION 'V0370 down 拒绝执行：下游成本表仍在（%），请先回滚建立它们的迁移（V0371 及之后），再回滚 V0370', v_left;
    END IF;
END $$;

DROP TABLE IF EXISTS biz_cost_object;
DROP TABLE IF EXISTS biz_lot;

DO $$
BEGIN
    IF to_regclass('public.biz_lot') IS NOT NULL THEN
        RAISE EXCEPTION 'V0370 down 自检失败：biz_lot 仍在';
    END IF;
    IF to_regclass('public.biz_cost_object') IS NOT NULL THEN
        RAISE EXCEPTION 'V0370 down 自检失败：biz_cost_object 仍在';
    END IF;
    RAISE NOTICE 'V0370 down 完成：biz_lot / biz_cost_object 及其索引、注释已全部删除（零残留）';
END $$;
