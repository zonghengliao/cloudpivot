-- ============================================================================
-- V0417__cashflow_statement_perm.down（M1-16 · 回滚出表两个权限码）
--
-- 顺序不能反：`sys_role_menu.menu_id` 有外键指向 `sys_menu`，先删授权再删菜单。
-- 幂等：重复执行不报错（`DELETE` 天然幂等；菜单行不存在时什么都不删）。
--
-- 回滚语义：**出表功能下线**。接口代码仍在（`cashflow_statement.rs`），但两个码
-- 谁都拿不到 ⇒ 除 SUPER_ADMIN（`*:*:*` 短路）外没人能调 GET / export。
-- 本迁移不新增业务数据，故无数据护栏；注意已导出到系统外的 xlsx 不受回滚影响。
-- ============================================================================
DO $$
BEGIN
    IF to_regclass('public.sys_menu') IS NULL OR to_regclass('public.sys_role_menu') IS NULL THEN
        RAISE NOTICE 'V0417 down: sys_menu / sys_role_menu 不存在，跳过';
        RETURN;
    END IF;

    DELETE FROM sys_role_menu
     WHERE menu_id IN ('20000000-0000-0000-0000-000000001810'::uuid,
                       '20000000-0000-0000-0000-000000001820'::uuid);

    DELETE FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001810'::uuid,
                  '20000000-0000-0000-0000-000000001820'::uuid);

    IF EXISTS (
        SELECT 1 FROM sys_menu
         WHERE id IN ('20000000-0000-0000-0000-000000001810'::uuid,
                      '20000000-0000-0000-0000-000000001820'::uuid)
    ) THEN
        RAISE EXCEPTION 'V0417 down 失败：两个出表按钮节点没删干净';
    END IF;
    RAISE NOTICE 'V0417 down 完成：出表两个权限码（含授权）已移除，出表功能对非超管下线';
END $$;
