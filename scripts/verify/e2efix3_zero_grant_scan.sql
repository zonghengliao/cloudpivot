-- 零授予扫描：所有 business 模块下 {create,list,status} 权限码 → 非超管角色授予情况
-- 口径：按 auth_code 聚合（同一码可能同时挂在页面行与按钮行上），任一 menu 行被非超管角色持有即算「有授予」。
WITH codes AS (
    SELECT DISTINCT m.auth_code
      FROM sys_menu m
     WHERE m.deleted_at IS NULL
       AND m.module_code = 'business'
       AND coalesce(m.auth_code, '') ~ ':(create|list|status)$'
)
SELECT c.auth_code,
       coalesce(
         (SELECT string_agg(DISTINCT r.role_code, ',' ORDER BY r.role_code)
            FROM sys_menu m
            JOIN sys_role_menu rm ON rm.menu_id = m.id
            JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
           WHERE m.auth_code = c.auth_code AND m.deleted_at IS NULL
             AND r.role_code <> 'SUPER_ADMIN'),
         '<ZERO>') AS non_super_roles
  FROM codes c
 ORDER BY (coalesce(
         (SELECT string_agg(DISTINCT r.role_code, ',' ORDER BY r.role_code)
            FROM sys_menu m
            JOIN sys_role_menu rm ON rm.menu_id = m.id
            JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
           WHERE m.auth_code = c.auth_code AND m.deleted_at IS NULL
             AND r.role_code <> 'SUPER_ADMIN'), '<ZERO>') = '<ZERO>') DESC, c.auth_code;
