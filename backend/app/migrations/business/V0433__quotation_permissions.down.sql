-- ============================================================================
-- V0433__quotation_permissions.down.sql（回滚 M3-02 的 5 行菜单）
--
-- 顺序：先删授权行（`sys_role_menu`，本迁移不写，但别处可能已授权）→ 再删按钮 → 最后删页面。
-- 全部 `DELETE … WHERE id IN (…)`，可反复执行（幂等）。
--
-- 注意：本迁移只删**菜单与授权**，不动 `biz_quotation` / `biz_quotation_line`
-- （那是 V0432 的事，它自己的 down 有「已有非作废报价单时拒绝回滚」的护栏）。
-- 回滚本迁移之后，`quotation_handler` 的 5 个接口**除超管外一律 403**
-- —— 这正是 `perm_menu_gate.py` 要防的状态，所以只会出现在「主动回滚」场景。
-- ============================================================================

DO $$
DECLARE
    v_menu_ids uuid[] := ARRAY[
        '20000000-0000-0000-0000-000000004331'::uuid,   -- 页面
        '20000000-0000-0000-0000-000000004332'::uuid,   -- create
        '20000000-0000-0000-0000-000000004333'::uuid,   -- update
        '20000000-0000-0000-0000-000000004334'::uuid,   -- delete
        '20000000-0000-0000-0000-000000004335'::uuid    -- status
    ];
BEGIN
    IF to_regclass('public.sys_role_menu') IS NOT NULL THEN
        DELETE FROM sys_role_menu WHERE menu_id = ANY(v_menu_ids);
    END IF;

    -- 按钮先删（外键 / 逻辑上都从叶子开始）
    DELETE FROM sys_menu
     WHERE id = ANY(v_menu_ids)
       AND menu_type = 3;

    DELETE FROM sys_menu
     WHERE id = ANY(v_menu_ids)
       AND menu_type = 2;

    RAISE NOTICE 'V0433 down：报价单 5 行菜单已删除（授权行一并清理）';
END $$;
