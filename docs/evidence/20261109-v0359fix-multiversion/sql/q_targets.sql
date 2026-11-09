\pset pager off
\echo === 目标流程清单（动态发现：current PUBLISHED 版本里仍有 APPROVAL+ROLE+ALL） ===
SELECT d.flow_code, v.version_no AS cur_ver, d.current_version_no AS def_cur_no,
       (SELECT max(version_no) FROM biz_flow_def_version x WHERE x.def_id=d.id) AS max_ver,
       string_agg(n.node_key, ',' ORDER BY n.node_key) AS bad_nodes,
       (SELECT count(*) FROM biz_flow_def_version x WHERE x.def_id=d.id AND x.version_no > v.version_no) AS higher_versions
  FROM biz_flow_def d
  JOIN biz_flow_def_version v ON v.def_id=d.id AND v.is_current AND v.version_status='PUBLISHED'
  JOIN biz_flow_node n ON n.version_id=v.id AND n.node_type='APPROVAL' AND n.assignee_type='ROLE' AND n.approval_mode='ALL'
 WHERE d.deleted_at IS NULL
 GROUP BY d.id, d.flow_code, v.version_no, d.current_version_no ORDER BY d.flow_code;
\echo === 计数器：流程数 / 节点数 / ALL 节点形态统计 ===
SELECT count(DISTINCT d.id) AS flows, count(*) AS nodes
  FROM biz_flow_def d
  JOIN biz_flow_def_version v ON v.def_id=d.id AND v.is_current AND v.version_status='PUBLISHED'
  JOIN biz_flow_node n ON n.version_id=v.id
 WHERE d.deleted_at IS NULL AND n.node_type='APPROVAL' AND n.assignee_type='ROLE' AND n.approval_mode='ALL';
\echo === 所有 ALL 节点按形状分组（确认 COUNTERSIGN 不动） ===
SELECT n.node_type, n.assignee_type, n.approval_mode, count(*) FROM biz_flow_node n
  JOIN biz_flow_def_version v ON v.id=n.version_id AND v.is_current
  JOIN biz_flow_def d ON d.id=v.def_id AND d.deleted_at IS NULL
 WHERE n.approval_mode='ALL' GROUP BY 1,2,3 ORDER BY 1,2;
\echo === 目标节点是否用到 fallback / subprocess / config（V0359 漏拷的三列） ===
SELECT n.node_key, n.node_type, n.assignee_type, n.approval_mode,
       n.fallback_user_id, n.fallback_role_id, n.subprocess_def_id, n.config
  FROM biz_flow_def d
  JOIN biz_flow_def_version v ON v.def_id=d.id AND v.is_current
  JOIN biz_flow_node n ON n.version_id=v.id AND n.node_type='APPROVAL' AND n.assignee_type='ROLE' AND n.approval_mode='ALL'
 WHERE d.deleted_at IS NULL ORDER BY d.flow_code, n.node_key LIMIT 40;
\echo === 一个目标节点的完整快照 JSON ===
SELECT jsonb_pretty(nn.value) FROM biz_flow_def_version v
  JOIN biz_flow_def d ON d.id=v.def_id AND d.flow_code='po_approve' AND v.is_current,
       jsonb_array_elements(v.graph_snapshot->'nodes') nn
 WHERE nn.value->>'node_type'='APPROVAL' AND nn.value->>'assignee_type'='ROLE' AND nn.value->>'approval_mode'='ALL';
\echo === 一个边样本 ===
SELECT jsonb_pretty(e.value) FROM biz_flow_def_version v
  JOIN biz_flow_def d ON d.id=v.def_id AND d.flow_code='po_approve' AND v.is_current,
       jsonb_array_elements(v.graph_snapshot->'edges') WITH ORDINALITY e(value,ord)
 WHERE ord=1;
\echo === 实例引用 ===
SELECT count(*) AS instances_on_current FROM biz_flow_instance i JOIN biz_flow_def_version v ON v.id=i.version_id WHERE v.is_current;
