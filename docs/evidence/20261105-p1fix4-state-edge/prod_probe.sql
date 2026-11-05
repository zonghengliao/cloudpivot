\pset footer off
\echo == 1) FLOW_EDGE_CFG 审批单（全部）
SELECT instance_no, status, biz_no, started_at::date AS started, ended_at::date AS ended
  FROM biz_flow_instance
 WHERE biz_type = 'FLOW_EDGE_CFG'
 ORDER BY started_at;
\echo == 2) FLOW_EDGE_CFG 审计行（from_status -> to_status 计数）
SELECT from_status, to_status, count(*)
  FROM biz_status_log
 WHERE entity_type = 'FLOW_EDGE_CFG'
 GROUP BY 1, 2
 ORDER BY 1, 2;
\echo == 3) biz_flow_state_edge 行数（存活 / 全部）
SELECT count(*) FILTER (WHERE deleted_at IS NULL) AS alive,
       count(*) AS all_rows
  FROM biz_flow_state_edge;
