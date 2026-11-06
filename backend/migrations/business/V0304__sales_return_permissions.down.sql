-- ======================================================
-- V0304 回滚：删掉销售退货单的 2 个按钮 + 1 个页面节点（及其角色授权行）
--
-- 顺序必须是「先授权行、再子菜单、最后父菜单」（`sys_role_menu` 与 `sys_menu.parent_id`
-- 都是硬引用；先删父会把子行挂成孤儿）。
--
-- 不删权限码的**历史使用痕迹**：`biz_status_log` / `sys_operation_log` 里已有的
-- `business:erp:sales-return:*` 是审计数据，回滚不动它们。
--
-- 断言：回滚后 3 个码在 sys_menu 里必须**都不存在**（否则说明只删了一半，
-- 留着按钮但没了页面 = 授权树上挂着一颗没有父级的按钮）。
-- ======================================================

DO $$
DECLARE
    v_page   uuid := '20000000-0000-0000-0000-000000003041'::uuid;
    v_btn_c  uuid := '20000000-0000-0000-0000-000000003042'::uuid;
    v_btn_s  uuid := '20000000-0000-0000-0000-000000003043'::uuid;
    v_left   integer;
BEGIN
    -- ① 授权行（角色勾过就需要先撤，否则 sys_role_menu 会留悬空引用）
    DELETE FROM sys_role_menu WHERE menu_id IN (v_btn_c, v_btn_s, v_page);

    -- ② 子菜单（按钮）→ ③ 父菜单（页面）
    DELETE FROM sys_menu WHERE id IN (v_btn_c, v_btn_s);
    DELETE FROM sys_menu WHERE id = v_page;

    -- ④ 断言：3 个码都不该再被任何菜单承载
    SELECT count(*) INTO v_left
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND auth_code IN ('business:erp:sales-return:list',
                         'business:erp:sales-return:create',
                         'business:erp:sales-return:status');

    IF v_left <> 0 THEN
        RAISE EXCEPTION 'V0304 回滚不完整：还有 % 行菜单承载着销售退货单权限码', v_left;
    END IF;

    RAISE NOTICE 'V0304 回滚完成：销售退货单页面节点与 2 个按钮已删（授权行一并清理）';
END $$;
