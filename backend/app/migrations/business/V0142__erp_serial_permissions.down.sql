-- V0142 回滚：删掉序列号菜单 / 按钮与它们的角色授权。
--
-- 顺序：先删 `sys_role_menu`（无 ON DELETE CASCADE），再删 `sys_menu`
--       （按钮先删、菜单后删 —— 按钮的 parent_id 指向菜单，虽然本仓建表没写
--         ON DELETE RESTRICT，但按依赖顺序删更稳，也便于自检）。
-- 只删本迁移种的固定 UUID（5 个节点），不动运维后来手工加的其它菜单。
-- 权限码本身没有独立表（`sys_menu.auth_code` 即来源），删菜单即删码。
-- 不动 `biz_material_serial` 与数据：本迁移只种权限，回滚不碰业务表。

DELETE FROM sys_role_menu
 WHERE menu_id IN (
     '20000000-0000-0000-0000-000000000981',
     '20000000-0000-0000-0000-000000000982',
     '20000000-0000-0000-0000-000000000983',
     '20000000-0000-0000-0000-000000000984',
     '20000000-0000-0000-0000-000000000985'
 );

-- 按钮（menu_type = 3）在前
DELETE FROM sys_menu
 WHERE id IN (
     '20000000-0000-0000-0000-000000000982',
     '20000000-0000-0000-0000-000000000983',
     '20000000-0000-0000-0000-000000000984',
     '20000000-0000-0000-0000-000000000985'
 );

-- 菜单（menu_type = 2）在后
DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000000981'::uuid;

-- 零残留自检
DO $$
DECLARE
    n_left integer;
BEGIN
    SELECT count(*) INTO n_left
      FROM sys_menu
     WHERE auth_code LIKE 'business:erp:serial:%'
       AND id::text LIKE '20000000-0000-0000-0000-00000000098%';
    IF n_left <> 0 THEN
        RAISE EXCEPTION 'V0142 down 失败：还剩 % 个序列号菜单节点', n_left;
    END IF;
    SELECT count(*) INTO n_left
      FROM sys_role_menu
     WHERE menu_id::text LIKE '20000000-0000-0000-0000-00000000098%';
    IF n_left <> 0 THEN
        RAISE EXCEPTION 'V0142 down 失败：还剩 % 条角色授权', n_left;
    END IF;
    RAISE NOTICE 'V0142 down 通过：序列号菜单 / 按钮 / 授权零残留';
END $$;
