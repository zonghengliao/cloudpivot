-- ============================================================================
-- V0434__quotation_convert_permission.down.sql（回滚 M3-03 的 1 行菜单）
--
-- 顺序：先删授权行（`sys_role_menu`，本迁移不写，但别处可能已授权）→ 再删按钮。
-- 可反复执行（幂等）。
--
-- 注意：本迁移只删**菜单与授权**，不动 `biz_quotation.converted_so_id` /
-- `converted_at`（V0432 的列，回滚迁移不该抹掉「这单转过哪张订单」的历史），
-- 也不动任何销售订单。
-- 回滚本迁移之后，转单接口 **除超管外一律 403** —— 这正是 `perm_menu_gate.py`
-- 要防的状态，所以只会出现在「主动回滚」场景。
-- ============================================================================

DO $$
DECLARE
    v_menu_id uuid := '20000000-0000-0000-0000-000000004336'::uuid;   -- 转单按钮
BEGIN
    IF to_regclass('public.sys_role_menu') IS NOT NULL THEN
        DELETE FROM sys_role_menu WHERE menu_id = v_menu_id;
    END IF;

    DELETE FROM sys_menu
     WHERE id = v_menu_id
       AND menu_type = 3;

    RAISE NOTICE 'V0434 down：转单权限码菜单行已删除（授权行一并清理）';
END $$;
