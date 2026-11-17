-- ============================================================================
-- V0439 down：回滚销售返利结算单（M3-16）
--
-- 顺序：护栏 → 菜单（先 sys_role_menu）→ biz_seq(RB) → 表
--
-- 护栏（**回滚必须比前进更保守**）：
--   表里已有**未作废**的结算单（`status <> 'CANCELLED'`）时拒绝回滚。
--   理由：结算单是「已经算出来、并且可能已经转成应付/红字发票的返利金额」。
--   删表等于把这笔债务从系统里抹掉，而它在下游（应付 / 凭证）里是真实存在的
--   —— 宁可让人先手工确认「这些结算单都不要了」，也不由迁移替人做这个决定。
--   已作废（CANCELLED）的行不拦：它们本来就是「不算数」的留痕。
-- ============================================================================

DO $$
BEGIN
    IF to_regclass('biz_sales_rebate_settlement') IS NOT NULL THEN
        PERFORM 1 FROM biz_sales_rebate_settlement WHERE status <> 'CANCELLED' LIMIT 1;
        IF FOUND THEN
            RAISE EXCEPTION 'V0439 down 拒绝执行：biz_sales_rebate_settlement 里还有未作废的返利结算单'
                            '（可能已转应付/红字发票）。请先确认这些结算单作废或归档后再回滚';
        END IF;
    END IF;
END $$;

-- 菜单：先解绑角色，再删行（sys_role_menu 有外键 → sys_menu.id）
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000004390'::uuid,
                   '20000000-0000-0000-0000-000000004391'::uuid,
                   '20000000-0000-0000-0000-000000004392'::uuid);

DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000004390'::uuid,
              '20000000-0000-0000-0000-000000004391'::uuid,
              '20000000-0000-0000-0000-000000004392'::uuid);

-- 号段：**无条件删本卡的 RB 行**（不要加 `current_no = 0` 之类条件 ——
-- 取过号之后 current_no 就不是 0 了，带条件会漏删，随后自检报「biz_seq(RB) 仍有行」，
-- 整个 down 因自检失败而回滚，落得「想回滚却回滚不掉」）。
-- 上面已保证没有未作废结算单，此时删号段不会让谁的单号失去解释。
DELETE FROM biz_seq WHERE seq_type = 'RB';

-- 表
DROP TABLE IF EXISTS biz_sales_rebate_settlement;

-- 回滚后自检：表 / 菜单 / 号段都不该在
DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('biz_sales_rebate_settlement') IS NOT NULL THEN
        RAISE EXCEPTION 'V0439 down 自检失败：biz_sales_rebate_settlement 仍存在';
    END IF;

    SELECT count(*) INTO n FROM sys_menu
     WHERE auth_code LIKE 'business:erp:rebate-settlement:%';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0439 down 自检失败：返利结算菜单仍有 % 行', n;
    END IF;

    SELECT count(*) INTO n FROM biz_seq WHERE seq_type = 'RB';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0439 down 自检失败：biz_seq(RB) 仍有 % 行', n;
    END IF;

    RAISE NOTICE 'V0439 down 完成：表 / 菜单 / 号段均已清理';
END $$;
