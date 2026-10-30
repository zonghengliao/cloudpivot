-- V0143 回滚：删掉库存巡检按钮与它的角色授权。
--
-- 顺序：先删 `sys_role_menu`（无 ON DELETE CASCADE），再删 `sys_menu`。
-- 只删本迁移种的固定 UUID（1 个节点），不动运维后来手工加的其它菜单。
-- 权限码本身没有独立表（`sys_menu.auth_code` 即来源），删菜单即删码。
-- 不动任何业务表：本迁移只种权限。

DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000000986'::uuid;

DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000000986'::uuid;

-- 零残留自检
DO $$
DECLARE
    n_left integer;
BEGIN
    SELECT count(*) INTO n_left
      FROM sys_menu
     WHERE auth_code = 'business:erp:inventory:audit';
    IF n_left <> 0 THEN
        RAISE EXCEPTION 'V0143 down 失败：还剩 % 个巡检菜单节点', n_left;
    END IF;
    SELECT count(*) INTO n_left
      FROM sys_role_menu
     WHERE menu_id = '20000000-0000-0000-0000-000000000986'::uuid;
    IF n_left <> 0 THEN
        RAISE EXCEPTION 'V0143 down 失败：还剩 % 条角色授权', n_left;
    END IF;
    RAISE NOTICE 'V0143 down 通过：巡检按钮 / 授权零残留';
END $$;
