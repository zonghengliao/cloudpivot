-- ============================================================================
-- V0421 down（M2-04 · 在途库存查询权限码）：只删本迁移种的东西
--
-- 删什么：① 本码的全部授权行（`sys_role_menu.menu_id = 4210`）
--         ② 权限点本身（`sys_menu.id = 4210`，menu_type=3）
-- 不删什么：任何业务数据（在途账 / 发货单 / 三张主数据一律不碰）；
--           不动 `business:erp:inventory:list`（那是 V0141 的，与本码无关）。
--
-- 幂等：两条 `DELETE` 都是条件删；重放第二遍影响 0 行。
--
-- 顺序不能反：先删授权行再删菜单行 —— 虽然 `sys_role_menu.menu_id` 是否有
-- `ON DELETE CASCADE` 取决于 V0045 的定义，显式两步在两种定义下都对。
-- ============================================================================

DO $$
DECLARE
    n_menu integer;
BEGIN
    SELECT count(*) INTO n_menu
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004210'::uuid;

    -- 护栏：如果这个 id 上挂的不是**本卡种的那个码**，立刻停手（说明 id 段被人抢用）
    IF n_menu > 0 AND NOT EXISTS (
        SELECT 1 FROM sys_menu
         WHERE id = '20000000-0000-0000-0000-000000004210'::uuid
           AND auth_code = 'business:erp:inventory-transit:list'
    ) THEN
        RAISE EXCEPTION 'V0421 down：4210 节点上的 auth_code 不是 business:erp:inventory-transit:list，拒绝回滚（id 段冲突）';
    END IF;

    -- 还有代码在用这个码时拒绝回滚（`handler.rs` 的两个 GET 注解），避免回滚后接口 403。
    -- 判据不查源码（迁移跑在库侧），只提示：回滚前确认 inventory_transit 模块已下线。
    IF n_menu > 0 THEN
        RAISE NOTICE 'V0421 down：删除权限点 business:erp:inventory-transit:list —— '
                     '请确认 backend 的 inventory_transit::handler 两个 GET 已一并下线，否则接口将 403';
    END IF;
END $$;

DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000004210'::uuid;

DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000004210'::uuid
   AND auth_code = 'business:erp:inventory-transit:list';

-- 自检：码与授权行都不复存在（`sys_role_menu` 由上面第一条清干净）
DO $$
DECLARE
    n_left integer;
BEGIN
    SELECT (SELECT count(*) FROM sys_menu
             WHERE auth_code = 'business:erp:inventory-transit:list')
         + (SELECT count(*) FROM sys_role_menu
             WHERE menu_id = '20000000-0000-0000-0000-000000004210'::uuid)
      INTO n_left;
    IF n_left <> 0 THEN
        RAISE EXCEPTION 'V0421 down 自检失败：仍残留 % 行（菜单 / 授权）', n_left;
    END IF;
    RAISE NOTICE 'V0421 down 完成：权限点与授权行已清除（业务数据未动）';
END $$;
