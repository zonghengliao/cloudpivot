\pset footer off
\echo == 1) biz_flow_state_edge 存活行（真机 E2E 跑完两轮后的实际数据）
SELECT entity, from_status, to_status, enabled, requires_reason,
       coalesce(label, '(默认文案)') AS label, deleted_at
  FROM biz_flow_state_edge
 WHERE deleted_at IS NULL
 ORDER BY entity, from_status, to_status;
\echo == 2) 每行的来源审批单（biz_status_log 的 APPLIED 行）
SELECT entity_no, from_status AS state, operator_name,
       left(reason, 60) AS reason
  FROM biz_status_log
 WHERE entity_type = 'FLOW_EDGE_CFG' AND from_status = 'APPLIED'
 ORDER BY created_at;
\echo == 3) 对应的审批单
SELECT instance_no, status, biz_no, instance_status
  FROM (SELECT instance_no, status, biz_no, status AS instance_status
          FROM biz_flow_instance WHERE biz_type = 'FLOW_EDGE_CFG'
         ORDER BY started_at) t;
