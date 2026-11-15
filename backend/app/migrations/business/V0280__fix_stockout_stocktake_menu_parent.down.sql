-- V0280__fix_stockout_stocktake_menu_parent.down.sql
--
-- 回滚：恢复到修正前的归属
--   出库单 → 采购订单（sort_order 7）
--   盘点单 → 采购管理（sort_order 90）
--   名称恢复为权限码

UPDATE sys_menu SET
  parent_id  = (SELECT id FROM sys_menu WHERE name = '采购订单' AND deleted_at IS NULL LIMIT 1),
  name       = 'business:erp:stock-out:list',
  title      = '出库单',
  sort_order = 7
WHERE auth_code = 'business:erp:stock-out:list'
  AND deleted_at IS NULL;

UPDATE sys_menu SET
  parent_id  = (SELECT id FROM sys_menu WHERE name = '采购管理' AND menu_type = 1 AND deleted_at IS NULL LIMIT 1),
  name       = 'business:erp:stock-take:list',
  title      = '盘点单',
  sort_order = 90
WHERE auth_code = 'business:erp:stock-take:list'
  AND deleted_at IS NULL;
