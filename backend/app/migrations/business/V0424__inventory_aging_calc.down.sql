-- ============================================================================
-- V0424__inventory_aging_calc.down（M2-08 · 回滚：只撤本迁移种的东西）
--
-- 顺序：先撤授权行，再删菜单行，最后删函数（函数被删时若有别的对象依赖会报错，
-- 那正是我们想要的提醒 —— 说明下游卡（M2-09/M2-10）已经在用它，不该回滚）。
--
-- 不碰任何业务数据：`biz_inventory_txn` / `biz_stock_in` / `biz_stock_out` 一行不动。
-- ============================================================================

-- ① 授权行（只删本迁移那个菜单节点的授权）
DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000004240'::uuid;

-- ② 菜单行（权限点本身）
DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000004240'::uuid
   AND auth_code = 'business:erp:inventory-aging:list';

-- ③ 库函数
DROP FUNCTION IF EXISTS biz_inventory_aging_layers(date, uuid, uuid);
