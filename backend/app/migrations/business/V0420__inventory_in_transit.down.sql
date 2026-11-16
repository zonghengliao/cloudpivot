-- ============================================================================
-- V0420__inventory_in_transit.down（在途库存账回滚 · M2-01 · 2026-11-16）
--
-- 幂等：`DROP TABLE IF EXISTS` —— 重复执行不报错（第二次什么都不做）。
--
-- 护栏（与 V0414 现金流底稿 down 同口径）：
--   表里只要还有**在途 / 已签收**的行（status <> 'CANCELLED'），就拒绝回滚。
--   理由：这些行是 M2-02..M2-05 的业务工作数据（货在路上、已签收未清），
--   删掉之后「这批货去哪了」在系统里再无痕迹，且无法从别的表重建
--   （发货单只说发了多少，不说签收了多少）。
--   要先回滚，请走业务把在途签收或作废（行留成 CANCELLED 就放行）。
--
-- 为什么 CANCELLED 行不拦：它们是纯审计痕迹（作废后不再算在途），
-- 删表不会让任何业务口径变错 —— 与「在途账里还有真在途」是两件事。
-- ============================================================================
DO $$
DECLARE
    n BIGINT;
BEGIN
    IF to_regclass('public.biz_inventory_in_transit') IS NULL THEN
        RAISE NOTICE 'V0420 down: biz_inventory_in_transit 不存在，无需回滚（幂等）';
        RETURN;
    END IF;

    SELECT count(*) INTO n
      FROM biz_inventory_in_transit
     WHERE status <> 'CANCELLED';
    IF n > 0 THEN
        RAISE EXCEPTION 'V0420 down 拒绝执行：biz_inventory_in_transit 还有 % 行在途/已签收记录（先走业务签收或作废，作废行可随表删除）', n;
    END IF;

    RAISE NOTICE 'V0420 down: 表内只剩作废行（或空表），开始回滚';
END $$;

DROP TABLE IF EXISTS biz_inventory_in_transit;

-- 收尾自检：表必须真的没了（只 NOTICE 不 EXCEPTION 等于没验）
DO $$
BEGIN
    IF to_regclass('public.biz_inventory_in_transit') IS NOT NULL THEN
        RAISE EXCEPTION 'V0420 down 失败：biz_inventory_in_transit 仍然存在';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_indexes
                WHERE schemaname = 'public' AND tablename = 'biz_inventory_in_transit') THEN
        RAISE EXCEPTION 'V0420 down 失败：biz_inventory_in_transit 的索引仍然存在';
    END IF;
    RAISE NOTICE 'V0420 down 完成：biz_inventory_in_transit 与索引已移除';
END $$;
