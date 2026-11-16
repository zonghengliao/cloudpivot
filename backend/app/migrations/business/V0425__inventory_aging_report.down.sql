-- ============================================================================
-- V0425__inventory_aging_report.down（M2-09 · 回滚：只撤本迁移种的东西）
--
-- 顺序：先撤授权行，再删菜单行。
--
-- **不碰**：V0424 的库龄层函数（报表只是它的下游消费者，回滚报表接口不该带走计算能力）、
-- `biz_inventory_txn` / `biz_stock_in` / `biz_stock_out` / `biz_inventory` 一行不动。
-- ============================================================================

-- ① 授权行（只删本迁移那个菜单节点的授权）
DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000004250'::uuid;

-- ② 菜单行（权限点本身）
DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000004250'::uuid
   AND auth_code = 'business:erp:inventory-aging:report';
