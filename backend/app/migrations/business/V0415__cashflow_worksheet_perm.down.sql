-- ============================================================================
-- V0415__cashflow_worksheet_perm（down）—— 回收 M1-14 种的现金流底稿权限码
--
-- 回滚语义：权限码退回去 = 底稿功能下线（接口还在，但除超管 `*:*:*` 通配外没人拿得到码）。
--
-- 【护栏】底稿表里**已有行**时拒绝回滚。
--   底稿是编制结果：留着数据却把所有人的码收走，等于「数据在、谁都改不了、也删不掉」，
--   只能由工程师改库 —— 这与 AGENTS.md 约定 11「不许留死单」是同一条原则。
--   要真回滚：先确认底稿已导出 / 已由业务作废（`DELETE FROM biz_cashflow_worksheet WHERE period_code = …`），
--   再重跑本 down。与 V0414 down 的护栏同口径。
--
-- 顺序：先删授权（`sys_role_menu` 有外键指向 `sys_menu`），再删菜单行；
--   末尾按 auth_code 兜底回收（防有人把同码种成别的 id）。
-- ============================================================================

DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('public.sys_menu') IS NULL THEN
        RAISE NOTICE 'V0415(down)：sys_menu 不存在，无可回收';
        RETURN;
    END IF;

    -- ===== 护栏：有底稿数据就拒绝回滚 =====
    IF to_regclass('public.biz_cashflow_worksheet') IS NOT NULL THEN
        SELECT count(*) INTO n FROM biz_cashflow_worksheet;
        IF n > 0 THEN
            RAISE EXCEPTION 'V0415(down) 拒绝回滚：biz_cashflow_worksheet 已有 % 行底稿。'
                '回滚权限码会让这些底稿变成「数据在、谁都改不了」的死数据 —— '
                '请先确认底稿已导出 / 作废（DELETE FROM biz_cashflow_worksheet），再重跑本 down', n;
        END IF;
    END IF;

    DELETE FROM sys_role_menu
     WHERE menu_id IN ('20000000-0000-0000-0000-000000001770'::uuid,
                       '20000000-0000-0000-0000-000000001780'::uuid);

    DELETE FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001770'::uuid,
                  '20000000-0000-0000-0000-000000001780'::uuid);

    -- 兜底：同码不同 id 的残留行一并回收（否则「迁移已回滚但码还在」会让菜单巡检对不上）
    DELETE FROM sys_role_menu
     WHERE menu_id IN (
            SELECT id FROM sys_menu
             WHERE auth_code IN ('business:erp:cashflow-worksheet:list',
                                 'business:erp:cashflow-worksheet:save')
          );
    DELETE FROM sys_menu
     WHERE auth_code IN ('business:erp:cashflow-worksheet:list',
                         'business:erp:cashflow-worksheet:save');

    RAISE NOTICE 'V0415(down)：现金流底稿权限码与授权已回收';
END $$;
