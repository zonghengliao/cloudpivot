\echo '--- 库存相关流程定义 ---'
SELECT id, flow_code, biz_type, flow_name, status, current_version_no, is_system
  FROM biz_flow_def WHERE flow_code IN ('stock_transfer','stock_take_post','stock_in_post') OR biz_type LIKE 'STOCK%';
\echo '--- 节点 + 指派 ---'
SELECT d.flow_code, v.version_no, n.node_key, n.node_name, n.node_type, n.assignee_type,
       n.empty_assignee, n.self_approval,
       (SELECT string_agg(COALESCE(r.role_code, a.ref_id::text), '+' ORDER BY a.sort_order)
          FROM biz_flow_node_assignee a LEFT JOIN sys_role r ON r.id = a.ref_id
         WHERE a.node_id = n.id) AS assignees
  FROM biz_flow_node n
  JOIN biz_flow_def_version v ON v.id = n.version_id
  JOIN biz_flow_def d ON d.id = v.def_id
 WHERE d.flow_code IN ('stock_transfer','stock_take_post') AND v.is_current
 ORDER BY d.flow_code, n.sort_order;
