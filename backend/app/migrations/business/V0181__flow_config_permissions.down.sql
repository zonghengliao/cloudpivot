-- ======================================================
-- V0181 回滚：撤掉流程配置化的权限点与授权
--
-- 按 `auth_code` 删（不按 UUID 硬编码），并先删授权再删菜单（FK 顺序）。
-- 全程可重复执行，零残留。
-- ======================================================

-- ---------- ① 先撤授权 ----------
DELETE FROM sys_role_menu
 WHERE menu_id IN (
     SELECT id FROM sys_menu WHERE auth_code = 'business:flow:tasknode:read'
 );

-- ---------- ② 再删菜单（软删之后硬删；本迁移种的行还没被任何人引用）----------
DELETE FROM sys_menu WHERE auth_code = 'business:flow:tasknode:read';

-- ---------- ③ 自检 ----------
DO $$
DECLARE
    left_rows int;
BEGIN
    SELECT count(*) INTO left_rows
      FROM sys_menu
     WHERE auth_code = 'business:flow:tasknode:read';
    IF left_rows <> 0 THEN
        RAISE EXCEPTION 'V0181 down 自检失败：还有 % 行 business:flow:tasknode:read 残留', left_rows;
    END IF;
    RAISE NOTICE 'V0181 down 自检通过：tasknode 权限点与授权全部撤除';
END
$$;
