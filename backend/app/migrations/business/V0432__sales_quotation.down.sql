-- ============================================================================
-- V0432__sales_quotation.down（销售报价单两张表 + QT 取号行回滚 · M3-01 · 2026-11-17）
--
-- 幂等：`DROP TABLE IF EXISTS` —— 重复执行不报错（第二次什么都不做）。
--
-- 护栏（与 V0420 在途库存账 down 同口径）：
--   表里只要还有**非作废**的报价单行（status <> 'CANCELLED'），就拒绝回滚。
--   理由：报价单是**客户认过的价**（报了什么价、什么有效期、哪几行），
--   删表之后「当时报了多少」在系统里再无痕迹，且无法从别的表重建
--   （销售订单只说成交价，不说报过哪些、被拒的是哪几单）。
--   要先回滚，请走业务把报价单作废（status=CANCELLED 就放行）。
--
-- 为什么 CANCELLED 行不拦：作废单是纯审计痕迹（我方撤回/报错重来），
-- 删表不会让任何业务口径变错 —— 与「还有客户在用的在途报价」是两件事。
--
-- 取号行：QT 规则一并删除（表都没了，留着规则只会让下次重建时号段错位）。
-- ============================================================================
DO $$
DECLARE
    n BIGINT;
BEGIN
    IF to_regclass('public.biz_quotation') IS NULL THEN
        RAISE NOTICE 'V0432 down: biz_quotation 不存在，无需回滚（幂等）';
    ELSE
        SELECT count(*) INTO n
          FROM biz_quotation
         WHERE status <> 'CANCELLED';
        IF n > 0 THEN
            RAISE EXCEPTION 'V0432 down 拒绝执行：biz_quotation 还有 % 张非作废报价单（先走业务作废，作废单可随表删除）', n;
        END IF;
        RAISE NOTICE 'V0432 down: 表内只剩作废单（或空表），开始回滚';
    END IF;
END $$;

-- 明细先删（外键是 CASCADE，但显式两句让「各表都真的没了」可读）
DROP TABLE IF EXISTS biz_quotation_line;
DROP TABLE IF EXISTS biz_quotation;

-- 取号行：按 seq_type 删（幂等；deleted_at 一并认，避免历史软删行留成"半个规则"）
DELETE FROM biz_seq WHERE seq_type = 'QT' AND deleted_at IS NULL;

-- 收尾自检：表和索引必须真的没了（只 NOTICE 不 EXCEPTION 等于没验）
DO $$
BEGIN
    IF to_regclass('public.biz_quotation') IS NOT NULL THEN
        RAISE EXCEPTION 'V0432 down 失败：biz_quotation 仍然存在';
    END IF;
    IF to_regclass('public.biz_quotation_line') IS NOT NULL THEN
        RAISE EXCEPTION 'V0432 down 失败：biz_quotation_line 仍然存在';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_indexes
                WHERE schemaname = 'public'
                  AND tablename IN ('biz_quotation', 'biz_quotation_line')) THEN
        RAISE EXCEPTION 'V0432 down 失败：biz_quotation / biz_quotation_line 的索引仍然存在';
    END IF;
    IF EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'QT' AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0432 down 失败：biz_seq(seq_type=QT) 取号行仍然存在';
    END IF;
    RAISE NOTICE 'V0432 down 完成：biz_quotation / biz_quotation_line 与 QT 取号行已移除';
END $$;
