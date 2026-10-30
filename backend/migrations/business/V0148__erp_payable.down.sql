-- V0148 down：删除应付单表（本迁移只建了 biz_payable，别的表一律不碰）
--
-- 回滚边界（给评审的一眼可见断言）：
--   * 只 DROP `biz_payable`；
--   * 凭证三表属 V0149（`biz_voucher` / `biz_voucher_entry` / `biz_voucher_rule`），
--     本 down 不许连带删 —— 回滚 V0148 而留下 V0149 时，biz_voucher 必须还在。
DO $$
DECLARE
    n BIGINT;
BEGIN
    IF to_regclass('public.biz_payable') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_payable' INTO n;
        RAISE NOTICE 'V0148 down：删除 biz_payable（% 行）', n;
    END IF;
END $$;

DROP TABLE IF EXISTS biz_payable;

DO $$
BEGIN
    IF to_regclass('public.biz_payable') IS NOT NULL THEN
        RAISE EXCEPTION 'V0148 down 失败：biz_payable 仍存在';
    END IF;
    RAISE NOTICE 'V0148 down 完成：biz_payable 零残留（biz_voucher* 三表未受影响）';
END $$;
