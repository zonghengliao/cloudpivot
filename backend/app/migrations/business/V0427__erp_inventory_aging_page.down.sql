-- ============================================================================
-- V0427__erp_inventory_aging_page.down（M2-12 回滚）
--
-- 只删本迁移种下的东西，不碰 V0424 / V0425 / V0426 的任何行（库函数、阈值参数、
-- 三个取数按钮行与它们的授权都保留）：
--   ① 页面行的角色授权（`sys_role_menu.menu_id` 有外键指向 `sys_menu`，必须先删授权）；
--   ② 因本迁移才拿到「仓库管理」目录的角色（父目录下已无其它授权 ⇒ 是空壳，一并清掉，
--      免得侧栏留一个点开是空的目录）；
--   ③ 页面菜单行本身。
--
-- 幂等：再跑一遍全部 `DELETE 0`（行已不存在）。
-- 反向影响：库龄页下线 —— 三个只读接口与它们的取数码都还在，超管仍能直接访问路由；
--   其它角色因拿不到页面码，侧栏看不到入口。
-- ============================================================================

-- ① 逐个角色清理（先取角色，再删页面授权，最后按「父目录下是否还有其它授权」决定要不要撤父目录）
DO $$
DECLARE
    page_id uuid := '20000000-0000-0000-0000-000000004270'::uuid;
    dir_id  uuid := '20000000-0000-0000-0000-000000000e01'::uuid;
    rec     record;
BEGIN
    IF to_regclass('public.sys_role_menu') IS NULL OR to_regclass('public.sys_menu') IS NULL THEN
        RAISE NOTICE 'V0427 down：sys_menu / sys_role_menu 不存在，无事可做';
        RETURN;
    END IF;

    FOR rec IN SELECT DISTINCT role_id FROM sys_role_menu WHERE menu_id = page_id LOOP
        DELETE FROM sys_role_menu WHERE role_id = rec.role_id AND menu_id = page_id;

        -- 父目录：该角色在「仓库管理」子树下已无任何其它授权时撤掉（本迁移可能给它补的这一行）
        DELETE FROM sys_role_menu
         WHERE role_id = rec.role_id
           AND menu_id = dir_id
           AND NOT EXISTS (
                SELECT 1
                  FROM sys_role_menu x
                  JOIN sys_menu m ON m.id = x.menu_id
                 WHERE x.role_id = rec.role_id
                   AND m.parent_id = dir_id
           );
    END LOOP;
END $$;

-- ② 兜底：仍挂在本页面行上的授权（理论上已被 ① 清空，防手工插过的行）
DELETE FROM sys_role_menu WHERE menu_id = '20000000-0000-0000-0000-000000004270'::uuid;

-- ③ 菜单行
DELETE FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000004270'::uuid;
