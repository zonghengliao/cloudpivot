\pset pager off
\echo === 12 个目标流程的 def.status / is_system / 审批单数 ===
SELECT d.flow_code, d.status, d.is_system,
       (SELECT count(*) FROM biz_flow_instance i WHERE i.version_id = d.current_version_id) AS live_instances_on_current
  FROM biz_flow_def d WHERE d.deleted_at IS NULL ORDER BY d.flow_code;
\echo === 快照 nodes 数与 biz_flow_node 行数一致性（所有当前版本） ===
SELECT d.flow_code, jsonb_array_length(v.graph_snapshot -> 'nodes') AS snap_nodes,
       (SELECT count(*) FROM biz_flow_node n WHERE n.version_id = v.id) AS row_nodes
  FROM biz_flow_def d JOIN biz_flow_def_version v ON v.def_id = d.id AND v.is_current
 WHERE v.version_status = 'PUBLISHED' AND d.deleted_at IS NULL ORDER BY 1;
