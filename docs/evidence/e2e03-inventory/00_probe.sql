-- E2E-03 前置探查（只读）
\echo '--- 角色 ---'
SELECT id, role_code, role_name FROM sys_role WHERE deleted_at IS NULL ORDER BY role_code;
\echo '--- demo_* 账号及其角色 ---'
SELECT u.username, u.status, u.is_locked, u.must_change_password, r.role_code
  FROM sys_user u
  LEFT JOIN sys_user_role ur ON ur.user_id = u.id
  LEFT JOIN sys_role r ON r.id = ur.role_id
 WHERE u.deleted_at IS NULL AND u.username LIKE 'demo%' ORDER BY u.username, r.role_code;
\echo '--- 审批流程 stock_transfer / stock_take_post ---'
SELECT f.id, f.flow_code, f.biz_type, f.name, f.status, f.version
  FROM biz_flow_definition f WHERE f.flow_code IN ('stock_transfer','stock_take_post') ORDER BY f.flow_code, f.version;
\echo '--- 流程节点 ---'
SELECT f.flow_code, f.version, n.node_key, n.node_name, n.node_type, n.assignee_type, n.assignee_value
  FROM biz_flow_node n JOIN biz_flow_definition f ON f.id = n.definition_id
 WHERE f.flow_code IN ('stock_transfer','stock_take_post') ORDER BY f.flow_code, n.sort_order;
\echo '--- sys_config 期初对方科目 / 相关配置 ---'
SELECT config_key, config_value FROM sys_config
 WHERE config_key LIKE '%inventory%' OR config_key LIKE '%opening%' OR config_key LIKE '%erp.%' ORDER BY config_key;
\echo '--- 库存基线 ---'
SELECT (SELECT count(*) FROM biz_inventory) AS inv_rows,
       (SELECT COALESCE(SUM(qty),0) FROM biz_inventory) AS inv_qty,
       (SELECT COALESCE(SUM(amount),0) FROM biz_inventory) AS inv_amount,
       (SELECT count(*) FROM biz_inventory_txn) AS txn_rows,
       (SELECT COALESCE(SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END),0) FROM biz_inventory_txn) AS txn_qty;
\echo '--- 已有 E2E03 痕迹（应为空）---'
SELECT id, warehouse_code FROM biz_warehouse WHERE warehouse_code LIKE 'E2E03%';
SELECT id, material_code, serial_required FROM biz_material WHERE material_code LIKE 'E2E03%';
\echo '--- 状态机边（库存相关）---'
SELECT biz_type, from_status, to_status, enabled FROM biz_flow_state_edge
 WHERE biz_type IN ('STOCK_TRANSFER','STOCK_TAKE','STOCK_IN','STOCK_OUT') ORDER BY biz_type, from_status, to_status;
\echo '--- 表结构：biz_opening_inventory ---'
SELECT column_name, data_type FROM information_schema.columns WHERE table_name='biz_opening_inventory' ORDER BY ordinal_position;
