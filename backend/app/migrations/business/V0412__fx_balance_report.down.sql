-- ============================================================================
-- V0412__fx_balance_report（down）—— 只回收 M1-07 种的权限节点与授权
--
-- 本迁移**没有业务数据落地**（外币余额表是凭证的派生视图，不建表、不落快照），
-- 所以回滚不需要「有数据就拒绝」的护栏：删掉节点不丢账、不丢配置。
--
-- 顺序：先删授权（sys_role_menu 有外键指向 sys_menu），再删菜单行。
-- 删完 SUPER_ADMIN 仍靠 `*:*:*` 通配访问；FINANCE_MANAGER 失去这张报表的入口，
-- 这是回滚的应有语义（迁移退回去 = 功能退回去）。
-- ============================================================================

DO $$
BEGIN
    IF to_regclass('public.sys_menu') IS NULL THEN
        RAISE NOTICE 'V0412(down)：sys_menu 不存在，无可回收';
        RETURN;
    END IF;

    DELETE FROM sys_role_menu
     WHERE menu_id = '20000000-0000-0000-0000-000000001750'::uuid;

    DELETE FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001750'::uuid;

    -- 兜底：万一有人把同一权限码种成了别的 id（重放/手工插入），按码一并回收，
    -- 否则「迁移已回滚但权限码还在」会让下一张卡的自检与菜单巡检对不上。
    DELETE FROM sys_role_menu
     WHERE menu_id IN (
            SELECT id FROM sys_menu
             WHERE auth_code = 'business:erp:report:fx-balance'
               AND path = '/business/erp/report/fx-balance'
          );
    DELETE FROM sys_menu
     WHERE auth_code = 'business:erp:report:fx-balance'
       AND path = '/business/erp/report/fx-balance';

    RAISE NOTICE 'V0412(down)：外币余额表权限节点与授权已回收';
END $$;
