-- ============================================================================
-- V0413__fx_ledger_report.down（回滚 M1-08 种的权限码）
--
-- 只删「本迁移自己种的东西」：角色授权 + 菜单页面行。不碰表结构（本迁移没建表、没加列），
-- 也不碰其它报表页的节点。
--
-- 为什么不做「有数据就拒绝」的护栏：本迁移没有任何业务数据落地（外币明细账是凭证的派生
-- 视图，不落快照），删掉权限节点不会丢一行账。删完 SUPER_ADMIN 仍有 `*:*:*` 通配，
-- 系统不会因为少了这个码而登不进去。
-- ============================================================================

DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000001760'::uuid;

DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000001760'::uuid
   AND auth_code = 'business:erp:report:fx-ledger';

DO $$
DECLARE
    n integer;
BEGIN
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001760'::uuid
       AND deleted_at IS NULL;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0413 down 自检失败：菜单节点没删干净（剩 % 行）', n;
    END IF;
    SELECT count(*) INTO n
      FROM sys_role_menu
     WHERE menu_id = '20000000-0000-0000-0000-000000001760'::uuid;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0413 down 自检失败：角色授权没删干净（剩 % 行）', n;
    END IF;
    RAISE NOTICE 'V0413 down 完成：外币明细账的权限节点与授权已清除';
END $$;
