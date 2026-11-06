-- V0280__fix_stockout_stocktake_menu_parent.up.sql
--
-- 问题：出库单菜单挂在「采购订单」下、盘点单挂在「采购管理」下，都不合理。
--   出库/盘点都是仓库动作，应归属「仓库管理」。
--   同时两个菜单的 name 是权限码（business:erp:stock-out:list）而不是中文名。
--
-- 修正：两者父菜单改为「仓库管理」，名称改中文，排到仓库管理子菜单末尾。
--
-- 影响：仅菜单归属与显示名，不动权限码（auth_code）本身，
--       所有已有的角色授权（sys_role_menu）继续有效。
--
-- 幂等：用 auth_code 定位 + 条件更新。

-- 出库单 → 仓库管理
UPDATE sys_menu SET
  parent_id  = (SELECT id FROM sys_menu WHERE name = '仓库管理' AND menu_type = 1 AND deleted_at IS NULL LIMIT 1),
  name       = '出库单',
  title      = '出库单',
  sort_order = 80
WHERE auth_code = 'business:erp:stock-out:list'
  AND deleted_at IS NULL
  AND name <> '出库单';

-- 盘点单 → 仓库管理
UPDATE sys_menu SET
  parent_id  = (SELECT id FROM sys_menu WHERE name = '仓库管理' AND menu_type = 1 AND deleted_at IS NULL LIMIT 1),
  name       = '盘点单',
  title      = '盘点单',
  sort_order = 90
WHERE auth_code = 'business:erp:stock-take:list'
  AND deleted_at IS NULL
  AND name <> '盘点单';
