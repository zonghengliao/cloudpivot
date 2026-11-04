-- V0227 down：删预算四表与权限/菜单。
--
-- ⚠️ 有单据在跑（SUBMITTED）时**拒绝**回滚：删表会把在跑的审批单变成指向空气的孤儿
--    （与 V0216 的「有单据跑在 v2 上就不许回滚」同口径）。
DO $$
DECLARE
    running int;
BEGIN
    IF to_regclass('public.biz_budget') IS NOT NULL THEN
        SELECT count(*) INTO running FROM biz_budget WHERE status = 'SUBMITTED' AND deleted_at IS NULL;
        IF running > 0 THEN
            RAISE EXCEPTION 'V0227 拒绝回滚：还有 % 张预算单在审批中（先结束审批再回滚）', running;
        END IF;
        SELECT running + count(*) INTO running FROM biz_budget_usage WHERE status = 'OCCUPIED';
        IF running > 0 THEN
            RAISE EXCEPTION 'V0227 拒绝回滚：还有 % 条预算占用未释放（先取消/关闭来源单）', running;
        END IF;
    END IF;
END $$;

DROP TABLE IF EXISTS biz_budget_usage;
DROP TABLE IF EXISTS biz_budget_period;
DROP TABLE IF EXISTS biz_budget_subject_map;
DROP TABLE IF EXISTS biz_budget;

DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000001300'::uuid,
                   '20000000-0000-0000-0000-000000001301'::uuid,
                   '20000000-0000-0000-0000-000000001302'::uuid,
                   '20000000-0000-0000-0000-000000001303'::uuid,
                   '20000000-0000-0000-0000-000000001311'::uuid,
                   '20000000-0000-0000-0000-000000001312'::uuid,
                   '20000000-0000-0000-0000-000000001313'::uuid,
                   '20000000-0000-0000-0000-000000001314'::uuid,
                   '20000000-0000-0000-0000-000000001315'::uuid);

DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000001300'::uuid,
              '20000000-0000-0000-0000-000000001301'::uuid,
              '20000000-0000-0000-0000-000000001302'::uuid,
              '20000000-0000-0000-0000-000000001303'::uuid,
              '20000000-0000-0000-0000-000000001311'::uuid,
              '20000000-0000-0000-0000-000000001312'::uuid,
              '20000000-0000-0000-0000-000000001313'::uuid,
              '20000000-0000-0000-0000-000000001314'::uuid,
              '20000000-0000-0000-0000-000000001315'::uuid);
