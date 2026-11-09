\echo '--- biz_flow_def ---'
SELECT id, flow_code, biz_type, flow_name, status, current_version FROM biz_flow_def WHERE flow_code IN ('stock_transfer','stock_take_post') ;
\echo '--- 节点 ---'
SELECT d.flow_code, v.version, n.node_key, n.node_name, n.node_type, n.sort_order
  FROM biz_flow_node n
  JOIN biz_flow_def_version v ON v.id = n.version_id
  JOIN biz_flow_def d ON d.id = v.def_id
 WHERE d.flow_code IN ('stock_transfer','stock_take_post') ORDER BY d.flow_code, v.version DESC, n.sort_order;
\echo '--- 节点指派 ---'
SELECT d.flow_code, n.node_key, a.assignee_type, a.assignee_id, r.role_code, a.strategy
  FROM biz_flow_node_assignee a
  JOIN biz_flow_node n ON n.id = a.node_id
  JOIN biz_flow_def_version v ON v.id = n.version_id
  JOIN biz_flow_def d ON d.id = v.def_id
  LEFT JOIN sys_role r ON r.id = a.assignee_id
 WHERE d.flow_code IN ('stock_transfer','stock_take_post') ORDER BY d.flow_code, n.node_key;
\echo '--- 状态机边 ---'
SELECT entity, from_status, to_status, enabled, requires_reason, auth_code FROM biz_flow_state_edge
 WHERE entity IN ('STOCK_TRANSFER','STOCK_TAKE','STOCK_IN','STOCK_OUT','TRANSFER') ORDER BY entity, from_status, to_status;
