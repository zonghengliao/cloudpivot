-- V0156：去掉 ERP 顶级目录，4 个业务目录提升为顶级
--
-- 老板要求（2026-10-31 08:30）：
--   「不需要ERP那个菜单目录。把采购管理、仓库管理、财务管理、基础数据放到根目录」
--
-- 改前：ERP > {采购管理, 仓库管理, 财务管理, 基础数据}
-- 改后：{采购管理, 仓库管理, 财务管理, 基础数据} 直接挂根级

BEGIN;

-- ① 4 个业务目录提升为顶级（parent_id = NULL），sort_order 避开与现有顶级冲突
UPDATE sys_menu SET parent_id = NULL, sort_order = 62, updated_at = now()
WHERE id = '20000000-0000-0000-0000-000000000946';   -- 采购管理（原 60 与工作任务冲突）

UPDATE sys_menu SET parent_id = NULL, sort_order = 66, updated_at = now()
WHERE id = '20000000-0000-0000-0000-000000000e01';   -- 仓库管理

UPDATE sys_menu SET parent_id = NULL, sort_order = 72, updated_at = now()
WHERE id = '20000000-0000-0000-0000-000000000e02';   -- 财务管理（原 70 与 content 冲突）

UPDATE sys_menu SET parent_id = NULL, sort_order = 76, updated_at = now()
WHERE id = '20000000-0000-0000-0000-000000000e03';   -- 基础数据（原 75 与通用底座冲突）

-- ② 删掉 ERP 目录（0940）：先删授权，再删菜单
DELETE FROM sys_role_menu WHERE menu_id = '20000000-0000-0000-0000-000000000940';
DELETE FROM sys_menu
WHERE id = '20000000-0000-0000-0000-000000000940'
  AND NOT EXISTS (
    SELECT 1 FROM sys_menu
    WHERE parent_id = '20000000-0000-0000-0000-000000000940' AND deleted_at IS NULL
  );

COMMIT;

SELECT sort_order, name, path, menu_type
FROM sys_menu WHERE parent_id IS NULL AND deleted_at IS NULL
ORDER BY sort_order;
